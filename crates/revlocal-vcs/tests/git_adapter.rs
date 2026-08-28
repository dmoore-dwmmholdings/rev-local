//! `GitAdapter`'s error classification (RL-506b).
//!
//! Assembly is covered by the CLI's e2e suite; what needs a test here is the part
//! that is a *decision* rather than a call: which `VcsError` a failure becomes. A
//! caller branches on the variant, so mapping a missing rev onto a generic command
//! failure would make "you asked for a commit that does not exist" indistinguishable
//! from "your repository is broken".

use std::path::{Path, PathBuf};
use std::process::Command;

use revlocal_core::{
    AutonomyMode, Change, ChangeId, ChangeKind, DiffStat, EngineKind, Repo, RepoId, RepoKind,
    Timestamp,
};
use revlocal_vcs::{GitAdapter, VcsAdapter, VcsError};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// Build the git fixture. Helpers return `Result` (ADR 0003).
fn build_fixture(out: &Path) -> Result<(), String> {
    let root = workspace_root();
    let output = Command::new("bash")
        .arg(root.join("fixtures/build.sh"))
        .arg("--out")
        .arg(out)
        .current_dir(&root)
        .output()
        .map_err(|e| format!("running build.sh: {e}"))?;

    output.status.success().then_some(()).ok_or_else(|| {
        format!(
            "build.sh failed (exit {}):\nstdout: {}\nstderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn repo_at(path: &Path) -> Repo {
    Repo {
        id: RepoId::new(1),
        name: "git-basic".to_owned(),
        kind: RepoKind::Git,
        local_path: Some(path.display().to_string()),
        remote_url: None,
        default_branch: None,
        engine: EngineKind::Claude,
        autonomy: AutonomyMode::Off,
        enabled: true,
        config_json: "{}".to_owned(),
        created_at: Timestamp::default(),
        updated_at: Timestamp::default(),
    }
}

fn change_at(rev: &str) -> Change {
    Change {
        id: ChangeId::new(1),
        repo_id: RepoId::new(1),
        kind: ChangeKind::Commit,
        external_id: rev.to_owned(),
        title: None,
        author_name: None,
        author_email: None,
        authored_at: None,
        branch: None,
        base_ref: None,
        head_ref: Some(rev.to_owned()),
        url: None,
        diff_stat: DiffStat::default(),
        detected_at: Timestamp::default(),
    }
}

/// A rev that does not resolve is the caller naming something that is not there, not
/// a broken repository. The distinction is what lets a UI say "no such commit"
/// instead of "git failed".
///
/// This exists because a negative probe on the CLI did **not** bite: the CLI test
/// passed on git's raw stderr, which happens to contain the sha, so the mapping could
/// have been absent entirely and nothing would have noticed. It was in fact broken —
/// the patterns were guessed, and git actually says "invalid reference".
#[test]
fn git_adapter_a_missing_rev_is_no_such_change_not_a_command_failure() {
    let temp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let out = temp.path().join("fixtures");
    build_fixture(&out).unwrap_or_else(|e| panic!("{e}"));

    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| panic!("{e}"));
    let error = runtime.block_on(async {
        GitAdapter::new()
            .materialize(
                &repo_at(&out.join("git-basic")),
                &change_at("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"),
                &temp.path().join("scratch"),
            )
            .await
            .expect_err("a missing rev must fail")
    });

    assert!(
        matches!(error, VcsError::NoSuchChange { .. }),
        "expected NoSuchChange, got: {error:?}"
    );
    assert!(error.to_string().contains("deadbeef"));
}

/// A repo row with no `local_path` is a configuration error, and saying so beats a
/// `NotARepository` about an empty path.
#[test]
fn git_adapter_a_repo_with_no_local_path_says_what_to_fix() {
    let mut repo = repo_at(Path::new("/unused"));
    repo.local_path = None;

    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| panic!("{e}"));
    let error = runtime.block_on(async {
        GitAdapter::new()
            .probe(&repo)
            .await
            .expect_err("no local_path must fail")
    });

    let message = error.to_string();
    assert!(message.contains("local"), "{message}");
    // §18: every user-visible error says what to do about it.
    assert!(message.contains("try:"), "{message}");
}

#[test]
fn git_adapter_probes_a_real_repository_as_usable() {
    let temp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let out = temp.path().join("fixtures");
    build_fixture(&out).unwrap_or_else(|e| panic!("{e}"));

    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| panic!("{e}"));
    let report = runtime.block_on(async {
        GitAdapter::new()
            .probe(&repo_at(&out.join("git-basic")))
            .await
            .unwrap_or_else(|e| panic!("{e}"))
    });

    assert!(report.usable, "problems: {:?}", report.problems);
    assert!(report.problems.is_empty());
    assert!(report
        .tool_version
        .as_deref()
        .is_some_and(|v| v.starts_with("git version")));
    assert_eq!(report.default_branch.as_deref(), Some("main"));
}

/// A repository where no branch matches `branches` would silently never be reviewed.
/// That is a configuration mistake the probe exists to catch, not a state to sit in.
#[test]
fn git_adapter_reports_a_repo_whose_branches_match_nothing() {
    let temp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let out = temp.path().join("fixtures");
    build_fixture(&out).unwrap_or_else(|e| panic!("{e}"));

    let mut repo = repo_at(&out.join("git-basic"));
    repo.config_json = r#"{"branches": ["no-such-branch-*"]}"#.to_owned();

    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| panic!("{e}"));
    let report = runtime.block_on(async {
        GitAdapter::new()
            .probe(&repo)
            .await
            .unwrap_or_else(|e| panic!("{e}"))
    });

    assert!(!report.usable);
    assert!(report
        .problems
        .iter()
        .any(|p| p.problem.contains("no branch")));
    assert!(report.problems.iter().all(|p| !p.remediation.is_empty()));
}

#[test]
fn git_adapter_discovers_commits_on_the_watched_branches() {
    let temp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let out = temp.path().join("fixtures");
    build_fixture(&out).unwrap_or_else(|e| panic!("{e}"));

    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| panic!("{e}"));
    let found = runtime.block_on(async {
        GitAdapter::new()
            .discover(&repo_at(&out.join("git-basic")), None, 5)
            .await
            .unwrap_or_else(|e| panic!("{e}"))
    });

    assert!(!found.is_empty());
    assert!(found.len() <= 5, "the limit was not honoured");
}

/// `install` and `uninstall` refuse rather than silently doing nothing. A user who
/// runs `install`, sees no error, and is not protected is worse off than one who is
/// told it is not built yet.
#[test]
fn git_adapter_unimplemented_hook_modes_refuse_rather_than_no_op() {
    use revlocal_vcs::HookMode;

    let repo = repo_at(Path::new("/unused"));
    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| panic!("{e}"));

    runtime.block_on(async {
        let adapter = GitAdapter::new();

        assert!(adapter
            .install_hooks(&repo, HookMode::Install)
            .await
            .is_err());
        assert!(adapter
            .install_hooks(&repo, HookMode::Uninstall)
            .await
            .is_err());

        let verified = adapter
            .install_hooks(&repo, HookMode::Verify)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(!verified.installed);
    });
}
