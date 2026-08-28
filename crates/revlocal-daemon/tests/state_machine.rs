//! Acceptance tests for `RL-501` — run stage transitions and crash recovery.
//!
//! The scenario throughout is a daemon that died. Not a clean shutdown — a laptop
//! that slept, an OOM kill, a `kill -9`. Whatever run was mid-flight is left in a
//! stage nothing will ever move it out of, because the thing that would have moved
//! it is gone.
//!
//! Recovery has to be **idempotent** and it has to **stop**. A recovery that ran
//! twice would double-enqueue; one that never gave up would spend rev-local's life
//! re-reviewing whichever commit crashes the daemon.

mod state_machine {
    use chrono::{Duration as ChronoDuration, TimeZone, Utc};
    use revlocal_core::{
        AutonomyMode, Change, ChangeId, ChangeKind, Depth, DiffStat, EngineKind, Repo, RepoId,
        RepoKind, Run, RunId, RunStatus, Timestamp, TriggerSource, Usage,
    };
    use revlocal_daemon::state_machine::{
        recover_interrupted, transition, RecoveryReport, RunEvent, RunEventSink,
        DEFAULT_MAX_ATTEMPTS, INTERRUPTED,
    };
    use revlocal_store::{Pool, RunStore};
    use std::sync::Mutex;
    use tempfile::TempDir;

    /// `minutes` after a fixed base instant.
    ///
    /// Added as a duration rather than written into the minute field: `at(60)` would
    /// otherwise be an invalid time, `single()` would return `None`, and
    /// `unwrap_or_default()` would silently hand back the Unix epoch — so a test
    /// advancing past an hour would quietly travel back to 1970 and find nothing
    /// stale. That is exactly what happened while writing these.
    fn at(minutes: i64) -> Timestamp {
        let base = Utc
            .with_ymd_and_hms(2026, 8, 27, 12, 0, 0)
            .single()
            .unwrap_or_default();
        base + ChronoDuration::minutes(minutes)
    }

    /// Collects events so a test can assert what the UI would have been told.
    #[derive(Default)]
    struct Collector(Mutex<Vec<RunEvent>>);

    impl Collector {
        fn events(&self) -> Vec<RunEvent> {
            self.0.lock().map(|e| e.clone()).unwrap_or_default()
        }
    }

    impl RunEventSink for Collector {
        fn emit(&self, event: RunEvent) {
            if let Ok(mut events) = self.0.lock() {
                events.push(event);
            }
        }
    }

    /// A database with a repo and a change to hang runs off.
    ///
    /// Returns `Result`; helpers are not `#[test]` fns (ADR 0003).
    async fn seeded() -> Result<(TempDir, Pool, ChangeId), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let pool = revlocal_store::open(&dir.path().join("rev-local.db")).await?;

        let repo = revlocal_store::RepoStore::new(&pool)
            .insert(&Repo {
                id: RepoId::new(0),
                name: "rev-local".to_owned(),
                kind: RepoKind::Git,
                local_path: None,
                remote_url: None,
                default_branch: Some("main".to_owned()),
                engine: EngineKind::Mock,
                autonomy: AutonomyMode::DryRun,
                enabled: true,
                config_json: "{}".to_owned(),
                created_at: at(0),
                updated_at: at(0),
            })
            .await?;

        let change = revlocal_store::ChangeStore::new(&pool)
            .upsert(&Change {
                id: ChangeId::new(0),
                repo_id: repo.id,
                kind: ChangeKind::Commit,
                external_id: "deadbeef".to_owned(),
                title: None,
                author_name: None,
                author_email: None,
                authored_at: None,
                branch: None,
                base_ref: None,
                head_ref: None,
                url: None,
                diff_stat: DiffStat::default(),
                detected_at: at(0),
            })
            .await?;

        Ok((dir, pool, change.id))
    }

    fn a_run(change: ChangeId, attempt: u32, status: RunStatus, created: Timestamp) -> Run {
        Run {
            id: RunId::new(0),
            change_id: change,
            attempt,
            status,
            engine: EngineKind::Mock,
            depth: Depth::Standard,
            trigger: TriggerSource::Poll,
            skip_reason: None,
            error: None,
            degraded: None,
            usage: Usage::default(),
            started_at: None,
            finished_at: None,
            transcript_path: None,
            truncated: false,
            omitted_files: Vec::new(),
            verdict: None,
            summary: None,
            created_at: created,
        }
    }

    async fn recover(
        pool: &Pool,
        sink: &Collector,
        now: Timestamp,
    ) -> Result<RecoveryReport, String> {
        recover_interrupted(
            pool,
            sink,
            now,
            ChronoDuration::minutes(10),
            DEFAULT_MAX_ATTEMPTS,
        )
        .await
        .map_err(|e| format!("recover: {e}"))
    }

    // --- transitions ------------------------------------------------------------

    #[tokio::test]
    async fn state_machine_illegal_transitions_are_rejected() {
        // Acceptance criterion 1. The lifecycle lives in revlocal-core and the store
        // enforces it; this asserts the daemon's own path honours it too, rather than
        // reaching around to an UPDATE.
        let (_dir, pool, change) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let sink = Collector::default();
        let run = RunStore::new(&pool)
            .insert(&a_run(change, 1, RunStatus::Queued, at(0)))
            .await
            .unwrap_or_else(|e| panic!("insert: {e}"));

        let error = transition(&pool, &sink, run.id, RunStatus::Queued, RunStatus::Done)
            .await
            .expect_err("queued -> done skips the entire pipeline");
        assert!(error.to_string().contains("queued"), "{error}");

        assert!(
            sink.events().is_empty(),
            "a refused transition must not announce itself; the UI would show a \
             stage the database does not have"
        );
    }

    #[tokio::test]
    async fn state_machine_a_legal_transition_is_persisted_before_it_is_announced() {
        // Order matters. Announcing first and persisting second would let a crash
        // between them leave a run that looks `reviewing` forever with nothing
        // behind it. This way a crash loses an event — the UI is stale until it
        // refreshes — but never lies.
        let (_dir, pool, change) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let sink = Collector::default();
        let run = RunStore::new(&pool)
            .insert(&a_run(change, 1, RunStatus::Queued, at(0)))
            .await
            .unwrap_or_else(|e| panic!("insert: {e}"));

        transition(
            &pool,
            &sink,
            run.id,
            RunStatus::Queued,
            RunStatus::Preparing,
        )
        .await
        .unwrap_or_else(|e| panic!("transition: {e}"));

        assert_eq!(
            RunStore::new(&pool)
                .get(run.id)
                .await
                .unwrap_or_else(|e| panic!("get: {e}"))
                .status,
            RunStatus::Preparing
        );
        assert_eq!(
            sink.events(),
            vec![RunEvent::StageChanged {
                run: run.id,
                from: RunStatus::Queued,
                to: RunStatus::Preparing,
            }]
        );
    }

    // --- crash recovery -----------------------------------------------------------

    #[tokio::test]
    async fn state_machine_a_crash_mid_reviewing_recovers_on_the_next_startup() {
        // Acceptance criterion 2. The run is left in `reviewing` with nothing running
        // — exactly what a `kill -9` during a review leaves behind.
        let (_dir, pool, change) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let store = RunStore::new(&pool);
        let sink = Collector::default();

        let crashed = store
            .insert(&a_run(change, 1, RunStatus::Reviewing, at(0)))
            .await
            .unwrap_or_else(|e| panic!("insert: {e}"));

        // Startup, twenty minutes later.
        let report = recover(&pool, &sink, at(20))
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(report.interrupted, [crashed.id]);
        assert_eq!(report.re_enqueued.len(), 1);

        let failed = store
            .get(crashed.id)
            .await
            .unwrap_or_else(|e| panic!("get: {e}"));
        assert_eq!(failed.status, RunStatus::Failed);
        assert_eq!(
            failed.error.as_deref(),
            Some(INTERRUPTED),
            "an interrupted run must be distinguishable from one that failed on its \
             own merits"
        );

        let successor = store
            .get(report.re_enqueued[0])
            .await
            .unwrap_or_else(|e| panic!("get: {e}"));
        assert_eq!(successor.attempt, 2);
        assert_eq!(successor.status, RunStatus::Queued);

        // The stage it was stuck in is the most useful thing to report: it says
        // where the daemon died.
        assert!(
            sink.events().iter().any(|e| matches!(
                e,
                RunEvent::Interrupted {
                    stuck_in: RunStatus::Reviewing,
                    ..
                }
            )),
            "{:?}",
            sink.events()
        );
    }

    #[tokio::test]
    async fn state_machine_recovery_re_enqueues_once_not_in_a_loop() {
        // Acceptance criterion 3. Running recovery twice — a daemon that starts,
        // crashes again before doing anything, and starts once more — must not
        // produce two successors.
        let (_dir, pool, change) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let store = RunStore::new(&pool);
        let sink = Collector::default();

        store
            .insert(&a_run(change, 1, RunStatus::Reviewing, at(0)))
            .await
            .unwrap_or_else(|e| panic!("insert: {e}"));

        let first = recover(&pool, &sink, at(20))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let second = recover(&pool, &sink, at(21))
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(first.re_enqueued.len(), 1);
        assert!(
            second.is_empty(),
            "the second pass found nothing, because the first left the run terminal: \
             {second:?}"
        );

        let runs = store
            .list_for_change(change)
            .await
            .unwrap_or_else(|e| panic!("list: {e}"));
        assert_eq!(runs.len(), 2, "one crashed run and one retry, not three");
    }

    #[tokio::test]
    async fn state_machine_a_change_that_keeps_crashing_is_eventually_given_up_on() {
        // The poison pill. Without a ceiling, every startup recovers it, every
        // recovery crashes, and rev-local spends its life re-reviewing one commit
        // and never reaches the rest.
        let (_dir, pool, change) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let store = RunStore::new(&pool);
        let sink = Collector::default();

        // The daemon dies on each attempt in turn. Recovery creates the successor
        // itself, so this only has to keep advancing the clock: each new attempt is
        // queued and then, twenty minutes later, is as abandoned as the last one.
        store
            .insert(&a_run(change, 1, RunStatus::Reviewing, at(0)))
            .await
            .unwrap_or_else(|e| panic!("insert: {e}"));

        for minute in [20, 40, 60, 80] {
            recover(&pool, &sink, at(minute))
                .await
                .unwrap_or_else(|e| panic!("recover at {minute}: {e}"));
        }

        let runs = store
            .list_for_change(change)
            .await
            .unwrap_or_else(|e| panic!("list: {e}"));
        assert_eq!(
            runs.len(),
            usize::try_from(DEFAULT_MAX_ATTEMPTS).unwrap_or(3),
            "recovery must stop at {DEFAULT_MAX_ATTEMPTS} attempts, not keep going: \
             {} runs",
            runs.len()
        );
        assert!(
            runs.iter().all(|r| r.status == RunStatus::Failed),
            "every attempt ended interrupted; got {:?}",
            runs.iter()
                .map(|r| (r.attempt, r.status, r.error.clone()))
                .collect::<Vec<_>>()
        );

        let given_up: Vec<RunEvent> = sink
            .events()
            .into_iter()
            .filter(|e| matches!(e, RunEvent::GivenUp { .. }))
            .collect();

        assert!(!given_up.is_empty(), "giving up must be announced");
        match &given_up[0] {
            RunEvent::GivenUp { reason, .. } => {
                assert!(
                    reason.contains("not retrying"),
                    "SPEC §18: giving up is a decision and must say so: {reason}"
                );
            }
            other => panic!("wrong event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn state_machine_a_run_that_is_merely_slow_is_not_interrupted() {
        // The guard that keeps the rest honest. If recovery took every non-terminal
        // run, it would kill the review that was running when the daemon started —
        // and there is always one.
        let (_dir, pool, change) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let store = RunStore::new(&pool);
        let sink = Collector::default();

        let fresh = store
            .insert(&a_run(change, 1, RunStatus::Reviewing, at(15)))
            .await
            .unwrap_or_else(|e| panic!("insert: {e}"));

        // Only five minutes old against a ten-minute staleness window.
        let report = recover(&pool, &sink, at(20))
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        assert!(
            report.is_empty(),
            "a run inside the window must be left alone: {report:?}"
        );
        assert_eq!(
            store
                .get(fresh.id)
                .await
                .unwrap_or_else(|e| panic!("get: {e}"))
                .status,
            RunStatus::Reviewing
        );
    }

    #[tokio::test]
    async fn state_machine_a_finished_run_is_never_recovered_however_old() {
        // Terminal is terminal. A run that completed last year is not stale, it is
        // done — and re-enqueueing it would re-review and re-publish a change whose
        // findings are already filed.
        let (_dir, pool, change) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let store = RunStore::new(&pool);
        let sink = Collector::default();

        for (attempt, status) in [(1, RunStatus::Done), (2, RunStatus::Cancelled)] {
            let mut run = a_run(change, attempt, status, at(0));
            if status == RunStatus::Failed {
                run.error = Some("x".to_owned());
            }
            store
                .insert(&run)
                .await
                .unwrap_or_else(|e| panic!("insert {status}: {e}"));
        }

        let report = recover(&pool, &sink, at(59))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(report.is_empty(), "{report:?}");
    }

    #[tokio::test]
    async fn state_machine_a_queued_run_is_as_abandoned_as_a_reviewing_one() {
        // A run queued an hour ago is not waiting its turn — nothing is going to
        // pick it up, because whatever would have is gone.
        let (_dir, pool, change) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let sink = Collector::default();

        RunStore::new(&pool)
            .insert(&a_run(change, 1, RunStatus::Queued, at(0)))
            .await
            .unwrap_or_else(|e| panic!("insert: {e}"));

        let report = recover(&pool, &sink, at(30))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(report.interrupted.len(), 1);
    }

    #[tokio::test]
    async fn state_machine_a_retry_starts_with_a_clean_slate() {
        // Carrying the previous attempt's usage forward would double-charge the
        // budget for work that was thrown away, and carrying its `degraded` forward
        // would escalate a retry that has not yet done anything.
        let (_dir, pool, change) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let store = RunStore::new(&pool);
        let sink = Collector::default();

        let mut crashed = a_run(change, 1, RunStatus::Reviewing, at(0));
        crashed.usage = Usage {
            tokens_in: 5_000,
            tokens_out: 900,
            cost_usd: Some(0.4),
        };
        crashed.degraded = Some("salvaged from a fenced block".to_owned());
        crashed.truncated = true;
        crashed.omitted_files = vec!["generated/a.rs".to_owned()];
        store
            .insert(&crashed)
            .await
            .unwrap_or_else(|e| panic!("insert: {e}"));

        let report = recover(&pool, &sink, at(20))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let successor = store
            .get(report.re_enqueued[0])
            .await
            .unwrap_or_else(|e| panic!("get: {e}"));

        assert_eq!(
            successor.usage.total_tokens(),
            0,
            "a retry has spent nothing yet"
        );
        assert_eq!(successor.degraded, None, "and salvaged nothing yet");
        assert!(!successor.truncated);
        assert!(successor.omitted_files.is_empty());
        // ...but it is the same work.
        assert_eq!(successor.change_id, crashed.change_id);
        assert_eq!(successor.depth, crashed.depth);
        assert_eq!(successor.trigger, crashed.trigger);
    }

    #[tokio::test]
    async fn state_machine_recovery_racing_a_run_that_finished_lets_finishing_win() {
        // Recovery reads a run as stale, and between that read and the write the run
        // completes. The run really did complete; overwriting it with `interrupted`
        // would discard a real review.
        let (_dir, pool, change) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let store = RunStore::new(&pool);

        let run = store
            .insert(&a_run(change, 1, RunStatus::Synthesizing, at(0)))
            .await
            .unwrap_or_else(|e| panic!("insert: {e}"));

        // It finishes first.
        store
            .transition(run.id, RunStatus::Synthesizing, RunStatus::Done)
            .await
            .unwrap_or_else(|e| panic!("transition: {e}"));

        // Recovery's write arrives late.
        store
            .mark_interrupted(run.id, INTERRUPTED)
            .await
            .unwrap_or_else(|e| panic!("mark: {e}"));

        assert_eq!(
            store
                .get(run.id)
                .await
                .unwrap_or_else(|e| panic!("get: {e}"))
                .status,
            RunStatus::Done,
            "a completed run must not be rewritten as interrupted"
        );
    }
}
