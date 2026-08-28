//! Acceptance tests for `RL-102` — the cross-platform CI matrix.
//!
//! CI being green is only observable on GitHub. What *is* observable here is that
//! the workflow is well-formed and still says what SPEC §16.3 requires it to say:
//! three OS legs, Subversion installed on each, and failure artifacts uploaded.
//! These tests fail if someone drops a leg or quietly removes the `svn` step.

use serde_norway::Value;
use std::path::PathBuf;

/// The three platforms decision D9 requires from day one.
const REQUIRED_RUNNERS: [&str; 3] = ["ubuntu-latest", "macos-latest", "windows-latest"];

/// Parse `.github/workflows/ci.yml` from the workspace root.
fn workflow() -> Result<Value, Box<dyn std::error::Error>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows/ci.yml");
    Ok(serde_norway::from_str(&std::fs::read_to_string(path)?)?)
}

/// The `test` job's steps, as a slice of mappings.
fn test_job_steps(wf: &Value) -> Vec<&Value> {
    wf["jobs"]["test"]["steps"]
        .as_sequence()
        .map(|s| s.iter().collect())
        .unwrap_or_default()
}

/// Every `run:` script in the `test` job, concatenated.
fn all_run_scripts(wf: &Value) -> String {
    test_job_steps(wf)
        .iter()
        .filter_map(|s| s["run"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn workflow_triggers_on_push_and_pull_request() {
    let wf = workflow().expect("ci.yml exists and is valid YAML");
    // `on` is YAML 1.1 truthy, so the key may parse as the boolean `true`.
    let triggers = wf
        .get("on")
        .or_else(|| wf.get(Value::Bool(true)))
        .expect("ci.yml declares triggers");
    assert!(
        triggers.get("push").is_some(),
        "CI must run on push (RL-102 acceptance criteria)"
    );
    assert!(
        triggers.get("pull_request").is_some(),
        "CI must run on pull_request (RL-102 acceptance criteria)"
    );
}

#[test]
fn the_matrix_covers_all_three_platforms() {
    let wf = workflow().expect("ci.yml exists and is valid YAML");
    let matrix = wf["jobs"]["test"]["strategy"]["matrix"]["os"]
        .as_sequence()
        .map(|s| {
            s.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    for runner in REQUIRED_RUNNERS {
        assert!(
            matrix.iter().any(|m| m == runner),
            "SPEC decision D9 requires a {runner} leg; matrix is {matrix:?}"
        );
    }
    assert_eq!(
        wf["jobs"]["test"]["strategy"]["fail-fast"].as_bool(),
        Some(false),
        "fail-fast must be off: one platform failing must not hide the others"
    );
}

#[test]
fn subversion_is_installed_and_verified_on_every_runner() {
    let wf = workflow().expect("ci.yml exists and is valid YAML");
    let scripts = all_run_scripts(&wf);

    for installer in [
        "apt-get install -y subversion",
        "brew install subversion",
        "choco install svn",
    ] {
        assert!(
            scripts.contains(installer),
            "M11 needs svn on every runner; missing installer step: {installer}"
        );
    }

    // The verification step must be unconditional — that is what makes
    // "`svn --version` succeeds on every runner" an assertion and not a hope.
    let verify_is_unconditional = test_job_steps(&wf).iter().any(|s| {
        s["run"]
            .as_str()
            .is_some_and(|r| r.contains("svn --version"))
            && s.get("if").is_none()
    });
    assert!(
        verify_is_unconditional,
        "the `svn --version` step must run on all three OSes, not behind an `if:`"
    );
}

#[test]
fn the_four_gates_run_in_order() {
    let wf = workflow().expect("ci.yml exists and is valid YAML");
    let scripts = all_run_scripts(&wf);
    let mut cursor = 0usize;

    for gate in [
        "cargo fmt --all -- --check",
        "cargo clippy --workspace --all-targets -- -D warnings",
        "cargo build --workspace",
        "cargo test --workspace",
    ] {
        let at = scripts[cursor..].find(gate).unwrap_or_else(|| {
            panic!("CI must run `{gate}`, in fmt -> clippy -> build -> test order")
        });
        cursor += at + gate.len();
    }
}

#[test]
fn failure_uploads_logs_and_gui_captures() {
    let wf = workflow().expect("ci.yml exists and is valid YAML");

    let upload = test_job_steps(&wf)
        .into_iter()
        .find(|s| {
            s["uses"]
                .as_str()
                .is_some_and(|u| u.starts_with("actions/upload-artifact"))
        })
        .expect("CI must upload artifacts so a red leg is diagnosable");

    assert_eq!(
        upload["if"].as_str(),
        Some("failure()"),
        "artifacts are uploaded on failure, not on every run"
    );
    let paths = upload["with"]["path"].as_str().unwrap_or_default();
    assert!(
        paths.contains("artifacts/logs/"),
        "failure upload must include test logs; got {paths:?}"
    );
    assert!(
        paths.contains("artifacts/gui/*.png"),
        "failure upload must include Framewatch captures (§16.4); got {paths:?}"
    );
}

#[test]
fn cargo_registry_and_target_dir_are_cached() {
    let wf = workflow().expect("ci.yml exists and is valid YAML");
    let cached = test_job_steps(&wf).iter().any(|s| {
        s["uses"]
            .as_str()
            .is_some_and(|u| u.starts_with("Swatinem/rust-cache") || u.starts_with("actions/cache"))
    });
    assert!(
        cached,
        "RL-102 requires the cargo registry and target dir to be cached"
    );
}
