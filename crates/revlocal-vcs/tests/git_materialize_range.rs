//! Materializing a *range* rather than a commit (SPEC §6.1).
//!
//! A change that names a base is a range: a branch against what it forked from, or
//! a whole repository against the empty tree. What needs testing here is not that
//! git can diff — it is that the *scope* is right, because every way of getting it
//! wrong produces a review that looks complete and is not:
//!
//! - Reviewing only the tip commit of a branch reports on one commit and reads as
//!   a review of the branch.
//! - Using a two-dot `base..head` diff attributes everything that landed on the
//!   base since the fork to a branch that never touched it.
//!
//! Both were live possibilities, so both have a test.

use std::path::Path;
use std::process::Command;

use revlocal_core::{
    AutonomyMode, Change, ChangeId, ChangeKind, DiffStat, EngineKind, Repo, RepoId, RepoKind,
    Timestamp,
};
use revlocal_vcs::{VcsAdapter, EMPTY_TREE};

/// Run one git command in `dir`. Helpers return `Result` (ADR 0003).
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("git {args:?}: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| e.to_string())
}

fn write(dir: &Path, name: &str, body: &str) -> Result<(), String> {
    std::fs::write(dir.join(name), body).map_err(|e| format!("writing {name}: {e}"))
}

fn commit(dir: &Path, name: &str, body: &str) -> Result<String, String> {
    write(dir, name, body)?;
    git(dir, &["add", "."])?;
    git(dir, &["commit", "--quiet", "-m", &format!("add {name}")])?;
    Ok(git(dir, &["rev-parse", "HEAD"])?.trim().to_owned())
}

/// A repository whose `feature` branch has two commits, and whose `main` has moved
/// on since the branch was cut.
///
/// The repository is a *subdirectory* of the temp dir, and scratch is a sibling of
/// it. Materializing into a path inside the repository under review would leave an
/// untracked directory there, and the read-only assertion below would be reading
/// this test's own artifact rather than anything materialization did.
struct Fixture {
    dir: tempfile::TempDir,
    feature_head: String,
}

impl Fixture {
    fn repo(&self) -> std::path::PathBuf {
        self.dir.path().join("repo")
    }
}

fn build() -> Result<Fixture, String> {
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let path = &dir.path().join("repo");
    std::fs::create_dir_all(path).map_err(|e| format!("creating the repository: {e}"))?;

    git(path, &["init", "--quiet", "--initial-branch=main", "."])?;
    git(path, &["config", "user.email", "test@example.invalid"])?;
    git(path, &["config", "user.name", "Test"])?;
    git(path, &["config", "commit.gpgsign", "false"])?;

    commit(path, "base.txt", "before the branch\n")?;

    git(path, &["checkout", "--quiet", "-b", "feature"])?;
    commit(path, "first.txt", "one\n")?;
    let feature_head = commit(path, "second.txt", "two\n")?;

    // Main moves on after the fork. This is the file a two-dot diff would wrongly
    // attribute to the branch.
    git(path, &["checkout", "--quiet", "main"])?;
    commit(path, "landed-on-main.txt", "not the branch's work\n")?;
    git(path, &["checkout", "--quiet", "feature"])?;

    Ok(Fixture { dir, feature_head })
}

fn repo_at(path: &Path) -> Repo {
    Repo {
        id: RepoId::new(1),
        name: "range".to_owned(),
        kind: RepoKind::Git,
        local_path: Some(path.display().to_string()),
        remote_url: None,
        default_branch: Some("main".to_owned()),
        engine: EngineKind::Claude,
        autonomy: AutonomyMode::Off,
        enabled: true,
        config_json: "{}".to_owned(),
        created_at: Timestamp::default(),
        updated_at: Timestamp::default(),
    }
}

fn change(head: &str, base: Option<&str>) -> Change {
    Change {
        id: ChangeId::new(1),
        repo_id: RepoId::new(1),
        kind: ChangeKind::Commit,
        external_id: head.to_owned(),
        title: None,
        author_name: None,
        author_email: None,
        authored_at: None,
        branch: Some("feature".to_owned()),
        base_ref: base.map(str::to_owned),
        head_ref: Some(head.to_owned()),
        url: None,
        diff_stat: DiffStat::default(),
        detected_at: Timestamp::default(),
    }
}

/// Materialize `change` from the fixture, returning the context.
fn materialize(
    fixture: &Fixture,
    base: Option<&str>,
) -> Result<revlocal_vcs::ChangeContext, String> {
    // A sibling of the repository, never inside it (§6.1).
    let scratch = fixture.dir.path().join("scratch").join(match base {
        Some(base) => base.to_owned(),
        None => "none".to_owned(),
    });
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    runtime.block_on(async {
        revlocal_vcs::GitAdapter::new()
            .materialize(
                &repo_at(&fixture.repo()),
                &change(&fixture.feature_head, base),
                &scratch,
            )
            .await
            .map_err(|e| e.to_string())
    })
}

fn touched(context: &revlocal_vcs::ChangeContext) -> Vec<String> {
    let mut files: Vec<String> = context.diff_files.iter().map(|f| f.path.clone()).collect();
    files.sort();
    files
}

/// The default is unchanged: no base means the change is one commit.
#[test]
fn git_materialize_range_without_a_base_reviews_only_the_named_commit() {
    let fixture = build().unwrap_or_else(|e| panic!("{e}"));
    let context = materialize(&fixture, None).unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(touched(&context), vec!["second.txt".to_owned()]);
    assert_eq!(context.stat.files, 1);
}

/// The point of a branch review: every commit on the branch, in one diff.
#[test]
fn git_materialize_range_a_branch_review_covers_the_whole_branch() {
    let fixture = build().unwrap_or_else(|e| panic!("{e}"));
    let context = materialize(&fixture, Some("main")).unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(
        touched(&context),
        vec!["first.txt".to_owned(), "second.txt".to_owned()],
        "a branch review that saw only the tip commit reads as a review of the branch"
    );
    assert!(context.diff_unified.contains("first.txt"));
    assert!(context.diff_unified.contains("second.txt"));
}

/// The half a two-dot diff gets wrong.
#[test]
fn git_materialize_range_a_branch_review_excludes_what_landed_on_the_base() {
    let fixture = build().unwrap_or_else(|e| panic!("{e}"));
    let context = materialize(&fixture, Some("main")).unwrap_or_else(|e| panic!("{e}"));

    assert!(
        !touched(&context).contains(&"landed-on-main.txt".to_owned()),
        "work that landed on the base after the fork is not the branch's work: {:?}",
        touched(&context)
    );
    // And the base's own pre-fork content is not a change either.
    assert!(!touched(&context).contains(&"base.txt".to_owned()));
}

/// A whole-repository review is a diff against the empty tree — every tracked file,
/// including the ones the branch never touched.
#[test]
fn git_materialize_range_the_empty_tree_base_reviews_every_tracked_file() {
    let fixture = build().unwrap_or_else(|e| panic!("{e}"));
    let context = materialize(&fixture, Some(EMPTY_TREE)).unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(
        touched(&context),
        vec![
            "base.txt".to_owned(),
            "first.txt".to_owned(),
            "second.txt".to_owned()
        ]
    );
    assert_eq!(context.stat.files, 3);
}

/// A branch's message is its commits' subjects, not its tip's alone — using the tip
/// would describe one commit as if it were the branch.
#[test]
fn git_materialize_range_a_branch_message_covers_the_range() {
    let fixture = build().unwrap_or_else(|e| panic!("{e}"));
    let context = materialize(&fixture, Some("main")).unwrap_or_else(|e| panic!("{e}"));

    assert!(
        context.message.contains("add first.txt"),
        "{}",
        context.message
    );
    assert!(
        context.message.contains("add second.txt"),
        "{}",
        context.message
    );
}

/// A whole-repository review is not described by the entire history.
///
/// `EMPTY_TREE..sha` is accepted by git and lists every commit ever made, which
/// would arrive at the engine as if it were this change's commit message.
#[test]
fn git_materialize_range_the_empty_tree_base_does_not_use_the_whole_log_as_a_message() {
    let fixture = build().unwrap_or_else(|e| panic!("{e}"));
    let context = materialize(&fixture, Some(EMPTY_TREE)).unwrap_or_else(|e| panic!("{e}"));

    assert!(
        context.message.contains("add second.txt"),
        "{}",
        context.message
    );
    assert!(
        !context.message.contains("add base.txt"),
        "the message is the tip commit's, not the entire history: {}",
        context.message
    );
}

/// Materializing still must not touch the repository under review (§6.1).
#[test]
fn git_materialize_range_leaves_the_source_repository_on_its_own_branch() {
    let fixture = build().unwrap_or_else(|e| panic!("{e}"));
    // Branch, index and untracked files. Not `worktree list`: `materialize` adds a
    // worktree and leaves releasing it to the caller by design (`release_worktree`
    // is an async git call, and `Drop` cannot make one), so an entry here after
    // materializing is the contract rather than a leak.
    let state = || git(&fixture.repo(), &["status", "--porcelain", "--branch"]);
    let before = state();

    materialize(&fixture, Some("main")).unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(before, state());
}
