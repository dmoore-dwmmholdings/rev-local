//! Partial-failure reporting and per-target replay (RL-710, SPEC §11.6).
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::TimeZone;
use revlocal_core::{
    AutonomyMode, Capability, CapabilitySet, Change, ChangeId, ChangeKind, Depth, DiffStat,
    EngineKind, PublishAction, PublishActionId, PublishActionStatus, PublishReceipt, Repo, RepoId,
    RepoKind, RiskClass, Run, RunId, RunStatus, TargetHealth, Timestamp, TriggerSource, Usage,
};
use revlocal_publish::{
    PublishError, PublishQueue, PublishTarget, QueueConfig, RunPublishReport, TargetState,
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

/// A target whose behaviour can be changed between dispatch passes — which is what
/// a replay is for: the thing that was broken has been fixed.
struct FlakyTarget {
    id: String,
    fail: Arc<std::sync::atomic::AtomicBool>,
    calls: Arc<AtomicUsize>,
}

impl FlakyTarget {
    fn new(id: &str, failing: bool) -> Self {
        Self {
            id: id.to_owned(),
            fail: Arc::new(std::sync::atomic::AtomicBool::new(failing)),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }
}

#[async_trait]
impl PublishTarget for FlakyTarget {
    fn id(&self) -> &str {
        &self.id
    }

    async fn discover(&self) -> Result<CapabilitySet, PublishError> {
        Ok(CapabilitySet::new([Capability::CreateIssue]))
    }

    async fn execute(&self, action: &PublishAction) -> Result<PublishReceipt, PublishError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            // Terminal, so one pass is enough to reach `failed` — the point of
            // this test is the replay, not the backoff.
            return Err(PublishError::Rejected {
                target: self.id.clone(),
                status: Some(422),
                detail: "the project does not exist".to_owned(),
            });
        }
        Ok(PublishReceipt {
            external_ref: Some(format!("{}-{}", self.id, action.idempotency_key)),
            response_json: None,
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

// --- criterion 1: run detail exposes status per target ---------------------

#[tokio::test]
async fn partial_failure_reports_one_target_posted_and_another_failed() {
    let (_dir, pool, run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));

    let mut queue = PublishQueue::new(pool.clone(), QueueConfig::default());
    queue.register(Arc::new(FlakyTarget::new("github", false)));
    queue.register(Arc::new(FlakyTarget::new("andare", true)));

    queue
        .enqueue(&an_action(run, "github", "g1"))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    queue
        .enqueue(&an_action(run, "andare", "a1"))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    queue
        .dispatch_pending(at(4))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    let report = RunPublishReport::load(&pool, run)
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    let github = report.target("github").expect("github is reported");
    assert_eq!(github.state(), TargetState::Delivered);
    assert_eq!(github.sent, 1);
    assert_eq!(github.external_refs, vec!["github-g1".to_owned()]);

    let andare = report.target("andare").expect("andare is reported");
    assert_eq!(andare.state(), TargetState::Failed);
    assert!(
        andare
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("project does not exist")),
        "the run detail carries why, not just that: {:?}",
        andare.last_error
    );

    assert!(report.any_failed());
    assert_eq!(report.summary_lines().len(), 2);
}

/// A target that delivered some and failed others is neither, and saying
/// "delivered" would be a lie in the direction that matters.
#[tokio::test]
async fn partial_failure_distinguishes_partial_from_failed() {
    let (_dir, pool, run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));

    let target = Arc::new(FlakyTarget::new("andare", false));
    let switch = Arc::clone(&target.fail);
    let mut queue = PublishQueue::new(pool.clone(), QueueConfig::default());
    queue.register(target);

    queue
        .enqueue(&an_action(run, "andare", "a1"))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    queue
        .dispatch_pending(at(4))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    switch.store(true, Ordering::SeqCst);
    queue
        .enqueue(&an_action(run, "andare", "a2"))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    queue
        .dispatch_pending(at(5))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    let report = RunPublishReport::load(&pool, run)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let andare = report.target("andare").expect("reported");

    assert_eq!(andare.sent, 1);
    assert_eq!(andare.failed, 1);
    assert_eq!(andare.state(), TargetState::Partial);
}

// --- criterion 3: a failed target does not hold the run open ---------------

#[tokio::test]
async fn partial_failure_does_not_block_the_run_from_finishing() {
    let (_dir, pool, run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));

    let mut queue = PublishQueue::new(pool.clone(), QueueConfig::default());
    queue.register(Arc::new(FlakyTarget::new("github", false)));
    queue.register(Arc::new(FlakyTarget::new("andare", true)));

    queue
        .enqueue(&an_action(run, "github", "g1"))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    queue
        .enqueue(&an_action(run, "andare", "a1"))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    queue
        .dispatch_pending(at(4))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    let report = RunPublishReport::load(&pool, run)
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert!(report.any_failed());
    assert!(
        !report.blocks_completion(),
        "§11.6: a run can be done with GitHub posted and Andare failed. If a \
         failure held the run open, one unreachable system would leave every run \
         of the day stuck in `publishing`"
    );
}

#[tokio::test]
async fn partial_failure_outstanding_work_does_block_the_run() {
    let (_dir, pool, run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));

    let queue = PublishQueue::new(pool.clone(), QueueConfig::default());
    queue
        .enqueue(&an_action(run, "andare", "a1"))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    let report = RunPublishReport::load(&pool, run)
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert!(
        report.blocks_completion(),
        "something not yet attempted is outstanding work, not partial failure"
    );
    assert!(!report.any_failed());
}

// --- criterion 2: replay is scoped to one target ---------------------------

#[tokio::test]
async fn partial_failure_replay_retries_only_the_named_target() {
    let (_dir, pool, run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));

    let github = Arc::new(FlakyTarget::new("github", false));
    let andare = Arc::new(FlakyTarget::new("andare", true));
    let github_calls = Arc::clone(&github.calls);
    let andare_switch = Arc::clone(&andare.fail);

    let mut queue = PublishQueue::new(pool.clone(), QueueConfig::default());
    queue.register(github);
    queue.register(andare);

    queue
        .enqueue(&an_action(run, "github", "g1"))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    queue
        .enqueue(&an_action(run, "andare", "a1"))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    queue
        .dispatch_pending(at(4))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(github_calls.load(Ordering::SeqCst), 1);

    // Whatever was wrong with Andare is fixed.
    andare_switch.store(false, Ordering::SeqCst);

    let (requeued, report) = queue
        .replay(run, "andare", at(5))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(requeued, 1);
    assert_eq!(report.sent, 1);
    assert_eq!(
        github_calls.load(Ordering::SeqCst),
        1,
        "replaying Andare must not re-post the GitHub review — `it was a no-op` is \
         a worse answer than `it was never attempted` to somebody watching a \
         tracker for duplicates"
    );

    let after = RunPublishReport::load(&pool, run)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        after
            .target("andare")
            .map(revlocal_publish::TargetOutcome::state),
        Some(TargetState::Delivered)
    );
    assert!(!after.any_failed());
}

#[tokio::test]
async fn partial_failure_replay_gives_the_action_a_fresh_attempt_budget() {
    let (_dir, pool, run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));

    let andare = Arc::new(FlakyTarget::new("andare", true));
    let switch = Arc::clone(&andare.fail);
    let mut queue = PublishQueue::new(pool.clone(), QueueConfig::default());
    queue.register(andare);

    let action = queue
        .enqueue(&an_action(run, "andare", "a1"))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    queue
        .dispatch_pending(at(4))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    let failed = PublishActionStore::new(&pool)
        .get(action.id)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(failed.status, PublishActionStatus::Failed);
    assert_eq!(failed.attempts, 1);

    switch.store(false, Ordering::SeqCst);
    queue
        .replay(run, "andare", at(5))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    let replayed = PublishActionStore::new(&pool)
        .get(action.id)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(replayed.status, PublishActionStatus::Sent);
    assert_eq!(
        replayed.attempts, 1,
        "a replay resets the counter, so `attempts` means attempts in the current \
         delivery cycle — leaving it at the exhausted value would honour the \
         request in form and refuse it in substance"
    );
}

#[tokio::test]
async fn partial_failure_replaying_a_target_with_nothing_failed_is_a_no_op() {
    let (_dir, pool, run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));

    let github = Arc::new(FlakyTarget::new("github", false));
    let calls = Arc::clone(&github.calls);
    let mut queue = PublishQueue::new(pool.clone(), QueueConfig::default());
    queue.register(github);

    queue
        .enqueue(&an_action(run, "github", "g1"))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    queue
        .dispatch_pending(at(4))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    let (requeued, report) = queue
        .replay(run, "github", at(5))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(requeued, 0);
    assert_eq!(report.attempted(), 0);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "a delivered action must not be re-sent by a replay aimed at its target"
    );
}
