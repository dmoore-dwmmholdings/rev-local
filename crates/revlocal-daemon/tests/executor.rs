//! Discovery joined to review (RL-1207, SPEC §4.2, §4.3, §9.1).
//!
//! The criterion is "a change discovered by `watch` is reviewed without anybody
//! naming it", so these tests never name one: they record a change the way
//! discovery does, then call the executor and read the database.
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use chrono::TimeZone;
use revlocal_core::{
    AutonomyMode, Change, ChangeId, ChangeKind, DiffStat, EngineKind, GlobalConfig,
    PublishActionStatus, Repo, RepoId, RepoKind, RunStatus, Timestamp,
};
use revlocal_daemon::executor;
use revlocal_daemon::state_machine::NullSink;
use revlocal_store::{open, ChangeStore, FindingStore, Pool, RepoStore, RunStore};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

fn at(minute: u32) -> Timestamp {
    chrono::Utc
        .with_ymd_and_hms(2026, 8, 30, 14, minute, 0)
        .single()
        .unwrap_or_default()
}

fn workspace_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// A real git repository with two commits, so a change can be materialised.
fn git_repo(dir: &std::path::Path) -> Result<String, String> {
    let git = |args: &[&str]| -> Result<String, String> {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .map_err(|e| format!("git {args:?}: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    };

    git(&["init", "-q", "-b", "main", "."])?;
    git(&["config", "user.email", "fixture@rev-local.invalid"])?;
    git(&["config", "user.name", "Executor fixture"])?;
    std::fs::write(dir.join("main.rs"), "fn main() {}\n").map_err(|e| e.to_string())?;
    git(&["add", "main.rs"])?;
    git(&["commit", "-q", "-m", "add a main"])?;

    std::fs::write(dir.join("main.rs"), "fn main() {\n    let x = 1;\n}\n")
        .map_err(|e| e.to_string())?;
    git(&["add", "main.rs"])?;
    git(&["commit", "-q", "-m", "bind a value"])?;

    git(&["rev-parse", "HEAD"])
}

struct Fixture {
    dir: TempDir,
    pool: Pool,
    repo: Repo,
}

impl Fixture {
    /// §4.1's data directory: scratch lives under it, keyed by run id.
    ///
    /// Per-fixture rather than shared, which is the point — `ScratchDir::create`
    /// refuses a path that already exists, so two tests running concurrently with
    /// a shared data dir would collide on run id 1 and one would fail with
    /// something that reads like a git bug.
    fn data_dir(&self) -> std::path::PathBuf {
        self.dir.path().join("data")
    }
}

/// A repository with one discovered change and no run — the state `watch` leaves.
async fn discovered(autonomy: AutonomyMode) -> Result<Fixture, Box<dyn std::error::Error>> {
    let dir = TempDir::new()?;
    let checkout = dir.path().join("acme");
    std::fs::create_dir_all(&checkout)?;
    let head = git_repo(&checkout)?;

    let pool = open(&dir.path().join("rev-local.db")).await?;

    let repo = RepoStore::new(&pool)
        .insert(&Repo {
            id: RepoId::new(0),
            name: "acme".to_owned(),
            kind: RepoKind::Git,
            local_path: Some(checkout.display().to_string()),
            remote_url: None,
            default_branch: Some("main".to_owned()),
            // The mock spends nothing and announces itself; the point of these
            // tests is the join, not the engine.
            engine: EngineKind::Mock,
            autonomy,
            enabled: true,
            config_json: "{}".to_owned(),
            created_at: at(0),
            updated_at: at(0),
        })
        .await?;

    ChangeStore::new(&pool)
        .upsert(&Change {
            id: ChangeId::new(0),
            repo_id: repo.id,
            kind: ChangeKind::Commit,
            external_id: head,
            title: Some("bind a value".to_owned()),
            author_name: None,
            author_email: None,
            authored_at: None,
            branch: Some("main".to_owned()),
            base_ref: None,
            head_ref: None,
            url: None,
            diff_stat: DiffStat::default(),
            detected_at: at(1),
        })
        .await?;

    Ok(Fixture { dir, pool, repo })
}

fn config(mode: AutonomyMode) -> GlobalConfig {
    let mut config = GlobalConfig::default();
    config.global.mode = mode;
    config
}

#[tokio::test]
async fn a_discovered_change_is_queued_without_anybody_naming_it() {
    let fixture = discovered(AutonomyMode::DryRun).await.expect("fixture");

    let report = executor::enqueue(&fixture.pool, &fixture.repo, at(2))
        .await
        .expect("enqueue");

    assert_eq!(report.queued.len(), 1);
    assert!(!report.more_waiting);

    // A second pass must not queue it again: the change now has a run, which is
    // what "covered" means. Without this, every tick would add another attempt.
    let again = executor::enqueue(&fixture.pool, &fixture.repo, at(3))
        .await
        .expect("enqueue");
    assert!(again.queued.is_empty(), "queued twice: {:?}", again.queued);
}

#[tokio::test]
async fn a_queued_change_is_reviewed_and_its_findings_are_stored() {
    // The whole criterion, end to end: discovery's output goes in, a completed
    // review comes out, and nothing in between was named by hand.
    let fixture = discovered(AutonomyMode::DryRun).await.expect("fixture");
    executor::enqueue(&fixture.pool, &fixture.repo, at(2))
        .await
        .expect("enqueue");

    let report = executor::drain(
        &fixture.pool,
        &config(AutonomyMode::AutoLowAskHigh),
        &NullSink,
        &fixture.data_dir(),
        4,
        at(3),
        &CancellationToken::new(),
    )
    .await
    .expect("drain");

    assert!(report.held.is_empty(), "held: {:?}", report.held);
    assert_eq!(report.finished.len(), 1, "{report:?}");

    let outcome = &report.finished[0];
    assert_eq!(outcome.repo, "acme");
    // §8.4: the report says which engine ran, and it is the repository's.
    assert_eq!(outcome.engine, "mock");
    assert!(outcome.findings > 0, "the mock engine reports findings");

    // The run row carries its result — until RL-1207 these columns could only be
    // written at insert, when there is no verdict yet.
    let runs = RunStore::new(&fixture.pool)
        .list_recent(Some(fixture.repo.id), None, 10)
        .await
        .expect("runs");
    let run = runs.first().expect("a run");
    assert!(
        matches!(run.status, RunStatus::Done | RunStatus::AwaitingApproval),
        "status: {:?}",
        run.status
    );
    assert!(run.summary.is_some(), "the engine's summary was not stored");
    assert!(run.finished_at.is_some());

    // And the findings are rows, not just a report — a finding that is not in the
    // store is one the findings screen cannot show and a suppression cannot match.
    let stored = FindingStore::new(&fixture.pool)
        .list_for_run(run.id)
        .await
        .expect("findings");
    assert_eq!(stored.len(), outcome.findings);
    assert!(!stored[0].fingerprint.is_empty());
}

#[tokio::test]
async fn a_paused_daemon_reviews_nothing() {
    // §12.1. The kill switch stops work rather than queueing it differently: the
    // runs stay queued, which is what makes it reversible.
    let fixture = discovered(AutonomyMode::DryRun).await.expect("fixture");
    executor::enqueue(&fixture.pool, &fixture.repo, at(2))
        .await
        .expect("enqueue");

    revlocal_store::SettingStore::new(&fixture.pool)
        .set_paused(true, at(3))
        .await
        .expect("pause");

    let report = executor::drain(
        &fixture.pool,
        &config(AutonomyMode::AutoLowAskHigh),
        &NullSink,
        &fixture.data_dir(),
        4,
        at(3),
        &CancellationToken::new(),
    )
    .await
    .expect("drain");

    assert!(report.paused);
    assert!(
        report.finished.is_empty(),
        "a paused daemon reviewed something"
    );
    // Said out loud rather than looking like an idle tick.
    assert!(report
        .idle_line()
        .unwrap_or_default()
        .contains("kill switch"));

    let runs = RunStore::new(&fixture.pool)
        .list_recent(Some(fixture.repo.id), None, 10)
        .await
        .expect("runs");
    assert_eq!(
        runs[0].status,
        RunStatus::Queued,
        "the run was not left queued"
    );
}

#[tokio::test]
async fn findings_reach_the_publish_queue_under_the_repository_autonomy_mode() {
    // §12.2 and §12.3: creating an issue is high risk, so under the default mode
    // it waits for a person rather than going out.
    let fixture = discovered(AutonomyMode::AutoLowAskHigh)
        .await
        .expect("fixture");
    executor::enqueue(&fixture.pool, &fixture.repo, at(2))
        .await
        .expect("enqueue");

    let report = executor::drain(
        &fixture.pool,
        &config(AutonomyMode::AutoLowAskHigh),
        &NullSink,
        &fixture.data_dir(),
        4,
        at(3),
        &CancellationToken::new(),
    )
    .await
    .expect("drain");

    let outcome = &report.finished[0];
    assert!(outcome.actions > 0, "no publish action was queued");
    assert_eq!(outcome.status, "awaiting_approval");

    let waiting = revlocal_store::PublishActionStore::new(&fixture.pool)
        .list_awaiting_approval()
        .await
        .expect("waiting");
    assert_eq!(waiting.len(), outcome.actions);
    assert_eq!(waiting[0].target, "andare");
    // §11.6: the fingerprint is in the key, so re-reviewing this change reuses the
    // issue rather than filing a second one.
    assert!(waiting[0].idempotency_key.starts_with("andare-"));
}

#[tokio::test]
async fn a_dry_run_repository_records_actions_without_sending_them() {
    // The mode is the repository owner's, and it is applied here rather than at
    // dispatch — an action written as `pending` in dry run would eventually go.
    let fixture = discovered(AutonomyMode::DryRun).await.expect("fixture");
    executor::enqueue(&fixture.pool, &fixture.repo, at(2))
        .await
        .expect("enqueue");

    executor::drain(
        &fixture.pool,
        &config(AutonomyMode::AutoLowAskHigh),
        &NullSink,
        &fixture.data_dir(),
        4,
        at(3),
        &CancellationToken::new(),
    )
    .await
    .expect("drain");

    let waiting = revlocal_store::PublishActionStore::new(&fixture.pool)
        .list_awaiting_approval()
        .await
        .expect("waiting");
    assert!(
        waiting.is_empty(),
        "dry run must not put anything in the inbox"
    );

    let runs = RunStore::new(&fixture.pool)
        .list_recent(Some(fixture.repo.id), None, 10)
        .await
        .expect("runs");
    let actions = revlocal_store::PublishActionStore::new(&fixture.pool)
        .list_for_run(runs[0].id)
        .await
        .expect("actions");
    assert!(
        !actions.is_empty(),
        "dry run records what it would have done"
    );
    assert!(actions
        .iter()
        .all(|a| a.status == PublishActionStatus::SkippedDryRun));
}

#[tokio::test]
async fn a_repository_over_its_daily_budget_is_held_and_says_so() {
    // §13, §18: a run that did not happen is reported with its reason, never
    // dropped quietly — "nothing ran" and "we ran out of budget" are different
    // facts with different remedies.
    let fixture = discovered(AutonomyMode::DryRun).await.expect("fixture");
    executor::enqueue(&fixture.pool, &fixture.repo, at(2))
        .await
        .expect("enqueue");

    let mut config = config(AutonomyMode::AutoLowAskHigh);
    config.budgets.daily_runs_per_repo = 1;
    revlocal_store::BudgetLedgerStore::new(&fixture.pool)
        .add_run(
            fixture.repo.id,
            &revlocal_daemon::budgets::day_of(at(3)),
            1,
            &revlocal_core::Usage::default(),
        )
        .await
        .expect("ledger");

    let report = executor::drain(
        &fixture.pool,
        &config,
        &NullSink,
        &fixture.data_dir(),
        4,
        at(3),
        &CancellationToken::new(),
    )
    .await
    .expect("drain");

    assert!(report.finished.is_empty(), "budget was not enforced");
    assert_eq!(report.held.len(), 1, "held: {:?}", report.held);
    assert!(
        report.held[0].contains("budget") || report.held[0].contains("runs"),
        "the reason must name the budget: {}",
        report.held[0]
    );
}

#[tokio::test]
async fn a_disabled_repository_is_not_reviewed() {
    let fixture = discovered(AutonomyMode::DryRun).await.expect("fixture");
    executor::enqueue(&fixture.pool, &fixture.repo, at(2))
        .await
        .expect("enqueue");

    let mut disabled = fixture.repo.clone();
    disabled.enabled = false;
    RepoStore::new(&fixture.pool)
        .update(&disabled)
        .await
        .expect("disable");

    let report = executor::drain(
        &fixture.pool,
        &config(AutonomyMode::AutoLowAskHigh),
        &NullSink,
        &fixture.data_dir(),
        4,
        at(3),
        &CancellationToken::new(),
    )
    .await
    .expect("drain");

    assert!(report.finished.is_empty());
    assert_eq!(report.held.len(), 1);
    assert!(report.held[0].contains("disabled"), "{}", report.held[0]);
}

#[tokio::test]
async fn the_workspace_root_fixture_is_reachable() {
    // Guards the helper above rather than the executor: a test suite whose fixture
    // path is wrong fails in a way that looks like the product is broken.
    assert!(workspace_root().join("SPEC.md").exists());
}
