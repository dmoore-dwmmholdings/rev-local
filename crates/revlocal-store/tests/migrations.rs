//! Acceptance tests for `RL-108` — the SQLite schema and its migrations.
//!
//! Wrapped in a `migrations` module so the item's gate,
//! `cargo test -p revlocal-store migrations`, selects them by name. A filter that
//! matches nothing exits 0, so a gate whose tests do not match its filter passes
//! while testing nothing.
//!
//! Every test uses a real file-backed database in a temp dir, never `:memory:`.
//! An in-memory SQLite database silently reports `journal_mode = memory` no matter
//! what is requested, so the WAL assertion would be vacuous against one.

mod migrations {
    use revlocal_store::{migrate, open, open_unmigrated, revert_to, Pool};
    use sqlx::Row;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// A fresh database directory, kept alive for the test's duration.
    ///
    /// These three helpers return `Result` rather than unwrapping: they are not
    /// `#[test]` fns, so clippy's unwrap/expect/panic ban still applies to them
    /// (ADR 0003). The callers do the unwrapping.
    fn scratch() -> std::io::Result<(TempDir, PathBuf)> {
        let dir = TempDir::new()?;
        let path = dir.path().join("rev-local.db");
        Ok((dir, path))
    }

    /// Every table SPEC §5 defines.
    const SPEC_TABLES: [&str; 9] = [
        "repo",
        "cursor",
        "change",
        "run",
        "finding",
        "suppression",
        "publish_action",
        "audit",
        "budget_ledger",
    ];

    async fn table_names(pool: &Pool) -> Result<Vec<String>, sqlx::Error> {
        Ok(
            sqlx::query("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
                .fetch_all(pool)
                .await?
                .into_iter()
                .map(|row| row.get::<String, _>("name"))
                .collect(),
        )
    }

    async fn column_names(pool: &Pool, table: &'static str) -> Result<Vec<String>, sqlx::Error> {
        // PRAGMA does not accept a bind parameter, so the table name has to be
        // interpolated. `table` is a `&'static str` from this file's own
        // constants — never user input — which is the audit sqlx 0.9's
        // `AssertSqlSafe` asks for.
        Ok(
            sqlx::query(sqlx::AssertSqlSafe(format!("PRAGMA table_info({table})")))
                .fetch_all(pool)
                .await?
                .into_iter()
                .map(|row| row.get::<String, _>("name"))
                .collect(),
        )
    }

    #[tokio::test]
    async fn creates_the_schema_from_empty() {
        let (_dir, path) = scratch().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let pool = open(&path).await.unwrap_or_else(|e| panic!("open: {e}"));

        let tables = table_names(&pool)
            .await
            .unwrap_or_else(|e| panic!("listing tables: {e}"));
        for expected in SPEC_TABLES {
            assert!(
                tables.iter().any(|t| t == expected),
                "SPEC §5 defines {expected}, but the schema has {tables:?}"
            );
        }
    }

    #[tokio::test]
    async fn journal_mode_is_wal() {
        let (_dir, path) = scratch().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let pool = open(&path).await.unwrap_or_else(|e| panic!("open: {e}"));

        let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&pool)
            .await
            .unwrap_or_else(|e| panic!("journal_mode: {e}"));
        assert_eq!(mode.to_lowercase(), "wal");
    }

    #[tokio::test]
    async fn foreign_keys_are_enforced_on_every_pooled_connection() {
        // SQLite defaults foreign_keys to OFF. Without it every ON DELETE CASCADE
        // in SPEC §5 is inert, and deleting a repo leaves orphaned runs behind.
        // Checked across several connections because the pool opens more of them
        // under load, and a pragma applied after connecting would be lost on those.
        let (_dir, path) = scratch().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let pool = open(&path).await.unwrap_or_else(|e| panic!("open: {e}"));

        for _ in 0..4 {
            let enabled: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
                .fetch_one(&pool)
                .await
                .unwrap_or_else(|e| panic!("foreign_keys: {e}"));
            assert_eq!(enabled, 1, "foreign keys must be on for every connection");
        }
    }

    #[tokio::test]
    async fn a_cascade_actually_cascades() {
        // The pragma being on is the mechanism; this is the behaviour it buys.
        let (_dir, path) = scratch().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let pool = open(&path).await.unwrap_or_else(|e| panic!("open: {e}"));

        sqlx::query(
            "INSERT INTO repo (id, name, kind, created_at, updated_at)
             VALUES (1, 'r', 'git', '2026-08-27T00:00:00Z', '2026-08-27T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap_or_else(|e| panic!("insert repo: {e}"));

        sqlx::query(
            "INSERT INTO change (id, repo_id, kind, external_id, detected_at)
             VALUES (1, 1, 'commit', 'deadbeef', '2026-08-27T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap_or_else(|e| panic!("insert change: {e}"));

        sqlx::query("DELETE FROM repo WHERE id = 1")
            .execute(&pool)
            .await
            .unwrap_or_else(|e| panic!("delete repo: {e}"));

        let orphans: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM change")
            .fetch_one(&pool)
            .await
            .unwrap_or_else(|e| panic!("count: {e}"));
        assert_eq!(
            orphans, 0,
            "deleting a repo must not leave its changes behind"
        );
    }

    #[tokio::test]
    async fn migrations_are_idempotent_when_re_run() {
        let (_dir, path) = scratch().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let pool = open(&path).await.unwrap_or_else(|e| panic!("open: {e}"));
        let before = table_names(&pool)
            .await
            .unwrap_or_else(|e| panic!("listing tables: {e}"));

        for _ in 0..3 {
            migrate(&pool)
                .await
                .unwrap_or_else(|e| panic!("re-running migrations must be a no-op: {e}"));
        }

        assert_eq!(
            table_names(&pool)
                .await
                .unwrap_or_else(|e| panic!("listing tables: {e}")),
            before
        );
    }

    #[tokio::test]
    async fn reopening_an_existing_database_does_not_re_migrate() {
        // The app calls open() on every start. That must be safe on a database
        // that already has data in it.
        let (_dir, path) = scratch().unwrap_or_else(|e| panic!("temp dir: {e}"));
        {
            let pool = open(&path)
                .await
                .unwrap_or_else(|e| panic!("first open: {e}"));
            sqlx::query(
                "INSERT INTO repo (id, name, kind, created_at, updated_at)
                 VALUES (1, 'kept', 'git', '2026-08-27T00:00:00Z', '2026-08-27T00:00:00Z')",
            )
            .execute(&pool)
            .await
            .unwrap_or_else(|e| panic!("insert: {e}"));
            pool.close().await;
        }

        let pool = open(&path)
            .await
            .unwrap_or_else(|e| panic!("second open: {e}"));
        let name: String = sqlx::query_scalar("SELECT name FROM repo WHERE id = 1")
            .fetch_one(&pool)
            .await
            .unwrap_or_else(|e| panic!("select: {e}"));
        assert_eq!(name, "kept", "reopening must not have wiped the database");
    }

    #[tokio::test]
    async fn the_down_migration_path_works_and_can_be_re_applied() {
        let (_dir, path) = scratch().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let pool = open(&path).await.unwrap_or_else(|e| panic!("open: {e}"));
        assert!(!table_names(&pool)
            .await
            .unwrap_or_else(|e| panic!("listing tables: {e}"))
            .is_empty());

        revert_to(&pool, 0)
            .await
            .unwrap_or_else(|e| panic!("down migration: {e}"));

        let after = table_names(&pool)
            .await
            .unwrap_or_else(|e| panic!("listing tables: {e}"));
        for gone in SPEC_TABLES {
            assert!(
                !after.iter().any(|t| t == gone),
                "{gone} survived the down migration: {after:?}"
            );
        }

        // A down migration that cannot be followed by an up migration is a dead
        // end, not a path.
        migrate(&pool)
            .await
            .unwrap_or_else(|e| panic!("re-applying after revert: {e}"));
        for expected in SPEC_TABLES {
            assert!(table_names(&pool)
                .await
                .unwrap_or_else(|e| panic!("listing tables: {e}"))
                .iter()
                .any(|t| t == expected));
        }
    }

    #[tokio::test]
    async fn publish_action_rejects_a_duplicate_idempotency_key() {
        // SPEC §11.6: at-least-once delivery with exactly-once effect. This
        // constraint is the mechanism, not an optimisation — without it a retry
        // becomes a second issue in somebody's tracker.
        let (_dir, path) = scratch().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let pool = open(&path).await.unwrap_or_else(|e| panic!("open: {e}"));

        sqlx::query(
            "INSERT INTO repo (id, name, kind, created_at, updated_at)
             VALUES (1, 'r', 'git', '2026-08-27T00:00:00Z', '2026-08-27T00:00:00Z');
             INSERT INTO change (id, repo_id, kind, external_id, detected_at)
             VALUES (1, 1, 'commit', 'deadbeef', '2026-08-27T00:00:00Z');
             INSERT INTO run (id, change_id, status, engine, depth, trigger, created_at)
             VALUES (1, 1, 'done', 'mock', 'standard', 'manual', '2026-08-27T00:00:00Z');",
        )
        .execute(&pool)
        .await
        .unwrap_or_else(|e| panic!("seed: {e}"));

        let insert = |id: i64| {
            sqlx::query(
                "INSERT INTO publish_action
                   (id, run_id, target, capability, risk, idempotency_key, payload_json,
                    status, created_at)
                 VALUES (?, 1, 'andare', 'create_issue', 'high', 'andare:REVL:abc', '{}',
                         'pending', '2026-08-27T00:00:00Z')",
            )
            .bind(id)
            .execute(&pool)
        };

        insert(1)
            .await
            .unwrap_or_else(|e| panic!("first insert: {e}"));
        let duplicate = insert(2).await;
        assert!(
            duplicate.is_err(),
            "a second action with the same (target, idempotency_key) must be rejected"
        );

        // The same key against a *different* target is a different action.
        sqlx::query(
            "INSERT INTO publish_action
               (id, run_id, target, capability, risk, idempotency_key, payload_json,
                status, created_at)
             VALUES (3, 1, 'github', 'comment', 'low', 'andare:REVL:abc', '{}',
                     'pending', '2026-08-27T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap_or_else(|e| panic!("same key, different target must be allowed: {e}"));
    }

    #[tokio::test]
    async fn the_run_table_carries_the_degraded_column() {
        // Added by RL-103b under the SPEC §5 implementation note (ADR 0005).
        // §12.3 escalates every action on a degraded run to high risk, so a schema
        // without this column silently loses the reason for every escalation.
        let (_dir, path) = scratch().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let pool = open(&path).await.unwrap_or_else(|e| panic!("open: {e}"));

        let columns = column_names(&pool, "run")
            .await
            .unwrap_or_else(|e| panic!("table_info: {e}"));
        assert!(
            columns.iter().any(|c| c == "degraded"),
            "run.degraded is missing; columns are {columns:?}"
        );
    }

    #[tokio::test]
    async fn check_constraints_reject_values_outside_the_domain_enums() {
        // The CHECK lists and the Rust enums' wire spellings have to agree
        // (ADR 0004). This asserts the SQL half actually rejects.
        let (_dir, path) = scratch().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let pool = open(&path).await.unwrap_or_else(|e| panic!("open: {e}"));

        let bad_kind = sqlx::query(
            "INSERT INTO repo (id, name, kind, created_at, updated_at)
             VALUES (1, 'r', 'mercurial', '2026-08-27T00:00:00Z', '2026-08-27T00:00:00Z')",
        )
        .execute(&pool)
        .await;
        assert!(
            bad_kind.is_err(),
            "repo.kind must reject a VCS the app does not support"
        );
    }

    #[tokio::test]
    async fn every_spec_5_enum_column_accepts_exactly_its_rust_wire_spellings() {
        // Pairs each CHECK-constrained column with the enum that owns it. A
        // variant added in Rust without a migration fails here rather than at the
        // first insert in production.
        use revlocal_core::{AutonomyMode, Depth, RepoKind, RiskClass, Severity, TriggerSource};
        let (_dir, path) = scratch().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let pool = open(&path).await.unwrap_or_else(|e| panic!("open: {e}"));

        sqlx::query(
            "INSERT INTO repo (id, name, kind, created_at, updated_at)
             VALUES (1, 'r', 'git', '2026-08-27T00:00:00Z', '2026-08-27T00:00:00Z');
             INSERT INTO change (id, repo_id, kind, external_id, detected_at)
             VALUES (1, 1, 'commit', 'deadbeef', '2026-08-27T00:00:00Z');",
        )
        .execute(&pool)
        .await
        .unwrap_or_else(|e| panic!("seed: {e}"));

        for (index, depth) in Depth::ALL.iter().enumerate() {
            for trigger in TriggerSource::ALL {
                let id = (index as i64 + 1) * 100 + trigger.as_str().len() as i64;
                let inserted = sqlx::query(
                    "INSERT OR REPLACE INTO run
                       (id, change_id, attempt, status, engine, depth, trigger, created_at)
                     VALUES (?, 1, ?, 'queued', 'mock', ?, ?, '2026-08-27T00:00:00Z')",
                )
                .bind(id)
                .bind(id)
                .bind(depth.as_str())
                .bind(trigger.as_str())
                .execute(&pool)
                .await;
                assert!(
                    inserted.is_ok(),
                    "run.depth/trigger rejected the Rust spelling ({depth}, {trigger})"
                );
            }
        }

        for kind in RepoKind::ALL {
            let inserted = sqlx::query(
                "INSERT OR REPLACE INTO repo (id, name, kind, created_at, updated_at)
                 VALUES (99, ?, ?, '2026-08-27T00:00:00Z', '2026-08-27T00:00:00Z')",
            )
            .bind(kind.as_str())
            .bind(kind.as_str())
            .execute(&pool)
            .await;
            assert!(
                inserted.is_ok(),
                "repo.kind rejected the Rust spelling {kind}"
            );
        }

        for mode in AutonomyMode::ALL {
            let inserted = sqlx::query(
                "INSERT OR REPLACE INTO repo (id, name, kind, autonomy, created_at, updated_at)
                 VALUES (98, 'autonomy-probe', 'git', ?, '2026-08-27T00:00:00Z', '2026-08-27T00:00:00Z')",
            )
            .bind(mode.as_str())
            .execute(&pool)
            .await;
            assert!(
                inserted.is_ok(),
                "repo.autonomy rejected the Rust spelling {mode}"
            );
        }

        sqlx::query(
            "INSERT INTO run (id, change_id, attempt, status, engine, depth, trigger, created_at)
             VALUES (1, 1, 1, 'done', 'mock', 'standard', 'manual', '2026-08-27T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap_or_else(|e| panic!("seed run: {e}"));

        for (index, severity) in Severity::ALL.iter().enumerate() {
            let inserted = sqlx::query(
                "INSERT INTO finding
                   (id, run_id, fingerprint, severity, category, title, body, created_at)
                 VALUES (?, 1, 'fp', ?, 'correctness', 't', 'b', '2026-08-27T00:00:00Z')",
            )
            .bind(index as i64 + 1)
            .bind(severity.as_str())
            .execute(&pool)
            .await;
            assert!(inserted.is_ok(), "finding.severity rejected {severity}");
        }

        for (index, risk) in RiskClass::ALL.iter().enumerate() {
            let inserted = sqlx::query(
                "INSERT INTO publish_action
                   (id, run_id, target, capability, risk, idempotency_key, payload_json,
                    status, created_at)
                 VALUES (?, 1, 'github', 'comment', ?, ?, '{}', 'pending', '2026-08-27T00:00:00Z')",
            )
            .bind(index as i64 + 1)
            .bind(risk.as_str())
            .bind(format!("key-{risk}"))
            .execute(&pool)
            .await;
            assert!(inserted.is_ok(), "publish_action.risk rejected {risk}");
        }
    }

    #[tokio::test]
    async fn an_unmigrated_database_has_no_spec_tables() {
        // Guards the test above from passing vacuously: if open_unmigrated also
        // produced the schema, "creates the schema from empty" would prove nothing.
        let (_dir, path) = scratch().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let pool = open_unmigrated(&path)
            .await
            .unwrap_or_else(|e| panic!("open: {e}"));
        let tables = table_names(&pool)
            .await
            .unwrap_or_else(|e| panic!("listing tables: {e}"));
        for absent in SPEC_TABLES {
            assert!(
                !tables.iter().any(|t| t == absent),
                "{absent} exists before migrating"
            );
        }
    }
}
