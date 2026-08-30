//! Acceptance tests for `RL-203` — the mock engine fixture.
//!
//! Every rung of SPEC §8.2's fallback ladder has a mode, and each mode is
//! exercised here. These tests assert on the *fixture*, not on the runner that
//! will consume it (`RL-40x`) — the point is that when the runner is written, the
//! failure it needs to handle is already reproducible on demand rather than
//! something it has to be trusted to handle.

mod mock_engine {
    use std::path::{Path, PathBuf};
    use std::process::Command;
    // Only the SIGTERM test pipes stdio, and that test is `#[cfg(unix)]` — so on
    // Windows this import is unused and `-D warnings` fails the build. A platform
    // gate on an item is also a gate on everything only that item uses.
    #[cfg(unix)]
    use std::process::Stdio;

    /// The modes SPEC §8.2 needs covered, and what each is for.
    const LADDER_MODES: [&str; 8] = [
        "valid",
        "malformed_json",
        "fenced_only",
        "no_file",
        "hang",
        "partial_findings",
        "nonzero_exit",
        "slow_but_ok",
    ];

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
    }

    fn engine() -> PathBuf {
        revlocal_engine::mock_engine_program()
    }

    /// Run the mock engine in `mode`, returning (exit code, stdout, out_dir).
    ///
    /// Returns `Result`; helpers are not `#[test]` fns (ADR 0003).
    fn run(mode: &str, out: &Path) -> Result<(i32, String), String> {
        let output = Command::new(engine())
            .env("MOCK_ENGINE_MODE", mode)
            .env("REVLOCAL_OUT", out)
            .output()
            .map_err(|e| format!("spawning the mock engine: {e}"))?;

        Ok((
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        ))
    }

    fn read_result(out: &Path) -> Option<String> {
        std::fs::read_to_string(out.join("result.json")).ok()
    }

    fn scratch() -> Result<tempfile::TempDir, std::io::Error> {
        tempfile::TempDir::new()
    }

    #[test]
    fn mock_engine_valid_mode_emits_a_result_that_validates_against_the_schema() {
        // Acceptance criterion 3, checked with the real schema rather than by
        // eyeballing the shape.
        let dir = scratch().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let (code, _) = run("valid", dir.path()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(code, 0);

        let schema_text = std::fs::read_to_string(
            workspace_root().join("crates/revlocal-engine/schema/result.v1.json"),
        )
        .unwrap_or_else(|e| panic!("reading the schema: {e}"));
        let schema: serde_json::Value = serde_json::from_str(&schema_text)
            .unwrap_or_else(|e| panic!("schema is not JSON: {e}"));

        let body = read_result(dir.path()).unwrap_or_else(|| panic!("no result.json was written"));
        let instance: serde_json::Value =
            serde_json::from_str(&body).unwrap_or_else(|e| panic!("result.json is not JSON: {e}"));

        let validator = jsonschema::validator_for(&schema)
            .unwrap_or_else(|e| panic!("result.v1.json is not a valid schema: {e}"));

        let errors: Vec<String> = validator
            .iter_errors(&instance)
            .map(|e| e.to_string())
            .collect();
        assert!(
            errors.is_empty(),
            "result.json does not validate: {errors:#?}"
        );
    }

    #[test]
    fn mock_engine_the_schema_actually_rejects_a_bad_result() {
        // A schema that accepts everything would make the test above meaningless.
        let schema_text = std::fs::read_to_string(
            workspace_root().join("crates/revlocal-engine/schema/result.v1.json"),
        )
        .unwrap_or_else(|e| panic!("reading the schema: {e}"));
        let schema: serde_json::Value =
            serde_json::from_str(&schema_text).unwrap_or_else(|e| panic!("{e}"));
        let validator = jsonschema::validator_for(&schema).unwrap_or_else(|e| panic!("{e}"));

        let bad = [
            serde_json::json!({}),
            serde_json::json!({"schema_version": 2, "verdict": "approve", "summary": "", "findings": []}),
            serde_json::json!({"schema_version": 1, "verdict": "lgtm", "summary": "", "findings": []}),
            serde_json::json!({"schema_version": 1, "verdict": "approve", "summary": "",
                               "findings": [{"severity": "high", "category": "correctness", "title": "t"}]}),
            serde_json::json!({"schema_version": 1, "verdict": "approve", "summary": "",
                               "findings": [{"severity": "high", "category": "correctness",
                                             "title": "x".repeat(81), "body": "b"}]}),
        ];
        for instance in bad {
            assert!(
                !validator.is_valid(&instance),
                "the schema accepted something it should reject: {instance}"
            );
        }
    }

    #[test]
    fn mock_engine_has_a_mode_for_every_rung_of_the_fallback_ladder() {
        for mode in LADDER_MODES {
            if mode == "hang" {
                continue; // exercised separately; it never exits on its own
            }
            let dir = scratch().unwrap_or_else(|e| panic!("temp dir: {e}"));
            let result = run(mode, dir.path());
            assert!(result.is_ok(), "mode {mode} could not be run: {result:?}");
        }
    }

    #[test]
    fn mock_engine_an_unknown_mode_fails_loudly_rather_than_defaulting() {
        // Defaulting a typo'd mode to `valid` would make a test think it exercised
        // a failure path when it exercised the happy one.
        let dir = scratch().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let (code, _) = run("valdi", dir.path()).unwrap_or_else(|e| panic!("{e}"));
        assert_ne!(code, 0, "an unknown mode must not succeed");
        assert!(
            read_result(dir.path()).is_none(),
            "and must not write output"
        );
    }

    #[test]
    fn mock_engine_malformed_json_leaves_an_unparseable_file_and_a_good_fence() {
        // Rung (a): the file exists but does not parse, so the runner has to fall
        // through to the fenced block.
        let dir = scratch().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let (code, stdout) = run("malformed_json", dir.path()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(code, 0);

        let body = read_result(dir.path()).unwrap_or_else(|| panic!("no result.json"));
        assert!(
            serde_json::from_str::<serde_json::Value>(&body).is_err(),
            "the file must NOT parse, or this rung is not exercised: {body}"
        );
        assert!(
            stdout.contains("```json"),
            "a recoverable fence must be present"
        );
    }

    #[test]
    fn mock_engine_fenced_only_emits_two_fences_so_last_wins_is_testable() {
        // §8.2 says the LAST fenced block is authoritative. With one block, a
        // runner that took the first would pass and be wrong in production.
        let dir = scratch().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let (code, stdout) = run("fenced_only", dir.path()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(code, 0);
        assert!(
            read_result(dir.path()).is_none(),
            "this rung must write no file"
        );

        assert_eq!(
            stdout.matches("```json").count(),
            2,
            "two fences are needed to tell first-wins from last-wins: {stdout}"
        );
        let last = stdout.rsplit("```json").next().unwrap_or_default();
        assert!(
            last.contains("request_changes"),
            "the LAST fence must be the authoritative one: {last}"
        );
    }

    #[test]
    fn mock_engine_no_file_mode_puts_bare_json_on_stdout() {
        // Rung (b): parse the whole of stdout as JSON.
        let dir = scratch().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let (code, stdout) = run("no_file", dir.path()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(code, 0);
        assert!(read_result(dir.path()).is_none());
        assert!(
            !stdout.contains("```"),
            "no fence, or this is the previous rung"
        );
        assert!(
            serde_json::from_str::<serde_json::Value>(stdout.trim()).is_ok(),
            "stdout must parse whole: {stdout}"
        );
    }

    #[test]
    fn mock_engine_partial_findings_keeps_a_valid_envelope_with_invalid_findings() {
        // §8.3: findings failing validation are dropped individually and the run
        // still succeeds if the envelope parsed. That needs a result where exactly
        // that is true.
        let dir = scratch().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let (code, _) = run("partial_findings", dir.path()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(code, 0);

        let body = read_result(dir.path()).unwrap_or_else(|| panic!("no result.json"));
        let parsed: serde_json::Value =
            serde_json::from_str(&body).unwrap_or_else(|e| panic!("envelope must parse: {e}"));

        assert_eq!(parsed["schema_version"], 1, "the envelope must be valid");
        let findings = parsed["findings"].as_array().unwrap_or(&Vec::new()).clone();
        assert!(
            findings.len() >= 4,
            "need good and bad findings side by side"
        );

        let schema_text = std::fs::read_to_string(
            workspace_root().join("crates/revlocal-engine/schema/result.v1.json"),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let schema: serde_json::Value =
            serde_json::from_str(&schema_text).unwrap_or_else(|e| panic!("{e}"));
        let finding_schema = serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "$defs": schema["$defs"].clone(),
            "$ref": "#/$defs/finding"
        });
        let validator =
            jsonschema::validator_for(&finding_schema).unwrap_or_else(|e| panic!("{e}"));

        let valid = findings.iter().filter(|f| validator.is_valid(f)).count();
        let invalid = findings.len() - valid;
        assert!(
            valid >= 2,
            "some findings must survive, or nothing is dropped-and-kept"
        );
        assert!(
            invalid >= 2,
            "some findings must fail, or nothing is dropped"
        );
    }

    #[test]
    fn mock_engine_nonzero_exit_still_writes_usable_output() {
        // A runner that keys only off the exit code would throw away a review the
        // engine actually produced.
        let dir = scratch().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let (code, _) = run("nonzero_exit", dir.path()).unwrap_or_else(|e| panic!("{e}"));
        assert_ne!(code, 0, "this mode exists to fail");

        let body = read_result(dir.path()).unwrap_or_else(|| panic!("no result.json"));
        assert!(serde_json::from_str::<serde_json::Value>(&body).is_ok());
    }

    #[test]
    fn mock_engine_slow_but_ok_is_slow_enough_to_tell_from_instant() {
        // Distinguishes "slow" from "hung", so a timeout test cannot pass by being
        // merely impatient.
        let dir = scratch().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let started = std::time::Instant::now();
        let (code, _) = run("slow_but_ok", dir.path()).unwrap_or_else(|e| panic!("{e}"));
        let elapsed = started.elapsed();

        assert_eq!(code, 0);
        assert!(read_result(dir.path()).is_some());
        assert!(
            elapsed >= std::time::Duration::from_millis(500),
            "slow_but_ok returned in {elapsed:?}, which is not distinguishable from instant"
        );
    }

    // SIGTERM is a POSIX concept. Windows has no signal a console process can
    // choose to ignore, so "ignores SIGTERM" is not a behaviour that exists there
    // to test — `terminate` says as much, and uses TerminateProcess instead.
    //
    // Gated rather than adapted, and gated *loudly*: REVL-106's third criterion is
    // that no test is skipped on Windows without an explicit documented reason,
    // and this is the reason.
    //
    // This gate is also what unblocked the Windows CI leg. Ungated, the test ran
    // `kill -TERM <pid>` against a native Windows pid — which Git-bash's `kill`
    // cannot address, reporting "No such process" — and the resulting panic fired
    // *before* the cleanup below, leaking the hang-mode process. That orphan held
    // the inherited stdout pipe, so `tee` never saw EOF and the whole job hung
    // until the 45-minute bound. One leaked child, two symptoms: a failing test
    // and a job that would not end.
    #[cfg(unix)]
    #[test]
    fn mock_engine_hang_mode_ignores_sigterm_so_the_sigkill_path_is_real() {
        // Acceptance criterion 2, and the one most easily faked. A `hang` mode that
        // died on SIGTERM would let a runner claiming to escalate to SIGKILL pass
        // without ever escalating.
        let dir = scratch().unwrap_or_else(|e| panic!("temp dir: {e}"));

        let mut child = Command::new(engine())
            .env("MOCK_ENGINE_MODE", "hang")
            .env("REVLOCAL_OUT", dir.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("spawning: {e}"));

        // Give node time to install the handler before signalling; otherwise this
        // would test the default disposition and pass for the wrong reason.
        std::thread::sleep(std::time::Duration::from_millis(600));

        let pid = child.id();
        let termed = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status();

        // Everything below is gathered first and asserted last, so no failure path
        // can leave the child running. The previous shape asserted on the kill's
        // status immediately, and a process that outlives its test holds whatever
        // pipes it inherited — which is how a failing assertion became a hung CI
        // job rather than a red one.
        let sent = matches!(&termed, Ok(status) if status.success());

        let still_running = if sent {
            std::thread::sleep(std::time::Duration::from_millis(600));
            child
                .try_wait()
                .unwrap_or_else(|e| panic!("try_wait: {e}"))
                .is_none()
        } else {
            false
        };

        // `Child::kill` rather than shelling out: cleanup must not depend on a
        // `kill` binary being present, since a missing one is exactly the case
        // that leaks.
        let _ = child.kill();
        let _ = child.wait();

        assert!(sent, "could not send SIGTERM to {pid}: {termed:?}");
        assert!(
            still_running,
            "hang mode exited on SIGTERM; the SIGKILL escalation path would never be exercised"
        );
    }

    #[test]
    fn mock_engine_reports_a_version_because_doctor_probes_for_one() {
        // SPEC §8.4: `revlocal doctor` runs version_args before anything else.
        let output = Command::new(engine())
            .arg("--version")
            .output()
            .unwrap_or_else(|e| panic!("spawning: {e}"));
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("mock-engine"), "got {stdout:?}");
    }

    #[test]
    fn mock_engine_refuses_to_run_without_revlocal_out() {
        // §8.2 has the runner create out_dir and pass it in the environment. A
        // fixture that quietly wrote nowhere would look exactly like the `no_file`
        // rung, and a plumbing bug would be diagnosed as a fallback test.
        let output = Command::new(engine())
            .env("MOCK_ENGINE_MODE", "valid")
            .env_remove("REVLOCAL_OUT")
            .output()
            .unwrap_or_else(|e| panic!("spawning: {e}"));

        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("REVLOCAL_OUT"),
            "the error must name the missing variable"
        );
    }

    #[test]
    fn mock_engine_has_a_windows_shim() {
        // Acceptance criterion 4. The logic is a portable node script; `run` and
        // `run.cmd` are the two shims onto it.
        let dir = workspace_root().join("fixtures/mock-engine");
        for shim in ["run", "run.cmd", "mock-engine.mjs"] {
            assert!(dir.join(shim).is_file(), "missing {shim}");
        }
        let cmd = std::fs::read_to_string(dir.join("run.cmd")).unwrap_or_default();
        assert!(
            cmd.contains("mock-engine.mjs"),
            "run.cmd must invoke the script"
        );
        assert!(cmd.contains("%*"), "run.cmd must forward its arguments");
    }
}

/// A grandchild that outlives its parent must not hang the drain (RL-1303, §18).
///
/// The drain was unbounded whenever the process exited on its own, on the
/// reasoning that an exited process has closed its pipes. That is true of the
/// process and false of anything it spawned — and the gap between those two is an
/// unbounded await with no timeout around it, on a path where the Windows job
/// object that would reap the grandchild is not closed until after the await
/// returns.
///
/// This is the shape that deadlocks: a parent that spawns a child holding stdout,
/// then exits successfully.
/// Multi-threaded on purpose. Under the default current-thread runtime a blocked
/// drain starves the timer, so `tokio::time::timeout` never fires and the test
/// hangs instead of failing — which is precisely how this class of bug ate four
/// CI rounds. A regression here must be reportable.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_grandchild_holding_the_pipe_does_not_hang_a_successful_exit() {
    if std::process::Command::new("sh")
        .arg("-c")
        .arg("true")
        .status()
        .is_err()
    {
        println!("SKIPPED (no sh, nothing verified): a_grandchild_holding_the_pipe...");
        return;
    }

    let dir = match tempfile::tempdir() {
        Ok(dir) => dir,
        Err(error) => {
            println!("SKIPPED (no temp dir, nothing verified): {error}");
            return;
        }
    };

    // The parent writes a line, backgrounds a child that keeps stdout open for
    // well past the drain bound, and exits 0 immediately.
    let script = dir.path().join("leak.sh");
    if std::fs::write(
        &script,
        "#!/bin/sh\necho parent-said-this\n( sleep 60 ) &\nexit 0\n",
    )
    .is_err()
    {
        println!("SKIPPED (could not write the fixture, nothing verified)");
        return;
    }

    let invocation = revlocal_engine::Invocation {
        program: "sh".to_owned(),
        args: vec![script.display().to_string()],
        stdin: None,
    };

    // Generously more than the drain bound and far less than `sleep 60`. If the
    // drain were unbounded this would sit here until the harness gave up.
    let supervised = tokio::time::timeout(
        std::time::Duration::from_secs(25),
        revlocal_engine::supervise::supervise(
            revlocal_engine::EngineId::Mock,
            &invocation,
            dir.path(),
            &std::collections::BTreeMap::new(),
            std::time::Duration::from_secs(30),
            &tokio_util::sync::CancellationToken::new(),
        ),
    )
    .await;

    let supervised = match supervised {
        Ok(Ok(supervised)) => supervised,
        Ok(Err(error)) => {
            println!("SKIPPED (the fixture would not run, nothing verified): {error}");
            return;
        }
        Err(_) => panic!(
            "the drain never returned: a grandchild holding stdout hung a process \
             that had already exited"
        ),
    };

    assert!(
        supervised.completed(),
        "the parent exited on its own; it was not killed"
    );
    assert!(
        supervised.stdout.contains("parent-said-this"),
        "what the parent wrote before exiting must still be captured: {:?}",
        supervised.stdout
    );

    // §18: the run may not claim output it did not finish reading. The grandchild
    // holds the pipe for a minute against a ten-second bound, so this reliably
    // truncates — and if it ever stops doing so, that is a change worth failing
    // on rather than a condition to tiptoe around.
    assert!(
        supervised.output_truncated,
        "the grandchild held the pipe past the bound; the run must say the output \
         is not necessarily all of it"
    );
    assert!(
        !supervised.output_is_complete(),
        "and it must be visible through the accessor, not only the field"
    );
}
