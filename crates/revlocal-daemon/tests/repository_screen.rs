//! The repository screen against a real store and a real repository on disk
//! (RL-1106, SPEC §15 screen 2).
//!
//! The unit tests beside `repository_view` pin the indicator logic. These pin the
//! parts that only a filesystem and a database can answer: that the hooks
//! indicator reports what is actually on disk, that saving an invalid config
//! leaves the stored one alone, and that a git and an SVN repository render in
//! their own vocabularies end to end.
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use chrono::TimeZone;
use revlocal_core::{AutonomyMode, BudgetSettings, EngineKind, Repo, RepoId, RepoKind, Timestamp};
use revlocal_daemon::hooks::{self, HookMode};
use revlocal_daemon::repository_view::{self, TriggerState, Watching};
use revlocal_store::{open, Pool, RepoStore};
use tempfile::TempDir;

fn at(minute: u32) -> Timestamp {
    chrono::Utc
        .with_ymd_and_hms(2026, 8, 30, 10, minute, 0)
        .single()
        .unwrap_or_default()
}

/// Returns an `Option` rather than panicking: ADR 0003 keeps panics inside
/// `#[test]` functions, where a failure is a failing test rather than a helper
/// aborting mid-way through one.
fn find<'a>(
    view: &'a repository_view::RepositoryView,
    name: &str,
) -> Option<&'a repository_view::TriggerStatus> {
    view.triggers.iter().find(|s| s.trigger == name)
}

async fn seeded(
    kind: RepoKind,
    local_path: Option<String>,
    config_json: &str,
) -> Result<(TempDir, Pool, i64), Box<dyn std::error::Error>> {
    let dir = TempDir::new()?;
    let pool = open(&dir.path().join("rev-local.db")).await?;

    let repo = RepoStore::new(&pool)
        .insert(&Repo {
            id: RepoId::new(0),
            name: "acme".to_owned(),
            kind,
            local_path,
            remote_url: None,
            default_branch: Some("main".to_owned()),
            engine: EngineKind::Mock,
            autonomy: AutonomyMode::DryRun,
            enabled: true,
            config_json: config_json.to_owned(),
            created_at: at(0),
            updated_at: at(0),
        })
        .await?;

    Ok((dir, pool, repo.id.get()))
}

/// A real git repository with rev-local's hooks installed.
fn repo_with_hooks() -> Result<TempDir, Box<dyn std::error::Error>> {
    let dir = TempDir::new()?;
    std::process::Command::new("git")
        .args(["init", "-q", "-b", "main", "."])
        .current_dir(dir.path())
        .status()?;
    hooks::install(
        dir.path(),
        "acme",
        HookMode::Reference,
        8080,
        "REVLOCAL_HOOK_SECRET",
    )?;
    Ok(dir)
}

#[tokio::test]
async fn a_git_repository_reports_branches_and_four_triggers() {
    let (_dir, pool, id) = seeded(RepoKind::Git, None, r#"{"branches":["main","release/*"]}"#)
        .await
        .expect("seed");

    let view = repository_view::gather(&pool, id, &BudgetSettings::default(), 0, at(1))
        .await
        .expect("gather");

    assert_eq!(
        view.watching,
        Watching::Branches {
            globs: vec!["main".to_owned(), "release/*".to_owned()]
        }
    );
    let names: Vec<&str> = view.triggers.iter().map(|s| s.trigger.as_str()).collect();
    assert_eq!(names, vec!["poll", "hooks", "webhook", "manual"]);

    // The editor is handed the whole document, defaults spelled out, so a field
    // nobody set is visible rather than absent.
    assert!(view.config_json.contains("review_prs"));
    assert!(view.config_json.contains("release/*"));
}

#[tokio::test]
async fn an_svn_repository_watches_paths_and_says_hooks_do_not_apply() {
    // Criterion 3 end to end. §6.4: an SVN "branch" is a directory, and the
    // screen has to say so rather than borrowing git's words.
    let (_dir, pool, id) = seeded(
        RepoKind::Svn,
        None,
        r#"{"branches":["main"],"watch_branches":true}"#,
    )
    .await
    .expect("seed");

    let view = repository_view::gather(&pool, id, &BudgetSettings::default(), 8080, at(1))
        .await
        .expect("gather");

    assert_eq!(
        view.watching,
        Watching::Paths {
            paths: vec!["trunk".to_owned(), "branches/*".to_owned()]
        }
    );
    // Stale git branch globs in the config must not surface as a filter that is
    // not being applied.
    match &view.watching {
        Watching::Paths { paths } => assert!(!paths.contains(&"main".to_owned())),
        Watching::Branches { .. } => panic!("an SVN repository must not report branches"),
    }

    assert_eq!(
        find(&view, "hooks").expect("hooks indicator").state,
        TriggerState::NotApplicable
    );
    assert_eq!(
        find(&view, "webhook").expect("webhook indicator").state,
        TriggerState::NotApplicable
    );
    // Polling is the only way an SVN repository is watched, so it must be live.
    assert_eq!(
        find(&view, "poll").expect("poll indicator").state,
        TriggerState::Active
    );
}

#[tokio::test]
async fn the_hooks_indicator_reports_what_is_on_disk() {
    // Not what the database believes. A row saying "installed" survives somebody
    // replacing `.git/hooks/post-commit`, re-cloning, or a `husky` install that
    // overwrote it — and would then be reporting an intention, not a fact.
    let checkout = repo_with_hooks().expect("git repo with hooks");
    let path = checkout.path().to_string_lossy().to_string();

    let (_dir, pool, id) = seeded(RepoKind::Git, Some(path.clone()), "{}")
        .await
        .expect("seed");

    let view = repository_view::gather(&pool, id, &BudgetSettings::default(), 0, at(1))
        .await
        .expect("gather");

    let hooks_line = find(&view, "hooks").expect("hooks indicator");
    assert_eq!(hooks_line.state, TriggerState::Active);
    // Named, not counted: "3 hooks installed" cannot be checked and a list can.
    assert!(
        hooks_line.detail.contains("post-commit"),
        "expected the hook names, got {:?}",
        hooks_line.detail
    );

    // Remove them the way something else on the machine would, and the indicator
    // has to follow the disk rather than remembering the install.
    hooks::uninstall(checkout.path(), HookMode::Reference).expect("uninstall");
    let after = repository_view::gather(&pool, id, &BudgetSettings::default(), 0, at(2))
        .await
        .expect("gather");
    assert_eq!(
        find(&after, "hooks").expect("hooks indicator").state,
        TriggerState::Off
    );
}

#[tokio::test]
async fn an_invalid_config_is_refused_and_the_stored_one_is_untouched() {
    // Criterion 2. Validation before the write, not after: a repository polling
    // on a corrupt blob is one §13.2's fallback to defaults would keep quiet.
    let (_dir, pool, id) = seeded(RepoKind::Git, None, r#"{"poll_interval_secs":900}"#)
        .await
        .expect("seed");

    let error = repository_view::save_config(&pool, id, r#"{"poll_interval_secs":"soon"}"#, at(3))
        .await
        .expect_err("a string is not an interval");

    let text = error.to_string();
    assert!(text.contains("line") && text.contains("column"), "{text:?}");

    let stored = RepoStore::new(&pool)
        .get(RepoId::new(id))
        .await
        .expect("get");
    assert!(
        stored.config_json.contains("900"),
        "the old config survived"
    );
}

#[tokio::test]
async fn a_valid_config_is_stored_normalised() {
    let (_dir, pool, id) = seeded(RepoKind::Git, None, "{}").await.expect("seed");

    let saved = repository_view::save_config(&pool, id, r#"{"poll_interval_secs":900}"#, at(3))
        .await
        .expect("save");

    // What is stored is what was validated, not the bytes somebody typed: a
    // field the parser ignored must not survive into the database and come to
    // mean something later.
    let stored = RepoStore::new(&pool)
        .get(RepoId::new(id))
        .await
        .expect("get");
    assert_eq!(stored.config_json, saved);

    let view = repository_view::gather(&pool, id, &BudgetSettings::default(), 0, at(4))
        .await
        .expect("gather");
    assert!(view.config_json.contains("900"));
    // And the change reaches the indicator, which is the point of editing it.
    assert!(find(&view, "poll")
        .expect("poll indicator")
        .detail
        .contains("900"));
}
