//! The §9 pipeline end to end, over the real git fixture (RL-506).
//!
//! Every stage runs: ignore filtering, truncation, depth selection, prompt assembly,
//! the engine, normalization, and §9.3's escalated re-run. The engine is
//! `MockEngine`, in process, so the suite spends no tokens.
//!
//! # The no-network wrapper is not decorative
//!
//! Criterion 4 asks for "zero network access, asserted by a no-network test wrapper".
//! A wrapper that merely *claims* to block the network is worse than none: it makes
//! the assertion look done. So [`no_network`] restricts git to the `file` protocol
//! and points every proxy variable at an unroutable address, and
//! `the_no_network_wrapper_actually_blocks_the_network` proves it bites by attempting
//! an outbound git operation inside it and requiring a **protocol-blocked** error
//! specifically — not merely "it failed", which a machine with no network gives you
//! for free and which would pass whether the wrapper worked or not.

use std::path::{Path, PathBuf};
use std::process::Command;

use revlocal_core::{Change, ChangeId, ChangeKind, DiffStat, RepoConfig, RepoId, Timestamp};
use revlocal_daemon::pipeline::{self, ReviewInputs, ReviewStatus};
use revlocal_engine::{EngineOutcome, MockBehaviour, MockEngine, RawFinding};
use revlocal_vcs::GitRunner;
use tokio_util::sync::CancellationToken;

// --- fixtures -------------------------------------------------------------

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

impl Manifest {
    /// The sha for a role. Roles, never shas: a fixture rebuild changes every sha
    /// and would otherwise change every test.
    fn sha(&self, role: &str) -> Result<String, String> {
        self.commits
            .iter()
            .find(|c| c.role == role)
            .map(|c| c.sha.clone())
            .ok_or_else(|| format!("no fixture commit with role {role:?}"))
    }
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// Build the git fixture into `out`. Helpers return `Result` (ADR 0003).
fn build_fixture(out: &Path) -> Result<Manifest, String> {
    let root = workspace_root();
    let script = root.join("fixtures/build.sh");

    let output = Command::new("bash")
        .arg(&script)
        .arg("--out")
        .arg(out)
        .current_dir(&root)
        .output()
        .map_err(|e| format!("running {}: {e}", script.display()))?;

    if !output.status.success() {
        return Err(format!(
            "build.sh failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let text = std::fs::read_to_string(out.join("git-basic/.manifest.json"))
        .map_err(|e| format!("reading the manifest: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("parsing the manifest: {e}"))
}

// --- the no-network wrapper ----------------------------------------------

/// Environment that makes any outbound access fail, deterministically.
///
/// `GIT_ALLOW_PROTOCOL=file` is the load-bearing one: git refuses `https://` with a
/// distinctive "protocol ... is not supported" error **without touching the
/// network**, so the assertion is about the wrapper rather than about whether this
/// machine happens to be offline. The proxy variables point at a closed port so that
/// anything speaking HTTP directly fails immediately instead of hanging.
fn no_network() -> Vec<(&'static str, &'static str)> {
    vec![
        ("GIT_ALLOW_PROTOCOL", "file"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("http_proxy", "http://127.0.0.1:1"),
        ("https_proxy", "http://127.0.0.1:1"),
        ("HTTP_PROXY", "http://127.0.0.1:1"),
        ("HTTPS_PROXY", "http://127.0.0.1:1"),
        ("ALL_PROXY", "http://127.0.0.1:1"),
        ("no_proxy", ""),
    ]
}

/// Apply the wrapper to this process for the duration of a closure.
///
/// Restores what was there before, so one test cannot leak its environment into
/// another running in the same process.
fn with_no_network<T>(body: impl FnOnce() -> T) -> T {
    let saved: Vec<(&str, Option<String>)> = no_network()
        .iter()
        .map(|(key, _)| (*key, std::env::var(key).ok()))
        .collect();

    for (key, value) in no_network() {
        // Safety of the *test* kind: these are process-wide, and cargo runs tests in
        // threads. Every test that cares runs inside this wrapper, and the values
        // are identical, so a concurrent overlap sets the same bytes.
        std::env::set_var(key, value);
    }

    let result = body();

    for (key, value) in saved {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }

    result
}

// --- pipeline plumbing ----------------------------------------------------

fn change_for(sha: &str, stat: DiffStat) -> Change {
    Change {
        id: ChangeId::new(1),
        repo_id: RepoId::new(1),
        kind: ChangeKind::Commit,
        external_id: sha.to_owned(),
        title: Some("fixture commit".to_owned()),
        author_name: Some("Fixture Author".to_owned()),
        author_email: None,
        authored_at: None,
        branch: Some("main".to_owned()),
        base_ref: None,
        head_ref: Some(sha.to_owned()),
        url: None,
        diff_stat: stat,
        detected_at: Timestamp::default(),
    }
}

fn finding(severity: &str, file: &str, title: &str) -> RawFinding {
    RawFinding {
        severity: severity.parse().unwrap_or(revlocal_core::Severity::Medium),
        category: revlocal_core::Category::Correctness,
        confidence: Some(0.9),
        file: Some(file.to_owned()),
        line_start: Some(1),
        line_end: Some(2),
        title: title.to_owned(),
        body: "why".to_owned(),
        failure_scenario: Some("inputs".to_owned()),
        suggested_fix: None,
    }
}

fn outcome_with(findings: Vec<RawFinding>) -> EngineOutcome {
    EngineOutcome {
        findings,
        summary: "a fixture review".to_owned(),
        verdict: revlocal_core::Verdict::Comment,
        usage: revlocal_core::Usage::default(),
        transcript: String::new(),
        degraded: None,
        coverage_notes: None,
    }
}

/// Materialize one fixture commit and run the pipeline over it.
fn review_role(
    role: &str,
    behaviour: MockBehaviour,
    config: RepoConfig,
) -> Result<(pipeline::ReviewReport, MockEngine), String> {
    let temp = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let out = temp.path().join("fixtures");
    let manifest = build_fixture(&out)?;
    let sha = manifest.sha(role)?;
    let repo_dir = out.join("git-basic");

    let runtime = tokio::runtime::Runtime::new().map_err(|e| format!("runtime: {e}"))?;

    runtime.block_on(async {
        let runner = GitRunner::new();
        let scratch = temp.path().join("scratch");
        let change = change_for(&sha, DiffStat::default());

        let context = revlocal_vcs::git::materialize(&runner, &repo_dir, &change, &scratch)
            .await
            .map_err(|e| format!("materialize: {e}"))?;

        let change = change_for(&sha, context.stat);
        let engine = MockEngine::with_behaviour(behaviour);

        let report = pipeline::review(
            &ReviewInputs {
                repo_name: "git-basic",
                repo_kind: "git",
                change: &change,
                config: &config,
                worktree: &context.worktree,
                diff_unified: &context.diff_unified,
                diff_files: &context.diff_files,
                labels: &[],
                suppressions: &[],
                published_fingerprints: &[],
                prior_findings: &[],
                skip: None,
                now: Timestamp::default(),
            },
            &engine,
            &scratch,
            &CancellationToken::new(),
        )
        .await
        .map_err(|e| format!("pipeline: {e}"))?;

        Ok((report, engine))
    })
}

// --- criterion 4: the wrapper, first, because the rest relies on it -------

/// Proves the wrapper bites. A machine with no network makes *any* outbound command
/// fail, so "it failed" would pass whether the wrapper worked or not; this requires
/// the specific refusal `GIT_ALLOW_PROTOCOL` produces, which git emits before it
/// opens a socket.
#[test]
fn the_no_network_wrapper_actually_blocks_the_network() {
    let stderr = with_no_network(|| {
        let output = Command::new("git")
            .args(["ls-remote", "https://github.com/anthropics/does-not-matter"])
            .envs(no_network())
            .output()
            .expect("git is on PATH");
        assert!(
            !output.status.success(),
            "the wrapper let an outbound git call succeed"
        );
        String::from_utf8_lossy(&output.stderr).to_lowercase()
    });

    assert!(
        stderr.contains("transport 'https' not allowed")
            || (stderr.contains("protocol") && stderr.contains("not allowed")),
        "git failed, but not because the wrapper blocked it — the assertion would \
         pass on any offline machine and prove nothing. stderr: {stderr}"
    );
}

#[test]
fn the_whole_flow_runs_with_no_network_access() {
    let (report, _) = with_no_network(|| {
        review_role(
            "planted_bug_off_by_one",
            MockBehaviour::Succeed(Box::new(outcome_with(vec![finding(
                "high",
                "src/pager.rs",
                "off-by-one in page bounds",
            )]))),
            RepoConfig::default(),
        )
    })
    .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(report.status, ReviewStatus::Done);
}

// --- criterion 1: the planted bug ----------------------------------------

#[test]
fn reviewing_the_planted_bug_commit_yields_a_done_run_with_a_finding() {
    let (report, engine) = with_no_network(|| {
        review_role(
            "planted_bug_off_by_one",
            MockBehaviour::Succeed(Box::new(outcome_with(vec![finding(
                "medium",
                "src/pager.rs",
                "off-by-one in page bounds",
            )]))),
            RepoConfig::default(),
        )
    })
    .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(report.status, ReviewStatus::Done);
    assert!(!report.findings.is_empty(), "expected at least one finding");
    assert!(report.is_consistent());
    assert_eq!(report.attempts, 1);
    assert_eq!(engine.run_count(), 1);

    // The engine was actually given the change: the pipeline is not reporting a
    // review of nothing.
    let tasks = engine.seen_tasks.lock().unwrap_or_else(|e| panic!("{e}"));
    let task = tasks.first().expect("the engine was invoked");
    assert!(task.prompt.contains("## 3. The diff"));
    assert!(task.prompt.contains("pager"), "the diff reached the prompt");
    assert_ne!(
        task.cwd, task.out_dir,
        "the engine could edit its own subject"
    );
}

/// §9.3's escalation, end to end: a standard run that reports a high-severity finding
/// is re-run once at `deep`, and only once.
#[test]
fn a_high_severity_finding_escalates_the_run_to_deep_exactly_once() {
    let (report, engine) = with_no_network(|| {
        review_role(
            "planted_bug_sql_injection",
            MockBehaviour::Succeed(Box::new(outcome_with(vec![finding(
                "critical",
                // The file this commit actually touches. Naming any other file makes
                // the finding out-of-diff, which §9.5 caps at medium — and a capped
                // finding correctly does not escalate. See the test below.
                "src/db.rs",
                "sql injection in lookup",
            )]))),
            RepoConfig::default(),
        )
    })
    .unwrap_or_else(|e| panic!("{e}"));

    assert!(report.escalated);
    assert_eq!(report.depth, "deep");
    assert_eq!(report.attempts, 2, "escalated exactly once");
    assert_eq!(engine.run_count(), 2);

    let tasks = engine.seen_tasks.lock().unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(tasks[0].depth, revlocal_core::Depth::Standard);
    assert_eq!(tasks[1].depth, revlocal_core::Depth::Deep);
    // §8.5/§9.3: the budget follows the depth.
    assert!(tasks[1].timeout > tasks[0].timeout);
}

/// The other half of "escalation asks the *normalized* findings". A `critical` the
/// user has suppressed must not buy them a 25-minute deep re-run they explicitly
/// asked not to have.
///
/// Written after a negative probe showed the design claim was unpinned: swapping
/// `publishable()` for every finding changed no test, because the only capped-finding
/// case was still `Open`.
#[test]
fn a_suppressed_critical_does_not_escalate() {
    let temp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let out = temp.path().join("fixtures");
    let manifest = build_fixture(&out).unwrap_or_else(|e| panic!("{e}"));
    let sha = manifest
        .sha("planted_bug_sql_injection")
        .unwrap_or_else(|e| panic!("{e}"));
    let repo_dir = out.join("git-basic");

    let print = revlocal_core::fingerprint(
        "git-basic",
        Some("src/db.rs"),
        revlocal_core::Category::Correctness,
        "sql injection in lookup",
    );
    let suppressions = [revlocal_core::Suppression {
        id: revlocal_core::SuppressionId::new(1),
        repo_id: None,
        fingerprint: Some(print),
        glob: None,
        reason: Some("tracked separately".to_owned()),
        created_at: Timestamp::default(),
    }];

    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| panic!("{e}"));
    let (report, runs) = runtime.block_on(async {
        let runner = GitRunner::new();
        let scratch = temp.path().join("scratch");
        let change = change_for(&sha, DiffStat::default());
        let context = revlocal_vcs::git::materialize(&runner, &repo_dir, &change, &scratch)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let change = change_for(&sha, context.stat);
        let engine =
            MockEngine::with_behaviour(MockBehaviour::Succeed(Box::new(outcome_with(vec![
                finding("critical", "src/db.rs", "sql injection in lookup"),
            ]))));

        let report = pipeline::review(
            &ReviewInputs {
                repo_name: "git-basic",
                repo_kind: "git",
                change: &change,
                config: &RepoConfig::default(),
                worktree: &context.worktree,
                diff_unified: &context.diff_unified,
                diff_files: &context.diff_files,
                labels: &[],
                suppressions: &suppressions,
                published_fingerprints: &[],
                prior_findings: &[],
                skip: None,
                now: Timestamp::default(),
            },
            &engine,
            &scratch,
            &CancellationToken::new(),
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));

        (report, engine.run_count())
    });

    assert!(!report.escalated, "a suppressed finding must not escalate");
    assert_eq!(runs, 1);
    assert!(report.findings.is_empty(), "suppressed, so not published");
    assert_eq!(report.withheld.len(), 1, "recorded, not discarded (§9.5)");
    assert!(report.withheld[0].reason.contains("tracked separately"));
}

/// Two stages interacting, and the interaction is the point. A `critical` finding
/// naming a file the change never touched is capped at `medium` by §9.5 — so it must
/// **not** buy a 25-minute deep re-run. Getting this wrong would let any engine
/// hallucinating a file name spend the escalation budget at will.
#[test]
fn an_out_of_diff_critical_does_not_escalate_because_it_was_capped() {
    let (report, engine) = with_no_network(|| {
        review_role(
            "planted_bug_sql_injection",
            MockBehaviour::Succeed(Box::new(outcome_with(vec![finding(
                "critical",
                "src/somewhere_else.rs",
                "sql injection in lookup",
            )]))),
            RepoConfig::default(),
        )
    })
    .unwrap_or_else(|e| panic!("{e}"));

    assert!(!report.escalated, "a capped finding must not escalate");
    assert_eq!(report.attempts, 1);
    assert_eq!(engine.run_count(), 1);
    assert_eq!(report.findings.len(), 1, "retained, not dropped (§9.5)");
    assert_eq!(report.findings[0].severity, "medium", "capped");
    assert!(report.findings[0].out_of_diff);
}

#[test]
fn a_clean_review_does_not_escalate() {
    let (report, engine) = with_no_network(|| {
        review_role(
            "clean",
            MockBehaviour::Succeed(Box::new(outcome_with(Vec::new()))),
            RepoConfig::default(),
        )
    })
    .unwrap_or_else(|e| panic!("{e}"));

    assert!(!report.escalated);
    assert_eq!(report.attempts, 1);
    assert_eq!(engine.run_count(), 1);
    assert!(report.findings.is_empty());
}

// --- criterion 2: the 200-file commit ------------------------------------

#[test]
fn the_200_file_commit_is_summarised_and_truncated() {
    let config = RepoConfig {
        // A budget the 200-file commit blows, so truncation is exercised rather than
        // merely available. The default 512 KB is larger than the whole fixture.
        max_total_diff_bytes: 4_000,
        ..RepoConfig::default()
    };

    let (report, _) = with_no_network(|| {
        review_role(
            "large_200_files",
            MockBehaviour::Succeed(Box::new(outcome_with(Vec::new()))),
            config,
        )
    })
    .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(
        report.depth, "summary",
        "reasons: {:?}",
        report.depth_reasons
    );
    assert!(report.truncated);
    assert!(
        !report.omitted_files.is_empty(),
        "truncated with nothing named is the silent cap §18 forbids"
    );
    assert!(report.is_consistent());
    // §18: the reason the depth was chosen is on the record, not just the depth.
    assert!(report
        .depth_reasons
        .iter()
        .any(|r| r.contains("files changed")));
}

/// **SPEC §17's M6 exit gate, at DEFAULT settings.**
///
/// The gate reads: "the 200-file commit yields `depth=summary` and `truncated=true`
/// with the full omitted-file list present in the prompt". Every word of that is
/// asserted here, and the default config is the point — the test above lowers
/// `max_total_diff_bytes` to 4,000 to exercise the truncation *logic*, which is
/// sound and which is exactly why nobody noticed for seven stories that §9.4's
/// default path never ran at all (REVL-118).
///
/// The omitted list is checked **name by name**, not by count. A count would pass
/// against a list that named 58 wrong files.
#[test]
fn the_m6_exit_gate_runs_at_default_settings() {
    let temp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let out = temp.path().join("fixtures");
    let manifest = build_fixture(&out).unwrap_or_else(|e| panic!("{e}"));
    let sha = manifest
        .sha("large_200_files")
        .unwrap_or_else(|e| panic!("{e}"));
    let repo_dir = out.join("git-basic");

    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| panic!("{e}"));
    runtime.block_on(async {
        let runner = GitRunner::new();
        let scratch = temp.path().join("scratch");
        let change = change_for(&sha, DiffStat::default());
        let context = revlocal_vcs::git::materialize(&runner, &repo_dir, &change, &scratch)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let change = change_for(&sha, context.stat);
        let engine =
            MockEngine::with_behaviour(MockBehaviour::Succeed(Box::new(outcome_with(Vec::new()))));

        let report = pipeline::review(
            &ReviewInputs {
                repo_name: "git-basic",
                repo_kind: "git",
                change: &change,
                config: &RepoConfig::default(), // <- the whole point
                worktree: &context.worktree,
                diff_unified: &context.diff_unified,
                diff_files: &context.diff_files,
                labels: &[],
                suppressions: &[],
                published_fingerprints: &[],
                prior_findings: &[],
                skip: None,
                now: Timestamp::default(),
            },
            &engine,
            &scratch,
            &CancellationToken::new(),
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(
            report.depth, "summary",
            "reasons: {:?}",
            report.depth_reasons
        );
        assert!(report.truncated, "the default budget was not exceeded");
        assert!(!report.omitted_files.is_empty());
        assert!(report.is_consistent());

        // §9.4's absolute rule: "Truncation must never silently hide a file."
        let tasks = engine.seen_tasks.lock().unwrap_or_else(|e| panic!("{e}"));
        let prompt = &tasks.first().expect("the engine ran").prompt;

        for omitted in &report.omitted_files {
            assert!(
                prompt.contains(omitted.as_str()),
                "`{omitted}` was dropped from the diff and never named in the prompt"
            );
            assert!(
                !prompt.contains(&format!("+pub fn value_{}", &omitted[14..17])),
                "`{omitted}` was reported omitted but its hunks are still in the diff"
            );
        }
        assert!(prompt.contains("This diff has been truncated"));
    });
}

// --- criterion 3: stable JSON --------------------------------------------

#[test]
fn report_is_byte_stable_across_runs() {
    let review = || {
        review_role(
            "planted_bug_off_by_one",
            MockBehaviour::Succeed(Box::new(outcome_with(vec![finding(
                "medium",
                "src/pager.rs",
                "off-by-one in page bounds",
            )]))),
            RepoConfig::default(),
        )
    };

    let (first, _) = with_no_network(review).unwrap_or_else(|e| panic!("{e}"));
    let (second, _) = with_no_network(review).unwrap_or_else(|e| panic!("{e}"));

    let a = first.to_json().unwrap_or_else(|e| panic!("{e}"));
    let b = second.to_json().unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(a, b, "the report is not reproducible");
}

/// The specific way stability breaks: the worktree is a temp directory whose path is
/// different every run, so a single leaked absolute path would destroy the guarantee
/// while every other assertion still passed.
#[test]
fn report_carries_no_absolute_paths() {
    let (report, _) = with_no_network(|| {
        review_role(
            "planted_bug_off_by_one",
            MockBehaviour::Succeed(Box::new(outcome_with(vec![finding(
                "medium",
                "src/pager.rs",
                "off-by-one",
            )]))),
            RepoConfig::default(),
        )
    })
    .unwrap_or_else(|e| panic!("{e}"));

    let json = report.to_json().unwrap_or_else(|e| panic!("{e}"));
    for marker in ["/tmp/", "/var/folders/", "/scratch/", "C:\\"] {
        assert!(
            !json.contains(marker),
            "the report leaks {marker:?}, so it cannot be reproducible:\n{json}"
        );
    }
}

#[test]
fn report_round_trips_through_json() {
    let (report, _) = with_no_network(|| {
        review_role(
            "clean",
            MockBehaviour::Succeed(Box::new(outcome_with(Vec::new()))),
            RepoConfig::default(),
        )
    })
    .unwrap_or_else(|e| panic!("{e}"));

    let json = report.to_json().unwrap_or_else(|e| panic!("{e}"));
    let parsed: pipeline::ReviewReport =
        serde_json::from_str(&json).unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(parsed, report);
    assert_eq!(parsed.schema_version, pipeline::REPORT_SCHEMA_VERSION);
}

// --- the stages that must not spend an engine ----------------------------

#[test]
fn a_lockfile_only_commit_is_skipped_without_spending_an_engine() {
    let (report, engine) = with_no_network(|| {
        review_role(
            "lockfile_only",
            MockBehaviour::Succeed(Box::new(outcome_with(Vec::new()))),
            RepoConfig::default(),
        )
    })
    .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(report.status, ReviewStatus::Skipped);
    assert!(
        report.skip_reason.is_some(),
        "a skip must say which rule fired"
    );
    assert_eq!(engine.run_count(), 0, "a skipped change must cost nothing");
    assert!(report.is_consistent());
}

#[test]
fn an_engine_failure_is_reported_not_panicked() {
    let (report, _) = with_no_network(|| {
        review_role(
            "clean",
            MockBehaviour::Fail(revlocal_engine::EngineError::InvalidTask {
                detail: "deliberate".to_owned(),
            }),
            RepoConfig::default(),
        )
    })
    .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(report.status, ReviewStatus::Failed);
    assert!(report.failure.is_some(), "a failed review must say why");
    assert!(report.findings.is_empty());
    assert!(report.is_consistent());
}

/// The hard constraint the whole product rests on: reviewing a repository must not
/// change it. M4 asserts this for materialization; this asserts it for the pipeline,
/// which is the layer that actually hands a worktree to a third-party binary.
#[test]
fn reviewing_a_repository_does_not_mutate_it() {
    let temp = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
    let out = temp.path().join("fixtures");
    let manifest = build_fixture(&out).unwrap_or_else(|e| panic!("{e}"));
    let sha = manifest
        .sha("planted_bug_off_by_one")
        .unwrap_or_else(|e| panic!("{e}"));
    let repo_dir = out.join("git-basic");

    let status_before = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&repo_dir)
        .output()
        .unwrap_or_else(|e| panic!("{e}"));

    let runtime = tokio::runtime::Runtime::new().unwrap_or_else(|e| panic!("{e}"));
    runtime.block_on(async {
        let runner = GitRunner::new();
        let scratch = temp.path().join("scratch");
        let change = change_for(&sha, DiffStat::default());
        let context = revlocal_vcs::git::materialize(&runner, &repo_dir, &change, &scratch)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let change = change_for(&sha, context.stat);
        let engine = MockEngine::new();

        pipeline::review(
            &ReviewInputs {
                repo_name: "git-basic",
                repo_kind: "git",
                change: &change,
                config: &RepoConfig::default(),
                worktree: &context.worktree,
                diff_unified: &context.diff_unified,
                diff_files: &context.diff_files,
                labels: &[],
                suppressions: &[],
                published_fingerprints: &[],
                prior_findings: &[],
                skip: None,
                now: Timestamp::default(),
            },
            &engine,
            &scratch,
            &CancellationToken::new(),
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    });

    let status_after = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&repo_dir)
        .output()
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(
        String::from_utf8_lossy(&status_before.stdout),
        String::from_utf8_lossy(&status_after.stdout),
        "the pipeline mutated the repository under review"
    );
    assert!(
        String::from_utf8_lossy(&status_after.stdout)
            .trim()
            .is_empty(),
        "the fixture was not clean after a review"
    );
}
