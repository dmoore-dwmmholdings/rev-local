//! Round-trip and concurrency tests for `RL-109c` — publish actions, the audit
//! log and the budget ledger.

mod publish {
    use chrono::TimeZone;
    use revlocal_core::{
        AuditEntry, AuditId, AutonomyMode, Capability, Change, ChangeId, ChangeKind, Depth,
        DiffStat, EngineKind, PublishAction, PublishActionId, PublishActionStatus, Repo, RepoId,
        RepoKind, RiskClass, Run, RunId, RunStatus, Timestamp, TriggerSource, Usage,
    };
    use revlocal_store::{
        open, AuditStore, BudgetLedgerStore, ChangeStore, Pool, PublishActionStore, RepoStore,
        RunStore,
    };
    use tempfile::TempDir;

    fn at(minute: u32) -> Timestamp {
        chrono::Utc
            .with_ymd_and_hms(2026, 8, 27, 12, minute, 0)
            .single()
            .unwrap_or_default()
    }

    /// A database with one repo, one change and one run to hang actions off.
    async fn seeded() -> Result<(TempDir, Pool, RepoId, RunId), Box<dyn std::error::Error>> {
        let dir = TempDir::new()?;
        let pool = open(&dir.path().join("rev-local.db")).await?;

        let repo = RepoStore::new(&pool)
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

        let change = ChangeStore::new(&pool)
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
                detected_at: at(1),
            })
            .await?;

        let run = RunStore::new(&pool)
            .insert(&Run {
                id: RunId::new(0),
                change_id: change.id,
                attempt: 1,
                status: RunStatus::Publishing,
                engine: EngineKind::Mock,
                depth: Depth::Standard,
                trigger: TriggerSource::Manual,
                skip_reason: None,
                error: None,
                degraded: None,
                usage: Usage::default(),
                started_at: Some(at(2)),
                finished_at: None,
                transcript_path: None,
                truncated: false,
                omitted_files: Vec::new(),
                verdict: None,
                summary: None,
                created_at: at(2),
            })
            .await?;

        Ok((dir, pool, repo.id, run.id))
    }

    fn an_action(run_id: RunId, target: &str, key: &str) -> PublishAction {
        PublishAction {
            id: PublishActionId::new(0),
            run_id,
            finding_id: None,
            target: target.to_owned(),
            capability: Capability::CreateIssue,
            risk: RiskClass::High,
            idempotency_key: key.to_owned(),
            payload_json: "{}".to_owned(),
            status: PublishActionStatus::Pending,
            attempts: 0,
            response_json: None,
            external_ref: None,
            error: None,
            created_at: at(3),
            sent_at: None,
        }
    }

    // --- publish_action ------------------------------------------------------

    #[tokio::test]
    async fn a_publish_action_round_trips() {
        let (_dir, pool, _, run_id) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let store = PublishActionStore::new(&pool);

        let inserted = store
            .insert(&an_action(run_id, "andare", "andare:REVL:abc"))
            .await
            .unwrap_or_else(|e| panic!("insert: {e}"));
        let fetched = store
            .get(inserted.id)
            .await
            .unwrap_or_else(|e| panic!("get: {e}"));

        assert_eq!(fetched, inserted);
        assert!(fetched.needs_approval() || !fetched.is_terminal());
    }

    #[tokio::test]
    async fn a_duplicate_idempotency_key_is_a_typed_already_exists() {
        // REVL-22's second acceptance criterion. The publish queue reads this as a
        // success — SPEC §11.6 wants exactly-once effect, so a redelivery landing
        // on a recorded action means the effect happened.
        let (_dir, pool, _, run_id) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let store = PublishActionStore::new(&pool);

        store
            .insert(&an_action(run_id, "andare", "andare:REVL:abc"))
            .await
            .unwrap_or_else(|e| panic!("first: {e}"));
        let error = store
            .insert(&an_action(run_id, "andare", "andare:REVL:abc"))
            .await
            .expect_err("a duplicate key must be refused");

        assert!(error.is_already_exists(), "got {error:?}");
        assert!(error.to_string().contains("andare:REVL:abc"), "{error}");
        assert!(
            !matches!(error, revlocal_store::StoreError::Database(_)),
            "it must not arrive as a raw sqlx error"
        );
    }

    #[tokio::test]
    async fn the_same_key_against_a_different_target_is_a_different_action() {
        let (_dir, pool, _, run_id) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let store = PublishActionStore::new(&pool);

        store
            .insert(&an_action(run_id, "andare", "shared-key"))
            .await
            .unwrap_or_else(|e| panic!("andare: {e}"));
        store
            .insert(&an_action(run_id, "trama", "shared-key"))
            .await
            .unwrap_or_else(|e| panic!("filing to a second target must be allowed: {e}"));

        assert_eq!(
            store
                .list_for_run(run_id)
                .await
                .unwrap_or_else(|e| panic!("list: {e}"))
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn a_collision_yields_the_existing_actions_external_ref() {
        // Knowing a duplicate exists is not enough: the caller needs the issue key
        // that was already created, so it can link to it rather than re-file.
        let (_dir, pool, _, run_id) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let store = PublishActionStore::new(&pool);

        let action = store
            .insert(&an_action(run_id, "andare", "andare:REVL:abc"))
            .await
            .unwrap_or_else(|e| panic!("insert: {e}"));
        store
            .record_outcome(
                action.id,
                PublishActionStatus::Sent,
                Some("REVL-111"),
                Some("{}"),
                None,
                at(4),
            )
            .await
            .unwrap_or_else(|e| panic!("record: {e}"));

        let existing = store
            .find_by_idempotency_key("andare", "andare:REVL:abc")
            .await
            .unwrap_or_else(|e| panic!("find: {e}"))
            .unwrap_or_else(|| panic!("the recorded action must be findable by its key"));

        assert_eq!(existing.external_ref.as_deref(), Some("REVL-111"));
        assert_eq!(existing.status, PublishActionStatus::Sent);
        assert_eq!(
            existing.attempts, 1,
            "recording an outcome counts an attempt"
        );
        assert_eq!(existing.sent_at, Some(at(4)));
        assert!(existing.is_terminal());
    }

    #[tokio::test]
    async fn a_failed_delivery_does_not_stamp_a_sent_time() {
        let (_dir, pool, _, run_id) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let store = PublishActionStore::new(&pool);
        let action = store
            .insert(&an_action(run_id, "andare", "k"))
            .await
            .unwrap_or_else(|e| panic!("insert: {e}"));

        store
            .record_outcome(
                action.id,
                PublishActionStatus::Failed,
                None,
                None,
                Some("connection refused"),
                at(4),
            )
            .await
            .unwrap_or_else(|e| panic!("record: {e}"));

        let back = store
            .get(action.id)
            .await
            .unwrap_or_else(|e| panic!("get: {e}"));
        assert_eq!(
            back.sent_at, None,
            "nothing was sent, so nothing was sent at"
        );
        assert_eq!(back.error.as_deref(), Some("connection refused"));
    }

    // --- the two queries the risk model needs --------------------------------

    #[tokio::test]
    async fn only_a_delivered_action_establishes_a_target_capability_pair() {
        // Decision of record: first use of a pair is always high risk. An action
        // that failed has not shown rev-local can safely write to that system, so
        // it must not count as establishing the pair.
        let (_dir, pool, _, run_id) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let store = PublishActionStore::new(&pool);

        assert!(
            !store
                .pair_has_succeeded("andare", Capability::CreateIssue)
                .await
                .unwrap_or_else(|e| panic!("query: {e}")),
            "a pair never used must be reported as never used"
        );

        let failed = store
            .insert(&an_action(run_id, "andare", "k1"))
            .await
            .unwrap_or_else(|e| panic!("insert: {e}"));
        store
            .record_outcome(
                failed.id,
                PublishActionStatus::Failed,
                None,
                None,
                Some("nope"),
                at(4),
            )
            .await
            .unwrap_or_else(|e| panic!("record: {e}"));

        assert!(
            !store
                .pair_has_succeeded("andare", Capability::CreateIssue)
                .await
                .unwrap_or_else(|e| panic!("query: {e}")),
            "a failed action must not establish the pair"
        );

        let sent = store
            .insert(&an_action(run_id, "andare", "k2"))
            .await
            .unwrap_or_else(|e| panic!("insert: {e}"));
        store
            .record_outcome(
                sent.id,
                PublishActionStatus::Sent,
                Some("REVL-1"),
                None,
                None,
                at(5),
            )
            .await
            .unwrap_or_else(|e| panic!("record: {e}"));

        assert!(store
            .pair_has_succeeded("andare", Capability::CreateIssue)
            .await
            .unwrap_or_else(|e| panic!("query: {e}")));
        assert!(
            !store
                .pair_has_succeeded("andare", Capability::SetStatus)
                .await
                .unwrap_or_else(|e| panic!("query: {e}")),
            "the pair is (target, capability); create_issue does not vouch for set_status"
        );
        assert!(
            !store
                .pair_has_succeeded("trama", Capability::CreateIssue)
                .await
                .unwrap_or_else(|e| panic!("query: {e}")),
            "...nor does andare vouch for trama"
        );
    }

    #[tokio::test]
    async fn the_burst_count_covers_one_repo_within_a_window() {
        let (_dir, pool, repo_id, run_id) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let store = PublishActionStore::new(&pool);

        for (n, minute) in [(0u32, 10u32), (1, 20), (2, 50)] {
            let action = store
                .insert(&an_action(run_id, "github", &format!("k{n}")))
                .await
                .unwrap_or_else(|e| panic!("insert: {e}"));
            store
                .record_outcome(
                    action.id,
                    PublishActionStatus::Sent,
                    None,
                    None,
                    None,
                    at(minute),
                )
                .await
                .unwrap_or_else(|e| panic!("record: {e}"));
        }

        assert_eq!(
            store
                .actions_sent_since(repo_id, at(0))
                .await
                .unwrap_or_else(|e| panic!("query: {e}")),
            3
        );
        assert_eq!(
            store
                .actions_sent_since(repo_id, at(30))
                .await
                .unwrap_or_else(|e| panic!("query: {e}")),
            1,
            "the window must exclude what fell out of it"
        );
        assert_eq!(
            store
                .actions_sent_since(RepoId::new(999), at(0))
                .await
                .unwrap_or_else(|e| panic!("query: {e}")),
            0,
            "one repo's burst must not escalate another's actions"
        );
    }

    #[tokio::test]
    async fn a_backoff_deadline_survives_a_restart() {
        // §11.6: 5 attempts with exponential backoff. `attempts` alone cannot say
        // WHEN the next attempt is due, so without this every pending action becomes
        // due the instant the process restarts — and the backoff that exists to stop
        // rev-local hammering a rate-limited target is defeated by exactly the event
        // most likely to follow a burst of failures.
        let (dir, pool, _, run_id) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let store = PublishActionStore::new(&pool);

        let action = store
            .insert(&an_action(run_id, "andare", "k"))
            .await
            .unwrap_or_else(|e| panic!("insert: {e}"));

        store
            .schedule_retry(action.id, at(5))
            .await
            .unwrap_or_else(|e| panic!("schedule: {e}"));

        let path = dir.path().join("rev-local.db");
        pool.close().await;

        // Reopen, as a restart would.
        let reopened = revlocal_store::open(&path)
            .await
            .unwrap_or_else(|e| panic!("reopen: {e}"));
        let due = PublishActionStore::new(&reopened)
            .next_attempt_at(action.id)
            .await
            .unwrap_or_else(|e| panic!("read: {e}"));

        assert_eq!(
            due,
            Some(at(5)),
            "the backoff deadline must survive a restart"
        );
    }

    // --- audit ---------------------------------------------------------------

    #[tokio::test]
    async fn audit_entries_append_and_read_back_in_order() {
        let (_dir, pool, repo_id, run_id) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let store = AuditStore::new(&pool);

        for (n, kind) in ["run.started", "run.reviewing", "run.done"]
            .iter()
            .enumerate()
        {
            store
                .append(&AuditEntry {
                    id: AuditId::new(0),
                    at: at(u32::try_from(n).unwrap_or(0)),
                    actor: "daemon".to_owned(),
                    kind: (*kind).to_owned(),
                    repo_id: Some(repo_id),
                    run_id: Some(run_id),
                    detail_json: "{}".to_owned(),
                })
                .await
                .unwrap_or_else(|e| panic!("append: {e}"));
        }

        let kinds: Vec<String> = store
            .list_for_run(run_id)
            .await
            .unwrap_or_else(|e| panic!("list: {e}"))
            .into_iter()
            .map(|e| e.kind)
            .collect();
        assert_eq!(kinds, ["run.started", "run.reviewing", "run.done"]);

        let recent = store
            .recent(2)
            .await
            .unwrap_or_else(|e| panic!("recent: {e}"));
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].kind, "run.done", "recent is newest first");
    }

    #[tokio::test]
    async fn an_audit_entry_can_stand_alone_without_a_repo_or_run() {
        // App-level events — the kill switch, a config reload — belong in the log
        // too, and they have no run behind them.
        let (_dir, pool, _, _) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let entry = AuditStore::new(&pool)
            .append(&AuditEntry {
                id: AuditId::new(0),
                at: at(0),
                actor: "user".to_owned(),
                kind: "kill_switch.engaged".to_owned(),
                repo_id: None,
                run_id: None,
                detail_json: "{}".to_owned(),
            })
            .await
            .unwrap_or_else(|e| panic!("append: {e}"));

        assert_eq!(entry.repo_id, None);
        assert_eq!(entry.run_id, None);
    }

    // --- budget ledger -------------------------------------------------------

    #[tokio::test]
    async fn budget_increments_accumulate_within_a_day() {
        let (_dir, pool, repo_id, _) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let store = BudgetLedgerStore::new(&pool);

        for _ in 0..3 {
            store
                .add_run(
                    repo_id,
                    "2026-08-27",
                    1,
                    &Usage {
                        tokens_in: 100,
                        tokens_out: 20,
                        tokens_known: true,
                        cost_usd: Some(0.05),
                    },
                )
                .await
                .unwrap_or_else(|e| panic!("add_run: {e}"));
        }

        let entry = store
            .get(repo_id, "2026-08-27")
            .await
            .unwrap_or_else(|e| panic!("get: {e}"))
            .unwrap_or_else(|| panic!("the day must exist after three runs"));

        assert_eq!(entry.runs, 3);
        assert_eq!(entry.usage.total_tokens(), 360);
        assert!(entry.cost_is_complete());
        assert!(
            (entry.known_cost_usd - 0.15).abs() < 1e-9,
            "{}",
            entry.known_cost_usd
        );
    }

    #[tokio::test]
    async fn days_are_independent() {
        let (_dir, pool, repo_id, _) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let store = BudgetLedgerStore::new(&pool);
        let usage = Usage {
            tokens_in: 10,
            tokens_out: 1,
            tokens_known: true,
            cost_usd: Some(0.01),
        };

        store
            .add_run(repo_id, "2026-08-27", 1, &usage)
            .await
            .unwrap_or_else(|e| panic!("day 1: {e}"));
        store
            .add_run(repo_id, "2026-08-28", 1, &usage)
            .await
            .unwrap_or_else(|e| panic!("day 2: {e}"));

        for day in ["2026-08-27", "2026-08-28"] {
            let entry = store
                .get(repo_id, day)
                .await
                .unwrap_or_else(|e| panic!("get: {e}"))
                .unwrap_or_else(|| panic!("{day} missing"));
            assert_eq!(entry.runs, 1, "{day} must not carry the other day's spend");
        }
    }

    #[tokio::test]
    async fn a_day_never_spent_on_is_none_rather_than_a_zero_row() {
        let (_dir, pool, repo_id, _) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let entry = BudgetLedgerStore::new(&pool)
            .get(repo_id, "2026-01-01")
            .await
            .unwrap_or_else(|e| panic!("get: {e}"));
        assert!(entry.is_none());
    }

    #[tokio::test]
    async fn an_unreported_cost_does_not_make_the_day_look_free() {
        // Decision D10 and SPEC §18. An engine that reports no price must not let
        // a cost budget read the day as cheap.
        let (_dir, pool, repo_id, _) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        let store = BudgetLedgerStore::new(&pool);

        store
            .add_run(
                repo_id,
                "2026-08-27",
                1,
                &Usage {
                    tokens_in: 100,
                    tokens_out: 10,
                    tokens_known: true,
                    cost_usd: Some(5.0),
                },
            )
            .await
            .unwrap_or_else(|e| panic!("priced run: {e}"));
        store
            .add_run(
                repo_id,
                "2026-08-27",
                1,
                &Usage {
                    tokens_in: 100,
                    tokens_out: 10,
                    tokens_known: true,
                    cost_usd: None,
                },
            )
            .await
            .unwrap_or_else(|e| panic!("unpriced run: {e}"));

        let entry = store
            .get(repo_id, "2026-08-27")
            .await
            .unwrap_or_else(|e| panic!("get: {e}"))
            .unwrap_or_else(|| panic!("day missing"));

        assert!(
            !entry.cost_is_complete(),
            "one unpriced run makes the day unmeasured"
        );
        assert_eq!(
            entry.usage.cost_usd, None,
            "an incomplete total must not be reported as one"
        );
        assert!(
            (entry.known_cost_usd - 5.0).abs() < 1e-9,
            "the costs that WERE reported are not discarded: {}",
            entry.known_cost_usd
        );

        // Tokens are known *here* — every run in this test reported counts — so
        // the token budget still holds. "Always known" was the assumption RL-409
        // disproved; it is a property of these runs, not of tokens.
        assert_eq!(entry.tokens_exhausted(220), Some(true));
        // The cost budget cannot answer, and "cannot tell" is not "not exhausted".
        assert_eq!(entry.cost_exhausted(100.0), None);
        // ...unless the known portion already passed it.
        assert_eq!(entry.cost_exhausted(4.0), Some(true));
    }

    #[tokio::test]
    async fn concurrent_increments_lose_nothing() {
        // SPEC §4.3 runs concurrently, so two runs finish at once routinely. A
        // read-then-write would lose one of their increments, and a budget that
        // under-counts is a budget that does not hold.
        let (_dir, pool, repo_id, _) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));

        let mut handles = Vec::new();
        for _ in 0..16 {
            let pool = pool.clone();
            handles.push(tokio::spawn(async move {
                BudgetLedgerStore::new(&pool)
                    .add_run(
                        repo_id,
                        "2026-08-27",
                        1,
                        &Usage {
                            tokens_in: 10,
                            tokens_out: 5,
                            tokens_known: true,
                            cost_usd: Some(0.25),
                        },
                    )
                    .await
            }));
        }
        for handle in handles {
            handle
                .await
                .unwrap_or_else(|e| panic!("task panicked: {e}"))
                .unwrap_or_else(|e| panic!("increment failed: {e}"));
        }

        let entry = BudgetLedgerStore::new(&pool)
            .get(repo_id, "2026-08-27")
            .await
            .unwrap_or_else(|e| panic!("get: {e}"))
            .unwrap_or_else(|| panic!("day missing"));

        assert_eq!(entry.runs, 16, "sixteen increments must count sixteen");
        assert_eq!(entry.usage.total_tokens(), 240);
        assert!(
            (entry.known_cost_usd - 4.0).abs() < 1e-9,
            "{}",
            entry.known_cost_usd
        );
    }

    #[tokio::test]
    async fn budget_rows_belong_to_their_repo_and_go_with_it() {
        let (_dir, pool, repo_id, _) = seeded().await.unwrap_or_else(|e| panic!("seed: {e}"));
        BudgetLedgerStore::new(&pool)
            .add_run(repo_id, "2026-08-27", 1, &Usage::default())
            .await
            .unwrap_or_else(|e| panic!("add_run: {e}"));

        RepoStore::new(&pool)
            .delete(repo_id)
            .await
            .unwrap_or_else(|e| panic!("delete: {e}"));

        let left = BudgetLedgerStore::new(&pool)
            .get(repo_id, "2026-08-27")
            .await
            .unwrap_or_else(|e| panic!("get: {e}"));
        assert!(left.is_none(), "the cascade must take the ledger with it");
    }
}
