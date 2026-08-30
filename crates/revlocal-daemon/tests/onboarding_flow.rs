//! The guided path, end to end (RL-1205, SPEC §15).
//!
//! Criterion 1 is "a user with no config reaches a completed dry-run review
//! without editing a file", so this test edits no file: it walks the steps the
//! way the screen does — doctor, add, pick, pick, review — and reads the result.
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use chrono::TimeZone;
use revlocal_core::{AutonomyMode, EngineKind, GlobalConfig, RepoKind, Timestamp};
use revlocal_daemon::onboarding::{self, Draft, Step};
use revlocal_store::{open, Pool};
use tempfile::TempDir;

fn at(minute: u32) -> Timestamp {
    chrono::Utc
        .with_ymd_and_hms(2026, 8, 30, 16, minute, 0)
        .single()
        .unwrap_or_default()
}

/// A real git repository with a commit to review.
fn git_repo(dir: &std::path::Path) -> Result<(), String> {
    let git = |args: &[&str]| -> Result<(), String> {
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
    };

    git(&["init", "-q", "-b", "main", "."])?;
    git(&["config", "user.email", "fixture@rev-local.invalid"])?;
    git(&["config", "user.name", "Onboarding fixture"])?;
    std::fs::write(dir.join("main.rs"), "fn main() {}\n").map_err(|e| e.to_string())?;
    git(&["add", "main.rs"])?;
    git(&["commit", "-q", "-m", "add a main"])?;
    std::fs::write(dir.join("main.rs"), "fn main() {\n    let x = 1;\n}\n")
        .map_err(|e| e.to_string())?;
    git(&["add", "main.rs"])?;
    git(&["commit", "-q", "-m", "bind a value"])
}

async fn fresh() -> Result<(TempDir, Pool, std::path::PathBuf), Box<dyn std::error::Error>> {
    let dir = TempDir::new()?;
    let checkout = dir.path().join("acme");
    std::fs::create_dir_all(&checkout)?;
    git_repo(&checkout)?;

    let pool = open(&dir.path().join("rev-local.db")).await?;
    Ok((dir, pool, checkout))
}

#[tokio::test]
async fn a_fresh_install_is_recognised_as_one() {
    // Asked of the database rather than a stored flag: a flag can be true on a
    // machine with nothing configured — after a restore, or a deleted database —
    // and then onboarding does not offer itself to the person who needs it.
    let (_dir, pool, _checkout) = fresh().await.expect("fixture");

    assert!(onboarding::is_first_run(&pool).await.expect("first run"));
}

#[tokio::test]
async fn the_guided_path_reaches_a_completed_review_without_editing_a_file() {
    // Criterion 1. Nothing here writes a config: §13.1's defaults are what a
    // fresh install has, and this walks the same steps the screen does.
    let (dir, pool, checkout) = fresh().await.expect("fixture");

    let mut flow = onboarding::Onboarding::start(true);
    assert_eq!(flow.step, Step::Check);

    // Step 1 — doctor. Whatever it finds, it must not stop the walk: somebody
    // with no engine installed still gets to the end, with the mock.
    flow.doctor = Some(revlocal_daemon::doctor::gather(0));
    flow.step = flow.step.next().expect("a step after the check");

    // Step 2 — the repository.
    flow.draft = Draft {
        path: checkout.display().to_string(),
        name: "acme".to_owned(),
        kind: RepoKind::Git,
        ..Draft::default()
    };
    assert!(flow.draft.blocker(Step::AddRepo).is_none());

    // Steps 3 and 4 — engine and autonomy, left at their defaults, which is the
    // case worth testing: what happens to somebody who clicks through.
    assert_eq!(flow.draft.engine, EngineKind::Mock);
    assert_eq!(flow.draft.autonomy, AutonomyMode::DryRun);

    let added = onboarding::add_repo(&pool, &flow.draft, at(1))
        .await
        .expect("the repository is added");
    assert_eq!(added.name, "acme");

    // The repository really is in dry run — the criterion is about what reached
    // the database, not about what a form said.
    let stored = revlocal_store::RepoStore::new(&pool)
        .list()
        .await
        .expect("repos");
    assert_eq!(stored[0].autonomy, AutonomyMode::DryRun);

    // Discovery, which is what gives the first review something to look at.
    revlocal_cli_discover(&pool, at(2))
        .await
        .expect("discovery");

    // Step 5 — one review, and a result.
    let review = onboarding::first_review(
        &pool,
        &GlobalConfig::default(),
        &dir.path().join("data"),
        "acme",
        at(3),
    )
    .await
    .expect("the first review runs");

    assert_eq!(review.repo, "acme");
    assert_eq!(review.engine, "mock");
    assert!(review.run_id > 0);
    // §18: a rehearsal that reads like a real review is the worst possible first
    // impression — everything after it is judged against invented findings.
    assert!(
        review
            .caveat
            .as_deref()
            .unwrap_or_default()
            .contains("mock"),
        "the mock must announce itself: {:?}",
        review.caveat
    );
}

#[tokio::test]
async fn onboarding_with_nothing_to_review_says_so_rather_than_failing_blankly() {
    // A repository with no discovered change is the common case for somebody who
    // just added one and has not run `watch`. §18: say what to do next.
    let (dir, pool, checkout) = fresh().await.expect("fixture");

    let draft = Draft {
        path: checkout.display().to_string(),
        name: "acme".to_owned(),
        ..Draft::default()
    };
    onboarding::add_repo(&pool, &draft, at(1))
        .await
        .expect("added");

    let error = onboarding::first_review(
        &pool,
        &GlobalConfig::default(),
        &dir.path().join("data"),
        "acme",
        at(2),
    )
    .await
    .expect_err("nothing has been discovered");

    let text = error.to_string();
    assert!(text.contains("watch") || text.contains("commit"), "{text}");
}

/// Discover this repository's commits, the way `revlocal watch` does.
///
/// Inline rather than shelling out to the CLI: this test is about onboarding, and
/// a subprocess would make a failure here look like a failure there.
///
/// Returns `Result` (ADR 0003): only `#[test]` functions panic, so a broken
/// fixture fails the test that used it rather than aborting a helper mid-way.
async fn revlocal_cli_discover(pool: &Pool, at: Timestamp) -> Result<(), String> {
    use revlocal_vcs::VcsAdapter as _;

    let repos = revlocal_store::RepoStore::new(pool)
        .list()
        .await
        .map_err(|e| e.to_string())?;
    let repo = repos.first().ok_or("no repository was added")?;

    let changes = revlocal_vcs::GitAdapter::new()
        .discover(repo, None, 50)
        .await
        .map_err(|e| e.to_string())?;

    for change in &changes {
        revlocal_store::ChangeStore::new(pool)
            .upsert(&revlocal_core::Change {
                id: revlocal_core::ChangeId::new(0),
                repo_id: repo.id,
                kind: change.kind,
                external_id: change.external_id.clone(),
                title: change.title.clone(),
                author_name: change.author_name.clone(),
                author_email: change.author_email.clone(),
                authored_at: change.authored_at,
                branch: change.branch.clone(),
                base_ref: change.base_ref.clone(),
                head_ref: change.head_ref.clone(),
                url: change.url.clone(),
                diff_stat: change.diff_stat,
                detected_at: at,
            })
            .await
            .map_err(|e| e.to_string())?;
    }

    Ok(())
}
