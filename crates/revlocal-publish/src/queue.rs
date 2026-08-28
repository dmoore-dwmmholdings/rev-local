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

use revlocal_core::{PublishAction, PublishActionId, PublishActionStatus, RunId, Timestamp};
use revlocal_store::{Pool, PublishActionStore, StoreError};
use tokio::sync::{Mutex, Semaphore};
use tokio::time::Instant;

use crate::retry::RetryPolicy;
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
    /// How many attempts an action gets, and how long between them (§11.6).
    pub retry: RetryPolicy,
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            concurrency: DEFAULT_CONCURRENCY,
            rate_limits: BTreeMap::new(),
            default_rate_limit: Duration::ZERO,
            retry: RetryPolicy::default(),
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
    /// Actions that failed and are scheduled to be tried again.
    pub retryable: usize,
    /// Actions that failed terminally.
    pub failed: usize,
    /// Actions whose target is not registered, so nothing was attempted.
    pub unroutable: usize,
    /// Actions approved and then edited, which are refused rather than sent.
    pub approval_stale: usize,
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

    /// The retry policy in force.
    pub const fn retry_policy(&self) -> &RetryPolicy {
        &self.config.retry
    }

    /// Persist an action. **Does not send it.**
    ///
    /// Returns as soon as the row is written, which is what keeps a slow target
    /// out of the review path. A duplicate `(target, idempotency_key)` is a
    /// success and returns the existing action: §11.6 wants at-least-once
    /// delivery with exactly-once effect, so an action already recorded means the
    /// effect is already accounted for. When that action was already sent, the row
    /// it returns carries the original `external_ref` and response — replaying is
    /// a no-op that hands back the first receipt rather than producing a second
    /// one (RL-702).
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

    /// Try one target's failed actions for one run again (RL-710).
    ///
    /// Scoped to one target on purpose. A run whose GitHub review posted and
    /// whose Andare issue failed should be retryable without re-posting the
    /// review, and `UNIQUE(target, idempotency_key)` would make the re-post a
    /// no-op anyway — but "it was a no-op" is a worse answer than "it was never
    /// attempted" when somebody is watching a tracker for duplicates.
    ///
    /// Returns the number of actions put back in the queue and what dispatching
    /// them did.
    pub async fn replay(
        &self,
        run_id: RunId,
        target: &str,
        now: Timestamp,
    ) -> Result<(u64, DispatchReport), QueueError> {
        let store = PublishActionStore::new(&self.pool);
        let requeued = store.reset_for_retry(run_id, target).await?;

        let due = store
            .list_pending(now)
            .await?
            .into_iter()
            .filter(|action| action.run_id == run_id && action.target == target)
            .collect();

        let report = self.dispatch(due, now).await?;
        Ok((requeued, report))
    }

    /// Deliver everything pending, up to `concurrency` at a time.    /// Deliver everything pending, up to `concurrency` at a time.
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

        let store_for_digests = PublishActionStore::new(&self.pool);

        for action in actions {
            // RL-803: the bytes a human approved are the bytes that go. Checked
            // here rather than at approval time because anything can write
            // `payload_json` in between — a replay, a second run, a bug. An action
            // nobody approved has no digest and passes straight through; the check
            // is about honouring an approval, not requiring one.
            if let Some(digest) = store_for_digests.approved_digest(action.id).await? {
                if revlocal_core::payload_digest(&action.payload_json) != digest {
                    tasks.spawn(async move { Outcome::ApprovalStale(action.id) });
                    continue;
                }
            }

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
                    Err(error) => Outcome::Failed(action.id, action.attempts, Box::new(error)),
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
                Outcome::Failed(id, attempts_before, error) => {
                    let attempts = attempts_before.saturating_add(1);
                    let retry_after = error.retry_after_secs().map(Duration::from_secs);

                    // Three outcomes, not two: retryable and within budget,
                    // retryable and out of attempts, or terminal. Collapsing the
                    // middle one into "pending" is how an action retries forever.
                    let delay = error
                        .is_retryable()
                        .then(|| self.config.retry.next_delay(id, attempts, retry_after))
                        .flatten();

                    let detail = if error.is_retryable() && delay.is_none() {
                        format!("gave up after {attempts} attempts: {error}")
                    } else {
                        error.to_string()
                    };

                    let status = if delay.is_some() {
                        PublishActionStatus::Pending
                    } else {
                        PublishActionStatus::Failed
                    };

                    store
                        .record_outcome(id, status, None, None, Some(&detail), now)
                        .await?;

                    if let Some(delay) = delay {
                        // Stored rather than held in memory: a restart often
                        // follows the burst of failures that caused it, and an
                        // in-memory schedule would make every pending action
                        // immediately due at exactly the wrong moment.
                        let step = chrono::Duration::from_std(delay)
                            .unwrap_or_else(|_| chrono::Duration::seconds(60));
                        store.schedule_retry(id, now + step).await?;
                        report.retryable += 1;
                    } else {
                        report.failed += 1;
                    }
                }
                Outcome::ApprovalStale(id) => {
                    // Terminal: the bytes a human agreed to are gone, and retrying
                    // would send the replacement they never saw.
                    store
                        .record_outcome(
                            id,
                            PublishActionStatus::Failed,
                            None,
                            None,
                            Some("the payload changed after it was approved; it was not sent"),
                            now,
                        )
                        .await?;
                    report.approval_stale += 1;
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
    /// The id, how many attempts had been made *before* this one, and why it
    /// failed.
    Failed(PublishActionId, u32, Box<PublishError>),
    Unroutable(PublishActionId),
    /// Approved, then edited. Never sent.
    ApprovalStale(PublishActionId),
}
