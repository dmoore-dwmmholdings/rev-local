//! Acceptance tests for `RL-110` — the append-only audit log and the budget
//! ledger's behaviour under concurrency and across a day boundary.
//!
//! The module is named `budget` so the item's gate,
//! `cargo test -p revlocal-store -- --include-ignored budget`, selects it. The
//! genuinely slow cases are `#[ignore]`d and run only under that gate, which is
//! why the gate passes `--include-ignored` rather than being a plain filter.

mod budget {
    use chrono::TimeZone;
    use revlocal_core::{
        AuditEntry, AuditId, AutonomyMode, BudgetLimits, EngineKind, ExhaustedLimit, OnExhausted,
        Repo, RepoId, RepoKind, Timestamp, Usage,
    };
    use revlocal_store::{open, AuditStore, BudgetLedgerStore, Pool, RepoStore};
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    fn at(minute: u32) -> Timestamp {
        chrono::Utc
            .with_ymd_and_hms(2026, 8, 27, 12, minute, 0)
            .single()
            .unwrap_or_default()
    }

    /// A migrated database plus one repo, and the path so more pools can be opened
    /// over the same file.
    async fn seeded() -> Result<(TempDir, PathBuf, Pool, RepoId), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let path = dir.path().join("rev-local.db");
        let pool = open(&path).await?;

        let repo = RepoStore::new(&pool)
            .insert(&Repo {
                id: RepoId::new(0),
                name: "rev-local".to_owned(),
                kind: RepoKind::Git,
                local_path: None,
                remote_url: None,
                default_branch: None,
                engine: EngineKind::Mock,
                autonomy: AutonomyMode::DryRun,
                enabled: true,
                config_json: "{}".to_owned(),
                created_at: at(0),
                updated_at: at(0),
            })
            .await?;

        Ok((dir, path, pool, repo.id))
    }

    // --- the audit log is append-only ----------------------------------------

    #[test]
    fn no_store_method_updates_or_deletes_an_audit_row() {
        // Asserted structurally rather than by inspection: the guarantee is that
        // no such method EXISTS, and a test that only exercises the methods that
        // do exist could never notice one being added.
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders = Vec::new();

        let files = std::fs::read_dir(&src).unwrap_or_else(|e| panic!("reading {src:?}: {e}"));
        for entry in files.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("reading {path:?}: {e}"))
                .to_uppercase();

            for statement in ["UPDATE AUDIT", "DELETE FROM AUDIT"] {
                if text.contains(statement) {
                    offenders.push(format!("{}: {statement}", path.display()));
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "the audit log is the record of what rev-local did on a user's behalf; \
             a log that can be rewritten is not one. Found: {offenders:?}"
        );
    }

    #[tokio::test]
    async fn audit_rows_survive_everything_the_store_can_do_to_them() {
        // The structural test above says no method rewrites the log. This says the
        // rows are still there afterwards.
        let (_dir, _path, pool, repo_id) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let audit = AuditStore::new(&pool);

        for kind in ["repo.added", "budget.exhausted", "kill_switch.engaged"] {
            audit
                .append(&AuditEntry {
                    id: AuditId::new(0),
                    at: at(0),
                    actor: "daemon".to_owned(),
                    kind: kind.to_owned(),
                    repo_id: Some(repo_id),
                    run_id: None,
                    detail_json: "{}".to_owned(),
                })
                .await
                .unwrap_or_else(|e| panic!("append {kind}: {e}"));
        }

        let before = audit
            .recent(100)
            .await
            .unwrap_or_else(|e| panic!("recent: {e}"))
            .len();
        assert_eq!(before, 3);

        // audit has no ON DELETE CASCADE in SPEC §5 — deliberately. The record of
        // what was done to a repo must outlive the repo.
        RepoStore::new(&pool)
            .delete(repo_id)
            .await
            .unwrap_or_else(|e| panic!("delete repo: {e}"));

        let after = audit
            .recent(100)
            .await
            .unwrap_or_else(|e| panic!("recent: {e}"));
        assert_eq!(
            after.len(),
            3,
            "deleting a repo must not erase the record of what was done to it"
        );
    }

    // --- day rollover ---------------------------------------------------------

    #[tokio::test]
    async fn budget_day_rollover_creates_a_new_row_and_leaves_yesterday_alone() {
        let (_dir, _path, pool, repo_id) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let ledger = BudgetLedgerStore::new(&pool);
        let usage = Usage {
            tokens_in: 100,
            tokens_out: 50,
            tokens_known: true,
            cost_usd: Some(1.0),
        };

        for _ in 0..3 {
            ledger
                .add_run(repo_id, "2026-08-27", 1, &usage)
                .await
                .unwrap_or_else(|e| panic!("yesterday: {e}"));
        }
        ledger
            .add_run(repo_id, "2026-08-28", 1, &usage)
            .await
            .unwrap_or_else(|e| panic!("today: {e}"));

        let yesterday = ledger
            .get(repo_id, "2026-08-27")
            .await
            .unwrap_or_else(|e| panic!("get: {e}"))
            .unwrap_or_else(|| panic!("yesterday missing"));
        let today = ledger
            .get(repo_id, "2026-08-28")
            .await
            .unwrap_or_else(|e| panic!("get: {e}"))
            .unwrap_or_else(|| panic!("today missing"));

        assert_eq!(
            yesterday.runs, 3,
            "yesterday must be untouched by today's run"
        );
        assert_eq!(yesterday.usage.total_tokens(), 450);
        assert_eq!(
            today.runs, 1,
            "a new day starts from zero, not from yesterday"
        );
        assert_eq!(today.usage.total_tokens(), 150);
    }

    #[tokio::test]
    async fn budget_rollover_restores_a_repo_that_was_exhausted_yesterday() {
        // The point of a daily budget: exhaustion is temporary. If rollover did not
        // reset the decision, a repo would pause permanently after one heavy day.
        let (_dir, _path, pool, repo_id) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let ledger = BudgetLedgerStore::new(&pool);
        let limits = BudgetLimits {
            daily_runs: 2,
            daily_tokens: 0,
            daily_cost_usd: 0.0,
            on_exhausted: OnExhausted::Pause,
        };

        for _ in 0..2 {
            ledger
                .add_run(repo_id, "2026-08-27", 1, &Usage::default())
                .await
                .unwrap_or_else(|e| panic!("add: {e}"));
        }

        let yesterday = ledger
            .get(repo_id, "2026-08-27")
            .await
            .unwrap_or_else(|e| panic!("get: {e}"));
        let decision = revlocal_core::budget::check(&limits, yesterday.as_ref());
        assert!(!decision.may_run(), "two of two runs used: {decision:?}");

        let today = ledger
            .get(repo_id, "2026-08-28")
            .await
            .unwrap_or_else(|e| panic!("get: {e}"));
        assert!(
            today.is_none(),
            "the new day has no row until something is spent"
        );
        assert!(
            revlocal_core::budget::check(&limits, today.as_ref()).may_run(),
            "rollover must lift yesterday's exhaustion"
        );
    }

    #[tokio::test]
    async fn budget_the_mock_engine_reporting_no_price_does_not_stop_the_inner_loop() {
        // ADR 0010's consequence, asserted rather than assumed: every inner-loop
        // day is cost-incomplete because the mock engine reports no price. With no
        // cost ceiling configured that must be harmless; with one, it must stop.
        let (_dir, _path, pool, repo_id) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        BudgetLedgerStore::new(&pool)
            .add_run(
                repo_id,
                "2026-08-27",
                1,
                &Usage {
                    tokens_in: 10,
                    tokens_out: 5,
                    tokens_known: true,
                    cost_usd: None,
                },
            )
            .await
            .unwrap_or_else(|e| panic!("add: {e}"));

        let day = BudgetLedgerStore::new(&pool)
            .get(repo_id, "2026-08-27")
            .await
            .unwrap_or_else(|e| panic!("get: {e}"));
        assert!(day.as_ref().is_some_and(|d| !d.cost_is_complete()));

        let no_ceiling = BudgetLimits {
            daily_runs: 100,
            daily_tokens: 1_000_000,
            daily_cost_usd: 0.0,
            on_exhausted: OnExhausted::Pause,
        };
        assert!(
            revlocal_core::budget::check(&no_ceiling, day.as_ref()).may_run(),
            "an unpriced run must not stop a repo with no cost limit"
        );

        let with_ceiling = BudgetLimits {
            daily_cost_usd: 10.0,
            ..no_ceiling
        };
        match revlocal_core::budget::check(&with_ceiling, day.as_ref()) {
            revlocal_core::BudgetDecision::Exhausted { limit, .. } => {
                assert_eq!(limit, ExhaustedLimit::CostUnknown);
            }
            revlocal_core::BudgetDecision::Proceed => {
                panic!("a cost ceiling that cannot be enforced must not proceed")
            }
        }
    }

    // --- concurrency ----------------------------------------------------------

    #[tokio::test]
    async fn budget_increments_from_two_tasks_sum_correctly() {
        let (_dir, _path, pool, repo_id) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));

        let mut handles = Vec::new();
        for _ in 0..2 {
            let pool = pool.clone();
            handles.push(tokio::spawn(async move {
                BudgetLedgerStore::new(&pool)
                    .add_run(
                        repo_id,
                        "2026-08-27",
                        1,
                        &Usage {
                            tokens_in: 100,
                            tokens_out: 50,
                            tokens_known: true,
                            cost_usd: Some(0.5),
                        },
                    )
                    .await
            }));
        }
        for handle in handles {
            handle
                .await
                .unwrap_or_else(|e| panic!("task panicked: {e}"))
                .unwrap_or_else(|e| panic!("increment: {e}"));
        }

        let entry = BudgetLedgerStore::new(&pool)
            .get(repo_id, "2026-08-27")
            .await
            .unwrap_or_else(|e| panic!("get: {e}"))
            .unwrap_or_else(|| panic!("day missing"));
        assert_eq!(entry.runs, 2);
        assert_eq!(entry.usage.total_tokens(), 300);
    }

    /// The real WAL test: separate pools over the same file.
    ///
    /// Two tasks on one pool can be serialised by the pool itself, so that test
    /// proves less than it looks. Independent pools each hold their own
    /// connections and contend through SQLite's WAL locking, which is what the
    /// daemon actually does — and what `busy_timeout` exists for.
    ///
    /// `#[ignore]`d because it opens 8 pools and does real lock contention; the
    /// item's gate runs it with `--include-ignored`.
    #[tokio::test]
    #[ignore = "slow: opens independent pools and contends on WAL locks"]
    async fn budget_increments_across_independent_pools_lose_nothing() {
        let (_dir, path, pool, repo_id) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        drop(pool);

        const WRITERS: u32 = 8;
        const PER_WRITER: u32 = 25;

        let mut handles = Vec::new();
        for _ in 0..WRITERS {
            let path = path.clone();
            handles.push(tokio::spawn(async move {
                let pool = open(&path).await?;
                let ledger = BudgetLedgerStore::new(&pool);
                for _ in 0..PER_WRITER {
                    ledger
                        .add_run(
                            repo_id,
                            "2026-08-27",
                            1,
                            &Usage {
                                tokens_in: 10,
                                tokens_out: 0,
                                tokens_known: true,
                                cost_usd: Some(0.01),
                            },
                        )
                        .await?;
                }
                pool.close().await;
                Ok::<(), revlocal_store::StoreError>(())
            }));
        }
        for handle in handles {
            handle
                .await
                .unwrap_or_else(|e| panic!("task panicked: {e}"))
                .unwrap_or_else(|e| panic!("a writer failed — busy_timeout too short? {e}"));
        }

        let pool = open(&path).await.unwrap_or_else(|e| panic!("reopen: {e}"));
        let entry = BudgetLedgerStore::new(&pool)
            .get(repo_id, "2026-08-27")
            .await
            .unwrap_or_else(|e| panic!("get: {e}"))
            .unwrap_or_else(|| panic!("day missing"));

        let expected = WRITERS * PER_WRITER;
        assert_eq!(
            entry.runs, expected,
            "{WRITERS} writers x {PER_WRITER} increments must count {expected}"
        );
        assert_eq!(entry.usage.tokens_in, u64::from(expected) * 10);
        assert!(entry.cost_is_complete());
    }

    /// Concurrent writers on *different* days must not serialise into one row.
    #[tokio::test]
    #[ignore = "slow: opens independent pools and contends on WAL locks"]
    async fn budget_concurrent_writers_on_different_days_stay_separate() {
        let (_dir, path, pool, repo_id) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        drop(pool);

        let days = ["2026-08-26", "2026-08-27", "2026-08-28"];
        let mut handles = Vec::new();
        for day in days {
            let path = path.clone();
            handles.push(tokio::spawn(async move {
                let pool = open(&path).await?;
                for _ in 0..10 {
                    BudgetLedgerStore::new(&pool)
                        .add_run(repo_id, day, 1, &Usage::default())
                        .await?;
                }
                pool.close().await;
                Ok::<(), revlocal_store::StoreError>(())
            }));
        }
        for handle in handles {
            handle
                .await
                .unwrap_or_else(|e| panic!("task panicked: {e}"))
                .unwrap_or_else(|e| panic!("writer: {e}"));
        }

        let pool = open(&path).await.unwrap_or_else(|e| panic!("reopen: {e}"));
        let ledger = BudgetLedgerStore::new(&pool);
        for day in days {
            let entry = ledger
                .get(repo_id, day)
                .await
                .unwrap_or_else(|e| panic!("get {day}: {e}"))
                .unwrap_or_else(|| panic!("{day} missing"));
            assert_eq!(entry.runs, 10, "{day} must hold exactly its own increments");
        }
    }
}
