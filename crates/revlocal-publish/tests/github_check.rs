//! The `rev-local/review` check run and commit comments (RL-704, SPEC §11.3).
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use chrono::TimeZone;
use revlocal_core::{
    AutonomyMode, Capability, Change, ChangeId, ChangeKind, Depth, DiffStat, EngineKind,
    PublishAction, PublishActionId, PublishActionStatus, Repo, RepoId, RepoKind, RiskClass, Run,
    RunId, RunStatus, Timestamp, TriggerSource, Usage, Verdict,
};
use revlocal_publish::{
    conclusion_for, gh_commit_comment, gh_set_check, unresolved_check, CheckConclusion,
    CheckPayload, CheckStatus, ReviewOptions, CHECK_NAME,
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

/// A run in a given state, with an error where one applies.
fn a_run(status: RunStatus, error: Option<&str>) -> Run {
    Run {
        id: RunId::new(1),
        change_id: ChangeId::new(1),
        attempt: 1,
        status,
        engine: EngineKind::Mock,
        depth: Depth::Standard,
        trigger: TriggerSource::Manual,
        skip_reason: None,
        error: error.map(str::to_owned),
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
    }
}

/// A `SetCheck` action carrying `payload`, in a given delivery state.
fn check_action(payload: &CheckPayload, status: PublishActionStatus, key: &str) -> PublishAction {
    PublishAction {
        id: PublishActionId::new(1),
        run_id: RunId::new(1),
        finding_id: None,
        target: "github".to_owned(),
        capability: Capability::SetCheck,
        risk: RiskClass::Low,
        idempotency_key: key.to_owned(),
        payload_json: serde_json::to_string(payload).unwrap_or_default(),
        status,
        attempts: 0,
        response_json: None,
        external_ref: None,
        error: None,
        created_at: at(3),
        sent_at: None,
    }
}

// --- criterion 2: failure only when the repo asked for it -----------------

#[test]
fn github_check_failure_requires_block_on_findings() {
    let default = ReviewOptions::default();
    assert!(!default.block_on_findings, "SPEC §11.3: default is false");

    assert_eq!(
        conclusion_for(Verdict::RequestChanges, default),
        CheckConclusion::Neutral,
        "a failing required check stops a merge; doing that by default decides \
         something about somebody's process that they did not ask for"
    );
    assert_eq!(
        conclusion_for(
            Verdict::RequestChanges,
            ReviewOptions {
                block_on_findings: true,
                ..default
            }
        ),
        CheckConclusion::Failure
    );

    // §11.3: success on approve or comment, whatever the block setting.
    for options in [
        default,
        ReviewOptions {
            block_on_findings: true,
            ..default
        },
    ] {
        assert_eq!(
            conclusion_for(Verdict::Approve, options),
            CheckConclusion::Success
        );
        assert_eq!(
            conclusion_for(Verdict::Comment, options),
            CheckConclusion::Success
        );
    }
}

// --- criterion 1: started in progress, always resolved --------------------

#[test]
fn github_check_starts_in_progress_with_no_conclusion() {
    let payload = CheckPayload::starting("acme/widgets", "abc123");
    assert_eq!(payload.status, CheckStatus::InProgress);
    assert!(
        payload.conclusion.is_none(),
        "GitHub rejects a conclusion on an in_progress check"
    );

    let request = gh_set_check(&payload).unwrap_or_else(|e| panic!("{e}"));
    let body: serde_json::Value =
        serde_json::from_str(&request.stdin.unwrap_or_default()).unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(body["name"], CHECK_NAME);
    assert_eq!(body["head_sha"], "abc123");
    assert_eq!(body["status"], "in_progress");
    assert!(body.get("conclusion").is_none());
    assert_eq!(
        request.args[3], "repos/acme/widgets/check-runs",
        "{:?}",
        request.args
    );
}

#[test]
fn github_check_a_resolved_check_carries_its_conclusion() {
    let payload = CheckPayload::resolved(
        "acme/widgets",
        "abc123",
        Verdict::Comment,
        ReviewOptions::default(),
        3,
    );
    assert_eq!(payload.status, CheckStatus::Completed);
    assert_eq!(payload.conclusion, Some(CheckConclusion::Success));
    assert!(payload.title.contains('3'), "{}", payload.title);

    let request = gh_set_check(&payload).unwrap_or_else(|e| panic!("{e}"));
    let body: serde_json::Value =
        serde_json::from_str(&request.stdin.unwrap_or_default()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(body["status"], "completed");
    assert_eq!(body["conclusion"], "success");
}

#[test]
fn github_check_a_clean_run_says_so_rather_than_counting_to_zero() {
    let payload =
        CheckPayload::resolved("a/b", "sha", Verdict::Approve, ReviewOptions::default(), 0);
    assert_eq!(payload.title, "No findings");
}

// --- criterion 3: a crashed run leaves no spinning check ------------------

#[test]
fn github_check_a_crashed_run_still_owes_a_resolution() {
    let started = CheckPayload::starting("acme/widgets", "abc123");
    let actions = vec![check_action(&started, PublishActionStatus::Sent, "start")];

    let owed = unresolved_check(
        &a_run(RunStatus::Failed, Some("the engine timed out")),
        &actions,
    )
    .expect("a started check on a finished run is owed a resolution");

    assert_eq!(owed.status, CheckStatus::Completed);
    assert_eq!(
        owed.conclusion,
        Some(CheckConclusion::Neutral),
        "`failure` is a statement about the code, and a run that crashed made no \
         statement about the code"
    );
    assert!(owed.title.contains("did not finish"), "{}", owed.title);
    assert!(
        owed.summary.contains("the engine timed out"),
        "the reason travels into what a person reads on the commit: {}",
        owed.summary
    );
}

#[test]
fn github_check_a_run_that_resolved_its_check_owes_nothing() {
    let started = CheckPayload::starting("acme/widgets", "abc123");
    let done = CheckPayload::resolved(
        "acme/widgets",
        "abc123",
        Verdict::Comment,
        ReviewOptions::default(),
        1,
    );
    let actions = vec![
        check_action(&started, PublishActionStatus::Sent, "start"),
        check_action(&done, PublishActionStatus::Sent, "end"),
    ];

    assert!(unresolved_check(&a_run(RunStatus::Done, None), &actions).is_none());
}

/// The case counting actions would get wrong.
#[test]
fn github_check_a_resolution_that_never_sent_does_not_count_as_resolved() {
    let started = CheckPayload::starting("acme/widgets", "abc123");
    let done = CheckPayload::resolved(
        "acme/widgets",
        "abc123",
        Verdict::Comment,
        ReviewOptions::default(),
        1,
    );
    let actions = vec![
        check_action(&started, PublishActionStatus::Sent, "start"),
        // Written, never delivered — the process died before the queue drained.
        check_action(&done, PublishActionStatus::Pending, "end"),
    ];

    assert!(
        unresolved_check(&a_run(RunStatus::Done, None), &actions).is_some(),
        "two actions where the second never sent is not a resolved check, and \
         counting actions would say it was"
    );
}

#[test]
fn github_check_a_running_run_owes_nothing_yet() {
    let started = CheckPayload::starting("acme/widgets", "abc123");
    let actions = vec![check_action(&started, PublishActionStatus::Sent, "start")];

    for status in [
        RunStatus::Queued,
        RunStatus::Reviewing,
        RunStatus::Publishing,
        RunStatus::AwaitingApproval,
    ] {
        assert!(
            unresolved_check(&a_run(status, None), &actions).is_none(),
            "{status:?} is still in flight; §11.3 wants the check in_progress"
        );
    }
}

#[test]
fn github_check_a_run_that_never_started_a_check_owes_nothing() {
    assert!(unresolved_check(&a_run(RunStatus::Failed, Some("boom")), &[]).is_none());
}

#[test]
fn github_check_cancelled_and_skipped_runs_are_resolved_too() {
    let started = CheckPayload::starting("a/b", "sha");
    let actions = vec![check_action(&started, PublishActionStatus::Sent, "start")];

    for status in [RunStatus::Cancelled, RunStatus::Skipped] {
        assert!(
            unresolved_check(&a_run(status, None), &actions).is_some(),
            "a kill switch or a skip still leaves a check spinning on the commit"
        );
    }
}

/// End to end against the database, because the reconciliation is meant to work
/// from stored state after the process that started the check is gone.
#[tokio::test]
async fn github_check_reconciliation_reads_the_crash_back_off_disk() {
    let (_dir, pool, run_id) = seeded().await.unwrap_or_else(|e| panic!("{e}"));

    let started = CheckPayload::starting("acme/widgets", "abc123");
    let mut action = check_action(&started, PublishActionStatus::Pending, "gh:check:start");
    action.run_id = run_id;
    let stored = PublishActionStore::new(&pool)
        .insert(&action)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    PublishActionStore::new(&pool)
        .record_outcome(
            stored.id,
            PublishActionStatus::Sent,
            Some("check-1"),
            None,
            None,
            at(4),
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    // The run died mid-review.
    RunStore::new(&pool)
        .mark_interrupted(run_id, "the process was killed")
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    let run = RunStore::new(&pool)
        .get(run_id)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let actions = PublishActionStore::new(&pool)
        .list_for_run(run_id)
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    let owed = unresolved_check(&run, &actions)
        .expect("a check started by a process that is gone is still owed");
    assert_eq!(owed.conclusion, Some(CheckConclusion::Neutral));
    assert_eq!(owed.head_sha, "abc123");
}

// --- commit comments -------------------------------------------------------

#[test]
fn github_check_a_commit_comment_targets_the_commit() {
    let request = gh_commit_comment(
        "acme/widgets",
        "abc123",
        "## rev-local review\n\nNo findings.",
    )
    .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(
        request.args,
        vec![
            "api",
            "--method",
            "POST",
            "repos/acme/widgets/commits/abc123/comments",
            "--input",
            "-"
        ]
    );
    let body: serde_json::Value =
        serde_json::from_str(&request.stdin.unwrap_or_default()).unwrap_or_else(|e| panic!("{e}"));
    assert!(body["body"]
        .as_str()
        .is_some_and(|b| b.contains("rev-local")));
}
