//! CRUD round-trip tests for `RL-109a` — the `repo` and `cursor` repositories.

mod repos {
    use chrono::TimeZone;
    use revlocal_core::{AutonomyMode, Cursor, EngineKind, Repo, RepoId, RepoKind, Timestamp};
    use revlocal_store::{open, CursorStore, Pool, RepoStore, StoreError};
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn scratch() -> std::io::Result<(TempDir, PathBuf)> {
        let dir = TempDir::new()?;
        let path = dir.path().join("rev-local.db");
        Ok((dir, path))
    }

    fn at(minute: u32) -> Timestamp {
        chrono::Utc
            .with_ymd_and_hms(2026, 8, 27, 12, minute, 0)
            .single()
            .unwrap_or_default()
    }

    fn a_repo(name: &str) -> Repo {
        Repo {
            id: RepoId::new(0),
            name: name.to_owned(),
            kind: RepoKind::Git,
            local_path: Some("/srv/rev-local".to_owned()),
            remote_url: None,
            default_branch: Some("main".to_owned()),
            engine: EngineKind::Mock,
            autonomy: AutonomyMode::DryRun,
            enabled: true,
            config_json: "{}".to_owned(),
            created_at: at(0),
            updated_at: at(0),
        }
    }

    /// A migrated, file-backed database in a temp dir.
    ///
    /// Returns `Result`; helpers are not `#[test]` fns, so clippy's
    /// unwrap/expect/panic ban still applies to them (ADR 0003).
    async fn db() -> Result<(TempDir, Pool), Box<dyn std::error::Error>> {
        let (dir, path) = scratch()?;
        let pool = open(&path).await?;
        Ok((dir, pool))
    }

    #[tokio::test]
    async fn a_repo_round_trips_through_insert_and_get() {
        let (_dir, pool) = db().await.unwrap_or_else(|e| panic!("open db: {e}"));
        let store = RepoStore::new(&pool);

        let inserted = store
            .insert(&a_repo("rev-local"))
            .await
            .unwrap_or_else(|e| panic!("insert: {e}"));
        assert_ne!(inserted.id, RepoId::new(0), "the database assigns the id");

        let fetched = store
            .get(inserted.id)
            .await
            .unwrap_or_else(|e| panic!("get: {e}"));
        assert_eq!(fetched, inserted, "what came back must be what went in");
    }

    #[tokio::test]
    async fn every_field_survives_the_round_trip_including_the_optional_ones() {
        // A None that comes back as Some("") — or vice versa — is the kind of bug
        // a shallow round-trip test misses.
        let (_dir, pool) = db().await.unwrap_or_else(|e| panic!("open db: {e}"));
        let store = RepoStore::new(&pool);

        let sparse = Repo {
            local_path: None,
            remote_url: None,
            default_branch: None,
            enabled: false,
            ..a_repo("sparse")
        };
        let back = store
            .get(
                store
                    .insert(&sparse)
                    .await
                    .unwrap_or_else(|e| panic!("insert: {e}"))
                    .id,
            )
            .await
            .unwrap_or_else(|e| panic!("get: {e}"));

        assert_eq!(back.local_path, None);
        assert_eq!(back.remote_url, None);
        assert_eq!(back.default_branch, None);
        assert!(!back.enabled, "a false bool must not come back as true");
    }

    #[tokio::test]
    async fn a_duplicate_name_is_a_typed_already_exists_not_a_raw_sqlx_error() {
        let (_dir, pool) = db().await.unwrap_or_else(|e| panic!("open db: {e}"));
        let store = RepoStore::new(&pool);

        store
            .insert(&a_repo("dup"))
            .await
            .unwrap_or_else(|e| panic!("first insert: {e}"));
        let error = store
            .insert(&a_repo("dup"))
            .await
            .expect_err("a second repo with the same name must be refused");

        assert!(error.is_already_exists(), "got {error:?}");
        assert!(error.to_string().contains("dup"), "{error}");
    }

    #[tokio::test]
    async fn update_changes_the_mutable_fields_and_leaves_identity_alone() {
        let (_dir, pool) = db().await.unwrap_or_else(|e| panic!("open db: {e}"));
        let store = RepoStore::new(&pool);
        let mut repo = store
            .insert(&a_repo("rev-local"))
            .await
            .unwrap_or_else(|e| panic!("insert: {e}"));

        repo.autonomy = AutonomyMode::Off;
        repo.enabled = false;
        repo.updated_at = at(5);
        store
            .update(&repo)
            .await
            .unwrap_or_else(|e| panic!("update: {e}"));

        let back = store
            .get(repo.id)
            .await
            .unwrap_or_else(|e| panic!("get: {e}"));
        assert_eq!(back.autonomy, AutonomyMode::Off);
        assert!(!back.enabled);
        assert_eq!(back.updated_at, at(5));
        assert_eq!(
            back.name, "rev-local",
            "the name is identity and must not move"
        );
    }

    #[tokio::test]
    async fn addressing_a_missing_row_is_not_found_rather_than_an_empty_success() {
        let (_dir, pool) = db().await.unwrap_or_else(|e| panic!("open db: {e}"));
        let store = RepoStore::new(&pool);

        let missing = RepoId::new(404);
        assert!(matches!(
            store.get(missing).await,
            Err(StoreError::NotFound { entity: "repo", .. })
        ));
        assert!(matches!(
            store.delete(missing).await,
            Err(StoreError::NotFound { .. })
        ));
        assert!(
            matches!(
                store
                    .update(&Repo {
                        id: missing,
                        ..a_repo("ghost")
                    })
                    .await,
                Err(StoreError::NotFound { .. })
            ),
            "updating nothing must not report success"
        );
    }

    #[tokio::test]
    async fn deleting_a_repo_removes_its_cursors_too() {
        let (_dir, pool) = db().await.unwrap_or_else(|e| panic!("open db: {e}"));
        let repos = RepoStore::new(&pool);
        let cursors = CursorStore::new(&pool);

        let repo = repos
            .insert(&a_repo("rev-local"))
            .await
            .unwrap_or_else(|e| panic!("insert: {e}"));
        cursors
            .advance(repo.id, &Cursor::commits_scope("main"), "deadbeef", at(0))
            .await
            .unwrap_or_else(|e| panic!("advance: {e}"));

        repos
            .delete(repo.id)
            .await
            .unwrap_or_else(|e| panic!("delete: {e}"));

        let left = cursors
            .list_for_repo(repo.id)
            .await
            .unwrap_or_else(|e| panic!("list: {e}"));
        assert!(left.is_empty(), "the cascade must take the cursors with it");
    }

    #[tokio::test]
    async fn list_returns_every_repo_oldest_first() {
        let (_dir, pool) = db().await.unwrap_or_else(|e| panic!("open db: {e}"));
        let store = RepoStore::new(&pool);
        for name in ["alpha", "beta", "gamma"] {
            store
                .insert(&a_repo(name))
                .await
                .unwrap_or_else(|e| panic!("insert {name}: {e}"));
        }

        let names: Vec<String> = store
            .list()
            .await
            .unwrap_or_else(|e| panic!("list: {e}"))
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(names, ["alpha", "beta", "gamma"]);
    }

    // --- cursors ------------------------------------------------------------

    #[tokio::test]
    async fn a_cursor_that_never_advanced_is_none_not_empty() {
        // "Never looked" means backfill from the beginning; "looked and found
        // nothing" means do nothing. Collapsing them would re-review history.
        let (_dir, pool) = db().await.unwrap_or_else(|e| panic!("open db: {e}"));
        let repo = RepoStore::new(&pool)
            .insert(&a_repo("rev-local"))
            .await
            .unwrap_or_else(|e| panic!("insert: {e}"));

        let cursor = CursorStore::new(&pool)
            .get(repo.id, &Cursor::commits_scope("main"))
            .await
            .unwrap_or_else(|e| panic!("get: {e}"));
        assert!(cursor.is_none());
    }

    #[tokio::test]
    async fn a_cursor_round_trips_and_advances_in_place() {
        let (_dir, pool) = db().await.unwrap_or_else(|e| panic!("open db: {e}"));
        let repo = RepoStore::new(&pool)
            .insert(&a_repo("rev-local"))
            .await
            .unwrap_or_else(|e| panic!("insert: {e}"));
        let cursors = CursorStore::new(&pool);
        let scope = Cursor::commits_scope("main");

        cursors
            .advance(repo.id, &scope, "aaa", at(0))
            .await
            .unwrap_or_else(|e| panic!("first advance: {e}"));
        cursors
            .advance(repo.id, &scope, "bbb", at(1))
            .await
            .unwrap_or_else(|e| panic!("second advance: {e}"));

        let cursor = cursors
            .get(repo.id, &scope)
            .await
            .unwrap_or_else(|e| panic!("get: {e}"))
            .unwrap_or_else(|| panic!("cursor must exist after advancing"));
        assert_eq!(cursor.value, "bbb");
        assert_eq!(cursor.updated_at, at(1));
        assert_eq!(
            cursors
                .list_for_repo(repo.id)
                .await
                .unwrap_or_else(|e| panic!("list: {e}"))
                .len(),
            1,
            "advancing must update the row, not insert a second one"
        );
    }

    #[tokio::test]
    async fn scopes_are_independent_of_one_another() {
        let (_dir, pool) = db().await.unwrap_or_else(|e| panic!("open db: {e}"));
        let repo = RepoStore::new(&pool)
            .insert(&a_repo("rev-local"))
            .await
            .unwrap_or_else(|e| panic!("insert: {e}"));
        let cursors = CursorStore::new(&pool);

        for (scope, value) in [
            (Cursor::commits_scope("main"), "aaa"),
            (Cursor::commits_scope("release"), "bbb"),
            (Cursor::PRS_SCOPE.to_owned(), "2026-08-27T00:00:00Z"),
            (Cursor::svn_scope("/trunk"), "r1234"),
        ] {
            cursors
                .advance(repo.id, &scope, value, at(0))
                .await
                .unwrap_or_else(|e| panic!("advance {scope}: {e}"));
        }

        let all = cursors
            .list_for_repo(repo.id)
            .await
            .unwrap_or_else(|e| panic!("list: {e}"));
        assert_eq!(all.len(), 4, "one cursor per scope, not one per repo");

        let main = cursors
            .get(repo.id, &Cursor::commits_scope("main"))
            .await
            .unwrap_or_else(|e| panic!("get: {e}"))
            .unwrap_or_else(|| panic!("main cursor missing"));
        assert_eq!(main.value, "aaa", "advancing release must not move main");
    }

    #[tokio::test]
    async fn concurrent_advances_leave_exactly_one_row_and_one_winner() {
        // A poll and a webhook can discover the same branch at the same moment.
        // A read-then-write would let one silently overwrite the other's advance;
        // the upsert makes the last writer win with no lost row and no duplicate.
        let (_dir, pool) = db().await.unwrap_or_else(|e| panic!("open db: {e}"));
        let repo = RepoStore::new(&pool)
            .insert(&a_repo("rev-local"))
            .await
            .unwrap_or_else(|e| panic!("insert: {e}"));
        let scope = Cursor::commits_scope("main");

        let mut handles = Vec::new();
        for n in 0..8u32 {
            let pool = pool.clone();
            let scope = scope.clone();
            let id = repo.id;
            handles.push(tokio::spawn(async move {
                CursorStore::new(&pool)
                    .advance(id, &scope, &format!("sha-{n}"), at(n))
                    .await
            }));
        }
        for handle in handles {
            handle
                .await
                .unwrap_or_else(|e| panic!("task panicked: {e}"))
                .unwrap_or_else(|e| panic!("concurrent advance failed: {e}"));
        }

        let cursors = CursorStore::new(&pool);
        let all = cursors
            .list_for_repo(repo.id)
            .await
            .unwrap_or_else(|e| panic!("list: {e}"));
        assert_eq!(
            all.len(),
            1,
            "eight concurrent advances must leave one cursor"
        );
        assert!(
            all[0].value.starts_with("sha-"),
            "the surviving value must be one that was actually written: {}",
            all[0].value
        );
    }

    #[tokio::test]
    async fn a_cursor_for_an_unknown_repo_is_refused_by_the_foreign_key() {
        let (_dir, pool) = db().await.unwrap_or_else(|e| panic!("open db: {e}"));
        let result = CursorStore::new(&pool)
            .advance(RepoId::new(404), "commits:main", "aaa", at(0))
            .await;
        assert!(
            result.is_err(),
            "a cursor must not outlive — or precede — its repo"
        );
    }
}
