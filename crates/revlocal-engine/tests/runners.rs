//! Acceptance tests for `RL-407` — the Claude Code and Codex runners (SPEC §8.4).
//!
//! Neither CLI can be driven here: `codex` is not installed and `claude` is not
//! authenticated for an unattended review. So these tests do two things instead of
//! pretending otherwise.
//!
//! They assert the parts that are **machine-independent**: that both engines are
//! constructible and selectable per repository, that a missing binary is a *report*
//! rather than a panic or an error, and that a withheld credential is explained.
//!
//! And they drive the whole runner end to end against the **fixture engine**, which
//! is a real subprocess honouring §8.2's contract. That covers template rendering,
//! process supervision, the fallback ladder and transcript capture in one path —
//! everything except the specific CLI's own flags.

mod runners {
    use revlocal_core::{Depth, EngineKind, Verdict};
    use revlocal_engine::engine::{Engine, EngineTask};
    use revlocal_engine::template::InvocationTemplate;
    use revlocal_engine::CliEngine;
    use std::collections::BTreeMap;
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    /// A runner pointed at the fixture engine, in `mode`.
    ///
    /// The fixture takes its behaviour from the environment rather than argv, so the
    /// template passes the prompt and nothing else.
    fn fixture_engine(mode: &str) -> CliEngine {
        let template = InvocationTemplate {
            bin: revlocal_engine::mock_engine_program().display().to_string(),
            args: vec!["--prompt".to_owned(), "{prompt_file_content}".to_owned()],
            version_args: vec!["--version".to_owned()],
            stdin_prompt: false,
            pass_env: vec!["MOCK_ENGINE_MODE".to_owned(), "REVLOCAL_OUT".to_owned()],
        };

        CliEngine::new(EngineKind::Mock, template).with_parent_env(BTreeMap::from([
            ("PATH".to_owned(), std::env::var("PATH").unwrap_or_default()),
            ("HOME".to_owned(), std::env::var("HOME").unwrap_or_default()),
            ("MOCK_ENGINE_MODE".to_owned(), mode.to_owned()),
        ]))
    }

    fn task(cwd: &std::path::Path, out: &std::path::Path) -> EngineTask {
        EngineTask {
            cwd: cwd.to_path_buf(),
            out_dir: out.to_path_buf(),
            prompt: "Review this change.".to_owned(),
            attachments: Vec::new(),
            timeout: Duration::from_secs(30),
            depth: Depth::Standard,
        }
    }

    // --- both engines are selectable per repo ---------------------------------

    #[test]
    fn runners_both_engines_are_constructible_and_selectable_per_repo() {
        // Acceptance criterion 3. D3 makes engine selection per-repository, so the
        // daemon holds them behind a trait object and picks one per repo.
        let engines: Vec<Box<dyn Engine>> =
            vec![Box::new(CliEngine::claude()), Box::new(CliEngine::codex())];

        let ids: Vec<EngineKind> = engines.iter().map(|e| e.id()).collect();
        assert_eq!(ids, [EngineKind::Claude, EngineKind::Codex]);
    }

    #[test]
    fn runners_each_engine_ships_spec_8_4s_template() {
        assert_eq!(CliEngine::claude().template().bin, "claude");
        assert_eq!(CliEngine::codex().template().bin, "codex");
        // And both must be usable as shipped — a default failing its own validator
        // would break every fresh install and blame the user's config.
        for engine in [CliEngine::claude(), CliEngine::codex()] {
            engine
                .template()
                .validate(engine.id().as_str())
                .unwrap_or_else(|e| panic!("{}: {e}", engine.id()));
        }
    }

    // --- probe ------------------------------------------------------------------

    #[tokio::test]
    async fn runners_a_missing_binary_is_reported_not_raised() {
        // Acceptance criterion 2. `doctor` shows every engine's state at once;
        // returning an error here would stop at the first missing one and the user
        // would fix them one release at a time.
        let template = InvocationTemplate {
            bin: "definitely-not-a-real-engine".to_owned(),
            args: vec!["{prompt_file_content}".to_owned()],
            ..InvocationTemplate::default()
        };
        let engine = CliEngine::new(EngineKind::Codex, template).with_parent_env(BTreeMap::new());

        let probe = engine
            .probe()
            .await
            .unwrap_or_else(|e| panic!("a missing binary must not be an error: {e}"));

        assert!(!probe.installed);
        assert!(!probe.is_usable());
        assert_eq!(probe.version, None);
        assert!(!probe.problems.is_empty(), "and it must say what is wrong");
        assert!(
            probe.problems.iter().all(|p| !p.remediation.is_empty()),
            "every problem carries a remedy (SPEC §18)"
        );
    }

    #[tokio::test]
    async fn runners_probe_reports_the_version_of_a_present_binary() {
        // Acceptance criterion 1, for the halves that can be observed here. The
        // fixture answers --version like a real CLI.
        let engine = fixture_engine("valid");
        let probe = engine.probe().await.unwrap_or_else(|e| panic!("{e}"));

        assert!(probe.installed);
        assert!(
            probe
                .version
                .as_deref()
                .unwrap_or_default()
                .contains("mock-engine"),
            "got {:?}",
            probe.version
        );
    }

    #[tokio::test]
    async fn runners_probe_does_not_run_a_smoke_task_and_says_so() {
        // probe() is called at startup for every engine. An engine probe that
        // quietly billed someone for starting the app would be a genuinely bad
        // surprise, so §8.4's smoke task is a separate call.
        //
        // `honours_output_contract: None` means "not smoke-tested", which is not the
        // same as "failed" — the type distinguishes them for this reason.
        let out = TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let engine = fixture_engine("valid");

        let probe = engine.probe().await.unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(probe.honours_output_contract, None);
        assert!(
            !out.path().join("result.json").exists(),
            "probing must not have produced a review"
        );
        assert!(
            probe.is_usable(),
            "not-yet-smoke-tested must not read as broken"
        );
    }

    #[tokio::test]
    async fn runners_a_withheld_credential_is_explained_on_the_probe() {
        // The likeliest reason a present, working CLI reports itself unauthenticated
        // — and the least guessable, because nothing on screen connects the two.
        let engine = fixture_engine("valid").with_parent_env(BTreeMap::from([
            ("PATH".to_owned(), std::env::var("PATH").unwrap_or_default()),
            (
                "ANTHROPIC_API_KEY".to_owned(),
                "sk-ant-set-by-the-user".to_owned(),
            ),
        ]));

        let probe = engine.probe().await.unwrap_or_else(|e| panic!("{e}"));
        let explained = probe
            .problems
            .iter()
            .find(|p| p.problem.contains("ANTHROPIC_API_KEY"))
            .unwrap_or_else(|| panic!("the withheld credential was not mentioned: {probe:?}"));

        assert!(
            explained.remediation.contains("pass_env"),
            "{}",
            explained.remediation
        );
    }

    // --- the whole runner, end to end -------------------------------------------

    #[tokio::test]
    async fn runners_a_full_run_renders_supervises_and_climbs_the_ladder() {
        // Everything except a specific CLI's flags, in one path.
        let cwd = TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let out = TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let engine = fixture_engine("valid").with_parent_env(BTreeMap::from([
            ("PATH".to_owned(), std::env::var("PATH").unwrap_or_default()),
            ("HOME".to_owned(), std::env::var("HOME").unwrap_or_default()),
            ("MOCK_ENGINE_MODE".to_owned(), "valid".to_owned()),
            ("REVLOCAL_OUT".to_owned(), out.path().display().to_string()),
        ]));

        let outcome = engine
            .run(task(cwd.path(), out.path()), CancellationToken::new())
            .await
            .unwrap_or_else(|e| panic!("run: {e}"));

        assert_eq!(outcome.verdict, Verdict::RequestChanges);
        assert_eq!(outcome.findings.len(), 2);
        assert!(!outcome.is_degraded(), "the fixture honoured the contract");
        assert!(
            !outcome.transcript.is_empty(),
            "the transcript must be captured; it is the archive (§5.1)"
        );
    }

    #[tokio::test]
    async fn runners_the_prompt_is_written_to_disk_so_a_run_can_be_reproduced() {
        // §8.4's {prompt_file} must resolve to something real whichever form the
        // template uses, and a copy on disk is what makes a failed run reproducible
        // by hand — which is the first thing anyone debugging one wants.
        let cwd = TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let out = TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let engine = fixture_engine("valid").with_parent_env(BTreeMap::from([
            ("PATH".to_owned(), std::env::var("PATH").unwrap_or_default()),
            ("HOME".to_owned(), std::env::var("HOME").unwrap_or_default()),
            ("MOCK_ENGINE_MODE".to_owned(), "valid".to_owned()),
            ("REVLOCAL_OUT".to_owned(), out.path().display().to_string()),
        ]));

        engine
            .run(task(cwd.path(), out.path()), CancellationToken::new())
            .await
            .unwrap_or_else(|e| panic!("run: {e}"));

        let written = std::fs::read_to_string(out.path().join(revlocal_engine::PROMPT_FILE))
            .unwrap_or_else(|e| panic!("the prompt was not written: {e}"));
        assert_eq!(written, "Review this change.");
    }

    #[tokio::test]
    async fn runners_a_degraded_run_reports_which_rung_salvaged_it() {
        let cwd = TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let out = TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let engine = fixture_engine("malformed_json").with_parent_env(BTreeMap::from([
            ("PATH".to_owned(), std::env::var("PATH").unwrap_or_default()),
            ("HOME".to_owned(), std::env::var("HOME").unwrap_or_default()),
            ("MOCK_ENGINE_MODE".to_owned(), "malformed_json".to_owned()),
            ("REVLOCAL_OUT".to_owned(), out.path().display().to_string()),
        ]));

        let outcome = engine
            .run(task(cwd.path(), out.path()), CancellationToken::new())
            .await
            .unwrap_or_else(|e| panic!("run: {e}"));

        assert!(
            outcome.is_degraded(),
            "a salvaged run must say so; §12.3 escalates on it"
        );
        assert!(outcome.findings.len() == 2, "and the review still arrived");
    }

    // Unix only. These are the two tests in this file that kill a hang-mode
    // engine, and on Windows killing `run.cmd` terminates cmd.exe while the `node`
    // grandchild survives holding the pipes — so they hang rather than fail, which
    // stops the whole run. §8.5's Job Object is unimplemented; REVL-106 tracks it,
    // and both gates come back with it.
    //
    // The fourth file to need this. Every test that kills a hang-mode engine hangs
    // on Windows and no other test does, which is what makes the mechanism — not
    // the file — the thing being gated.
    #[cfg(unix)]
    #[tokio::test]
    async fn runners_a_cancelled_run_is_a_cancellation_not_a_failure() {
        // They lead to different terminal run statuses. Reporting a deliberate stop
        // as a failure would fill the UI with errors when a user pulled the switch.
        let cwd = TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let out = TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let engine = fixture_engine("hang").with_parent_env(BTreeMap::from([
            ("PATH".to_owned(), std::env::var("PATH").unwrap_or_default()),
            ("HOME".to_owned(), std::env::var("HOME").unwrap_or_default()),
            ("MOCK_ENGINE_MODE".to_owned(), "hang".to_owned()),
            ("REVLOCAL_OUT".to_owned(), out.path().display().to_string()),
        ]));

        let cancel = CancellationToken::new();
        let mut spec = task(cwd.path(), out.path());
        spec.timeout = Duration::from_secs(300);

        let handle = {
            let cancel = cancel.clone();
            tokio::spawn(async move { engine.run(spec, cancel).await })
        };
        tokio::time::sleep(Duration::from_millis(400)).await;
        cancel.cancel();

        let error = handle
            .await
            .unwrap_or_else(|e| panic!("task panicked: {e}"))
            .expect_err("a cancelled run produces no outcome");
        assert!(error.is_cancellation());
        assert_eq!(error.code(), "engine_cancelled");
    }

    // Unix only. These are the two tests in this file that kill a hang-mode
    // engine, and on Windows killing `run.cmd` terminates cmd.exe while the `node`
    // grandchild survives holding the pipes — so they hang rather than fail, which
    // stops the whole run. §8.5's Job Object is unimplemented; REVL-106 tracks it,
    // and both gates come back with it.
    //
    // The fourth file to need this. Every test that kills a hang-mode engine hangs
    // on Windows and no other test does, which is what makes the mechanism — not
    // the file — the thing being gated.
    #[cfg(unix)]
    #[tokio::test]
    async fn runners_a_timeout_is_reported_as_a_timeout_and_keeps_nothing_fabricated() {
        let cwd = TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let out = TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let engine = fixture_engine("hang").with_parent_env(BTreeMap::from([
            ("PATH".to_owned(), std::env::var("PATH").unwrap_or_default()),
            ("HOME".to_owned(), std::env::var("HOME").unwrap_or_default()),
            ("MOCK_ENGINE_MODE".to_owned(), "hang".to_owned()),
            ("REVLOCAL_OUT".to_owned(), out.path().display().to_string()),
        ]));

        let mut spec = task(cwd.path(), out.path());
        spec.timeout = Duration::from_millis(400);

        let error = engine
            .run(spec, CancellationToken::new())
            .await
            .expect_err("a hung engine must not produce a review");
        assert_eq!(error.code(), "engine_timeout");
    }

    #[tokio::test]
    async fn runners_an_unrunnable_task_is_refused_before_anything_is_spawned() {
        let cwd = TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let engine = fixture_engine("valid");

        // out_dir == cwd would make the worktree writable (§8.5).
        let mut spec = task(cwd.path(), cwd.path());
        spec.prompt = "Review this.".to_owned();

        let error = engine
            .run(spec, CancellationToken::new())
            .await
            .expect_err("this must be refused");
        assert_eq!(error.code(), "engine_invalid_task");
    }
}
