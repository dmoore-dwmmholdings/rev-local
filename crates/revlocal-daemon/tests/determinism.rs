//! The determinism guarantee (RL-507, SPEC §18).
//!
//! *Same change + same engine output ⇒ same findings, fingerprints and publish plan.*
//!
//! This is not a nice-to-have. Two things depend on it directly:
//!
//! - **Dedupe.** §10.3's fingerprint is what decides whether a finding is new. A
//!   fingerprint that varied between runs would re-file every finding on every
//!   review, and the failure would look like an engine that cannot make up its mind
//!   rather than like a hashing bug.
//! - **Suppression.** A user suppresses a fingerprint. If the fingerprint moves, the
//!   suppression silently stops working, and the thing they asked never to hear about
//!   comes back.
//!
//! # Three kinds of assertion, because they catch different things
//!
//! 1. **Repetition** — the same review, several times, compared byte for byte.
//!    Catches ordering that varies between instances within one process.
//! 2. **Golden fingerprints** — values committed to this file. A golden is a
//!    comparison against a *past process*, which is the only way to catch something
//!    seeded once per process; repetition inside one process cannot see it.
//! 3. **A source guard** — no unordered collection on the output path at all.
//!    Observing that today's output is stable does not keep it stable; this does.

use std::path::{Path, PathBuf};
use std::process::Command;

use revlocal_core::{
    Category, Change, ChangeId, ChangeKind, DiffStat, RepoConfig, RepoId, Severity, Suppression,
    SuppressionId, Timestamp,
};
use revlocal_daemon::pipeline::{self, ReviewInputs, ReviewReport};
use revlocal_engine::{EngineOutcome, MockBehaviour, MockEngine, RawFinding};
use revlocal_vcs::GitRunner;
use tokio_util::sync::CancellationToken;

// --- fixtures -------------------------------------------------------------

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

/// A fixed set of findings, deliberately **not** in any sorted order.
///
/// If the pipeline ever imposed its own ordering — or lost the engine's — this is
/// what would show it. Severities ascend, files descend, titles are unsorted.
fn fixed_findings() -> Vec<RawFinding> {
    let make = |severity: Severity, file: &str, title: &str, category: Category| RawFinding {
        severity,
        category,
        confidence: Some(0.8),
        file: Some(file.to_owned()),
        line_start: Some(7),
        line_end: Some(9),
        title: title.to_owned(),
        body: "why".to_owned(),
        failure_scenario: Some("inputs".to_owned()),
        suggested_fix: None,
    };

    vec![
        make(Severity::Low, "src/pager.rs", "zulu", Category::Convention),
        make(
            Severity::Medium,
            "src/pager.rs",
            "alpha",
            Category::Correctness,
        ),
        make(Severity::Info, "src/pager.rs", "mike", Category::Tests),
        make(Severity::Medium, "src/pager.rs", "bravo", Category::Perf),
    ]
}

fn outcome() -> EngineOutcome {
    EngineOutcome {
        findings: fixed_findings(),
        summary: "a fixed review".to_owned(),
        verdict: revlocal_core::Verdict::Comment,
        usage: revlocal_core::Usage::default(),
        transcript: String::new(),
        degraded: None,
        coverage_notes: None,
    }
}

/// Review the planted-bug commit with a fixed engine output.
fn review_once(suppressions: &[Suppression]) -> Result<ReviewReport, String> {
    let temp = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let out = temp.path().join("fixtures");
    let manifest = build_fixture(&out)?;
    let sha = sha_for(&manifest, "planted_bug_off_by_one")?;
    let repo_dir = out.join("git-basic");

    let runtime = tokio::runtime::Runtime::new().map_err(|e| format!("runtime: {e}"))?;

    runtime.block_on(async {
        let runner = GitRunner::new();
        let scratch = temp.path().join("scratch");
        let change = Change {
            id: ChangeId::new(1),
            repo_id: RepoId::new(1),
            kind: ChangeKind::Commit,
            external_id: sha.clone(),
            title: Some("Add pagination helper".to_owned()),
            author_name: Some("Fixture Author".to_owned()),
            author_email: None,
            authored_at: None,
            branch: Some("main".to_owned()),
            base_ref: None,
            head_ref: Some(sha.clone()),
            url: None,
            diff_stat: DiffStat::default(),
            detected_at: Timestamp::default(),
        };

        let context = revlocal_vcs::git::materialize(&runner, &repo_dir, &change, &scratch)
            .await
            .map_err(|e| format!("materialize: {e}"))?;

        let change = Change {
            diff_stat: context.stat,
            ..change
        };

        let engine = MockEngine::with_behaviour(MockBehaviour::Succeed(Box::new(outcome())));

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
                suppressions,
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
        .map_err(|e| format!("pipeline: {e}"))
    })
}

// --- 1. repetition --------------------------------------------------------

#[test]
fn determinism_the_same_review_repeated_is_byte_identical() {
    let first = review_once(&[]).unwrap_or_else(|e| panic!("{e}"));
    let baseline = first.to_json().unwrap_or_else(|e| panic!("{e}"));

    // Several times, not twice: an ordering that differs between two hash instances
    // agrees by chance roughly half the time with four items.
    for attempt in 1..=5 {
        let again = review_once(&[]).unwrap_or_else(|e| panic!("{e}"));
        let json = again.to_json().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(json, baseline, "review {attempt} differed from the first");
    }
}

/// Findings come out in the order the engine reported them, not sorted and not
/// shuffled. Sorting would be defensible; doing it *sometimes* would not.
#[test]
fn determinism_finding_order_follows_the_engine() {
    let report = review_once(&[]).unwrap_or_else(|e| panic!("{e}"));

    let titles: Vec<&str> = report.findings.iter().map(|f| f.title.as_str()).collect();
    assert_eq!(
        titles,
        ["zulu", "alpha", "mike", "bravo"],
        "the pipeline reordered the engine's findings"
    );
}

/// The withheld list has to be stable too: it is what the UI shows to explain an
/// absence, and a list that reshuffles looks like the reasons changed.
#[test]
fn determinism_withheld_order_is_stable() {
    let print = revlocal_core::fingerprint(
        "git-basic",
        Some("src/pager.rs"),
        Category::Correctness,
        "alpha",
    );
    let suppressions = [Suppression {
        id: SuppressionId::new(1),
        repo_id: None,
        fingerprint: Some(print),
        glob: None,
        reason: Some("accepted".to_owned()),
        created_at: Timestamp::default(),
    }];

    let first = review_once(&suppressions).unwrap_or_else(|e| panic!("{e}"));
    let second = review_once(&suppressions).unwrap_or_else(|e| panic!("{e}"));

    assert!(
        !first.withheld.is_empty(),
        "nothing was withheld to compare"
    );
    assert_eq!(first.withheld, second.withheld);
    assert_eq!(first.findings, second.findings);
}

// --- 2. golden fingerprints ----------------------------------------------

/// Committed fingerprints for the fixed findings above.
///
/// A golden is a comparison against a **past process**, which is the only way to
/// catch something seeded once per process — repetition inside one process cannot see
/// it. They also pin §10.3's hash: changing the fingerprint algorithm breaks every
/// user's suppressions and every stored dedupe key, so it must never happen by
/// accident. If these change, that is the change, and it needs a migration.
/// **Cross-checked, not merely recorded.** Each value was independently recomputed
/// from §10.3's text — sha256 of repo, normalized path, category and normalized
/// title, NUL-separated, first 16 hex chars — by a separate implementation, and all
/// four agreed. A golden pasted from the code it tests only pins the current
/// behaviour, bug included.
const GOLDEN_FINGERPRINTS: [(&str, &str); 4] = [
    ("zulu", "40db4cf00a45eb8b"),
    ("alpha", "ea08d459049caa86"),
    ("mike", "c60dd658bed841e7"),
    ("bravo", "dfb1289d10551be6"),
];

#[test]
fn determinism_fingerprints_match_their_committed_goldens() {
    let report = review_once(&[]).unwrap_or_else(|e| panic!("{e}"));

    let actual: Vec<(String, String)> = report
        .findings
        .iter()
        .map(|f| (f.title.clone(), f.fingerprint.clone()))
        .collect();

    let expected: Vec<(String, String)> = GOLDEN_FINGERPRINTS
        .iter()
        .map(|(t, f)| ((*t).to_owned(), (*f).to_owned()))
        .collect();

    assert_eq!(
        actual, expected,
        "\nfingerprints changed. This breaks every stored suppression and every \
         dedupe key, so it must be a deliberate, migrated change — not a side effect.\n"
    );
}

// --- 3. the source guard --------------------------------------------------

/// No unordered collection anywhere on the path from an engine's output to a report.
///
/// Observing that today's output happens to be stable does not keep it stable. This
/// is the assertion that survives the next contributor, and it is deliberately a
/// *source* check rather than a behavioural one: a `HashMap` with three entries
/// iterates consistently often enough that a behavioural test would pass for months
/// and then fail in someone's CI.
///
/// The same shape as `revlocal-vcs`'s guard that only `git/cmd.rs` may spawn git.
#[test]
fn determinism_no_unordered_collection_reaches_the_output_path() {
    // Every crate a finding passes through on its way to a report.
    let crates = [
        "revlocal-core",
        "revlocal-engine",
        "revlocal-vcs",
        "revlocal-daemon",
        "revlocal-publish",
    ];

    // Iteration order of these is unspecified, and every one has an ordered
    // equivalent that costs nothing at these sizes.
    let banned = ["HashMap", "HashSet", "hash_map", "hash_set"];

    let root = workspace_root().join("crates");
    let mut offenders = Vec::new();

    fn walk(dir: &Path, banned: &[&str], offenders: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        // Sorted, because a guard that reports its findings in filesystem order is
        // itself nondeterministic — and this test of all tests should not be.
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();

        for path in paths {
            if path.is_dir() {
                walk(&path, banned, offenders);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for (number, line) in text.lines().enumerate() {
                // Prose about the rule is not a violation of it.
                let code = line.split("//").next().unwrap_or(line);
                for needle in banned {
                    if code.contains(needle) {
                        offenders.push(format!(
                            "{}:{}: {}",
                            path.display(),
                            number + 1,
                            line.trim()
                        ));
                    }
                }
            }
        }
    }

    for name in crates {
        walk(&root.join(name).join("src"), &banned, &mut offenders);
    }

    assert!(
        offenders.is_empty(),
        "unordered collections on the output path — use BTreeMap/BTreeSet/Vec, whose \
         iteration order is defined. §18: the same change must produce the same \
         findings, and a HashMap iterates consistently often enough to pass a \
         behavioural test for months before failing in someone's CI.\n{}",
        offenders.join("\n")
    );
}

/// The guard has to be able to fail, or it is decoration. This feeds it a file it
/// should reject and one it should accept.
#[test]
fn determinism_the_source_guard_recognises_a_violation() {
    let banned = ["HashMap", "HashSet"];

    let offending = "use std::collections::HashMap;\nlet m: HashMap<u8, u8> = HashMap::new();";
    let clean = "use std::collections::BTreeMap;\n// a HashMap would be wrong here";

    let count = |text: &str| -> usize {
        text.lines()
            .filter(|line| {
                let code = line.split("//").next().unwrap_or(line);
                banned.iter().any(|n| code.contains(n))
            })
            .count()
    };

    assert_eq!(count(offending), 2, "the guard would miss a real violation");
    assert_eq!(count(clean), 0, "the guard flags a comment about the rule");
}

// --- the publish plan -----------------------------------------------------

/// The criterion names "publish plans". There is no plan builder yet — §11 is M7 —
/// so the guarantee is asserted at the boundary that will feed it: the ordered,
/// fingerprinted set of publishable findings.
///
/// Stated rather than quietly skipped, because a criterion that looks covered and is
/// not is worse than one openly deferred.
#[test]
fn determinism_the_publish_plan_input_is_stable() {
    let first = review_once(&[]).unwrap_or_else(|e| panic!("{e}"));
    let second = review_once(&[]).unwrap_or_else(|e| panic!("{e}"));

    let plan_input = |r: &ReviewReport| -> Vec<(String, String, Option<String>)> {
        r.findings
            .iter()
            .map(|f| (f.fingerprint.clone(), f.severity.clone(), f.file.clone()))
            .collect()
    };

    assert_eq!(plan_input(&first), plan_input(&second));
    assert_eq!(first.verdict, second.verdict, "the verdict drives the plan");
    assert_eq!(first.depth, second.depth);
    assert_eq!(first.depth_reasons, second.depth_reasons);
}
