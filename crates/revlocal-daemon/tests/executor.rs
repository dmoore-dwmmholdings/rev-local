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
            // Andare needs a project to file into, and a repository that files
            // without naming one is a configuration error rather than a review
            // failure — `a_repository_with_no_andare_project_still_completes_its_review`
            // covers that case. These tests are about the gate, so they configure
            // the repository the way a real one filing issues has to be.
            config_json: r#"{"andare_project": "ENG"}"#.to_owned(),
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

    // Not every queued action waits. Since RL-1519 risk depends on where an
    // action goes, so one run's findings can produce an Andare issue that asks
    // and a local report that does not — which is the whole point of the local
    // report, and what this assertion used to rule out by equating the two.
    let waiting = revlocal_store::PublishActionStore::new(&fixture.pool)
        .list_awaiting_approval()
        .await
        .expect("waiting");
    assert!(!waiting.is_empty(), "the Andare issue must wait");
    assert!(
        waiting.iter().all(|action| action.target == "andare"),
        "only the tracker asks: {:?}",
        waiting.iter().map(|a| a.target.clone()).collect::<Vec<_>>()
    );
    assert!(
        waiting.len() < outcome.actions,
        "the local report must not be waiting with it: {} of {}",
        waiting.len(),
        outcome.actions
    );

    // §11.6: the fingerprint is in the key, so re-reviewing this change reuses the
    // issue rather than filing a second one.
    assert!(waiting[0].idempotency_key.starts_with("andare-"));
    // §11.4: the key carries the run as well as the fingerprint, so a later
    // review that still sees the problem reaches the target and can comment
    // rather than being dropped here (RL-1530).
    assert!(
        waiting[0].idempotency_key.contains("-run"),
        "the run must be in the key: {}",
        waiting[0].idempotency_key
    );
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
async fn the_repository_engine_is_the_one_that_runs() {
    // REVL-125's remaining criterion, which needed this loop to exist before it
    // could be true of anything: decision D3 puts the engine on the repository,
    // and until now no code path started a review *from* a repository row.
    //
    // Asserted through a template pointed at a binary that does not exist, so it
    // costs nothing: the run fails naming the engine rather than quietly
    // producing a mock review, which is the property that matters.
    let fixture = discovered(AutonomyMode::DryRun).await.expect("fixture");

    let mut claude = fixture.repo.clone();
    claude.engine = EngineKind::Claude;
    RepoStore::new(&fixture.pool)
        .update(&claude)
        .await
        .expect("set the engine");

    executor::enqueue(&fixture.pool, &claude, at(2))
        .await
        .expect("enqueue");

    let mut config = config(AutonomyMode::AutoLowAskHigh);
    config.engines.insert(
        "claude".to_owned(),
        toml::from_str::<toml::Value>("bin = \"revlocal-no-such-engine-9f3a\"").expect("table"),
    );

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

    // It went ahead — so it is `finished`, not `held` — and it failed, rather
    // than succeeding with the mock standing in.
    assert_eq!(report.finished.len(), 1, "{report:?}");
    let outcome = &report.finished[0];
    assert_eq!(outcome.engine, "claude");
    assert_eq!(outcome.status, "failed");
    assert_eq!(outcome.findings, 0);
    // §18: the failure says what went wrong. A failed run with no reason is
    // indistinguishable from a clean one, which is the whole point.
    assert!(outcome.detail.is_some(), "a failed run must say why");

    let runs = RunStore::new(&fixture.pool)
        .list_recent(Some(fixture.repo.id), None, 10)
        .await
        .expect("runs");
    assert_eq!(runs[0].status, RunStatus::Failed);
    assert!(runs[0].error.is_some(), "a failed run must say why");
}

#[tokio::test]
async fn the_workspace_root_fixture_is_reachable() {
    // Guards the helper above rather than the executor: a test suite whose fixture
    // path is wrong fails in a way that looks like the product is broken.
    assert!(workspace_root().join("SPEC.md").exists());
}

/// A manually requested change is queued for review, and a second request for the
/// same revision queues a second attempt rather than being coalesced away.
#[tokio::test]
async fn a_manual_request_is_queued_even_for_a_change_that_already_has_a_run() {
    // `enqueue` deliberately skips changes that already have a run — that is what
    // stops discovery re-queueing everything each tick. Routing a manual request
    // through it would make the second press of "Review" do nothing at all.
    let fixture = discovered(AutonomyMode::DryRun).await.expect("fixture");
    let change = ChangeStore::new(&fixture.pool)
        .without_runs(fixture.repo.id, 10)
        .await
        .expect("changes")
        .pop()
        .expect("a discovered change");

    let first = executor::enqueue_manual(&fixture.pool, &fixture.repo, &change, at(2))
        .await
        .expect("first manual request");
    let second = executor::enqueue_manual(&fixture.pool, &fixture.repo, &change, at(3))
        .await
        .expect("second manual request");

    assert_ne!(first.id, second.id, "the second request queued nothing");
    assert_eq!(second.attempt, first.attempt + 1);
    assert_eq!(first.status, RunStatus::Queued);
    assert_eq!(first.trigger, revlocal_core::TriggerSource::Manual);
}

/// Queueing returns before the engine runs, and the named run is the one executed.
#[tokio::test]
async fn a_manual_run_is_executed_by_id_rather_than_by_queue_position() {
    // Somebody who asked to review *this* commit is owed that commit's run. Using
    // `drain` would give them whatever was at the head of the queue.
    let fixture = discovered(AutonomyMode::DryRun).await.expect("fixture");
    let change = ChangeStore::new(&fixture.pool)
        .without_runs(fixture.repo.id, 10)
        .await
        .expect("changes")
        .pop()
        .expect("a discovered change");

    // An older run is already waiting, so "the head of the queue" and "the run I
    // asked for" are different runs.
    let older = executor::enqueue_manual(&fixture.pool, &fixture.repo, &change, at(2))
        .await
        .expect("older");
    let mine = executor::enqueue_manual(&fixture.pool, &fixture.repo, &change, at(3))
        .await
        .expect("mine");

    let outcome = executor::execute_run(
        &fixture.pool,
        &config(AutonomyMode::DryRun),
        &NullSink,
        &fixture.data_dir(),
        mine.id,
        at(4),
        &CancellationToken::new(),
    )
    .await
    .expect("execute")
    .expect("the run went ahead");

    assert_eq!(outcome.run_id, mine.id.get());

    let runs = RunStore::new(&fixture.pool);
    assert_eq!(
        runs.get(older.id).await.expect("older run").status,
        RunStatus::Queued,
        "executing one run must not execute the rest of the queue"
    );
}

/// The kill switch holds a named run rather than failing it.
#[tokio::test]
async fn a_manual_run_is_held_by_the_kill_switch_and_stays_queued() {
    let fixture = discovered(AutonomyMode::DryRun).await.expect("fixture");
    let change = ChangeStore::new(&fixture.pool)
        .without_runs(fixture.repo.id, 10)
        .await
        .expect("changes")
        .pop()
        .expect("a discovered change");
    let run = executor::enqueue_manual(&fixture.pool, &fixture.repo, &change, at(2))
        .await
        .expect("queued");

    revlocal_store::SettingStore::new(&fixture.pool)
        .set_paused(true, at(3))
        .await
        .expect("pause");

    let held = executor::execute_run(
        &fixture.pool,
        &config(AutonomyMode::DryRun),
        &NullSink,
        &fixture.data_dir(),
        run.id,
        at(4),
        &CancellationToken::new(),
    )
    .await
    .expect("execute")
    .expect_err("a paused daemon must not review");

    assert!(held.contains("kill switch"), "{held}");
    assert_eq!(
        RunStore::new(&fixture.pool)
            .get(run.id)
            .await
            .expect("run")
            .status,
        RunStatus::Queued,
        "a held run must stay queued so releasing the switch resumes it"
    );
}

/// A repository missing its Andare project holds the filing, not the review.
#[tokio::test]
async fn a_repository_with_no_andare_project_still_completes_its_review() {
    // This used to abort the whole executor pass with an outer error: the run was
    // left stuck in `publishing`, and one repository's missing setting stopped
    // every other repository's queue. §18 — the fact is reported, not raised.
    let fixture = discovered(AutonomyMode::Auto).await.expect("fixture");

    // The one thing this test is about: the setting the fixture normally carries,
    // taken away.
    let unconfigured = revlocal_core::Repo {
        config_json: "{}".to_owned(),
        ..fixture.repo.clone()
    };
    RepoStore::new(&fixture.pool)
        .update(&unconfigured)
        .await
        .expect("clear the andare project");

    executor::enqueue(&fixture.pool, &unconfigured, at(2))
        .await
        .expect("enqueue");

    let report = executor::drain(
        &fixture.pool,
        &config(AutonomyMode::Auto),
        &NullSink,
        &fixture.data_dir(),
        4,
        at(3),
        &CancellationToken::new(),
    )
    .await
    .expect("drain must not fail because a repository is missing a setting");

    let outcome = report.finished.first().expect("a finished run");
    assert!(outcome.findings > 0, "the findings are still stored");

    // Targets fail independently (RL-1507). Andare gets nothing, because there is
    // no project to file into; the local report needs no configuration and is
    // written anyway, which is the whole reason it exists.
    let actions = revlocal_store::PublishActionStore::new(&fixture.pool)
        .list_for_run(revlocal_core::RunId::new(outcome.run_id))
        .await
        .expect("actions");
    assert!(
        actions.iter().all(|action| action.target == "report"),
        "nothing may be filed into no project: {:?}",
        actions.iter().map(|a| a.target.clone()).collect::<Vec<_>>()
    );
    assert!(
        !actions.is_empty(),
        "the local report is written with nothing configured"
    );

    // Every reason, not the first. This fixture is missing what two targets need
    // — an Andare project and a GitHub remote — and while `held` was a single
    // value the second message silently replaced the first (§18).
    let detail = outcome.detail.clone().unwrap_or_default();
    assert!(
        detail.contains("Andare project"),
        "the reason must name the setting to fix: {detail:?}"
    );
    assert!(
        detail.contains("GitHub remote"),
        "a second target's reason must not be dropped: {detail:?}"
    );

    let run = RunStore::new(&fixture.pool)
        .list_recent(Some(fixture.repo.id), None, 10)
        .await
        .expect("runs")
        .into_iter()
        .next()
        .expect("a run");
    assert!(
        matches!(run.status, RunStatus::Done | RunStatus::AwaitingApproval),
        "the run must reach a terminal status, not sit in publishing: {:?}",
        run.status
    );
}

/// A run already queued against a checkout that has since gone must not run.
#[tokio::test]
async fn a_queued_run_whose_checkout_vanished_is_held_not_executed() {
    // RL-1513 put this check in the loop's discovery pass, which stopped new runs
    // being queued and did nothing about the ones already waiting — 51 of them on
    // a real install, most against deleted repositories. `drain` selects queued
    // runs globally, so the guard has to be where every path meets (RL-1515).
    let fixture = discovered(AutonomyMode::Auto).await.expect("fixture");
    executor::enqueue(&fixture.pool, &fixture.repo, at(2))
        .await
        .expect("enqueue");

    // Deleted *after* the run was queued, which is the whole case.
    std::fs::remove_dir_all(fixture.repo.local_path.as_deref().expect("a local path"))
        .expect("remove the checkout");

    let report = executor::drain(
        &fixture.pool,
        &config(AutonomyMode::Auto),
        &NullSink,
        &fixture.data_dir(),
        4,
        at(3),
        &CancellationToken::new(),
    )
    .await
    .expect("drain must not fail because a checkout is missing");

    assert!(
        report.finished.is_empty(),
        "nothing may be reviewed: {report:?}"
    );

    // §18: held with a reason naming the repository and what to do about it.
    let held = report.held.join("\n");
    assert!(held.contains("checkout is gone"), "{held}");
    assert!(held.contains("try:"), "{held}");

    // And the run is still queued rather than failed: putting the checkout back
    // should be enough to make it run, without anybody re-queuing anything.
    let runs = RunStore::new(&fixture.pool)
        .list_recent(None, Some(revlocal_core::RunStatus::Queued), 10)
        .await
        .expect("runs");
    assert!(!runs.is_empty(), "the run stays queued");
}

/// A repository that wants a wiki page gets one, once per run.
#[tokio::test]
async fn a_review_page_is_queued_once_for_the_run_not_once_per_finding() {
    // Andare, GitHub and the local report are one action per finding. A Trama page
    // is the review itself — verdict, summary and findings together — so it is one
    // page per run, and nothing queued it at all before RL-1529.
    let fixture = discovered(AutonomyMode::Auto).await.expect("fixture");
    let wants_trama = revlocal_core::Repo {
        config_json: r#"{"andare_project": "ENG", "trama_space": "ENG"}"#.to_owned(),
        ..fixture.repo.clone()
    };
    RepoStore::new(&fixture.pool)
        .update(&wants_trama)
        .await
        .expect("configure trama");

    executor::enqueue(&fixture.pool, &wants_trama, at(2))
        .await
        .expect("enqueue");
    let report = executor::drain(
        &fixture.pool,
        &config(AutonomyMode::Auto),
        &NullSink,
        &fixture.data_dir(),
        4,
        at(3),
        &CancellationToken::new(),
    )
    .await
    .expect("drain");

    let outcome = report.finished.first().expect("a finished run");
    assert!(outcome.findings > 1, "the fixture finds more than one");

    let actions = revlocal_store::PublishActionStore::new(&fixture.pool)
        .list_for_run(revlocal_core::RunId::new(outcome.run_id))
        .await
        .expect("actions");
    let pages: Vec<_> = actions.iter().filter(|a| a.target == "trama").collect();

    assert_eq!(
        pages.len(),
        1,
        "one page for the run, whatever the finding count"
    );
    assert_eq!(pages[0].capability, revlocal_core::Capability::UpsertDoc);
    // Keyed by the run, so re-reviewing the change updates the page it already has
    // rather than creating a second one.
    assert!(pages[0].idempotency_key.starts_with("trama-run-"));
}

/// Without a space there is nowhere to put a page, and none is invented.
#[tokio::test]
async fn no_trama_space_means_no_page() {
    let fixture = discovered(AutonomyMode::Auto).await.expect("fixture");
    executor::enqueue(&fixture.pool, &fixture.repo, at(2))
        .await
        .expect("enqueue");

    let report = executor::drain(
        &fixture.pool,
        &config(AutonomyMode::Auto),
        &NullSink,
        &fixture.data_dir(),
        4,
        at(3),
        &CancellationToken::new(),
    )
    .await
    .expect("drain");

    let outcome = report.finished.first().expect("a finished run");
    let actions = revlocal_store::PublishActionStore::new(&fixture.pool)
        .list_for_run(revlocal_core::RunId::new(outcome.run_id))
        .await
        .expect("actions");

    assert!(
        !actions.iter().any(|a| a.target == "trama"),
        "no space configured, so no page: {:?}",
        actions.iter().map(|a| a.target.clone()).collect::<Vec<_>>()
    );

    // §18: and it says so. `targets` includes `trama` by default, so a repository
    // that never mentioned Trama is still asking for a page — dropping it in
    // silence is indistinguishable from writing one (RL-1533).
    let detail = outcome.detail.clone().unwrap_or_default();
    assert!(
        detail.contains("Trama space"),
        "the missing space must be reported: {detail:?}"
    );
    assert!(
        detail.contains("try:"),
        "and say what to do about it: {detail:?}"
    );
}

/// Not asking for a target is not the same as asking and being unable.
#[tokio::test]
async fn a_repository_that_removed_trama_is_not_nagged_about_it() {
    // The distinction every one of these notes draws. A repository that took
    // `trama` out of its targets has said what it wants, and repeating it on
    // every run would be noise about a decision already made.
    let fixture = discovered(AutonomyMode::Auto).await.expect("fixture");
    let no_trama = revlocal_core::Repo {
        config_json: r#"{"andare_project": "ENG", "targets": ["andare", "report"]}"#.to_owned(),
        ..fixture.repo.clone()
    };
    RepoStore::new(&fixture.pool)
        .update(&no_trama)
        .await
        .expect("configure targets");

    executor::enqueue(&fixture.pool, &no_trama, at(2))
        .await
        .expect("enqueue");
    let report = executor::drain(
        &fixture.pool,
        &config(AutonomyMode::Auto),
        &NullSink,
        &fixture.data_dir(),
        4,
        at(3),
        &CancellationToken::new(),
    )
    .await
    .expect("drain");

    let detail = report
        .finished
        .first()
        .and_then(|outcome| outcome.detail.clone())
        .unwrap_or_default();
    assert!(
        !detail.contains("Trama"),
        "a repository that did not ask must not be told: {detail:?}"
    );
}

/// A finding still present on a later commit must reach the target again.
#[tokio::test]
async fn a_recurring_finding_queues_another_action_so_the_target_can_comment() {
    // §11.4 and M9: "a re-run for the same fingerprint produces a comment, not a
    // second issue". The comment is the *target's* decision — it searches for the
    // trailer and comments when it finds one — and it can only make that decision
    // if an action reaches it.
    //
    // RL-1509 keyed on the fingerprint alone to stop a duplicate-key crash, which
    // also stopped the second review queueing anything, which made
    // `recurrence_comment` unreachable (RL-1530).
    let fixture = discovered(AutonomyMode::Auto).await.expect("fixture");

    // Two runs over the same change: the mock returns the same findings, so the
    // fingerprints match, which is exactly the recurrence case.
    for at_minute in [2, 4] {
        executor::enqueue_manual(
            &fixture.pool,
            &fixture.repo,
            &a_change(&fixture).await.expect("the discovered change"),
            at(at_minute),
        )
        .await
        .expect("enqueue");

        executor::drain(
            &fixture.pool,
            &config(AutonomyMode::Auto),
            &NullSink,
            &fixture.data_dir(),
            4,
            at(at_minute + 1),
            &CancellationToken::new(),
        )
        .await
        .expect("drain");
    }

    let actions = revlocal_store::PublishActionStore::new(&fixture.pool)
        .list_pending(at(9))
        .await
        .expect("actions");
    let andare: Vec<_> = actions.iter().filter(|a| a.target == "andare").collect();

    assert!(
        andare.len() > 1,
        "the second review must queue its own action: {:?}",
        andare
            .iter()
            .map(|a| a.idempotency_key.clone())
            .collect::<Vec<_>>()
    );

    // Distinct keys, so the unique constraint RL-1509 tripped over still holds.
    let keys: std::collections::BTreeSet<_> =
        andare.iter().map(|a| a.idempotency_key.clone()).collect();
    assert_eq!(keys.len(), andare.len(), "keys must stay unique");
}

/// The change the fixture discovered, for a second manual run over it.
///
/// Returns `Result` because it is a helper, not a test — ADR 0003, which clippy
/// enforces: `expect_used` is allowed in `#[test]` functions and nowhere else.
async fn a_change(fixture: &Fixture) -> Result<revlocal_core::Change, String> {
    // `without_runs` is empty once the first run exists, so the change is looked
    // up by the identity the fixture gave it rather than by "not yet reviewed".
    let path = fixture
        .repo
        .local_path
        .as_deref()
        .ok_or("the fixture repository has no local path")?;

    let head = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(path)
        .output()
        .map_err(|error| format!("git rev-parse: {error}"))?;

    ChangeStore::new(&fixture.pool)
        .find(
            fixture.repo.id,
            ChangeKind::Commit,
            String::from_utf8_lossy(&head.stdout).trim(),
        )
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "the discovered change is not in the store".to_owned())
}
