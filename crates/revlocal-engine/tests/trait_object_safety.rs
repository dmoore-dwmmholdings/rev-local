//! Acceptance tests for `RL-401` — the `Engine` trait (SPEC §8.1).
//!
//! Two things are being asserted, and the second is easy to lose without noticing.
//!
//! The trait must stay **object-safe**, because the daemon holds a
//! `Box<dyn Engine>` chosen per repository (decision D3: engine selection is
//! per-repo). One generic method would break that, and the failure would appear in
//! the daemon rather than here — far from the change that caused it.
//!
//! And the mock must be genuinely **usable from tests**, which means more than
//! compiling: a pipeline test needs to see what task it was given, how many times it
//! ran, and to make it fail or hang on demand.

mod trait_ {
    use revlocal_core::{Depth, EngineKind, Verdict};
    use revlocal_engine::engine::{Engine, EngineError, EngineProbe, EngineTask};
    use revlocal_engine::{MockBehaviour, MockEngine};
    use std::path::PathBuf;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    fn a_task() -> EngineTask {
        EngineTask {
            cwd: PathBuf::from("/scratch/1/worktree"),
            out_dir: PathBuf::from("/scratch/1/out"),
            prompt: "Review this change.".to_owned(),
            attachments: vec![PathBuf::from("/scratch/1/out/diff.patch")],
            timeout: Duration::from_secs(600),
            depth: Depth::Standard,
        }
    }

    // --- object safety --------------------------------------------------------

    #[tokio::test]
    async fn trait_is_object_safe_so_the_daemon_can_hold_one_per_repo() {
        // Acceptance criterion 2. D3 makes the engine a per-repository setting, so
        // the daemon holds `Box<dyn Engine>`; a generic method on the trait would
        // break this and the error would surface in the daemon, not here.
        let engine: Box<dyn Engine> = Box::new(MockEngine::new());

        assert_eq!(engine.id(), EngineKind::Mock);
        let probe = engine
            .probe()
            .await
            .unwrap_or_else(|e| panic!("probe: {e}"));
        assert!(probe.is_usable());

        let outcome = engine
            .run(a_task(), CancellationToken::new())
            .await
            .unwrap_or_else(|e| panic!("run: {e}"));
        assert_eq!(outcome.verdict, Verdict::RequestChanges);
    }

    #[tokio::test]
    async fn trait_a_collection_of_engines_can_be_held_together() {
        // What the daemon actually does: several repos, several engines, one map.
        let engines: Vec<Box<dyn Engine>> = vec![
            Box::new(MockEngine::new()),
            Box::new(MockEngine::with_behaviour(MockBehaviour::Fail(
                EngineError::NotInstalled {
                    id: EngineKind::Codex,
                    remediation: "install the Codex CLI".to_owned(),
                },
            ))),
        ];

        assert_eq!(engines.len(), 2);
        assert!(engines.iter().all(|e| e.id() == EngineKind::Mock));
    }

    #[test]
    fn trait_is_send_and_sync_so_it_can_cross_a_task_boundary() {
        // The run queue spawns engines onto a tokio task (§4.3). Losing Send here
        // fails at the spawn site with an error that names a lifetime rather than
        // the trait.
        fn assert_send_sync<T: Send + Sync + ?Sized>() {}
        assert_send_sync::<dyn Engine>();
    }

    // --- the mock is usable ---------------------------------------------------

    #[tokio::test]
    async fn trait_the_mock_records_the_task_it_was_given() {
        // Acceptance criterion 1, read strictly: "usable from tests" means a
        // pipeline test can assert on what the pipeline BUILT, not only on what came
        // back. A prompt that forgot its diff produces a perfectly valid outcome.
        let engine = MockEngine::new();
        engine
            .run(a_task(), CancellationToken::new())
            .await
            .unwrap_or_else(|e| panic!("run: {e}"));

        let seen = engine
            .seen_tasks
            .lock()
            .map(|t| t.clone())
            .unwrap_or_default();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].depth, Depth::Standard);
        assert_eq!(
            seen[0].attachments.len(),
            1,
            "the diff attachment must reach the engine"
        );
        assert_eq!(engine.run_count(), 1);
    }

    #[tokio::test]
    async fn trait_the_mock_counts_runs_so_a_double_review_is_visible() {
        // A pipeline that reviewed one change twice, or skipped one it should have
        // reviewed, shows up here and nowhere else.
        let engine = MockEngine::new();
        for _ in 0..3 {
            engine
                .run(a_task(), CancellationToken::new())
                .await
                .unwrap_or_else(|e| panic!("run: {e}"));
        }
        assert_eq!(engine.run_count(), 3);
    }

    #[tokio::test]
    async fn trait_the_mock_can_be_told_to_fail() {
        let engine =
            MockEngine::with_behaviour(MockBehaviour::Fail(EngineError::OutputUnparseable {
                id: EngineKind::Mock,
            }));
        let error = engine
            .run(a_task(), CancellationToken::new())
            .await
            .expect_err("it was told to fail");
        assert_eq!(error.code(), "engine_output_unparseable");
    }

    #[tokio::test]
    async fn trait_the_mock_observes_cancellation_rather_than_finishing() {
        // §12.1: the kill switch cancels every token. A mock that ignored the token
        // would let a kill-switch test pass because the run happened to be fast.
        let engine = MockEngine::with_behaviour(MockBehaviour::BlockUntilCancelled);
        let cancel = CancellationToken::new();

        let handle = {
            let cancel = cancel.clone();
            tokio::spawn(async move { engine.run(a_task(), cancel).await })
        };

        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();

        let result = handle
            .await
            .unwrap_or_else(|e| panic!("task panicked: {e}"));
        let error = result.expect_err("cancelling must not produce an outcome");
        assert!(error.is_cancellation());
        assert_eq!(error.code(), "engine_cancelled");
    }

    #[tokio::test]
    async fn trait_an_already_cancelled_token_stops_even_a_successful_run() {
        let engine = MockEngine::new();
        let cancel = CancellationToken::new();
        cancel.cancel();

        let error = engine
            .run(a_task(), cancel)
            .await
            .expect_err("a cancelled run must not return an outcome");
        assert!(error.is_cancellation());
    }

    // --- the task validates itself --------------------------------------------

    #[tokio::test]
    async fn trait_a_task_whose_out_dir_is_the_worktree_is_refused() {
        // §8.5 makes out_dir the ONLY writable path. If it were the worktree, the
        // engine could edit the code it is reviewing — and no assertion on the
        // resulting findings would notice.
        let mut task = a_task();
        task.out_dir = task.cwd.clone();

        let error = MockEngine::new()
            .run(task, CancellationToken::new())
            .await
            .expect_err("this must be refused");
        assert_eq!(error.code(), "engine_invalid_task");
        assert!(error.to_string().contains("writable"), "{error}");
    }

    #[tokio::test]
    async fn trait_an_empty_prompt_is_refused_rather_than_reviewing_nothing() {
        let mut task = a_task();
        task.prompt = "   \n".to_owned();

        let error = MockEngine::new()
            .run(task, CancellationToken::new())
            .await
            .expect_err("an empty prompt must be refused");
        assert_eq!(error.code(), "engine_invalid_task");
    }

    #[tokio::test]
    async fn trait_a_zero_timeout_is_refused() {
        let mut task = a_task();
        task.timeout = Duration::ZERO;
        assert!(MockEngine::new()
            .run(task, CancellationToken::new())
            .await
            .is_err());
    }

    // --- the surrounding types ------------------------------------------------

    #[test]
    fn trait_error_codes_are_stable_because_they_are_stored() {
        // These land in `run.error` and the UI groups by them, so renaming one
        // silently changes stored data.
        assert_eq!(
            EngineError::Timeout {
                id: EngineKind::Mock,
                timeout: Duration::from_secs(1)
            }
            .code(),
            "engine_timeout"
        );
        assert_eq!(
            EngineError::OutputUnparseable {
                id: EngineKind::Mock
            }
            .code(),
            "engine_output_unparseable",
            "SPEC §8.2 names this one specifically"
        );
    }

    #[test]
    fn trait_cancellation_is_not_a_failure() {
        // They lead to different terminal run statuses. Treating a kill-switch
        // cancellation as a failure would fill the UI with errors when a user
        // deliberately stopped everything.
        assert!(EngineError::Cancelled {
            id: EngineKind::Mock
        }
        .is_cancellation());
        assert!(!EngineError::Timeout {
            id: EngineKind::Mock,
            timeout: Duration::from_secs(1)
        }
        .is_cancellation());
    }

    #[test]
    fn trait_a_probe_distinguishes_installed_from_usable() {
        // An engine can be installed and authenticated and still not honour §8.2's
        // output contract — a CLI whose flags changed. That is the failure `doctor`
        // exists to catch before a real review spends tokens discovering it.
        let mut probe = EngineProbe {
            id: EngineKind::Claude,
            installed: true,
            version: Some("1.0".to_owned()),
            authenticated: true,
            honours_output_contract: Some(false),
            problems: Vec::new(),
        };
        assert!(
            !probe.is_usable(),
            "installed and authenticated is not enough"
        );

        probe.honours_output_contract = Some(true);
        assert!(probe.is_usable());

        // Not yet smoke-tested is not the same as failed.
        probe.honours_output_contract = None;
        assert!(probe.is_usable());
    }

    #[test]
    fn trait_a_missing_engine_probe_carries_remediation() {
        let probe = EngineProbe::missing(EngineKind::Codex, "install the Codex CLI");
        assert!(!probe.is_usable());
        assert_eq!(probe.problems.len(), 1);
        assert!(probe.problems[0].remediation.contains("install"));
        assert!(
            probe.problems[0].problem.contains("codex"),
            "the problem must name the engine: {}",
            probe.problems[0].problem
        );
    }

    #[test]
    fn trait_the_mock_reports_no_cost_exactly_like_the_fixture_engine() {
        // ADR 0010's consequence, kept true in both mocks: every inner-loop run
        // produces a cost-incomplete budget day by design, and a mock that invented
        // a cost would hide that from the pipeline tests.
        let outcome = MockEngine::default_outcome_for_test();
        assert_eq!(outcome.usage.cost_usd, None);
        assert!(!outcome.usage.cost_is_complete());
        assert!(outcome.usage.total_tokens() > 0, "tokens are always known");
    }
}
