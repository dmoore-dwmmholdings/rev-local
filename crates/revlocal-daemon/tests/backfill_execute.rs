//! A backfill reviews history rather than listing it (REVL-209, SPEC §7.4).
//!
//! # What was wrong
//!
//! §7.4: `backfill` "enumerates historical changes, **enqueues them at low
//! priority behind live work**, respects budgets, and is resumable". Everything
//! but the enqueueing was built and unit-tested: `plan`, `next_step` with its
//! fairness and budget yields, `recorded` advancing the cursor per item. Nothing
//! drove them. The command enumerated, printed the list, and told you so in its
//! own last line — half a command against a spec clause that says otherwise.
//!
//! # Why these tests use the real thing
//!
//! The state machine is already covered against hand-built plans in `backfill.rs`.
//! What was missing was a *driver*, and a driver is only interesting against a
//! real repository, a real store and a real cursor: the questions are whether the
//! cursor lands where a resume needs it, whether the sweep stands aside when live
//! work appears, and whether §9.4's skip rules still apply to a commit from 2019.
//! None of those can be asked of a mock plan.
//!
//! Inner-loop rules hold: `EngineKind::Mock` spends nothing and announces itself,
//! and the repository is a `TempDir` built by this file.

use chrono::TimeZone;
use revlocal_core::{
    AutonomyMode, EngineKind, GlobalConfig, Repo, RepoId, RepoKind, RunStatus, Timestamp,
    TriggerSource,
};
use revlocal_daemon::backfill::{backfill_scope, execute, plan, BackfillItem, BACKFILL_TRIGGER};
use revlocal_daemon::state_machine::NullSink;
use revlocal_store::{open, CursorStore, Pool, RepoStore, RunStore};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

fn at(minute: u32) -> Timestamp {
    chrono::Utc
        .with_ymd_and_hms(2026, 9, 1, 9, minute, 0)
        .single()
        .unwrap_or_default()
}

fn git(dir: &std::path::Path, args: &[&str]) -> Result<String, String> {
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
}

struct Fixture {
    dir: TempDir,
    checkout: std::path::PathBuf,
    pool: Pool,
    repo: Repo,
}

impl Fixture {
    fn data_dir(&self) -> std::path::PathBuf {
        self.dir.path().join("data")
    }
}

/// A repository with a first commit plus `extra` more on top of it.
async fn install(extra: usize) -> Result<Fixture, Box<dyn std::error::Error>> {
    let dir = TempDir::new()?;
    let checkout = dir.path().join("legacy");
    std::fs::create_dir_all(&checkout)?;

    git(&checkout, &["init", "-q", "-b", "main", "."])?;
    git(
        &checkout,
        &["config", "user.email", "fixture@rev-local.invalid"],
    )?;
    git(&checkout, &["config", "user.name", "Backfill fixture"])?;
    std::fs::write(checkout.join("main.rs"), "fn main() {}\n")?;
    git(&checkout, &["add", "main.rs"])?;
    git(&checkout, &["commit", "-q", "-m", "add a main"])?;

    for n in 0..extra {
        std::fs::write(
            checkout.join("main.rs"),
            format!("fn main() {{\n    let x = {n};\n}}\n"),
        )?;
        git(&checkout, &["add", "main.rs"])?;
        git(&checkout, &["commit", "-q", "-m", &format!("bind {n}")])?;
    }

    let pool = open(&dir.path().join("rev-local.db")).await?;
    let repo = RepoStore::new(&pool)
        .insert(&Repo {
            id: RepoId::new(0),
            name: "legacy".to_owned(),
            kind: RepoKind::Git,
            local_path: Some(checkout.display().to_string()),
            remote_url: None,
            default_branch: Some("main".to_owned()),
            engine: EngineKind::Mock,
            autonomy: AutonomyMode::Auto,
            enabled: true,
            config_json: "{}".to_owned(),
            created_at: at(0),
            updated_at: at(0),
        })
        .await?;

    Ok(Fixture {
        dir,
        checkout,
        pool,
        repo,
    })
}

fn config() -> GlobalConfig {
    let mut config = GlobalConfig::default();
    config.global.mode = AutonomyMode::Auto;
    config
}

/// Everything after the first commit, oldest first, as the adapter reports it.
async fn history(
    fixture: &Fixture,
) -> Result<Vec<revlocal_vcs::DetectedChange>, Box<dyn std::error::Error>> {
    let root = git(&fixture.checkout, &["rev-list", "--max-parents=0", "HEAD"])?;
    let adapter = revlocal_vcs::adapter_for(&fixture.repo)?;
    let start = revlocal_core::Cursor {
        repo_id: fixture.repo.id,
        scope: revlocal_core::Cursor::commits_scope("main"),
        value: root,
        updated_at: at(0),
    };
    Ok(adapter.discover(&fixture.repo, Some(&start), 100).await?)
}

fn items(changes: &[revlocal_vcs::DetectedChange]) -> Vec<BackfillItem> {
    changes
        .iter()
        .map(|change| BackfillItem {
            external_id: change.external_id.clone(),
            summary: change.title.clone().unwrap_or_default(),
        })
        .collect()
}

/// The criterion the issue was filed on: it enqueues, and it reviews.
#[tokio::test]
async fn a_backfill_reviews_the_history_it_enumerates() -> Result<(), Box<dyn std::error::Error>> {
    let fixture = install(3).await?;
    let changes = history(&fixture).await?;
    assert_eq!(
        changes.len(),
        3,
        "the fixture should have three commits past the root"
    );

    let scope = backfill_scope(&revlocal_core::Cursor::commits_scope("main"));
    let planned = plan(fixture.repo.id, &scope, &items(&changes), None, None);

    let outcome = execute(
        &fixture.pool,
        &config(),
        &NullSink,
        &fixture.data_dir(),
        &fixture.repo,
        planned,
        &changes,
        at(1),
        &CancellationToken::new(),
    )
    .await?;

    assert_eq!(
        outcome.reviewed.len(),
        3,
        "every enumerated change should have been reviewed: {outcome:?}"
    );
    assert!(
        outcome.finished_the_plan(),
        "nothing should have stopped it: {outcome:?}"
    );

    // Recorded under backfill's own trigger, not poll's and not manual's. §7.4
    // gives it one so a history sweep is distinguishable afterwards from a human
    // asking, which is what makes "why was this reviewed?" answerable.
    let runs = RunStore::new(&fixture.pool)
        .list_recent(Some(fixture.repo.id), None, 100)
        .await?;
    assert!(
        runs.iter().all(|run| run.trigger == BACKFILL_TRIGGER),
        "every run a backfill creates is a backfill run: {runs:?}"
    );

    Ok(())
}

/// §7.4's resumability, asserted where it actually bites: the cursor must be at
/// the last item *when the sweep stops*, not written once at the end.
#[tokio::test]
async fn the_cursor_advances_per_item_so_a_resume_does_not_redo_work(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = install(4).await?;
    let changes = history(&fixture).await?;

    // Two items now, two later — the shape of an interrupted sweep.
    let scope = backfill_scope(&revlocal_core::Cursor::commits_scope("main"));
    let first = plan(fixture.repo.id, &scope, &items(&changes), None, Some(2));
    let outcome = execute(
        &fixture.pool,
        &config(),
        &NullSink,
        &fixture.data_dir(),
        &fixture.repo,
        first,
        &changes,
        at(1),
        &CancellationToken::new(),
    )
    .await?;
    assert_eq!(outcome.completed, 2, "{outcome:?}");

    let stored = CursorStore::new(&fixture.pool)
        .get(fixture.repo.id, &scope)
        .await?
        .ok_or("the backfill cursor was never written")?;
    assert_eq!(
        stored.value, changes[1].external_id,
        "the cursor must name the last item reviewed, not the last item planned"
    );

    // Resuming reads that cursor and starts after it — the second sweep must not
    // review either of the first two again.
    let resumed = plan(
        fixture.repo.id,
        &scope,
        &items(&changes),
        Some(&stored.value),
        None,
    );
    let outcome = execute(
        &fixture.pool,
        &config(),
        &NullSink,
        &fixture.data_dir(),
        &fixture.repo,
        resumed,
        &changes,
        at(2),
        &CancellationToken::new(),
    )
    .await?;

    let reviewed: Vec<&str> = outcome
        .reviewed
        .iter()
        .map(|run| run.change.as_str())
        .collect();
    assert_eq!(
        reviewed.len(),
        2,
        "a resume reviews only what is left: {outcome:?}"
    );
    assert!(
        !reviewed.contains(&changes[0].external_id.as_str())
            && !reviewed.contains(&changes[1].external_id.as_str()),
        "a resume must not re-review what the first sweep already did: {reviewed:?}"
    );

    Ok(())
}

/// The fairness guarantee §7.4 exists for, through the driver rather than
/// against a hand-built plan.
///
/// A queued live run is enough. It does not have to be running: the point is that
/// a commit somebody pushed a minute ago is not made to wait behind twenty
/// thousand from 2019.
#[tokio::test]
async fn live_work_stops_the_sweep_rather_than_being_queued_behind_it(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = install(3).await?;
    let changes = history(&fixture).await?;

    // A live run, queued and not started, exactly as a poll would leave one.
    let live = revlocal_store::ChangeStore::new(&fixture.pool)
        .upsert(&revlocal_core::Change {
            id: revlocal_core::ChangeId::new(0),
            repo_id: fixture.repo.id,
            kind: revlocal_core::ChangeKind::Commit,
            external_id: "pushed-a-minute-ago".to_owned(),
            title: None,
            author_name: None,
            author_email: None,
            authored_at: None,
            branch: Some("main".to_owned()),
            base_ref: None,
            head_ref: None,
            url: None,
            diff_stat: revlocal_core::DiffStat::default(),
            detected_at: at(1),
        })
        .await?;
    RunStore::new(&fixture.pool)
        .insert(&revlocal_core::Run {
            id: revlocal_core::RunId::new(0),
            change_id: live.id,
            attempt: 1,
            status: RunStatus::Queued,
            engine: EngineKind::Mock,
            depth: revlocal_core::Depth::Standard,
            trigger: TriggerSource::Poll,
            skip_reason: None,
            error: None,
            error_detail: None,
            degraded: None,
            usage: revlocal_core::Usage::default(),
            started_at: None,
            finished_at: None,
            transcript_path: None,
            truncated: false,
            omitted_files: Vec::new(),
            verdict: None,
            summary: None,
            created_at: at(1),
        })
        .await?;

    let scope = backfill_scope(&revlocal_core::Cursor::commits_scope("main"));
    let planned = plan(fixture.repo.id, &scope, &items(&changes), None, None);
    let outcome = execute(
        &fixture.pool,
        &config(),
        &NullSink,
        &fixture.data_dir(),
        &fixture.repo,
        planned,
        &changes,
        at(2),
        &CancellationToken::new(),
    )
    .await?;

    assert!(
        outcome.reviewed.is_empty(),
        "not one historical commit may be reviewed while live work waits: {outcome:?}"
    );
    let stopped = outcome
        .stopped
        .as_deref()
        .ok_or("standing aside must be reported, never silent (§18)")?;
    assert!(
        stopped.contains("live work"),
        "the reason must say what it stood aside for: {stopped}"
    );
    assert_eq!(
        outcome.remaining, 3,
        "yielding is not abandoning — the plan is still there: {outcome:?}"
    );

    Ok(())
}

/// §9.4 is about the change, not about when it was found.
///
/// A merge commit is a merge commit whether it landed this morning or in 2019,
/// and a sweep that reviewed the ones discovery skips would spend real tokens
/// producing findings the live loop had already decided nobody wants.
#[tokio::test]
async fn history_obeys_the_same_skip_rules_as_discovery() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = install(1).await?;

    // A merge, which §9.4 skips by default.
    git(&fixture.checkout, &["checkout", "-q", "-b", "side"])?;
    std::fs::write(fixture.checkout.join("side.rs"), "fn side() {}\n")?;
    git(&fixture.checkout, &["add", "side.rs"])?;
    git(&fixture.checkout, &["commit", "-q", "-m", "a side change"])?;
    git(&fixture.checkout, &["checkout", "-q", "main"])?;
    git(
        &fixture.checkout,
        &["merge", "-q", "--no-ff", "-m", "merge side", "side"],
    )?;

    let changes = history(&fixture).await?;
    let merges = changes.iter().filter(|c| c.parents.len() > 1).count();
    assert_eq!(
        merges, 1,
        "the fixture should contain one merge: {changes:?}"
    );

    let scope = backfill_scope(&revlocal_core::Cursor::commits_scope("main"));
    let planned = plan(fixture.repo.id, &scope, &items(&changes), None, None);
    let outcome = execute(
        &fixture.pool,
        &config(),
        &NullSink,
        &fixture.data_dir(),
        &fixture.repo,
        planned,
        &changes,
        at(1),
        &CancellationToken::new(),
    )
    .await?;

    assert_eq!(
        outcome.skipped.len(),
        1,
        "the merge must be skipped, and the skip must be reported: {outcome:?}"
    );
    assert!(
        outcome.skipped[0].contains("merge"),
        "the reason is the point — §9.4 requires it be recorded: {:?}",
        outcome.skipped
    );

    // Recorded as a skipped run, not merely omitted. "rev-local ignored my
    // commit" has an answer only if the decision is in the database.
    let skipped = RunStore::new(&fixture.pool)
        .list_recent(Some(fixture.repo.id), Some(RunStatus::Skipped), 50)
        .await?;
    assert_eq!(skipped.len(), 1, "{skipped:?}");
    assert!(
        skipped[0].skip_reason.is_some(),
        "a skipped run without its reason is the silence §9.4 forbids: {skipped:?}"
    );

    // And the cursor still moved past it: a skipped change is *handled*, and a
    // sweep that stalled on one would never reach anything behind it.
    let stored = CursorStore::new(&fixture.pool)
        .get(fixture.repo.id, &scope)
        .await?
        .ok_or("the cursor must advance past a skipped item too")?;
    assert_eq!(stored.value, changes[changes.len() - 1].external_id);

    Ok(())
}
