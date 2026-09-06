//! Registering every repository on the disk at once (REVL-193, REVL-194).
//!
//! The goal rev-local is measured against is that it sits pointed at every local
//! repository and reviews each new commit without anybody pressing anything. Two
//! things stood between the code and that: adding repositories was one command
//! each, and every repository added arrived in `dry_run`, which records findings
//! and publishes nothing — so a scan of thirty would have delivered nothing
//! until thirty `repo set` commands had been typed.
//!
//! These tests hold both ends: the scan registers what is there, and the
//! autonomy a scan gives is something an install decides once.
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use chrono::TimeZone;
use revlocal_core::{AutonomyMode, Timestamp};
use revlocal_daemon::repos;
use revlocal_store::{open, Pool, RepoStore};
use tempfile::TempDir;

fn at() -> Timestamp {
    chrono::Utc
        .with_ymd_and_hms(2026, 9, 3, 12, 0, 0)
        .single()
        .unwrap_or_default()
}

/// Run git in a directory, failing loudly.
fn git(dir: &std::path::Path, args: &[&str]) -> Result<(), String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| format!("git {args:?}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

/// A directory tree with `git` and `svn` working copies in it.
fn tree(root: &std::path::Path, dirs: &[&str]) -> Result<(), String> {
    for dir in dirs {
        std::fs::create_dir_all(root.join(dir)).map_err(|e| format!("{dir}: {e}"))?;
    }
    Ok(())
}

async fn fresh() -> Result<(TempDir, Pool), Box<dyn std::error::Error>> {
    let dir = TempDir::new()?;
    let pool = open(&dir.path().join("rev-local.db")).await?;
    Ok((dir, pool))
}

#[tokio::test]
async fn a_scan_registers_every_checkout_it_finds() -> Result<(), Box<dyn std::error::Error>> {
    let (dir, pool) = fresh().await?;
    let code = dir.path().join("code");
    tree(&code, &["acme/.git", "beta/.git", "legacy/.svn", "notes"])?;

    let report = repos::scan(&pool, &code, 4, "claude", None, false, at()).await?;

    assert_eq!(report.added, 3, "{report:?}");
    assert_eq!(report.skipped, 0, "{report:?}");

    let mut names: Vec<String> = RepoStore::new(&pool)
        .list()
        .await?
        .into_iter()
        .map(|repo| repo.name)
        .collect();
    names.sort();
    assert_eq!(names, vec!["acme", "beta", "legacy"]);
    Ok(())
}

#[tokio::test]
async fn a_scan_records_the_kind_each_working_copy_actually_is(
) -> Result<(), Box<dyn std::error::Error>> {
    let (dir, pool) = fresh().await?;
    let code = dir.path().join("code");
    tree(&code, &["gitrepo/.git", "svnrepo/.svn"])?;

    repos::scan(&pool, &code, 4, "claude", None, false, at()).await?;

    let kinds: Vec<(String, String)> = RepoStore::new(&pool)
        .list()
        .await?
        .into_iter()
        .map(|repo| (repo.name, repo.kind.as_str().to_owned()))
        .collect();
    assert!(
        kinds.contains(&("gitrepo".to_owned(), "git".to_owned()))
            && kinds.contains(&("svnrepo".to_owned(), "svn".to_owned())),
        "an svn checkout registered as git is reviewed by the wrong adapter: {kinds:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_dry_run_scan_writes_nothing_and_says_what_it_would_do(
) -> Result<(), Box<dyn std::error::Error>> {
    let (dir, pool) = fresh().await?;
    let code = dir.path().join("code");
    tree(&code, &["acme/.git", "beta/.git"])?;

    let report = repos::scan(&pool, &code, 4, "claude", None, true, at()).await?;

    assert_eq!(report.added, 2, "the preview still counts them: {report:?}");
    assert!(report.entries.iter().all(|e| e.outcome == "would_add"));
    assert!(
        RepoStore::new(&pool).list().await?.is_empty(),
        "a dry run wrote to the database"
    );
    Ok(())
}

#[tokio::test]
async fn scanning_twice_adds_nothing_the_second_time() -> Result<(), Box<dyn std::error::Error>> {
    let (dir, pool) = fresh().await?;
    let code = dir.path().join("code");
    tree(&code, &["acme/.git"])?;

    repos::scan(&pool, &code, 4, "claude", None, false, at()).await?;
    let second = repos::scan(&pool, &code, 4, "claude", None, false, at()).await?;

    assert_eq!(second.added, 0, "{second:?}");
    assert_eq!(second.skipped, 1);
    assert_eq!(
        second.entries[0].reason.as_deref(),
        Some("already configured"),
        "the second scan must say why, not fail: {second:?}"
    );
    assert_eq!(RepoStore::new(&pool).list().await?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn two_checkouts_with_the_same_directory_name_both_land(
) -> Result<(), Box<dyn std::error::Error>> {
    let (dir, pool) = fresh().await?;
    let code = dir.path().join("code");
    tree(&code, &["acme/api/.git", "beta/api/.git"])?;

    let report = repos::scan(&pool, &code, 4, "claude", None, false, at()).await?;

    assert_eq!(report.added, 2, "one of them was dropped: {report:?}");
    let mut names: Vec<String> = RepoStore::new(&pool)
        .list()
        .await?
        .into_iter()
        .map(|repo| repo.name)
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec!["api", "beta-api"],
        "the second is disambiguated by the directory above it"
    );
    Ok(())
}

#[tokio::test]
async fn a_repository_that_cannot_be_registered_does_not_stop_the_others(
) -> Result<(), Box<dyn std::error::Error>> {
    let (dir, pool) = fresh().await?;
    let code = dir.path().join("code");
    tree(&code, &["acme/.git", "beta/.git"])?;

    // The same name, at a different path: `add` refuses it, and the scan must
    // carry on to `beta` rather than giving up on the whole tree.
    repos::add(
        &pool,
        &dir.path().join("elsewhere/acme").display().to_string(),
        "git",
        Some("acme"),
        "claude",
        None,
        at(),
    )
    .await?;

    let report = repos::scan(&pool, &code, 4, "claude", None, false, at()).await?;

    assert_eq!(report.added, 2, "both should still land: {report:?}");
    let names: Vec<String> = RepoStore::new(&pool)
        .list()
        .await?
        .into_iter()
        .map(|repo| repo.name)
        .collect();
    assert!(
        names.contains(&"beta".to_owned()) && names.contains(&"code-acme".to_owned()),
        "the collision is renamed, not dropped: {names:?}"
    );
    Ok(())
}

#[tokio::test]
async fn adding_a_checkout_records_the_branch_it_is_actually_on(
) -> Result<(), Box<dyn std::error::Error>> {
    // REVL-200. `branches` defaults to `["main", "release/*"]`, so a repository
    // on `master` was registered, polled, reported healthy and never reviewed.
    // The branch is one `git symbolic-ref` away at the moment somebody adds it,
    // and the alternative was `repo set <name> default_branch=master` typed once
    // per repository that a scan had just added in bulk.
    let (dir, pool) = fresh().await?;
    let checkout = dir.path().join("on-master");
    std::fs::create_dir_all(&checkout)?;
    git(&checkout, &["init", "-q", "-b", "master", "."])?;

    repos::add(
        &pool,
        &checkout.display().to_string(),
        "git",
        None,
        "claude",
        None,
        at(),
    )
    .await?;

    let repo = RepoStore::new(&pool)
        .list()
        .await?
        .into_iter()
        .next()
        .ok_or("nothing was added")?;
    assert_eq!(repo.default_branch.as_deref(), Some("master"), "{repo:?}");

    let config: revlocal_core::RepoConfig = serde_json::from_str(&repo.config_json)?;
    assert!(
        config.branches.iter().any(|pattern| pattern == "master"),
        "the stored patterns decide what is ever discovered, and nothing matched \
         `master`: {:?}",
        config.branches
    );
    assert!(
        config.branches.iter().any(|pattern| pattern == "release/*"),
        "the defaults are kept, not replaced — a repository on master still \
         wants its release branches watched: {:?}",
        config.branches
    );
    Ok(())
}

#[tokio::test]
async fn a_checkout_already_on_main_stores_no_override() -> Result<(), Box<dyn std::error::Error>> {
    // The common case stores `{}` and keeps following the defaults, so a change
    // to what rev-local watches by default still reaches it. Freezing today's
    // patterns into every row would make that impossible.
    let (dir, pool) = fresh().await?;
    let checkout = dir.path().join("on-main");
    std::fs::create_dir_all(&checkout)?;
    git(&checkout, &["init", "-q", "-b", "main", "."])?;

    repos::add(
        &pool,
        &checkout.display().to_string(),
        "git",
        None,
        "claude",
        None,
        at(),
    )
    .await?;

    let repo = RepoStore::new(&pool)
        .list()
        .await?
        .into_iter()
        .next()
        .ok_or("nothing was added")?;
    assert_eq!(repo.default_branch.as_deref(), Some("main"));
    assert_eq!(repo.config_json, "{}", "{repo:?}");
    Ok(())
}

#[tokio::test]
async fn a_repository_added_without_an_autonomy_publishes_nothing_by_default(
) -> Result<(), Box<dyn std::error::Error>> {
    let (dir, pool) = fresh().await?;
    let path = dir.path().join("acme");
    std::fs::create_dir_all(&path)?;

    repos::add(
        &pool,
        &path.display().to_string(),
        "git",
        None,
        "claude",
        None,
        at(),
    )
    .await?;

    let repo = RepoStore::new(&pool)
        .list()
        .await?
        .into_iter()
        .next()
        .ok_or("nothing was added")?;
    assert_eq!(
        repo.autonomy,
        AutonomyMode::DryRun,
        "an install that never set a default must behave as it always did"
    );
    Ok(())
}

#[tokio::test]
async fn the_install_wide_default_is_what_a_scan_gives_every_repository(
) -> Result<(), Box<dyn std::error::Error>> {
    let (dir, pool) = fresh().await?;
    let code = dir.path().join("code");
    tree(&code, &["acme/.git", "beta/.git"])?;

    repos::set_default_autonomy(&pool, "auto", at()).await?;
    let report = repos::scan(&pool, &code, 4, "claude", None, false, at()).await?;

    assert_eq!(report.autonomy, "auto", "{report:?}");
    assert!(
        RepoStore::new(&pool)
            .list()
            .await?
            .iter()
            .all(|repo| repo.autonomy == AutonomyMode::Auto),
        "the point of the setting is not having to say it thirty times"
    );
    Ok(())
}

#[tokio::test]
async fn an_explicit_autonomy_still_wins_over_the_default() -> Result<(), Box<dyn std::error::Error>>
{
    let (dir, pool) = fresh().await?;
    let code = dir.path().join("code");
    tree(&code, &["acme/.git"])?;

    repos::set_default_autonomy(&pool, "auto", at()).await?;
    repos::scan(&pool, &code, 4, "claude", Some("dry_run"), false, at()).await?;

    let repo = RepoStore::new(&pool)
        .list()
        .await?
        .into_iter()
        .next()
        .ok_or("nothing was added")?;
    assert_eq!(repo.autonomy, AutonomyMode::DryRun);
    Ok(())
}

#[tokio::test]
async fn changing_the_default_leaves_repositories_that_already_exist_alone(
) -> Result<(), Box<dyn std::error::Error>> {
    let (dir, pool) = fresh().await?;
    let path = dir.path().join("acme");
    std::fs::create_dir_all(&path)?;
    repos::add(
        &pool,
        &path.display().to_string(),
        "git",
        None,
        "claude",
        None,
        at(),
    )
    .await?;

    repos::set_default_autonomy(&pool, "auto", at()).await?;

    let repo = RepoStore::new(&pool)
        .list()
        .await?
        .into_iter()
        .next()
        .ok_or("nothing was added")?;
    assert_eq!(
        repo.autonomy,
        AutonomyMode::DryRun,
        "widening the default must not silently widen thirty repositories \
         somebody has already looked at"
    );
    Ok(())
}

#[tokio::test]
async fn a_default_that_is_not_a_mode_does_not_make_adding_impossible(
) -> Result<(), Box<dyn std::error::Error>> {
    let (_dir, pool) = fresh().await?;
    revlocal_store::SettingStore::new(&pool)
        .set(repos::SETTING_DEFAULT_AUTONOMY, "enthusiastic", at())
        .await?;

    let report = repos::defaults(&pool, None, at()).await?;

    assert_eq!(
        report.autonomy, "dry_run",
        "a corrupt setting falls back to the safe mode: {report:?}"
    );
    Ok(())
}

#[tokio::test]
async fn defaults_reports_what_it_was_set_to() -> Result<(), Box<dyn std::error::Error>> {
    let (_dir, pool) = fresh().await?;

    let before = repos::defaults(&pool, None, at()).await?;
    assert!(!before.is_set, "nothing is stored yet: {before:?}");

    let after = repos::defaults(&pool, Some("auto_low_ask_high"), at()).await?;
    assert!(after.is_set);
    assert_eq!(after.autonomy, "auto_low_ask_high");
    assert!(
        after.detail.contains("approval"),
        "the report has to say what the mode actually does: {after:?}"
    );
    Ok(())
}
