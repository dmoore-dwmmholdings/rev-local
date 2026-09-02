//! The unattended loop (RL-1501, SPEC §4.2, §7, §14).
//!
//! The criterion is "leave it running and commits get reviewed", so these tests
//! never queue anything by hand: they make commits, call `tick`, and read the
//! database. Anything a test has to set up itself is something the loop was not
//! doing.
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use chrono::TimeZone;
use revlocal_core::{
    AutonomyMode, Cursor, EngineKind, GlobalConfig, Repo, RepoId, RepoKind, RunStatus, Timestamp,
};
use revlocal_daemon::autopilot;
use revlocal_daemon::state_machine::NullSink;
use revlocal_store::{
    open, CursorStore, Pool, PublishActionStore, RepoStore, RunStore, SettingStore,
};
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

/// A real repository with one commit past the initial one.
fn git_repo(dir: &std::path::Path) -> Result<(), String> {
    git(dir, &["init", "-q", "-b", "main", "."])?;
    git(dir, &["config", "user.email", "fixture@rev-local.invalid"])?;
    git(dir, &["config", "user.name", "Autopilot fixture"])?;
    std::fs::write(dir.join("main.rs"), "fn main() {}\n").map_err(|e| e.to_string())?;
    git(dir, &["add", "main.rs"])?;
    git(dir, &["commit", "-q", "-m", "add a main"])?;
    Ok(())
}

fn commit(dir: &std::path::Path, body: &str, message: &str) -> Result<(), String> {
    std::fs::write(dir.join("main.rs"), body).map_err(|e| e.to_string())?;
    git(dir, &["add", "main.rs"])?;
    git(dir, &["commit", "-q", "-m", message])?;
    Ok(())
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

/// A repository as somebody who has just added one has it: enabled, discovered
/// by nothing, reviewed by nothing.
async fn install() -> Result<Fixture, Box<dyn std::error::Error>> {
    let dir = TempDir::new()?;
    let checkout = dir.path().join("acme");
    std::fs::create_dir_all(&checkout)?;
    git_repo(&checkout)?;

    let pool = open(&dir.path().join("rev-local.db")).await?;
    let repo = RepoStore::new(&pool)
        .insert(&Repo {
            id: RepoId::new(0),
            name: "acme".to_owned(),
            kind: RepoKind::Git,
            local_path: Some(checkout.display().to_string()),
            remote_url: None,
            default_branch: Some("main".to_owned()),
            // The mock spends nothing and announces itself; these tests are about
            // the loop, not the engine.
            engine: EngineKind::Mock,
            autonomy: AutonomyMode::Auto,
            enabled: true,
            config_json: r#"{"andare_project": "ENG"}"#.to_owned(),
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

async fn tick(fixture: &Fixture, minute: u32) -> Result<autopilot::TickReport, String> {
    autopilot::tick(
        &fixture.pool,
        &config(),
        &NullSink,
        &fixture.data_dir(),
        // No *extra* targets: a real MCP endpoint in an inner-loop test would
        // spend tokens and touch the network, both forbidden by the ground rules.
        // The local report target is registered by `tick` itself and writes into
        // the fixture's own data directory.
        &[],
        at(minute),
        &CancellationToken::new(),
    )
    .await
    .map_err(|e| e.to_string())
}

#[tokio::test]
async fn a_new_commit_is_reviewed_without_anybody_asking() -> Result<(), Box<dyn std::error::Error>>
{
    // The whole product, as one sentence. Before this existed the app discovered
    // on a button and drained on another, and the two were never joined.
    let fixture = install().await?;
    commit(
        &fixture.checkout,
        "fn main() {\n    let x = 1;\n}\n",
        "bind a value",
    )?;

    let report = tick(&fixture, 1).await?;

    assert!(
        report.reviewed.iter().any(|outcome| outcome.repo == "acme"),
        "one pass must take a commit from unseen to reviewed: {report:?}"
    );
    let runs = RunStore::new(&fixture.pool)
        .list_recent(None, None, 10)
        .await?;
    assert!(
        runs.iter().any(|run| run.status == RunStatus::Done),
        "a reviewed change leaves a finished run: {runs:?}"
    );
    Ok(())
}

#[tokio::test]
async fn the_cursor_advances_so_a_quiet_repository_stays_quiet(
) -> Result<(), Box<dyn std::error::Error>> {
    // The app's own discovery passed `None` for the cursor and never wrote one
    // back, so every pass re-read the newest fifty commits and "new commit" and
    // "commit I already did" were indistinguishable.
    let fixture = install().await?;
    commit(
        &fixture.checkout,
        "fn main() {\n    let x = 1;\n}\n",
        "bind a value",
    )?;

    tick(&fixture, 1).await?;

    let scope = Cursor::commits_scope("main");
    let cursor = CursorStore::new(&fixture.pool)
        .get(fixture.repo.id, &scope)
        .await?;
    assert!(cursor.is_some(), "the first pass must leave a cursor");

    let second = tick(&fixture, 2).await?;
    let discovered: usize = second.passes.iter().map(|pass| pass.recorded).sum();
    assert_eq!(
        discovered, 0,
        "a second pass over an unchanged repository must find nothing: {second:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_paused_loop_says_how_to_release_it() -> Result<(), Box<dyn std::error::Error>> {
    // §12.1, and §18: "we are stopped" must never render as "nothing to do".
    let fixture = install().await?;
    SettingStore::new(&fixture.pool)
        .set_paused(true, at(0))
        .await?;

    let report = tick(&fixture, 1).await?;

    assert!(report.paused);
    assert!(report.passes.is_empty(), "nothing may run while paused");
    let stopped = report.stopped.clone().unwrap_or_default();
    assert!(stopped.contains("kill switch"), "{stopped}");
    assert!(
        stopped.contains("revlocal resume"),
        "a stop with no remedy is a dead end: {stopped}"
    );
    assert_eq!(report.line(), stopped);
    Ok(())
}

#[tokio::test]
async fn an_empty_install_is_not_a_stopped_one() -> Result<(), Box<dyn std::error::Error>> {
    // Somebody who has not added a repository yet is halfway through setting up,
    // not halted. Rendering the two the same sends them looking for a switch.
    let dir = TempDir::new()?;
    let pool = open(&dir.path().join("rev-local.db")).await?;
    let report = autopilot::tick(
        &pool,
        &config(),
        &NullSink,
        &dir.path().join("data"),
        &[],
        at(1),
        &CancellationToken::new(),
    )
    .await?;

    assert_eq!(report.repos, 0);
    assert!(report.stopped.is_none(), "{report:?}");
    assert!(
        report.line().contains("No repositories"),
        "{}",
        report.line()
    );
    Ok(())
}

#[tokio::test]
async fn a_finding_is_written_to_disk_with_no_tracker_configured(
) -> Result<(), Box<dyn std::error::Error>> {
    // "GitHub issues, Andare issues, **or local reports**" — the third one never
    // existed, so a machine with no tracker reviewed its commits and produced
    // nothing anybody could read. The report target needs no configuration, so
    // this is the output that must work on a fresh install.
    let fixture = install().await?;
    commit(
        &fixture.checkout,
        "fn main() {\n    let x = 1;\n}\n",
        "bind a value",
    )?;

    let report = tick(&fixture, 1).await?;

    assert!(
        report.published > 0,
        "a review with findings must leave a report on disk: {report:?}"
    );

    let directory = fixture.data_dir().join("reports").join("acme");
    let written: Vec<_> = std::fs::read_dir(&directory)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
        .collect();
    assert!(
        !written.is_empty(),
        "no report under {}: {report:?}",
        directory.display()
    );

    let body = std::fs::read_to_string(&written[0])?;
    assert!(
        body.starts_with("# "),
        "a report leads with its title: {body}"
    );
    assert!(
        body.contains("rev-local-fingerprint"),
        "the fingerprint trailer is what makes a re-review reuse this file: {body}"
    );

    // §11.6: reviewing the same finding again rewrites one file rather than
    // accumulating a directory of duplicates.
    commit(
        &fixture.checkout,
        "fn main() {\n    let x = 1;\n    let y = 2;\n}\n",
        "bind another",
    )?;
    tick(&fixture, 2).await?;
    let after = std::fs::read_dir(&directory)?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "md"))
        .count();
    assert_eq!(
        after,
        written.len(),
        "a recurring finding must not duplicate"
    );
    Ok(())
}

#[tokio::test]
async fn a_repository_whose_checkout_is_gone_costs_nothing_and_says_so(
) -> Result<(), Box<dyn std::error::Error>> {
    // Two of three repositories on a real install had been deleted, were still
    // enabled, and had 51 runs waiting against them. Every one of those would
    // have spent an engine to learn what one `exists` call knows (RL-1513).
    let fixture = install().await?;
    commit(
        &fixture.checkout,
        "fn main() {\n    let x = 1;\n}\n",
        "bind a value",
    )?;
    std::fs::remove_dir_all(&fixture.checkout)?;

    let report = tick(&fixture, 1).await?;

    assert!(
        report.reviewed.is_empty(),
        "a missing checkout must not be reviewed: {report:?}"
    );
    assert_eq!(report.queued, 0, "nor queued: {report:?}");
    assert!(report.passes.is_empty(), "nor discovered: {report:?}");

    // §18: it must say so, and say what to do. Silence here reads as a quiet
    // repository, which is the one thing it is not.
    let note = report.notes.join("\n");
    assert!(note.contains("checkout is gone"), "{note}");
    assert!(
        note.contains("acme"),
        "the note must name the repository: {note}"
    );
    assert!(
        note.contains("try:"),
        "a problem with no remedy is a dead end: {note}"
    );
    Ok(())
}

#[tokio::test]
async fn one_missing_checkout_does_not_stop_the_others() -> Result<(), Box<dyn std::error::Error>> {
    // The rule discovery already follows, applied one step earlier.
    let fixture = install().await?;
    commit(
        &fixture.checkout,
        "fn main() {\n    let x = 1;\n}\n",
        "bind a value",
    )?;

    let gone = fixture.dir.path().join("vanished");
    RepoStore::new(&fixture.pool)
        .insert(&Repo {
            id: RepoId::new(0),
            name: "vanished".to_owned(),
            kind: RepoKind::Git,
            local_path: Some(gone.display().to_string()),
            remote_url: None,
            default_branch: Some("main".to_owned()),
            engine: EngineKind::Mock,
            autonomy: AutonomyMode::Auto,
            enabled: true,
            config_json: r#"{"andare_project": "ENG"}"#.to_owned(),
            created_at: at(0),
            updated_at: at(0),
        })
        .await?;

    let report = tick(&fixture, 1).await?;

    assert!(
        report.reviewed.iter().any(|outcome| outcome.repo == "acme"),
        "the reachable repository must still be reviewed: {report:?}"
    );
    assert!(
        report.notes.iter().any(|note| note.contains("vanished")),
        "and the missing one still reported: {report:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_repository_added_without_its_remote_gets_one_recorded(
) -> Result<(), Box<dyn std::error::Error>> {
    // Every repository on the live install had an empty `remote_url`, because
    // `add` never asked the checkout for one. The GitHub target then held every
    // finding with "no recognisable GitHub remote" — right behaviour, wrong data
    // (RL-1514).
    let fixture = install().await?;
    git(
        &fixture.checkout,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/widgets.git",
        ],
    )?;
    assert!(
        fixture.repo.remote_url.is_none(),
        "the fixture starts empty"
    );

    tick(&fixture, 1).await?;

    let stored = RepoStore::new(&fixture.pool).list().await?;
    let acme = stored
        .iter()
        .find(|repo| repo.name == "acme")
        .ok_or("the repository")?;
    assert_eq!(
        acme.remote_url.as_deref(),
        Some("https://github.com/acme/widgets.git"),
        "the remote must be written down, not just read"
    );
    Ok(())
}

#[tokio::test]
async fn a_repository_with_no_remote_is_left_alone() -> Result<(), Box<dyn std::error::Error>> {
    // A local-only checkout is a normal thing to review. Recording an empty
    // string, or reporting it as a problem, would both be wrong.
    let fixture = install().await?;

    let report = tick(&fixture, 1).await?;

    let stored = RepoStore::new(&fixture.pool).list().await?;
    let acme = stored
        .iter()
        .find(|repo| repo.name == "acme")
        .ok_or("the repository")?;
    assert_eq!(acme.remote_url, None);
    assert!(
        !report.notes.iter().any(|note| note.contains("remote")),
        "having no remote is not worth telling somebody about: {report:?}"
    );
    Ok(())
}

// --- housekeeping (RL-1520) -------------------------------------------------

#[tokio::test]
async fn old_runs_and_their_transcripts_are_cleared() -> Result<(), Box<dyn std::error::Error>> {
    // `delete_finished_before` existed, was tested, and nothing called it.
    // `transcript_retention_days` was read in one place — to display it. A loop
    // reviewing every commit forever accumulated a row and a file per review.
    let fixture = install().await?;

    let transcript = fixture.data_dir().join("transcripts");
    std::fs::create_dir_all(&transcript)?;
    let old_log = transcript.join("1.log");
    std::fs::write(&old_log, "an old transcript")?;

    let long_ago = at(1) - chrono::Duration::days(400);
    RunStore::new(&fixture.pool)
        .insert(&revlocal_core::Run {
            id: revlocal_core::RunId::new(0),
            change_id: seed_change(&fixture, long_ago).await?,
            attempt: 1,
            status: revlocal_core::RunStatus::Done,
            engine: EngineKind::Mock,
            depth: revlocal_core::Depth::Summary,
            trigger: revlocal_core::TriggerSource::Poll,
            skip_reason: None,
            error: None,
            error_detail: None,
            degraded: None,
            usage: revlocal_core::Usage::default(),
            started_at: Some(long_ago),
            finished_at: Some(long_ago),
            transcript_path: Some(old_log.display().to_string()),
            truncated: false,
            omitted_files: Vec::new(),
            verdict: None,
            summary: None,
            created_at: long_ago,
        })
        .await?;

    let report = tick(&fixture, 1).await?;

    assert!(report.pruned > 0, "the old run must be cleared: {report:?}");
    assert!(
        !old_log.exists(),
        "the transcript goes with the row it belonged to"
    );
    Ok(())
}

#[tokio::test]
async fn a_recent_run_is_left_alone() -> Result<(), Box<dyn std::error::Error>> {
    // The window is thirty days. A run from this morning is not old.
    let fixture = install().await?;
    commit(
        &fixture.checkout,
        "fn main() {\n    let x = 1;\n}\n",
        "bind a value",
    )?;

    tick(&fixture, 1).await?;
    let before = RunStore::new(&fixture.pool)
        .list_recent(None, None, 50)
        .await?
        .len();

    // A second pass a day later sweeps, and must not take today's work with it.
    let later = autopilot::tick(
        &fixture.pool,
        &config(),
        &NullSink,
        &fixture.data_dir(),
        &[],
        at(1) + chrono::Duration::days(2),
        &CancellationToken::new(),
    )
    .await?;

    assert_eq!(later.pruned, 0, "{later:?}");
    let after = RunStore::new(&fixture.pool)
        .list_recent(None, None, 50)
        .await?
        .len();
    assert_eq!(after, before, "recent runs must survive a sweep");
    Ok(())
}

#[tokio::test]
async fn the_sweep_does_not_run_on_every_tick() -> Result<(), Box<dyn std::error::Error>> {
    // The delete scans, the window is thirty days, and the loop ticks every
    // minute. Sweeping every tick is a table scan a minute to remove nothing.
    let fixture = install().await?;

    tick(&fixture, 1).await?;
    let first = SettingStore::new(&fixture.pool)
        .get(autopilot::SETTING_LAST_SWEEP)
        .await?;
    assert!(first.is_some(), "the first pass sweeps and records when");

    tick(&fixture, 2).await?;
    let second = SettingStore::new(&fixture.pool)
        .get(autopilot::SETTING_LAST_SWEEP)
        .await?;
    assert_eq!(second, first, "a tick a minute later must not sweep again");
    Ok(())
}

/// A change row for a run to belong to.
async fn seed_change(
    fixture: &Fixture,
    when: Timestamp,
) -> Result<revlocal_core::ChangeId, Box<dyn std::error::Error>> {
    Ok(revlocal_store::ChangeStore::new(&fixture.pool)
        .upsert(&revlocal_core::Change {
            id: revlocal_core::ChangeId::new(0),
            repo_id: fixture.repo.id,
            kind: revlocal_core::ChangeKind::Commit,
            external_id: format!("old-{}", when.timestamp()),
            title: Some("an old commit".to_owned()),
            author_name: None,
            author_email: None,
            authored_at: None,
            branch: Some("main".to_owned()),
            base_ref: None,
            head_ref: None,
            url: None,
            diff_stat: revlocal_core::DiffStat::default(),
            detected_at: when,
        })
        .await?
        .id)
}

// --- approvals do not wait forever (RL-1523) --------------------------------

#[tokio::test]
async fn an_approval_nobody_answered_expires() -> Result<(), Box<dyn std::error::Error>> {
    // §12.4 gives an approval a deadline and every piece of the mechanism existed
    // — `expires_at`, `is_expired`, `REASON_EXPIRED`, `AUDIT_KIND_EXPIRED`,
    // `approval_ttl_hours` — with nothing calling any of it.
    let fixture = install().await?;
    let action = an_action_awaiting_approval(&fixture, at(1)).await?;

    // Well past the 72-hour default.
    let later = at(1) + chrono::Duration::days(5);
    let report = autopilot::tick(
        &fixture.pool,
        &config(),
        &NullSink,
        &fixture.data_dir(),
        &[],
        later,
        &CancellationToken::new(),
    )
    .await?;

    assert_eq!(report.expired, 1, "{report:?}");

    let stored = PublishActionStore::new(&fixture.pool).get(action).await?;
    assert_eq!(stored.status, revlocal_core::PublishActionStatus::Rejected);
    Ok(())
}

#[tokio::test]
async fn an_expiry_is_told_apart_from_somebody_declining() -> Result<(), Box<dyn std::error::Error>>
{
    // They mean opposite things about the finding: one is a judgement, the other
    // is that nobody made one.
    let fixture = install().await?;
    an_action_awaiting_approval(&fixture, at(1)).await?;

    autopilot::tick(
        &fixture.pool,
        &config(),
        &NullSink,
        &fixture.data_dir(),
        &[],
        at(1) + chrono::Duration::days(5),
        &CancellationToken::new(),
    )
    .await?;

    let audit = revlocal_store::AuditStore::new(&fixture.pool)
        .recent(10)
        .await?;
    let entry = audit
        .iter()
        .find(|entry| entry.kind == "approval_expired")
        .ok_or("an expiry must be in the audit log")?;
    assert!(
        entry.detail_json.contains("expired"),
        "{}",
        entry.detail_json
    );
    assert_eq!(entry.actor, "daemon", "nobody decided this");
    Ok(())
}

#[tokio::test]
async fn an_approval_still_inside_its_window_is_left_alone(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = install().await?;
    let action = an_action_awaiting_approval(&fixture, at(1)).await?;

    let report = autopilot::tick(
        &fixture.pool,
        &config(),
        &NullSink,
        &fixture.data_dir(),
        &[],
        at(1) + chrono::Duration::hours(2),
        &CancellationToken::new(),
    )
    .await?;

    assert_eq!(report.expired, 0, "{report:?}");
    let stored = PublishActionStore::new(&fixture.pool).get(action).await?;
    assert_eq!(
        stored.status,
        revlocal_core::PublishActionStatus::AwaitingApproval
    );
    Ok(())
}

/// One action sitting in the inbox since `queued_at`.
async fn an_action_awaiting_approval(
    fixture: &Fixture,
    queued_at: Timestamp,
) -> Result<revlocal_core::PublishActionId, Box<dyn std::error::Error>> {
    let change = revlocal_store::ChangeStore::new(&fixture.pool)
        .upsert(&revlocal_core::Change {
            id: revlocal_core::ChangeId::new(0),
            repo_id: fixture.repo.id,
            kind: revlocal_core::ChangeKind::Commit,
            external_id: "waiting".to_owned(),
            title: None,
            author_name: None,
            author_email: None,
            authored_at: None,
            branch: Some("main".to_owned()),
            base_ref: None,
            head_ref: None,
            url: None,
            diff_stat: revlocal_core::DiffStat::default(),
            detected_at: queued_at,
        })
        .await?;

    let run = RunStore::new(&fixture.pool)
        .insert(&revlocal_core::Run {
            id: revlocal_core::RunId::new(0),
            change_id: change.id,
            attempt: 1,
            status: revlocal_core::RunStatus::AwaitingApproval,
            engine: EngineKind::Mock,
            depth: revlocal_core::Depth::Summary,
            trigger: revlocal_core::TriggerSource::Poll,
            skip_reason: None,
            error: None,
            error_detail: None,
            degraded: None,
            usage: revlocal_core::Usage::default(),
            started_at: Some(queued_at),
            finished_at: Some(queued_at),
            transcript_path: None,
            truncated: false,
            omitted_files: Vec::new(),
            verdict: None,
            summary: None,
            created_at: queued_at,
        })
        .await?;

    Ok(PublishActionStore::new(&fixture.pool)
        .insert(&revlocal_core::PublishAction {
            id: revlocal_core::PublishActionId::new(0),
            run_id: run.id,
            finding_id: None,
            target: "andare".to_owned(),
            capability: revlocal_core::Capability::CreateIssue,
            risk: revlocal_core::RiskClass::High,
            idempotency_key: "andare-waiting".to_owned(),
            payload_json: "{}".to_owned(),
            status: revlocal_core::PublishActionStatus::AwaitingApproval,
            attempts: 0,
            response_json: None,
            external_ref: None,
            error: None,
            created_at: queued_at,
            sent_at: None,
        })
        .await?
        .id)
}

// --- the configured limits are the ones used (RL-1524) ----------------------

#[tokio::test]
async fn the_configured_concurrency_ceiling_is_the_one_that_applies(
) -> Result<(), Box<dyn std::error::Error>> {
    // Every call site used `DEFAULT_MAX_CONCURRENT_RUNS` rather than the config,
    // so `max_concurrent_runs` did nothing. The existing tests missed it by
    // asserting the constant equals its documented value — which it does, and
    // which says nothing about whether a configured value is honoured.
    let fixture = install().await?;
    for (n, body) in [(1, "let a = 1;"), (2, "let b = 2;"), (3, "let c = 3;")] {
        commit(
            &fixture.checkout,
            &format!("fn main() {{\n    {body}\n}}\n"),
            &format!("change {n}"),
        )?;
    }

    let mut config = config();
    config.global.max_concurrent_runs = 1;

    let report = autopilot::tick(
        &fixture.pool,
        &config,
        &NullSink,
        &fixture.data_dir(),
        &[],
        at(1),
        &CancellationToken::new(),
    )
    .await?;

    assert_eq!(
        report.reviewed.len(),
        1,
        "a ceiling of one means one review per pass: {report:?}"
    );
    assert!(
        report.still_queued > 0,
        "and the rest stay queued rather than being dropped: {report:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_ceiling_of_zero_falls_back_rather_than_stopping_everything(
) -> Result<(), Box<dyn std::error::Error>> {
    // An unset field must not become a loop that reviews nothing while reporting
    // itself healthy.
    let fixture = install().await?;
    commit(
        &fixture.checkout,
        "fn main() {\n    let x = 1;\n}\n",
        "bind a value",
    )?;

    let mut config = config();
    config.global.max_concurrent_runs = 0;

    let report = autopilot::tick(
        &fixture.pool,
        &config,
        &NullSink,
        &fixture.data_dir(),
        &[],
        at(1),
        &CancellationToken::new(),
    )
    .await?;

    assert!(!report.reviewed.is_empty(), "{report:?}");
    Ok(())
}
