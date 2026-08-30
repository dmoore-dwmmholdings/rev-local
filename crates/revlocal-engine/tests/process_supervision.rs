//! Acceptance tests for `RL-405` — process supervision (SPEC §8.5).
//!
//! An engine is somebody else's program, run on a schedule, with nobody watching.
//! These tests are about what happens when it misbehaves: a hang that ignores
//! SIGTERM, a process tree that outlives its root, a kill switch pulled mid-review.
//!
//! Every one drives the **real subprocess fixture**. There is no way to test a
//! process group without a process group.
//!
//! # Multi-threaded runtimes throughout (RL-1303)
//!
//! Every test here kills a real process, and a killed process can leave a child
//! holding the pipes. Under the default current-thread runtime a blocked drain
//! starves the timer, so `tokio::time::timeout` never fires and the test **hangs**
//! rather than failing — which stops every test binary queued behind it and turns
//! one bad assertion into a run that reports nothing.
//!
//! That is not hypothetical. It is what the Windows leg did four times before
//! SPEC §8.5's Job Object landed: forty-five minutes per round, each producing a
//! log whose last line was a test name.
//!
//! A regression here must be reportable.

mod process_supervision {
    //! Why every test that *kills* an engine is `#[cfg(unix)]`.
    //!
    //! Not because the behaviour is Unix-only. §8.5 requires process-group
    //! termination on every platform and §12.1 gives cancellation three seconds
    //! everywhere. On Windows both are currently **unmet**, and the tests do not
    //! fail — they **hang**.
    //!
    //! Windows has no process-group kill, so `terminate` reaches only the direct
    //! child. The fixture engine is `run.cmd`, so killing it terminates `cmd.exe`
    //! and leaves the `node` grandchild running, holding the pipes. §8.5 calls for
    //! a Job Object here; it is not implemented, and REVL-106 tracks it.
    //!
    //! A hung test binary stops the whole run, so **no test after it executes**.
    //! Five CI rounds ended inside this file, each costing 45 minutes and telling
    //! us nothing new — while roughly fifty test binaries after it had never run on
    //! Windows even once.
    //!
    //! These gates exist to find out what *else* is broken there. They are
    //! documented skips under REVL-106's third criterion, paired with a recorded
    //! gap rather than standing in for one, and every one comes back the moment
    //! the Job Object lands.

    use revlocal_core::Depth;
    use revlocal_engine::supervise::{supervise, timeout_for, KillReason, GRACE};
    use revlocal_engine::template::Invocation;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
    }

    /// An invocation of the fixture engine in `mode`.
    fn fixture(mode: &str, out: &std::path::Path) -> (Invocation, BTreeMap<String, String>) {
        let invocation = Invocation {
            program: revlocal_engine::mock_engine_program().display().to_string(),
            args: Vec::new(),
            stdin: None,
        };
        let env = BTreeMap::from([
            ("MOCK_ENGINE_MODE".to_owned(), mode.to_owned()),
            ("REVLOCAL_OUT".to_owned(), out.display().to_string()),
            // node and bash need these to start at all; §8.5 filters secrets, not
            // the ability to run a program.
            ("PATH".to_owned(), std::env::var("PATH").unwrap_or_default()),
            ("HOME".to_owned(), std::env::var("HOME").unwrap_or_default()),
        ]);
        (invocation, env)
    }

    /// Whether a pid is still alive. Unix only; signal 0 delivers nothing.
    #[cfg(unix)]
    fn is_alive(pid: i32) -> bool {
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
    }

    #[cfg(unix)]
    fn hard_kill(pid: i32) {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid),
            nix::sys::signal::Signal::SIGKILL,
        );
    }

    // --- depth-scaled timeouts -------------------------------------------------

    #[test]
    fn supervision_timeouts_scale_with_depth_as_spec_8_5_says() {
        assert_eq!(timeout_for(Depth::Summary), Duration::from_secs(3 * 60));
        assert_eq!(timeout_for(Depth::Standard), Duration::from_secs(10 * 60));
        assert_eq!(timeout_for(Depth::Deep), Duration::from_secs(25 * 60));

        // A deeper review is given more time, not less — the ordering is the part
        // that would break silently if someone edited one constant.
        assert!(timeout_for(Depth::Summary) < timeout_for(Depth::Standard));
        assert!(timeout_for(Depth::Standard) < timeout_for(Depth::Deep));
    }

    // --- the hang ---------------------------------------------------------------

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn supervision_an_engine_that_respects_sigterm_is_gone_within_timeout_plus_two_seconds() {
        // Acceptance criterion 1, for the case it is actually about: supervisor
        // overhead. A well-behaved CLI dies on SIGTERM, so the grace period never
        // elapses and the whole thing is over almost immediately after the timeout.
        //
        // `sleep` stands in for that CLI — it is the simplest program that runs long
        // and terminates politely.
        let invocation = Invocation {
            program: "sleep".to_owned(),
            args: vec!["300".to_owned()],
            stdin: None,
        };
        let timeout = Duration::from_millis(600);

        let started = Instant::now();
        let result = supervise(
            revlocal_core::EngineKind::Mock,
            &invocation,
            &workspace_root(),
            &BTreeMap::new(),
            timeout,
            &CancellationToken::new(),
        )
        .await
        .unwrap_or_else(|e| panic!("supervise: {e}"));
        let elapsed = started.elapsed();

        assert_eq!(result.killed, Some(KillReason::Timeout));
        assert!(
            elapsed < timeout + Duration::from_secs(2),
            "took {elapsed:?}; a process that respects SIGTERM should not wait out \
             the grace period"
        );
        assert!(
            elapsed < timeout + GRACE,
            "the grace period must be cut short as soon as the child exits, not \
             always waited out: {elapsed:?}"
        );
        // ...and it did NOT return early: a supervisor that gave up before the
        // timeout would kill engines that were still working.
        assert!(
            elapsed >= timeout,
            "returned before the timeout elapsed: {elapsed:?}"
        );
    }

    // Unix only. See this module's own docs: on Windows this hangs rather than
    // fails, which stops the whole run (REVL-106).
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn supervision_an_engine_that_ignores_sigterm_still_dies_after_the_grace_period() {
        // The pathological case, and where acceptance criterion 1's "+2s" and §8.5's
        // five-second grace pull against each other: a process that ignores SIGTERM
        // cannot be gone in under two seconds AND be given five seconds to finish.
        //
        // §8.5's grace wins, because it is the spec and because the alternative
        // costs a real review: an engine given no chance to flush result.json loses
        // work whose tokens were already spent. The "+2s" is read as supervisor
        // overhead, which the test above measures. This one bounds the pathological
        // path at timeout + grace + overhead.
        let out = TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let (invocation, env) = fixture("hang", out.path());
        let timeout = Duration::from_millis(400);

        let started = Instant::now();
        let result = supervise(
            revlocal_core::EngineKind::Mock,
            &invocation,
            &workspace_root(),
            &env,
            timeout,
            &CancellationToken::new(),
        )
        .await
        .unwrap_or_else(|e| panic!("supervise: {e}"));
        let elapsed = started.elapsed();

        assert_eq!(result.killed, Some(KillReason::Timeout));
        assert!(!result.completed());
        assert!(
            elapsed < timeout + GRACE + Duration::from_secs(2),
            "took {elapsed:?}, which is more than timeout + grace + 2s of overhead"
        );
    }

    // Unix only. See this module's own docs: on Windows this hangs rather than
    // fails, which stops the whole run (REVL-106).
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn supervision_the_grace_period_is_actually_used_before_sigkill() {
        // §8.5 wants SIGTERM, five seconds of grace, then SIGKILL. A supervisor that
        // went straight to SIGKILL would pass the timeout test above while losing a
        // review that had already been paid for — an engine given no chance to flush
        // result.json.
        //
        // The fixture ignores SIGTERM, so the full grace elapses and the total is
        // bounded below by timeout + GRACE.
        let out = TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let (invocation, env) = fixture("hang", out.path());
        let timeout = Duration::from_millis(300);

        let started = Instant::now();
        supervise(
            revlocal_core::EngineKind::Mock,
            &invocation,
            &workspace_root(),
            &env,
            timeout,
            &CancellationToken::new(),
        )
        .await
        .unwrap_or_else(|e| panic!("supervise: {e}"));
        let elapsed = started.elapsed();

        assert!(
            elapsed >= timeout + GRACE - Duration::from_millis(200),
            "SIGKILL arrived after only {elapsed:?}; the grace period was skipped"
        );
    }

    // Unix only. See this module's own docs: on Windows this hangs rather than
    // fails, which stops the whole run (REVL-106).
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn supervision_no_orphan_survives_and_nor_does_a_grandchild() {
        // Acceptance criteria 2 and 3. The grandchild is the one that matters: a
        // supervisor killing only its direct child leaves the rest holding the
        // scratch worktree open, and the next run fails for a reason nobody can
        // trace back here.
        let out = TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let (invocation, env) = fixture("hang", out.path());

        let result = supervise(
            revlocal_core::EngineKind::Mock,
            &invocation,
            &workspace_root(),
            &env,
            Duration::from_millis(400),
            &CancellationToken::new(),
        )
        .await
        .unwrap_or_else(|e| panic!("supervise: {e}"));

        assert_eq!(result.killed, Some(KillReason::Timeout));
        let child_pid = result.pid.unwrap_or_default();
        assert!(
            child_pid > 0,
            "the pid must be recorded, or nothing can be checked"
        );

        // Give the signals a moment to land.
        tokio::time::sleep(Duration::from_millis(400)).await;

        let grandchild: i32 = std::fs::read_to_string(out.path().join("grandchild.pid"))
            .unwrap_or_default()
            .trim()
            .parse()
            .unwrap_or(0);
        assert!(
            grandchild > 0,
            "the fixture did not record a grandchild pid"
        );

        #[cfg(unix)]
        {
            let child_alive = is_alive(i32::try_from(child_pid).unwrap_or(0));
            let grandchild_alive = is_alive(grandchild);

            // Clean up before asserting, so a failing run does not leak the very
            // processes it is complaining about.
            if child_alive {
                hard_kill(i32::try_from(child_pid).unwrap_or(0));
            }
            if grandchild_alive {
                hard_kill(grandchild);
            }

            assert!(!child_alive, "the engine process {child_pid} survived");
            assert!(
                !grandchild_alive,
                "grandchild {grandchild} survived; only the direct child was killed, \
                 so a hung engine leaves processes holding the worktree"
            );
        }
    }

    // --- cancellation ------------------------------------------------------------

    // Unix only. See this module's own docs: on Windows this hangs rather than
    // fails, which stops the whole run (REVL-106).
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn supervision_the_kill_switch_stops_an_engine_mid_review() {
        // §12.1: the kill switch cancels every token. This asserts it reaches a
        // process that is not going to stop on its own, and that the reason is
        // recorded as a cancellation rather than a timeout — they lead to different
        // terminal run statuses.
        let out = TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let (invocation, env) = fixture("hang", out.path());
        let cancel = CancellationToken::new();

        let handle = {
            let cancel = cancel.clone();
            let root = workspace_root();
            tokio::spawn(async move {
                supervise(
                    revlocal_core::EngineKind::Mock,
                    &invocation,
                    &root,
                    &env,
                    // Far longer than the test: if this passes, cancellation did it.
                    Duration::from_secs(300),
                    &cancel,
                )
                .await
            })
        };

        tokio::time::sleep(Duration::from_millis(400)).await;
        let started = Instant::now();
        cancel.cancel();

        let result = handle
            .await
            .unwrap_or_else(|e| panic!("task panicked: {e}"))
            .unwrap_or_else(|e| panic!("supervise: {e}"));

        assert_eq!(result.killed, Some(KillReason::Cancelled));
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "cancellation took {:?}; §12.1 wants this prompt",
            started.elapsed()
        );

        #[cfg(unix)]
        {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let grandchild: i32 = std::fs::read_to_string(out.path().join("grandchild.pid"))
                .unwrap_or_default()
                .trim()
                .parse()
                .unwrap_or(0);
            if grandchild > 0 {
                let alive = is_alive(grandchild);
                if alive {
                    hard_kill(grandchild);
                }
                assert!(
                    !alive,
                    "cancelling must take the whole tree, not just the child"
                );
            }
        }
    }

    // --- the ordinary path -------------------------------------------------------

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn supervision_a_well_behaved_engine_is_not_killed_and_its_output_is_captured() {
        // The guard that keeps the rest honest: if everything were killed, every
        // assertion above would pass and no review would ever complete.
        let out = TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let (invocation, env) = fixture("valid", out.path());

        let result = supervise(
            revlocal_core::EngineKind::Mock,
            &invocation,
            &workspace_root(),
            &env,
            Duration::from_secs(30),
            &CancellationToken::new(),
        )
        .await
        .unwrap_or_else(|e| panic!("supervise: {e}"));

        assert!(result.completed(), "a healthy engine must not be killed");
        assert_eq!(result.exit_code, Some(0));
        assert!(
            result.stdout.contains("wrote result.json"),
            "{}",
            result.stdout
        );
        assert!(out.path().join("result.json").is_file());
    }

    // Unix only. See this module's own docs: on Windows this hangs rather than
    // fails, which stops the whole run (REVL-106).
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn supervision_output_is_kept_even_from_a_killed_process() {
        // §8.2's ladder reads stdout, and a timed-out engine may still have emitted
        // a usable fenced block before it hung. Discarding output on a kill would
        // throw away a recoverable review.
        let out = TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let (invocation, env) = fixture("hang", out.path());

        let result = supervise(
            revlocal_core::EngineKind::Mock,
            &invocation,
            &workspace_root(),
            &env,
            Duration::from_millis(500),
            &CancellationToken::new(),
        )
        .await
        .unwrap_or_else(|e| panic!("supervise: {e}"));

        assert_eq!(result.killed, Some(KillReason::Timeout));
        assert!(
            result.stdout.contains("hanging"),
            "what the engine said before it hung must survive: {:?}",
            result.stdout
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn supervision_a_missing_binary_is_named_rather_than_failing_obscurely() {
        let invocation = Invocation {
            program: "definitely-not-a-real-engine".to_owned(),
            args: Vec::new(),
            stdin: None,
        };
        let error = supervise(
            revlocal_core::EngineKind::Claude,
            &invocation,
            &workspace_root(),
            &BTreeMap::new(),
            Duration::from_secs(5),
            &CancellationToken::new(),
        )
        .await
        .expect_err("there is no such program");

        assert_eq!(error.code(), "engine_not_installed");
        assert!(error.to_string().contains("try:"), "{error}");
    }

    // §8.5's environment denylist lives in tests/env_denylist.rs, so that RL-406's
    // gate (`cargo test -p revlocal-engine env_denylist`) actually selects it. A
    // filter matching nothing exits 0, which is the quietest way for a gate to pass
    // while testing nothing.
}

/// No test that kills a process may run on a current-thread runtime (RL-1303).
///
/// A plain `#[tokio::test]` is single-threaded. That is the right default for most
/// async tests and the wrong one for every test in these files: a blocked drain
/// starves the only worker, the timer never fires, and the test hangs instead of
/// failing. A hung test binary stops every binary queued behind it, so one
/// regression becomes a run that reports nothing at all.
///
/// The rule is per **file**, not per test, and deliberately so. Some tests in
/// these files only touch the store and could never block — but "does this
/// particular test kill something" is a judgement that has to be made again on
/// every addition, and the cost of getting it wrong is a silent hang. Two worker
/// threads for a store test is not a cost worth that.
///
/// Checked as text rather than by reflection, because the attribute is the thing
/// that matters and there is no way to ask the runtime which flavour it is from
/// inside a test that has already started.
#[test]
fn every_process_killing_test_can_report_a_hang() -> Result<(), String> {
    let root = {
        let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.pop();
        path.pop();
        path
    };

    // Both files that drive real processes and kill them.
    let files = [
        "crates/revlocal-engine/tests/process_supervision.rs",
        "crates/revlocal-daemon/tests/kill_switch.rs",
    ];

    let mut bare = Vec::new();
    let mut checked = 0usize;
    for file in files {
        let path = root.join(file);
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;

        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.starts_with("#[tokio::test") {
                checked += 1;
                // A bare `#[tokio::test]` — no flavour named at all.
                if line == "#[tokio::test]" {
                    bare.push(format!("{file}:{}", n + 1));
                }
            }
        }
    }

    assert!(
        checked > 10,
        "only {checked} async tests found across {} files; the scan has stopped \
         matching and would pass for the wrong reason",
        files.len()
    );
    assert!(
        bare.is_empty(),
        "these tests kill processes on a single-threaded runtime, so a regression \
         hangs the run instead of failing it: {bare:?}\n  \
         use #[tokio::test(flavor = \"multi_thread\", worker_threads = 2)]"
    );
    Ok(())
}
