//! The publish action queue (RL-701, SPEC §11.1, §11.6).
//!
//! # Enqueuing writes the row and stops
//!
//! §11.6's first sentence is that every action is written to `publish_action`
//! **before** it is attempted, and RL-701's criteria turn that into two
//! properties that are really the same property:
//!
//!   * a crash between persisting and sending leaves a pending row, which is
//!     retried on startup;
//!   * a slow target cannot block reviewing.
//!
//! Both fall out of [`PublishQueue::enqueue`] doing nothing but the insert. It
//! never touches a target, so a target that hangs forever cannot delay the run
//! that produced the finding, and there is no window in which an action has been
//! sent but not recorded. The opposite window — recorded but not sent — is the
//! safe one, because `UNIQUE(target, idempotency_key)` makes a redelivery land on
//! the same effect.
//!
//! Delivery is a separate pass, [`PublishQueue::dispatch_pending`], which the
//! daemon runs on startup and after each run. "Retried on startup" is not special
//! handling; it is the same pass finding rows that were already there.
//!
//! # Bounded, and bounded per target
//!
//! Concurrency is capped (§11.1: four) so a backlog of findings cannot open a
//! connection per action, and each target has its own minimum interval between
//! sends so one target's rate limit does not throttle the others. The limiter is
//! keyed by target for that reason: GitHub and Andare have nothing to do with each
//! other's quotas.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use revlocal_core::{PublishAction, PublishActionId, PublishActionStatus, Timestamp};
use revlocal_store::{Pool, PublishActionStore, StoreError};
use tokio::sync::{Mutex, Semaphore};
use tokio::time::Instant;

use crate::target::{PublishError, PublishTarget};

/// SPEC §11.1: four actions in flight at once.
pub const DEFAULT_CONCURRENCY: usize = 4;

/// How the queue is bounded.
#[derive(Debug, Clone)]
pub struct QueueConfig {
    /// How many actions may be in flight at once.
    pub concurrency: usize,
    /// Minimum gap between two sends to the same target.
    pub rate_limits: BTreeMap<String, Duration>,
    /// Minimum gap applied to targets with no specific limit.
    pub default_rate_limit: Duration,
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            concurrency: DEFAULT_CONCURRENCY,
            rate_limits: BTreeMap::new(),
            default_rate_limit: Duration::ZERO,
        }
    }
}

impl QueueConfig {
    /// The gap to leave before the next send to `target`.
    fn interval_for(&self, target: &str) -> Duration {
        self.rate_limits
            .get(target)
            .copied()
            .unwrap_or(self.default_rate_limit)
    }
}

/// What the queue itself can fail with.
///
/// Delivery failures are [`PublishError`] and are recorded against the action;
/// these are the failures that stop the queue from doing its job at all.
#[derive(Debug, thiserror::Error)]
pub enum QueueError {
    /// The action could not be persisted or updated.
    #[error(transparent)]
    Store(#[from] StoreError),

    /// An action names a target that is not registered.
    ///
    /// Not a delivery failure: nothing was attempted, and retrying changes
    /// nothing until the target is configured.
    #[error("no target `{target}` is registered\n  try: add it to your config, or remove the capability that asked for it")]
    UnknownTarget {
        /// What the action asked for.
        target: String,
    },
}

/// What one dispatch pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DispatchReport {
    /// Actions delivered.
    pub sent: usize,
    /// Actions that failed and may be retried.
    pub retryable: usize,
    /// Actions that failed terminally.
    pub failed: usize,
    /// Actions whose target is not registered, so nothing was attempted.
    pub unroutable: usize,
}

impl DispatchReport {
    /// How many actions were attempted.
    pub const fn attempted(&self) -> usize {
        self.sent + self.retryable + self.failed
    }
}

/// A per-target minimum interval between sends.
///
/// Deliberately not a token bucket. §11.6's rate limits are about being a good
/// citizen against a remote quota, and the failure being avoided is a burst; a
/// bucket would allow exactly the burst this is here to prevent.
#[derive(Debug, Default)]
struct Limiter {
    last_send: Mutex<BTreeMap<String, Instant>>,
}

impl Limiter {
    /// Wait until `target` may be sent to again, then claim the slot.
    async fn acquire(&self, target: &str, interval: Duration) {
        if interval.is_zero() {
            return;
        }

        // The lock is held across the sleep on purpose: two actions for the same
        // target must not both observe the same "last send" and then both go. It
        // is per-target contention only, so a slow target still cannot delay a
        // different one.
        let mut last = self.last_send.lock().await;
        let now = Instant::now();
        if let Some(previous) = last.get(target) {
            let elapsed = now.saturating_duration_since(*previous);
            if elapsed < interval {
                tokio::time::sleep(interval - elapsed).await;
            }
        }
        last.insert(target.to_owned(), Instant::now());
    }
}

/// The publish queue.
pub struct PublishQueue {
    pool: Pool,
    targets: BTreeMap<String, Arc<dyn PublishTarget>>,
    config: QueueConfig,
    limiter: Arc<Limiter>,
}

impl std::fmt::Debug for PublishQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PublishQueue")
            .field("targets", &self.targets.keys().collect::<Vec<_>>())
            .field("concurrency", &self.config.concurrency)
            .finish_non_exhaustive()
    }
}

impl PublishQueue {
    /// A queue over `pool`, with no targets registered yet.
    pub fn new(pool: Pool, config: QueueConfig) -> Self {
        Self {
            pool,
            targets: BTreeMap::new(),
            config,
            limiter: Arc::new(Limiter::default()),
        }
    }

    /// Register a target. Replaces any target already registered under its id.
    pub fn register(&mut self, target: Arc<dyn PublishTarget>) {
        self.targets.insert(target.id().to_owned(), target);
    }

    /// The registered target ids, in order.
    pub fn target_ids(&self) -> impl Iterator<Item = &str> {
        self.targets.keys().map(String::as_str)
    }

    /// Persist an action. **Does not send it.**
    ///
    /// Returns as soon as the row is written, which is what keeps a slow target
    /// out of the review path. A duplicate `(target, idempotency_key)` is a
    /// success and returns the existing action: §11.6 wants at-least-once
    /// delivery with exactly-once effect, so an action already recorded means the
    /// effect is already accounted for.
    pub async fn enqueue(&self, action: &PublishAction) -> Result<PublishAction, QueueError> {
        let store = PublishActionStore::new(&self.pool);
        match store.insert(action).await {
            Ok(stored) => Ok(stored),
            Err(StoreError::AlreadyExists { .. }) => {
                let existing = store
                    .find_by_idempotency_key(&action.target, &action.idempotency_key)
                    .await?;
                existing.map_or_else(
                    || {
                        // The insert said the row exists and the lookup says it
                        // does not. Nothing sensible follows from that, and
                        // inventing a row would be worse than saying so.
                        Err(QueueError::Store(StoreError::NotFound {
                            entity: "publish_action",
                            key: format!(
                                "target={}, idempotency_key={}",
                                action.target, action.idempotency_key
                            ),
                        }))
                    },
                    Ok,
                )
            }
            Err(other) => Err(QueueError::Store(other)),
        }
    }

    /// Deliver everything pending, up to `concurrency` at a time.
    ///
    /// This is what the daemon calls on startup, which is all "retried on
    /// startup" means: rows that were persisted and never sent are simply still
    /// pending, and this pass finds them.
    pub async fn dispatch_pending(&self, now: Timestamp) -> Result<DispatchReport, QueueError> {
        let store = PublishActionStore::new(&self.pool);
        let pending = store.list_pending(now).await?;
        self.dispatch(pending, now).await
    }

    /// Deliver a specific set of actions.
    async fn dispatch(
        &self,
        actions: Vec<PublishAction>,
        now: Timestamp,
    ) -> Result<DispatchReport, QueueError> {
        let permits = Arc::new(Semaphore::new(self.config.concurrency.max(1)));
        let mut tasks = tokio::task::JoinSet::new();

        for action in actions {
            let Some(target) = self.targets.get(&action.target).cloned() else {
                tasks.spawn(async move { Outcome::Unroutable(action.id) });
                continue;
            };

            let permits = Arc::clone(&permits);
            let limiter = Arc::clone(&self.limiter);
            let interval = self.config.interval_for(&action.target);

            tasks.spawn(async move {
                // Held for the whole attempt, so `concurrency` bounds actions in
                // flight rather than actions started.
                let _permit = permits.acquire_owned().await;
                limiter.acquire(&action.target, interval).await;

                match target.execute(&action).await {
                    Ok(receipt) => Outcome::Sent(action.id, Box::new(receipt)),
                    Err(error) => Outcome::Failed(action.id, Box::new(error)),
                }
            });
        }

        let mut report = DispatchReport::default();
        let store = PublishActionStore::new(&self.pool);

        while let Some(joined) = tasks.join_next().await {
            // A panicking target must not take the queue with it: the action
            // stays pending and the next pass picks it up.
            let Ok(outcome) = joined else {
                report.retryable += 1;
                continue;
            };

            match outcome {
                Outcome::Sent(id, receipt) => {
                    store
                        .record_outcome(
                            id,
                            PublishActionStatus::Sent,
                            receipt.external_ref.as_deref(),
                            receipt.response_json.as_deref(),
                            None,
                            now,
                        )
                        .await?;
                    report.sent += 1;
                }
                Outcome::Failed(id, error) => {
                    let retryable = error.is_retryable();
                    // A retryable failure leaves the row pending. Deciding *when*
                    // to try again is RL-702's; leaving it deliverable is this
                    // pass's job, and marking it failed here would take that
                    // decision away.
                    let status = if retryable {
                        PublishActionStatus::Pending
                    } else {
                        PublishActionStatus::Failed
                    };
                    store
                        .record_outcome(id, status, None, None, Some(&error.to_string()), now)
                        .await?;
                    if retryable {
                        report.retryable += 1;
                    } else {
                        report.failed += 1;
                    }
                }
                Outcome::Unroutable(id) => {
                    store
                        .record_outcome(
                            id,
                            PublishActionStatus::Pending,
                            None,
                            None,
                            Some("no such target is registered"),
                            now,
                        )
                        .await?;
                    report.unroutable += 1;
                }
            }
        }

        Ok(report)
    }
}

/// What one attempt produced. Boxed payloads keep the enum small.
enum Outcome {
    Sent(PublishActionId, Box<revlocal_core::PublishReceipt>),
    Failed(PublishActionId, Box<PublishError>),
    Unroutable(PublishActionId),
}
