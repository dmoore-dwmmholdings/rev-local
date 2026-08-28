//! The publish action queue (RL-701, SPEC §11.1, §11.6).
//!
//! Runs against a real SQLite database rather than a fake store, because two of
//! the three criteria are statements about what is *on disk* when something goes
//! wrong. A fake that agreed with the queue about ordering would prove only that
//! they agree.
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::TimeZone;
use revlocal_core::{
    AutonomyMode, Capability, CapabilitySet, Change, ChangeId, ChangeKind, Depth, DiffStat,
    EngineKind, PublishAction, PublishActionId, PublishActionStatus, PublishReceipt, Repo, RepoId,
    RepoKind, RiskClass, Run, RunId, RunStatus, TargetHealth, Timestamp, TriggerSource, Usage,
};
use revlocal_publish::{PublishError, PublishQueue, PublishTarget, QueueConfig};
use revlocal_store::{open, ChangeStore, Pool, PublishActionStore, RepoStore, RunStore};
use tempfile::TempDir;

fn at(minute: u32) -> Timestamp {
    chrono::Utc
        .with_ymd_and_hms(2026, 8, 28, 12, minute, 0)
        .single()
        .unwrap_or_default()
}

/// A database with one repo, one change and one run to hang actions off.
async fn seeded() -> Result<(TempDir, Pool, RunId), Box<dyn std::error::Error>> {
    let dir = TempDir::new()?;
    let pool = open(&dir.path().join("rev-local.db")).await?;

    let repo = RepoStore::new(&pool)
        .insert(&Repo {
            id: RepoId::new(0),
            name: "rev-local".to_owned(),
            kind: RepoKind::Git,
            local_path: None,
            remote_url: None,
            default_branch: Some("main".to_owned()),
            engine: EngineKind::Mock,
            autonomy: AutonomyMode::DryRun,
            enabled: true,
            config_json: "{}".to_owned(),
            created_at: at(0),
            updated_at: at(0),
        })
        .await?;

    let change = ChangeStore::new(&pool)
        .upsert(&Change {
            id: ChangeId::new(0),
            repo_id: repo.id,
            kind: ChangeKind::Commit,
            external_id: "deadbeef".to_owned(),
            title: None,
            author_name: None,
            author_email: None,
            authored_at: None,
            branch: None,
            base_ref: None,
            head_ref: None,
            url: None,
            diff_stat: DiffStat::default(),
            detected_at: at(1),
        })
        .await?;

    let run = RunStore::new(&pool)
        .insert(&Run {
            id: RunId::new(0),
            change_id: change.id,
            attempt: 1,
            status: RunStatus::Publishing,
            engine: EngineKind::Mock,
            depth: Depth::Standard,
            trigger: TriggerSource::Manual,
            skip_reason: None,
            error: None,
            degraded: None,
            usage: Usage::default(),
            started_at: Some(at(2)),
            finished_at: None,
            transcript_path: None,
            truncated: false,
            omitted_files: Vec::new(),
            verdict: None,
            summary: None,
            created_at: at(2),
        })
        .await?;

    Ok((dir, pool, run.id))
}

fn an_action(run_id: RunId, target: &str, key: &str) -> PublishAction {
    PublishAction {
        id: PublishActionId::new(0),
        run_id,
        finding_id: None,
        target: target.to_owned(),
        capability: Capability::CreateIssue,
        risk: RiskClass::High,
        idempotency_key: key.to_owned(),
        payload_json: "{}".to_owned(),
        status: PublishActionStatus::Pending,
        attempts: 0,
        response_json: None,
        external_ref: None,
        error: None,
        created_at: at(3),
        sent_at: None,
    }
}

// --- a target that reports on itself --------------------------------------

/// What a fake target should do when asked to execute.
#[derive(Debug, Clone)]
enum Behaviour {
    Succeed,
    Delay(Duration),
    Fail(&'static str),
}

/// A target that counts what happened to it.
struct FakeTarget {
    id: String,
    behaviour: Behaviour,
    calls: Arc<AtomicUsize>,
    in_flight: Arc<AtomicUsize>,
    peak_in_flight: Arc<AtomicUsize>,
}

impl FakeTarget {
    fn new(id: &str, behaviour: Behaviour) -> Self {
        Self {
            id: id.to_owned(),
            behaviour,
            calls: Arc::new(AtomicUsize::new(0)),
            in_flight: Arc::new(AtomicUsize::new(0)),
            peak_in_flight: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[async_trait]
impl PublishTarget for FakeTarget {
    fn id(&self) -> &str {
        &self.id
    }

    async fn discover(&self) -> Result<CapabilitySet, PublishError> {
        Ok(CapabilitySet::new([Capability::CreateIssue]))
    }

    async fn execute(&self, action: &PublishAction) -> Result<PublishReceipt, PublishError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak_in_flight.fetch_max(now, Ordering::SeqCst);

        let result = match &self.behaviour {
            Behaviour::Succeed => Ok(PublishReceipt {
                external_ref: Some(format!("{}-{}", self.id, action.idempotency_key)),
                response_json: Some("{}".to_owned()),
                deduplicated: false,
            }),
            Behaviour::Delay(delay) => {
                tokio::time::sleep(*delay).await;
                Ok(PublishReceipt {
                    external_ref: None,
                    response_json: None,
                    deduplicated: false,
                })
            }
            Behaviour::Fail("retryable") => Err(PublishError::Transport {
                target: self.id.clone(),
                detail: "connection reset".to_owned(),
            }),
            Behaviour::Fail(_) => Err(PublishError::Rejected {
                target: self.id.clone(),
                status: Some(422),
                detail: "the project does not exist".to_owned(),
            }),
        };

        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        result
    }

    async fn health(&self) -> Result<TargetHealth, PublishError> {
        Ok(TargetHealth {
            reachable: true,
            capabilities: CapabilitySet::new([Capability::CreateIssue]),
            detail: None,
        })
    }
}

// --- criterion 1: persisted before attempted, retried on the next pass -----

#[tokio::test]
async fn queue_enqueue_persists_the_row_and_does_not_touch_the_target() {
    let (_dir, pool, run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));

    let target = Arc::new(FakeTarget::new("github", Behaviour::Succeed));
    let calls = Arc::clone(&target.calls);

    let mut queue = PublishQueue::new(pool.clone(), QueueConfig::default());
    queue.register(target);

    let stored = queue
        .enqueue(&an_action(run, "github", "k1"))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "§11.6: the action is written before it is attempted, so enqueuing must \
         not reach the target at all"
    );
    assert_eq!(stored.status, PublishActionStatus::Pending);

    let on_disk = PublishActionStore::new(&pool)
        .get(stored.id)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(on_disk.status, PublishActionStatus::Pending);
    assert_eq!(on_disk.attempts, 0);
}

/// The criterion, stated as the crash it is about: a row exists, nothing sent it,
/// and the next pass — which on startup is the first pass — delivers it.
#[tokio::test]
async fn queue_a_row_persisted_before_a_crash_is_delivered_on_the_next_pass() {
    let (_dir, pool, run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));

    // Enqueue, then drop the queue entirely: this is the process dying between
    // the insert and the send.
    let stored = {
        let queue = PublishQueue::new(pool.clone(), QueueConfig::default());
        queue
            .enqueue(&an_action(run, "github", "k1"))
            .await
            .unwrap_or_else(|e| panic!("{e}"))
    };

    // A fresh process, with the target registered.
    let target = Arc::new(FakeTarget::new("github", Behaviour::Succeed));
    let calls = Arc::clone(&target.calls);
    let mut queue = PublishQueue::new(pool.clone(), QueueConfig::default());
    queue.register(target);

    let report = queue
        .dispatch_pending(at(4))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(report.sent, 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let on_disk = PublishActionStore::new(&pool)
        .get(stored.id)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(on_disk.status, PublishActionStatus::Sent);
    assert_eq!(on_disk.external_ref.as_deref(), Some("github-k1"));
    assert!(on_disk.sent_at.is_some());
}

// --- criterion 2: a slow target cannot block reviewing --------------------

#[tokio::test]
async fn queue_a_slow_target_does_not_delay_enqueue() {
    let (_dir, pool, run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));

    // Slower than any review would tolerate.
    let target = Arc::new(FakeTarget::new(
        "slow",
        Behaviour::Delay(Duration::from_secs(30)),
    ));
    let mut queue = PublishQueue::new(pool.clone(), QueueConfig::default());
    queue.register(target);

    let started = std::time::Instant::now();
    queue
        .enqueue(&an_action(run, "slow", "k1"))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(5),
        "enqueuing took {elapsed:?}; a target that hangs must not hold up the run \
         that produced the finding"
    );
}

#[tokio::test]
async fn queue_never_exceeds_the_configured_concurrency() {
    let (_dir, pool, run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));

    let target = Arc::new(FakeTarget::new(
        "github",
        Behaviour::Delay(Duration::from_millis(50)),
    ));
    let peak = Arc::clone(&target.peak_in_flight);

    let mut queue = PublishQueue::new(
        pool.clone(),
        QueueConfig {
            concurrency: 4,
            ..QueueConfig::default()
        },
    );
    queue.register(target);

    for n in 0..12 {
        queue
            .enqueue(&an_action(run, "github", &format!("k{n}")))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
    }

    let report = queue
        .dispatch_pending(at(4))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(report.sent, 12);
    assert!(
        peak.load(Ordering::SeqCst) <= 4,
        "SPEC §11.1 caps in-flight actions at four; peak was {}",
        peak.load(Ordering::SeqCst)
    );
    assert!(
        peak.load(Ordering::SeqCst) > 1,
        "if nothing ever ran concurrently the cap proves nothing"
    );
}

// --- criterion 3: per-target rate limits ----------------------------------

#[tokio::test]
async fn queue_honours_a_per_target_rate_limit() {
    let (_dir, pool, run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));

    let target = Arc::new(FakeTarget::new("github", Behaviour::Succeed));
    let mut limits = BTreeMap::new();
    limits.insert("github".to_owned(), Duration::from_millis(120));

    let mut queue = PublishQueue::new(
        pool.clone(),
        QueueConfig {
            rate_limits: limits,
            ..QueueConfig::default()
        },
    );
    queue.register(target);

    for n in 0..3 {
        queue
            .enqueue(&an_action(run, "github", &format!("k{n}")))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
    }

    let started = std::time::Instant::now();
    let report = queue
        .dispatch_pending(at(4))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let elapsed = started.elapsed();

    assert_eq!(report.sent, 3);
    assert!(
        elapsed >= Duration::from_millis(240),
        "three sends at a 120ms minimum gap cannot finish in {elapsed:?}"
    );
}

/// The reason the limiter is keyed by target: GitHub's quota is not Andare's.
#[tokio::test]
async fn queue_one_target_s_rate_limit_does_not_throttle_another() {
    let (_dir, pool, run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));

    let slow_lane = Arc::new(FakeTarget::new("github", Behaviour::Succeed));
    let fast_lane = Arc::new(FakeTarget::new("andare", Behaviour::Succeed));
    let andare_calls = Arc::clone(&fast_lane.calls);

    let mut limits = BTreeMap::new();
    limits.insert("github".to_owned(), Duration::from_millis(200));

    let mut queue = PublishQueue::new(
        pool.clone(),
        QueueConfig {
            rate_limits: limits,
            ..QueueConfig::default()
        },
    );
    queue.register(slow_lane);
    queue.register(fast_lane);

    for n in 0..3 {
        queue
            .enqueue(&an_action(run, "github", &format!("g{n}")))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        queue
            .enqueue(&an_action(run, "andare", &format!("a{n}")))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
    }

    let started = std::time::Instant::now();
    queue
        .dispatch_pending(at(4))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let elapsed = started.elapsed();

    assert_eq!(andare_calls.load(Ordering::SeqCst), 3);
    assert!(
        elapsed < Duration::from_millis(1200),
        "six sends took {elapsed:?}; if the limiter were global, GitHub's 200ms gap \
         would have been applied to Andare too"
    );
}

// --- how failures are recorded --------------------------------------------

#[tokio::test]
async fn queue_a_terminal_failure_is_marked_failed_rather_than_left_pending() {
    let (_dir, pool, run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));

    let mut queue = PublishQueue::new(pool.clone(), QueueConfig::default());
    queue.register(Arc::new(FakeTarget::new(
        "github",
        Behaviour::Fail("terminal"),
    )));

    let stored = queue
        .enqueue(&an_action(run, "github", "k1"))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let report = queue
        .dispatch_pending(at(4))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(report.failed, 1);
    let on_disk = PublishActionStore::new(&pool)
        .get(stored.id)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        on_disk.status,
        PublishActionStatus::Failed,
        "§11.6: 4xx is terminal, and a terminal failure left pending would be \
         retried forever"
    );
    assert_eq!(on_disk.attempts, 1);
    assert!(on_disk.error.is_some());
}

#[tokio::test]
async fn queue_a_retryable_failure_stays_pending() {
    let (_dir, pool, run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));

    let mut queue = PublishQueue::new(pool.clone(), QueueConfig::default());
    queue.register(Arc::new(FakeTarget::new(
        "github",
        Behaviour::Fail("retryable"),
    )));

    let stored = queue
        .enqueue(&an_action(run, "github", "k1"))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let report = queue
        .dispatch_pending(at(4))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(report.retryable, 1);
    let on_disk = PublishActionStore::new(&pool)
        .get(stored.id)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        on_disk.status,
        PublishActionStatus::Pending,
        "when to try again is RL-702's decision; leaving the row deliverable is \
         this pass's job, and marking it failed would take that decision away"
    );
    assert_eq!(on_disk.attempts, 1);
}

/// An action nobody can deliver must not look delivered.
#[tokio::test]
async fn queue_an_action_for_an_unregistered_target_is_reported_not_dropped() {
    let (_dir, pool, run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));

    let queue = PublishQueue::new(pool.clone(), QueueConfig::default());
    let stored = queue
        .enqueue(&an_action(run, "trama", "k1"))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    let report = queue
        .dispatch_pending(at(4))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(report.unroutable, 1);
    assert_eq!(report.attempted(), 0, "nothing was attempted");

    let on_disk = PublishActionStore::new(&pool)
        .get(stored.id)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(on_disk.status, PublishActionStatus::Pending);
    assert!(
        on_disk
            .error
            .as_deref()
            .is_some_and(|e| e.contains("target")),
        "the row says why it did not go: {:?}",
        on_disk.error
    );
}

// --- idempotency -----------------------------------------------------------

#[tokio::test]
async fn queue_enqueuing_the_same_action_twice_yields_one_row() {
    let (_dir, pool, run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));

    let queue = PublishQueue::new(pool.clone(), QueueConfig::default());
    let first = queue
        .enqueue(&an_action(run, "github", "same-key"))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let second = queue
        .enqueue(&an_action(run, "github", "same-key"))
        .await
        .unwrap_or_else(|e| panic!("a redelivery is a success, not a failure: {e}"));

    assert_eq!(
        first.id, second.id,
        "§11.6: UNIQUE(target, idempotency_key) makes double-publish structurally \
         impossible, so the second enqueue must find the first rather than fail"
    );
}
