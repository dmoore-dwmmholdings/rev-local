//! `revlocal review` (RL-506b).
//!
//! Runs the real binary against the real git fixture, because the criteria are about
//! what reaches a *pipe*: which stream each byte lands on, and whether two invocations
//! produce the same bytes. Calling the library function would test neither.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// One commit in `.manifest.json`.
#[derive(Debug, serde::Deserialize)]
struct ManifestCommit {
    role: String,
    sha: String,
}

#[derive(Debug, serde::Deserialize)]
struct Manifest {
    commits: Vec<ManifestCommit>,
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// Build the git fixture. Helpers return `Result` (ADR 0003).
fn build_fixture(out: &Path) -> Result<Manifest, String> {
    let root = workspace_root();
    let output = Command::new(revlocal_vcs::bash_program())
        .arg(root.join("fixtures/build.sh"))
        .arg("--out")
        .arg(out)
        .current_dir(&root)
        .output()
        .map_err(|e| format!("running build.sh: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "build.sh failed (exit {}):\nstdout: {}\nstderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let text = std::fs::read_to_string(out.join("git-basic/.manifest.json"))
        .map_err(|e| format!("reading the manifest: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("parsing the manifest: {e}"))
}

fn sha_for(manifest: &Manifest, role: &str) -> Result<String, String> {
    manifest
        .commits
        .iter()
        .find(|c| c.role == role)
        .map(|c| c.sha.clone())
        .ok_or_else(|| format!("no fixture commit with role {role:?}"))
}

/// Run `revlocal review` against a fixture commit.
fn review(role: &str, extra: &[&str]) -> Result<(Output, tempfile::TempDir), String> {
    let temp = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let out = temp.path().join("fixtures");
    let manifest = build_fixture(&out)?;
    let sha = sha_for(&manifest, role)?;

    let mut command = Command::new(env!("CARGO_BIN_EXE_revlocal"));
    command
        .arg("review")
        .arg("--repo")
        .arg(out.join("git-basic"))
        .arg("--rev")
        .arg(&sha)
        .args(extra);

    let output = command
        .output()
        .map_err(|e| format!("running revlocal review: {e}"))?;

    Ok((output, temp))
}

/// The load-bearing criterion. A `--json` flag whose stdout carries a progress line
/// is not machine-readable: a caller piping to `jq` gets a parse error and reasonably
/// blames their own pipeline.
#[test]
fn review_json_prints_exactly_one_document_and_nothing_else() {
    let (output, _temp) =
        review("planted_bug_off_by_one", &["--json"]).unwrap_or_else(|e| panic!("{e}"));

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Parses whole, in one go: anything else on the stream makes this fail.
    let parsed: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}):\n{stdout}"));

    assert_eq!(parsed["schema_version"], 1);
    assert_eq!(parsed["status"], "done");
    assert!(parsed["depth"].is_string());
}

/// The informational line the human path prints must go to stderr, or the test above
/// would only pass by accident of nothing currently being printed.
#[test]
fn review_json_sends_informational_output_to_stderr_only() {
    let (json, _a) = review("clean", &["--json"]).unwrap_or_else(|e| panic!("{e}"));
    let (human, _b) = review("clean", &[]).unwrap_or_else(|e| panic!("{e}"));

    let json_stdout = String::from_utf8_lossy(&json.stdout);
    assert!(
        !json_stdout.contains("mock engine"),
        "the notice leaked into --json stdout:\n{json_stdout}"
    );

    // And the human path does say it, so the check above is about routing rather
    // than about the message having been deleted.
    let human_stderr = String::from_utf8_lossy(&human.stderr);
    assert!(
        human_stderr.contains("mock engine"),
        "the notice vanished entirely; the stdout assertion would then prove nothing"
    );
}

#[test]
fn review_output_is_byte_stable_across_invocations() {
    let (first, _a) =
        review("planted_bug_off_by_one", &["--json"]).unwrap_or_else(|e| panic!("{e}"));
    let (second, _b) =
        review("planted_bug_off_by_one", &["--json"]).unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&second.stdout),
        "the CLI's report is not reproducible"
    );
}

/// The specific way stability breaks: the scratch directory is a temp path that
/// differs every run. One leaked absolute path and the guarantee is gone while every
/// other assertion still passes.
#[test]
fn review_json_carries_no_absolute_paths() {
    let (output, _temp) =
        review("planted_bug_off_by_one", &["--json"]).unwrap_or_else(|e| panic!("{e}"));
    let stdout = String::from_utf8_lossy(&output.stdout);

    for marker in ["/tmp/", "/var/folders/", ".tmp", "C:\\"] {
        assert!(
            !stdout.contains(marker),
            "the report leaks {marker:?}:\n{stdout}"
        );
    }
}

#[test]
fn review_human_output_is_a_separate_renderer() {
    let (output, _temp) = review("planted_bug_off_by_one", &[]).unwrap_or_else(|e| panic!("{e}"));
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(
        serde_json::from_str::<serde_json::Value>(stdout.trim()).is_err(),
        "the human path printed JSON, so there is only one renderer:\n{stdout}"
    );
    assert!(stdout.contains("git-basic"));
    assert!(stdout.contains("depth:"));
}

#[test]
fn review_a_nonexistent_rev_exits_non_zero_and_names_the_rev() {
    let temp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let out = temp.path().join("fixtures");
    build_fixture(&out).unwrap_or_else(|e| panic!("{e}"));

    let output = Command::new(env!("CARGO_BIN_EXE_revlocal"))
        .arg("review")
        .arg("--repo")
        .arg(out.join("git-basic"))
        .arg("--rev")
        .arg("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef")
        .output()
        .unwrap_or_else(|e| panic!("{e}"));

    assert!(!output.status.success(), "a missing rev must not exit 0");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"),
        "the error must name the rev the user asked for:\n{stderr}"
    );
    // A panic message would also be non-zero and would also mention the rev if the
    // rev happened to be in it, so the absence of a panic is asserted separately.
    assert!(
        !stderr.contains("panicked at"),
        "a missing rev panicked instead of erroring:\n{stderr}"
    );

    // Discriminating: git's own stderr contains the sha, so "names the rev" passes
    // whether or not the adapter classifies this as `NoSuchChange`. A negative probe
    // proved that — dropping the classification changed no test, while the mapping
    // was in fact broken. This requires the classified message.
    assert!(
        stderr.contains("has no commit"),
        "a missing rev must read as `no such commit`, not as a git command failure — \
         the two mean different things to a caller:\n{stderr}"
    );
    assert!(
        !stderr.contains("worktree add"),
        "the raw git command leaked into a user-facing error:\n{stderr}"
    );
}

#[test]
fn review_a_nonexistent_repository_exits_non_zero_without_panicking() {
    let output = Command::new(env!("CARGO_BIN_EXE_revlocal"))
        .arg("review")
        .arg("--repo")
        .arg("/nonexistent/revlocal-not-a-repo")
        .arg("--rev")
        .arg("HEAD")
        .output()
        .unwrap_or_else(|e| panic!("{e}"));

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("panicked at"), "{stderr}");
    assert!(stderr.contains("revlocal:"), "{stderr}");
}

/// The fixture is a repository under review, and reviewing must never change one.
#[test]
fn review_does_not_mutate_the_repository() {
    let temp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let out = temp.path().join("fixtures");
    let manifest = build_fixture(&out).unwrap_or_else(|e| panic!("{e}"));
    let sha = sha_for(&manifest, "planted_bug_off_by_one").unwrap_or_else(|e| panic!("{e}"));
    let repo = out.join("git-basic");

    let before = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&repo)
        .output()
        .unwrap_or_else(|e| panic!("{e}"));

    let review = Command::new(env!("CARGO_BIN_EXE_revlocal"))
        .args(["review", "--json", "--repo"])
        .arg(&repo)
        .arg("--rev")
        .arg(&sha)
        .output()
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(review.status.success());

    let after = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&repo)
        .output()
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(
        String::from_utf8_lossy(&before.stdout),
        String::from_utf8_lossy(&after.stdout)
    );
    assert!(String::from_utf8_lossy(&after.stdout).trim().is_empty());
}
