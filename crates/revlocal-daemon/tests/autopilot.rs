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
use revlocal_store::{open, CursorStore, Pool, RepoStore, RunStore, SettingStore};
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
