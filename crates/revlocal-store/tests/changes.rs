//! Round-trip and lifecycle tests for `RL-109b` — change, run and finding.

mod changes {
    use chrono::TimeZone;
    use revlocal_core::{
        AutonomyMode, Category, Change, ChangeId, ChangeKind, Depth, DiffStat, EngineKind, Finding,
        FindingId, FindingState, Repo, RepoId, Run, RunId, RunStatus, Severity, Timestamp,
        TriggerSource, Usage,
    };
    use revlocal_store::{open, ChangeStore, FindingStore, Pool, RepoStore, RunStore, StoreError};
    use tempfile::TempDir;

    fn at(minute: u32) -> Timestamp {
        chrono::Utc
            .with_ymd_and_hms(2026, 8, 27, 12, minute, 0)
            .single()
            .unwrap_or_default()
    }

    /// A migrated database plus one repo and one change to hang runs off.
    ///
    /// Returns `Result`; helpers are not `#[test]` fns (ADR 0003).
    async fn seeded() -> Result<(TempDir, Pool, RepoId, ChangeId), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let pool = open(&dir.path().join("rev-local.db")).await?;

        let repo = RepoStore::new(&pool)
            .insert(&Repo {
                id: RepoId::new(0),
                name: "rev-local".to_owned(),
                kind: revlocal_core::RepoKind::Git,
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

        let change = ChangeStore::new(&pool)
            .upsert(&a_change(repo.id, "deadbeef"))
            .await?;
        Ok((dir, pool, repo.id, change.id))
    }

    fn a_change(repo_id: RepoId, external_id: &str) -> Change {
        Change {
            id: ChangeId::new(0),
            repo_id,
            kind: ChangeKind::Commit,
            external_id: external_id.to_owned(),
            title: Some("RL-109b: repositories".to_owned()),
            author_name: Some("Dawson Moore".to_owned()),
            author_email: None,
            authored_at: Some(at(1)),
            branch: Some("main".to_owned()),
            base_ref: None,
            head_ref: Some(external_id.to_owned()),
            url: None,
            diff_stat: DiffStat {
                files: 3,
                insertions: 120,
                deletions: 4,
            },
            detected_at: at(2),
        }
    }

    fn a_run(change_id: ChangeId, attempt: u32, status: RunStatus) -> Run {
        Run {
            id: RunId::new(0),
            change_id,
            attempt,
            status,
            engine: EngineKind::Mock,
            depth: Depth::Standard,
            trigger: TriggerSource::Poll,
            skip_reason: None,
            error: None,
            degraded: None,
            usage: Usage {
                tokens_in: 100,
                tokens_out: 20,
                tokens_known: true,
                cost_usd: None,
            },
            started_at: Some(at(3)),
            finished_at: None,
            transcript_path: None,
            truncated: false,
            omitted_files: Vec::new(),
            verdict: None,
            summary: None,
            created_at: at(3),
        }
    }

    fn a_finding(run_id: RunId, fingerprint: &str) -> Finding {
        Finding {
            id: FindingId::new(0),
            run_id,
            fingerprint: fingerprint.to_owned(),
            severity: Severity::High,
            category: Category::Correctness,
            confidence: 0.8,
            file: Some("src/run.rs".to_owned()),
            line_start: Some(42),
            line_end: Some(47),
            title: "Unknown cost read as zero".to_owned(),
            body: "markdown".to_owned(),
            failure_scenario: Some("engine reports no cost".to_owned()),
            suggested_fix: None,
            state: FindingState::Open,
            created_at: at(4),
        }
    }

    // --- change --------------------------------------------------------------

    #[tokio::test]
    async fn a_change_round_trips_with_its_diff_stat() {
        let (_dir, pool, repo_id, _) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let store = ChangeStore::new(&pool);

        let inserted = store
            .upsert(&a_change(repo_id, "cafebabe"))
            .await
            .unwrap_or_else(|e| panic!("upsert: {e}"));
        let fetched = store
            .get(inserted.id)
            .await
            .unwrap_or_else(|e| panic!("get: {e}"));

        assert_eq!(fetched, inserted);
        assert_eq!(
            fetched.diff_stat,
            DiffStat {
                files: 3,
                insertions: 120,
                deletions: 4
            }
        );
    }

    #[tokio::test]
    async fn re_upserting_an_identical_change_creates_no_second_row() {
        // The poll trigger re-reads the same commits every interval (§7.1), so
        // rediscovery is the normal case. An insert would error every poll.
        let (_dir, pool, repo_id, _) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let store = ChangeStore::new(&pool);

        let first = store
            .upsert(&a_change(repo_id, "cafebabe"))
            .await
            .unwrap_or_else(|e| panic!("first: {e}"));
        let second = store
            .upsert(&a_change(repo_id, "cafebabe"))
            .await
            .unwrap_or_else(|e| panic!("second: {e}"));

        assert_eq!(first.id, second.id, "the same change must keep one row");
    }

    #[tokio::test]
    async fn upserting_a_moved_head_updates_the_existing_row() {
        // A PR whose head SHA advanced is the same change at a new state. An
        // insert-if-absent would never notice.
        let (_dir, pool, repo_id, _) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let store = ChangeStore::new(&pool);

        let first = store
            .upsert(&a_change(repo_id, "pr-7"))
            .await
            .unwrap_or_else(|e| panic!("first: {e}"));

        let mut moved = a_change(repo_id, "pr-7");
        moved.title = Some("RL-109b: repositories (updated)".to_owned());
        moved.head_ref = Some("newsha".to_owned());
        moved.diff_stat = DiffStat {
            files: 9,
            insertions: 400,
            deletions: 40,
        };
        let second = store
            .upsert(&moved)
            .await
            .unwrap_or_else(|e| panic!("second: {e}"));

        assert_eq!(first.id, second.id);
        assert_eq!(second.head_ref.as_deref(), Some("newsha"));
        assert_eq!(second.diff_stat.files, 9);
    }

    #[tokio::test]
    async fn rediscovery_does_not_reset_when_the_change_was_first_seen() {
        // detected_at is the record of first sight. Refreshing it every poll would
        // erase it and make every change look brand new.
        let (_dir, pool, repo_id, _) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let store = ChangeStore::new(&pool);

        store
            .upsert(&a_change(repo_id, "pr-7"))
            .await
            .unwrap_or_else(|e| panic!("first: {e}"));

        let mut later = a_change(repo_id, "pr-7");
        later.detected_at = at(59);
        let second = store
            .upsert(&later)
            .await
            .unwrap_or_else(|e| panic!("second: {e}"));

        assert_eq!(
            second.detected_at,
            at(2),
            "first sight must survive rediscovery"
        );
    }

    #[tokio::test]
    async fn the_same_external_id_under_a_different_kind_is_a_different_change() {
        let (_dir, pool, repo_id, _) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let store = ChangeStore::new(&pool);

        let commit = store
            .upsert(&a_change(repo_id, "shared"))
            .await
            .unwrap_or_else(|e| panic!("commit: {e}"));
        let mut as_pr = a_change(repo_id, "shared");
        as_pr.kind = ChangeKind::Pr;
        let pr = store
            .upsert(&as_pr)
            .await
            .unwrap_or_else(|e| panic!("pr: {e}"));

        assert_ne!(commit.id, pr.id);
    }

    #[tokio::test]
    async fn find_returns_none_for_a_change_never_seen() {
        let (_dir, pool, repo_id, _) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let found = ChangeStore::new(&pool)
            .find(repo_id, ChangeKind::Commit, "never")
            .await
            .unwrap_or_else(|e| panic!("find: {e}"));
        assert!(found.is_none());
    }

    // --- run -----------------------------------------------------------------

    #[tokio::test]
    async fn a_run_round_trips_including_its_usage() {
        let (_dir, pool, _, change_id) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let store = RunStore::new(&pool);

        let inserted = store
            .insert(&a_run(change_id, 1, RunStatus::Queued))
            .await
            .unwrap_or_else(|e| panic!("insert: {e}"));
        let fetched = store
            .get(inserted.id)
            .await
            .unwrap_or_else(|e| panic!("get: {e}"));

        assert_eq!(fetched, inserted);
        assert_eq!(fetched.usage.tokens_in, 100);
        assert_eq!(
            fetched.usage.cost_usd, None,
            "an unknown cost must not become 0.0"
        );
        assert!(!fetched.usage.cost_is_complete());
    }

    #[tokio::test]
    async fn a_retry_is_a_new_row_not_a_mutated_one() {
        let (_dir, pool, _, change_id) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let store = RunStore::new(&pool);

        store
            .insert(&a_run(change_id, 1, RunStatus::Failed).with_error())
            .await
            .unwrap_or_else(|e| panic!("attempt 1: {e}"));
        store
            .insert(&a_run(change_id, 2, RunStatus::Queued))
            .await
            .unwrap_or_else(|e| panic!("attempt 2: {e}"));

        let runs = store
            .list_for_change(change_id)
            .await
            .unwrap_or_else(|e| panic!("list: {e}"));
        assert_eq!(runs.len(), 2, "the history of what was tried must survive");
        assert_eq!(runs[0].attempt, 1);
        assert_eq!(runs[1].attempt, 2);
    }

    #[tokio::test]
    async fn a_duplicate_attempt_is_a_typed_already_exists() {
        let (_dir, pool, _, change_id) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let store = RunStore::new(&pool);

        store
            .insert(&a_run(change_id, 1, RunStatus::Queued))
            .await
            .unwrap_or_else(|e| panic!("first: {e}"));
        let error = store
            .insert(&a_run(change_id, 1, RunStatus::Queued))
            .await
            .expect_err("a second attempt 1 must be refused");
        assert!(error.is_already_exists(), "got {error:?}");
    }

    #[tokio::test]
    async fn a_run_moves_forward_through_the_pipeline() {
        let (_dir, pool, _, change_id) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let store = RunStore::new(&pool);
        let run = store
            .insert(&a_run(change_id, 1, RunStatus::Queued))
            .await
            .unwrap_or_else(|e| panic!("insert: {e}"));

        for (from, to) in [
            (RunStatus::Queued, RunStatus::Preparing),
            (RunStatus::Preparing, RunStatus::Reviewing),
            (RunStatus::Reviewing, RunStatus::Synthesizing),
            (RunStatus::Synthesizing, RunStatus::Done),
        ] {
            store
                .transition(run.id, from, to)
                .await
                .unwrap_or_else(|e| panic!("{from} -> {to}: {e}"));
        }

        assert_eq!(
            store
                .get(run.id)
                .await
                .unwrap_or_else(|e| panic!("get: {e}"))
                .status,
            RunStatus::Done
        );
    }

    #[tokio::test]
    async fn an_illegal_transition_is_refused_against_a_real_database() {
        // The pure function already refuses this; this asserts the store does too,
        // and that the row did not move.
        let (_dir, pool, _, change_id) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let store = RunStore::new(&pool);
        let run = store
            .insert(&a_run(change_id, 1, RunStatus::Queued))
            .await
            .unwrap_or_else(|e| panic!("insert: {e}"));

        store
            .transition(run.id, RunStatus::Queued, RunStatus::Preparing)
            .await
            .unwrap_or_else(|e| panic!("setup: {e}"));
        store
            .transition(run.id, RunStatus::Preparing, RunStatus::Reviewing)
            .await
            .unwrap_or_else(|e| panic!("setup: {e}"));
        store
            .transition(run.id, RunStatus::Reviewing, RunStatus::Synthesizing)
            .await
            .unwrap_or_else(|e| panic!("setup: {e}"));
        store
            .transition(run.id, RunStatus::Synthesizing, RunStatus::Done)
            .await
            .unwrap_or_else(|e| panic!("setup: {e}"));

        let error = store
            .transition(run.id, RunStatus::Done, RunStatus::Reviewing)
            .await
            .expect_err("done -> reviewing must be refused");
        assert!(
            matches!(error, StoreError::IllegalTransition(_)),
            "got {error:?}"
        );

        assert_eq!(
            store
                .get(run.id)
                .await
                .unwrap_or_else(|e| panic!("get: {e}"))
                .status,
            RunStatus::Done,
            "a refused transition must leave the row alone"
        );
    }

    #[tokio::test]
    async fn only_one_of_two_racing_transitions_succeeds() {
        // Both callers legitimately observe `queued` and both attempt the same
        // legal move. A read-modify-write would let both "succeed"; the
        // compare-and-swap means exactly one does.
        let (_dir, pool, _, change_id) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let run = RunStore::new(&pool)
            .insert(&a_run(change_id, 1, RunStatus::Queued))
            .await
            .unwrap_or_else(|e| panic!("insert: {e}"));

        let mut handles = Vec::new();
        for _ in 0..2 {
            let pool = pool.clone();
            let id = run.id;
            handles.push(tokio::spawn(async move {
                RunStore::new(&pool)
                    .transition(id, RunStatus::Queued, RunStatus::Preparing)
                    .await
            }));
        }

        let mut succeeded = 0;
        for handle in handles {
            if handle
                .await
                .unwrap_or_else(|e| panic!("task panicked: {e}"))
                .is_ok()
            {
                succeeded += 1;
            }
        }
        assert_eq!(succeeded, 1, "exactly one caller may claim the transition");
    }

    #[tokio::test]
    async fn a_skipped_run_without_a_reason_is_refused_on_write() {
        // SPEC §18, no silent caps. Enforced in the store rather than trusted to
        // callers, so it holds for whatever writes next.
        let (_dir, pool, _, change_id) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let store = RunStore::new(&pool);

        let error = store
            .insert(&a_run(change_id, 1, RunStatus::Skipped))
            .await
            .expect_err("a skip with no reason must be refused");
        assert!(matches!(error, StoreError::Corrupt { .. }), "got {error:?}");
        assert!(error.to_string().contains("skipped"), "{error}");

        let mut with_reason = a_run(change_id, 1, RunStatus::Skipped);
        with_reason.skip_reason = Some("lockfile-only change".to_owned());
        store
            .insert(&with_reason)
            .await
            .unwrap_or_else(|e| panic!("a skip WITH a reason must be accepted: {e}"));
    }

    #[tokio::test]
    async fn a_failed_run_without_an_error_is_refused_on_write() {
        let (_dir, pool, _, change_id) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let error = RunStore::new(&pool)
            .insert(&a_run(change_id, 1, RunStatus::Failed))
            .await
            .expect_err("a failure with no error must be refused");
        assert!(matches!(error, StoreError::Corrupt { .. }), "got {error:?}");
    }

    #[tokio::test]
    async fn a_truncated_run_records_what_it_did_not_see() {
        // SPEC §18: "a review that saw 60% of the diff must never look like a review
        // that saw all of it." Before RL-1304 this lived only in the in-memory
        // ChangeContext and died with the process, so the UI had nothing to show.
        let (_dir, pool, _, change_id) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let store = RunStore::new(&pool);

        let mut run = a_run(change_id, 1, RunStatus::Done);
        run.truncated = true;
        run.omitted_files = vec![
            "generated/mod_198.rs".to_owned(),
            "generated/mod_199.rs".to_owned(),
            "generated/mod_200.rs".to_owned(),
        ];

        let back = store
            .get(
                store
                    .insert(&run)
                    .await
                    .unwrap_or_else(|e| panic!("insert: {e}"))
                    .id,
            )
            .await
            .unwrap_or_else(|e| panic!("get: {e}"));

        assert!(back.truncated);
        assert_eq!(
            back.omitted_files.len(),
            3,
            "§9.4: the omitted list is stored IN FULL, not as a count"
        );
        assert!(back
            .omitted_files
            .contains(&"generated/mod_200.rs".to_owned()));
    }

    #[tokio::test]
    async fn a_run_claiming_truncation_with_no_omitted_files_is_refused() {
        // Claiming something was dropped without saying what is the silent cap §18
        // exists to prevent — and it is worse than not claiming it, because the UI
        // would show a truncation warning with nothing behind it.
        let (_dir, pool, _, change_id) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let mut run = a_run(change_id, 1, RunStatus::Done);
        run.truncated = true;

        let error = RunStore::new(&pool)
            .insert(&run)
            .await
            .expect_err("a truncated run with no omitted files must be refused");
        assert!(matches!(error, StoreError::Corrupt { .. }), "got {error:?}");
    }

    #[tokio::test]
    async fn a_runs_verdict_is_stored_rather_than_recomputed() {
        // §10.2's verdict is a HISTORICAL FACT — what was posted. Recomputing it
        // from findings would change retroactively as findings are suppressed or
        // superseded, so a run that requested changes would silently become one that
        // approved, and the audit trail would disagree with what GitHub shows.
        let (_dir, pool, _, change_id) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let runs = RunStore::new(&pool);
        let findings = FindingStore::new(&pool);

        let mut run = a_run(change_id, 1, RunStatus::Done);
        run.verdict = Some(revlocal_core::Verdict::RequestChanges);
        run.summary = Some("Two defects, one blocking.".to_owned());
        let stored = runs
            .insert(&run)
            .await
            .unwrap_or_else(|e| panic!("insert: {e}"));

        let finding = findings
            .insert(&a_finding(stored.id, "fp-blocking"))
            .await
            .unwrap_or_else(|e| panic!("insert finding: {e}"));

        // Suppress the only blocking finding. A recomputed verdict would now be
        // `approve`; the stored one must not move.
        findings
            .set_state(finding.id, revlocal_core::FindingState::Suppressed)
            .await
            .unwrap_or_else(|e| panic!("suppress: {e}"));

        let back = runs
            .get(stored.id)
            .await
            .unwrap_or_else(|e| panic!("get: {e}"));
        assert_eq!(
            back.verdict,
            Some(revlocal_core::Verdict::RequestChanges),
            "suppressing a finding must not rewrite history"
        );
        assert_eq!(
            back.summary.as_deref(),
            Some("Two defects, one blocking."),
            "the engine's summary outlives the transcript, which retention prunes"
        );
    }

    #[tokio::test]
    async fn a_degraded_run_keeps_its_reason() {
        // §12.3 escalates every action on a degraded run; the reason is what makes
        // that readable in the approvals inbox.
        let (_dir, pool, _, change_id) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let store = RunStore::new(&pool);
        let mut run = a_run(change_id, 1, RunStatus::Queued);
        run.degraded = Some("result.json missing; parsed fenced block".to_owned());

        let back = store
            .get(
                store
                    .insert(&run)
                    .await
                    .unwrap_or_else(|e| panic!("insert: {e}"))
                    .id,
            )
            .await
            .unwrap_or_else(|e| panic!("get: {e}"));
        assert!(back.is_degraded());
        assert_eq!(
            back.degraded.as_deref(),
            Some("result.json missing; parsed fenced block")
        );
    }

    // --- finding -------------------------------------------------------------

    #[tokio::test]
    async fn a_finding_round_trips() {
        let (_dir, pool, _, change_id) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let run = RunStore::new(&pool)
            .insert(&a_run(change_id, 1, RunStatus::Queued))
            .await
            .unwrap_or_else(|e| panic!("insert run: {e}"));
        let store = FindingStore::new(&pool);

        let inserted = store
            .insert(&a_finding(run.id, "0123456789abcdef"))
            .await
            .unwrap_or_else(|e| panic!("insert: {e}"));
        let fetched = store
            .get(inserted.id)
            .await
            .unwrap_or_else(|e| panic!("get: {e}"));
        assert_eq!(fetched, inserted);
    }

    #[tokio::test]
    async fn a_fingerprint_lookup_crosses_runs_which_is_what_dedupe_needs() {
        // The same defect after a rebase: same fingerprint, different run
        // (§10.3). If this query were scoped to one run, dedupe would never fire.
        let (_dir, pool, _, change_id) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let runs = RunStore::new(&pool);
        let findings = FindingStore::new(&pool);

        let first = runs
            .insert(&a_run(change_id, 1, RunStatus::Queued))
            .await
            .unwrap_or_else(|e| panic!("run 1: {e}"));
        let second = runs
            .insert(&a_run(change_id, 2, RunStatus::Queued))
            .await
            .unwrap_or_else(|e| panic!("run 2: {e}"));

        for run_id in [first.id, second.id] {
            findings
                .insert(&a_finding(run_id, "same-defect-fp"))
                .await
                .unwrap_or_else(|e| panic!("insert finding: {e}"));
        }
        findings
            .insert(&a_finding(first.id, "other-fp"))
            .await
            .unwrap_or_else(|e| panic!("insert other: {e}"));

        let matches = findings
            .by_fingerprint("same-defect-fp")
            .await
            .unwrap_or_else(|e| panic!("by_fingerprint: {e}"));
        assert_eq!(
            matches.len(),
            2,
            "dedupe must see the same defect across runs"
        );
        assert!(matches.iter().all(|f| f.fingerprint == "same-defect-fp"));
    }

    #[tokio::test]
    async fn findings_move_state_and_stay_addressable() {
        let (_dir, pool, _, change_id) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let run = RunStore::new(&pool)
            .insert(&a_run(change_id, 1, RunStatus::Queued))
            .await
            .unwrap_or_else(|e| panic!("insert run: {e}"));
        let store = FindingStore::new(&pool);
        let finding = store
            .insert(&a_finding(run.id, "fp"))
            .await
            .unwrap_or_else(|e| panic!("insert: {e}"));

        store
            .set_state(finding.id, FindingState::Published)
            .await
            .unwrap_or_else(|e| panic!("set_state: {e}"));
        assert_eq!(
            store
                .get(finding.id)
                .await
                .unwrap_or_else(|e| panic!("get: {e}"))
                .state,
            FindingState::Published
        );

        assert!(matches!(
            store
                .set_state(FindingId::new(404), FindingState::Resolved)
                .await,
            Err(StoreError::NotFound { .. })
        ));
    }

    #[tokio::test]
    async fn deleting_a_repo_cascades_all_the_way_to_findings() {
        let (_dir, pool, repo_id, change_id) =
            seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let run = RunStore::new(&pool)
            .insert(&a_run(change_id, 1, RunStatus::Queued))
            .await
            .unwrap_or_else(|e| panic!("insert run: {e}"));
        FindingStore::new(&pool)
            .insert(&a_finding(run.id, "fp"))
            .await
            .unwrap_or_else(|e| panic!("insert finding: {e}"));

        RepoStore::new(&pool)
            .delete(repo_id)
            .await
            .unwrap_or_else(|e| panic!("delete: {e}"));

        let left = FindingStore::new(&pool)
            .by_fingerprint("fp")
            .await
            .unwrap_or_else(|e| panic!("by_fingerprint: {e}"));
        assert!(
            left.is_empty(),
            "repo -> change -> run -> finding must all cascade"
        );
    }

    /// A failed run needs an error to be consistent (SPEC §18).
    trait WithError {
        fn with_error(self) -> Self;
    }

    impl WithError for Run {
        fn with_error(mut self) -> Self {
            self.error = Some("engine_output_unparseable".to_owned());
            self
        }
    }
}
