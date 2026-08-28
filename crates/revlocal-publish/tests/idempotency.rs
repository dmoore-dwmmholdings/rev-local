//! Idempotency and retry policy (RL-702, SPEC §11.6).
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

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
use revlocal_publish::{
    PublishError, PublishQueue, PublishTarget, QueueConfig, RetryPolicy, MAX_ATTEMPTS,
};
use revlocal_store::{open, ChangeStore, Pool, PublishActionStore, RepoStore, RunStore};
use tempfile::TempDir;

fn at(minute: u32) -> Timestamp {
    chrono::Utc
        .with_ymd_and_hms(2026, 8, 28, 12, minute, 0)
        .single()
        .unwrap_or_default()
}

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

/// A target that fails a fixed way, counting how often it was asked.
struct FailingTarget {
    id: String,
    error: fn(&str) -> PublishError,
    calls: Arc<AtomicUsize>,
}

impl FailingTarget {
    fn new(id: &str, error: fn(&str) -> PublishError) -> Self {
        Self {
            id: id.to_owned(),
            error,
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

fn rate_limited(target: &str) -> PublishError {
    PublishError::RateLimited {
        target: target.to_owned(),
        retry_after_secs: None,
    }
}

fn bad_request(target: &str) -> PublishError {
    PublishError::Rejected {
        target: target.to_owned(),
        status: Some(400),
        detail: "project is required".to_owned(),
    }
}

#[async_trait]
impl PublishTarget for FailingTarget {
    fn id(&self) -> &str {
        &self.id
    }

    async fn discover(&self) -> Result<CapabilitySet, PublishError> {
        Ok(CapabilitySet::new([Capability::CreateIssue]))
    }

    async fn execute(&self, _action: &PublishAction) -> Result<PublishReceipt, PublishError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err((self.error)(&self.id))
    }

    async fn health(&self) -> Result<TargetHealth, PublishError> {
        Ok(TargetHealth {
            reachable: true,
            capabilities: CapabilitySet::new([Capability::CreateIssue]),
            detail: None,
        })
    }
}

/// A target that always succeeds, counting calls.
struct CountingTarget {
    id: String,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl PublishTarget for CountingTarget {
    fn id(&self) -> &str {
        &self.id
    }

    async fn discover(&self) -> Result<CapabilitySet, PublishError> {
        Ok(CapabilitySet::new([Capability::CreateIssue]))
    }

    async fn execute(&self, action: &PublishAction) -> Result<PublishReceipt, PublishError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(PublishReceipt {
            external_ref: Some(format!("REVL-{}", n + 1)),
            response_json: Some(format!(r#"{{"key":"{}"}}"#, action.idempotency_key)),
            deduplicated: false,
        })
    }

    async fn health(&self) -> Result<TargetHealth, PublishError> {
        Ok(TargetHealth {
            reachable: true,
            capabilities: CapabilitySet::new([Capability::CreateIssue]),
            detail: None,
        })
    }
}

// --- criterion 1: replaying is a no-op that returns the first receipt ------

#[tokio::test]
async fn idempotency_replaying_a_sent_action_returns_the_original_receipt() {
    let (_dir, pool, run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));

    let calls = Arc::new(AtomicUsize::new(0));
    let mut queue = PublishQueue::new(pool.clone(), QueueConfig::default());
    queue.register(Arc::new(CountingTarget {
        id: "andare".to_owned(),
        calls: Arc::clone(&calls),
    }));

    let first = queue
        .enqueue(&an_action(run, "andare", "finding-abc"))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    queue
        .dispatch_pending(at(4))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    let sent = PublishActionStore::new(&pool)
        .get(first.id)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(sent.status, PublishActionStatus::Sent);
    assert_eq!(sent.external_ref.as_deref(), Some("REVL-1"));

    // The same finding, published again — a re-run, or a retry after a crash the
    // other side of the send.
    let replayed = queue
        .enqueue(&an_action(run, "andare", "finding-abc"))
        .await
        .unwrap_or_else(|e| panic!("a replay is a no-op, not a failure: {e}"));

    assert_eq!(replayed.id, sent.id);
    assert_eq!(
        replayed.external_ref.as_deref(),
        Some("REVL-1"),
        "the replay hands back the first receipt rather than producing a second"
    );

    // And nothing new is pending, so a dispatch does not send it again.
    let report = queue
        .dispatch_pending(at(5))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(report.attempted(), 0);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "UNIQUE(target, idempotency_key) is what makes double-publish structurally \
         impossible; one issue was filed, not two"
    );
}

// --- criterion 2: 429 retries with backoff, 400 does not ------------------

#[tokio::test]
async fn idempotency_a_rate_limit_is_retried_and_a_bad_request_is_not() {
    let (_dir, pool, run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));

    let limited = Arc::new(FailingTarget::new("github", rate_limited));
    let refused = Arc::new(FailingTarget::new("andare", bad_request));

    let mut queue = PublishQueue::new(pool.clone(), QueueConfig::default());
    queue.register(Arc::clone(&limited) as Arc<dyn PublishTarget>);
    queue.register(Arc::clone(&refused) as Arc<dyn PublishTarget>);

    let a = queue
        .enqueue(&an_action(run, "github", "k1"))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let b = queue
        .enqueue(&an_action(run, "andare", "k2"))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    let report = queue
        .dispatch_pending(at(4))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(report.retryable, 1);
    assert_eq!(report.failed, 1);

    let store = PublishActionStore::new(&pool);
    let limited_row = store.get(a.id).await.unwrap_or_else(|e| panic!("{e}"));
    let refused_row = store.get(b.id).await.unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(limited_row.status, PublishActionStatus::Pending);
    assert!(
        store
            .next_attempt_at(a.id)
            .await
            .unwrap_or_else(|e| panic!("{e}"))
            .is_some(),
        "a rate limit is retried, and the schedule is on the row so a restart does \
         not make it immediately due"
    );

    assert_eq!(
        refused_row.status,
        PublishActionStatus::Failed,
        "§11.6: 4xx is terminal — retrying a 400 changes nothing and hides the bug"
    );
    assert!(
        store
            .next_attempt_at(b.id)
            .await
            .unwrap_or_else(|e| panic!("{e}"))
            .is_none(),
        "nothing terminal should be scheduled"
    );
}

/// A retry that is not yet due is not attempted.
#[tokio::test]
async fn idempotency_a_scheduled_retry_waits_until_it_is_due() {
    let (_dir, pool, run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));

    let target = Arc::new(FailingTarget::new("github", rate_limited));
    let calls = Arc::clone(&target.calls);
    let mut queue = PublishQueue::new(pool.clone(), QueueConfig::default());
    queue.register(target);

    queue
        .enqueue(&an_action(run, "github", "k1"))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    queue
        .dispatch_pending(at(4))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // One second later: the backoff has not elapsed.
    let soon = at(4) + chrono::Duration::milliseconds(100);
    let report = queue
        .dispatch_pending(soon)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(report.attempted(), 0);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "it was tried again too soon"
    );

    // A minute later it is due.
    let report = queue
        .dispatch_pending(at(5))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(report.attempted(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn idempotency_an_action_is_given_up_on_after_five_attempts() {
    let (_dir, pool, run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));

    let target = Arc::new(FailingTarget::new("github", rate_limited));
    let calls = Arc::clone(&target.calls);
    let mut queue = PublishQueue::new(pool.clone(), QueueConfig::default());
    queue.register(target);

    let action = queue
        .enqueue(&an_action(run, "github", "k1"))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    // Each pass is far enough ahead that whatever backoff was chosen has elapsed.
    for minute in 4..12 {
        queue
            .dispatch_pending(at(minute))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
    }

    let row = PublishActionStore::new(&pool)
        .get(action.id)
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(
        calls.load(Ordering::SeqCst) as u32,
        MAX_ATTEMPTS,
        "§11.6 gives an action five attempts, not five retries and not forever"
    );
    assert_eq!(row.status, PublishActionStatus::Failed);
    assert_eq!(row.attempts, MAX_ATTEMPTS);
}

// --- criterion 3: the row shows what happened -----------------------------

#[tokio::test]
async fn idempotency_the_attempt_count_and_last_error_are_on_the_row() {
    let (_dir, pool, run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));

    let mut queue = PublishQueue::new(pool.clone(), QueueConfig::default());
    queue.register(Arc::new(FailingTarget::new("github", rate_limited)));

    let action = queue
        .enqueue(&an_action(run, "github", "k1"))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    queue
        .dispatch_pending(at(4))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let after_one = PublishActionStore::new(&pool)
        .get(action.id)
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(after_one.attempts, 1);
    assert!(
        after_one
            .error
            .as_deref()
            .is_some_and(|e| e.contains("rate limiting")),
        "the row carries why, not just that: {:?}",
        after_one.error
    );

    queue
        .dispatch_pending(at(6))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let after_two = PublishActionStore::new(&pool)
        .get(action.id)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(after_two.attempts, 2, "attempts accumulate across passes");
}

#[tokio::test]
async fn idempotency_giving_up_says_so_on_the_row() {
    let (_dir, pool, run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));

    let mut queue = PublishQueue::new(pool.clone(), QueueConfig::default());
    queue.register(Arc::new(FailingTarget::new("github", rate_limited)));

    let action = queue
        .enqueue(&an_action(run, "github", "k1"))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    for minute in 4..12 {
        queue
            .dispatch_pending(at(minute))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
    }

    let row = PublishActionStore::new(&pool)
        .get(action.id)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(
        row.error.as_deref().is_some_and(|e| e.contains("gave up")),
        "a run out of attempts must not look like a fresh failure: {:?}",
        row.error
    );
}

// --- criterion 4: jitter --------------------------------------------------

#[test]
fn idempotency_two_actions_retrying_together_do_not_align() {
    let policy = RetryPolicy::default();

    // Fifty actions that failed in the same second, as they would when a target
    // goes down mid-run.
    let delays: Vec<Duration> = (1..=50)
        .filter_map(|id| policy.next_delay(PublishActionId::new(id), 1, None))
        .collect();

    assert_eq!(delays.len(), 50);

    let distinct: std::collections::BTreeSet<u128> =
        delays.iter().map(std::time::Duration::as_millis).collect();
    assert!(
        distinct.len() > 40,
        "jitter exists to break up a thundering herd; {} of 50 delays were \
         distinct",
        distinct.len()
    );

    // And they stay inside the band, so jitter has not become "any delay at all".
    for delay in &delays {
        assert!(
            *delay >= Duration::from_millis(750) && *delay <= Duration::from_millis(1250),
            "{delay:?} is outside 1s ± 25%"
        );
    }
}

#[test]
fn idempotency_the_same_action_varies_across_its_own_attempts() {
    let policy = RetryPolicy::default();
    let id = PublishActionId::new(7);

    let first = policy.next_delay(id, 1, None).unwrap_or_default();
    let second = policy.next_delay(id, 2, None).unwrap_or_default();
    let third = policy.next_delay(id, 3, None).unwrap_or_default();

    assert!(second > first, "{first:?} then {second:?}");
    assert!(third > second, "{second:?} then {third:?}");
}

#[test]
fn idempotency_backoff_is_capped_and_bounded_by_attempts() {
    let policy = RetryPolicy::default();
    let id = PublishActionId::new(3);

    assert!(
        policy.next_delay(id, MAX_ATTEMPTS, None).is_none(),
        "the fifth attempt is the last one, not the last retry"
    );

    let long = RetryPolicy {
        max_attempts: 40,
        ..RetryPolicy::default()
    };
    let late = long.next_delay(id, 30, None).unwrap_or_default();
    assert!(
        late <= Duration::from_secs(75),
        "{late:?} exceeds the 60s cap plus its jitter band"
    );
}

/// A target that says how long to wait is obeyed rather than second-guessed.
#[test]
fn idempotency_a_retry_after_from_the_target_wins_over_the_curve() {
    let policy = RetryPolicy::default();
    let id = PublishActionId::new(11);

    let asked = policy
        .next_delay(id, 1, Some(Duration::from_secs(30)))
        .unwrap_or_default();
    assert_eq!(
        asked,
        Duration::from_secs(30),
        "backing off less than a target asked is how a rate limit becomes a ban"
    );

    let absurd = policy
        .next_delay(id, 1, Some(Duration::from_secs(3600)))
        .unwrap_or_default();
    assert_eq!(absurd, Duration::from_secs(60), "still capped");
}

/// Determinism: the schedule is reproducible, which is what lets the criterion
/// above be asserted rather than sampled (ADR 0024).
#[test]
fn idempotency_the_backoff_schedule_is_reproducible() {
    let policy = RetryPolicy::default();
    let id = PublishActionId::new(42);

    for attempt in 1..MAX_ATTEMPTS {
        assert_eq!(
            policy.next_delay(id, attempt, None),
            policy.next_delay(id, attempt, None)
        );
    }
}
