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

    // The condition is asserted as a property rather than a literal, because the
    // literal changed once for a good reason and an exact match turns that into a
    // test failure instead of a review.
    let condition = upload["if"].as_str().unwrap_or_default();

    assert!(
        !condition.contains("always()"),
        "artifacts are uploaded on a red leg, not on every run; got {condition:?}"
    );
    assert!(
        condition.contains("failure()"),
        "a failing leg must upload its logs; got {condition:?}"
    );
    // `cancelled()` is not optional. `timeout-minutes` kills a job as CANCELLED,
    // not failed, so `if: failure()` alone skips the upload on exactly the run
    // that most needs it. Observed on run 33160541055: the Windows leg hit the
    // 45-minute bound, this step was skipped, and the hang produced no artifacts
    // at all. A bound without an upload makes a hang cheaper, not diagnosable.
    assert!(
        condition.contains("cancelled()"),
        "a timed-out leg reports cancelled, not failed, and is the one that most \
         needs its logs; got {condition:?}"
    );
    let paths = upload["with"]["path"].as_str().unwrap_or_default();
    assert!(
        paths.contains("artifacts/logs/"),
        "failure upload must include test logs; got {paths:?}"
    );
    assert!(
        paths.contains("artifacts/gui/*.png"),
        "failure upload must include GUI captures (§16.4); got {paths:?}"
    );
}

#[test]
fn the_test_job_is_time_bounded() {
    // Without a bound a hang runs to GitHub's six-hour default, during which the
    // job is neither passing nor failing and nothing is uploaded. A hung job
    // produces strictly less signal than a failing one and costs far more to get
    // it. The Windows leg has hung twice; this is what makes the second kind of
    // failure visible.
    let wf = workflow().expect("ci.yml exists and is valid YAML");

    let timeout = wf["jobs"]["test"]["timeout-minutes"]
        .as_u64()
        .expect("the test job must set timeout-minutes so a hang cannot run for six hours");

    assert!(
        (20..=90).contains(&timeout),
        "the bound should be a few times the slowest green run, not so tight that a \
         slow runner fails and not so loose that a hang is free; got {timeout}"
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

/// The desktop shell cannot be linted before its front end is built (RL-1206).
///
/// `tauri.conf.json` points `frontendDist` at `ui/dist`, and Tauri's build script
/// fails at *compile* time when that directory is absent — it is a build input,
/// not a runtime one. `ui/dist` is generated and gitignored, so it exists on a
/// developer's machine and never on a fresh checkout.
///
/// That asymmetry is the whole reason this test exists: the clippy step passed
/// locally and failed on its first CI run, because "works here" and "works on a
/// clean tree" are different claims and only one of them was checked. Deleting the
/// npm step would restore the failure silently.
#[test]
fn the_desktop_lint_builds_its_front_end_first() {
    let wf = workflow().expect("ci.yml exists and is valid YAML");
    let scripts = all_run_scripts(&wf);

    // Every step that enables the feature, not just the first. There are two now
    // — clippy on macOS and a build on Windows — and checking only the earliest
    // would let a second one be added before the npm step without anybody
    // noticing.
    let uses: Vec<usize> = scripts
        .match_indices("--features desktop")
        .map(|(at, _)| at)
        .collect();
    if uses.is_empty() {
        // Not compiling the desktop shell at all is a different problem, and one
        // this test deliberately does not have an opinion about.
        return;
    }

    let build = scripts
        .find("npm --prefix crates/revlocal-tauri/ui run build")
        .unwrap_or_else(|| {
            panic!(
                "CI compiles the desktop shell but never builds `ui/dist`; \
                 Tauri's build script will fail on a fresh checkout"
            )
        });

    for at in uses {
        assert!(
            build < at,
            "the front end must be built before every step that compiles the \
             desktop shell, not after — `frontendDist` is read at compile time"
        );
    }
}

/// The desktop smoke test starts the thing it just built (RL-1101).
///
/// REVL-87's first criterion is that the app *launches*, and compiling is nowhere
/// near that. The smoke step is what makes the difference, and it is only
/// meaningful in one order: build, then run.
#[test]
fn the_desktop_smoke_test_runs_after_the_build() {
    let wf = workflow().expect("ci.yml exists and is valid YAML");
    let scripts = all_run_scripts(&wf);

    let Some(smoke) = scripts.find("revlocal-desktop.exe") else {
        // Not smoke-testing at all is a different problem, and one this test
        // deliberately does not have an opinion about.
        return;
    };

    let build = scripts
        .find("--bin revlocal-desktop")
        .unwrap_or_else(|| panic!("CI runs the desktop shell but never builds it"));

    assert!(
        build < smoke,
        "the desktop shell must be built before it is started"
    );

    // The ready line is the whole reason this proves more than "a process
    // exists": without it, an app hung inside setup and one that started look
    // identical from outside.
    assert!(
        scripts.contains("window ready"),
        "the smoke test must wait for the app's ready line, not merely for time \
         to pass — a hang would otherwise read as a pass"
    );
}
