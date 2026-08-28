//! Acceptance tests for `RL-304` — recovery from a force-pushed branch.
//!
//! The scenario this protects against is ordinary: someone rebases a branch and
//! force-pushes. Every commit above the merge-base comes back with a new SHA while
//! being *the same work*. Resetting the cursor to the merge-base is necessary — it
//! stops the commits below being replayed — but it is **not sufficient**, because
//! the rebased commits above it look brand new. Reviewing them again re-files every
//! finding on them, so a rebase would spam the tracker.
//!
//! The content hash is what closes that. `git patch-id --stable` normalises line
//! numbers and whitespace away, so the same change on a different base hashes the
//! same, which is exactly the property a rebase preserves and a SHA does not.

mod git_force_push {
    use revlocal_vcs::git::{
        classify_cursor, discover_branch, mark_superseded_by_rewrite, patch_ids, CursorState,
        DiscoveryEvent,
    };
    use revlocal_vcs::GitRunner;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use tempfile::TempDir;

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
    }

    /// Run git in `dir`, failing loudly. Test-local: the choke-point rule covers
    /// production code, and this arranges a repository rather than reviewing one.
    fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "T")
            .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
            .env("GIT_COMMITTER_NAME", "T")
            .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
            .env("GIT_AUTHOR_DATE", "1735689600 +0000")
            .env("GIT_COMMITTER_DATE", "1735689600 +0000")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .map_err(|e| format!("git {args:?}: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    /// A fixture repository.
    fn fixture() -> Result<(TempDir, PathBuf), String> {
        let dir = TempDir::new().map_err(|e| format!("temp dir: {e}"))?;
        let root = workspace_root();
        let output = Command::new(revlocal_vcs::bash_program())
            .arg(root.join("fixtures/build.sh"))
            .arg("--out")
            .arg(dir.path())
            .current_dir(&root)
            .output()
            .map_err(|e| format!("build.sh: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "build.sh: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let repo = dir.path().join("git-basic");
        Ok((dir, repo))
    }

    /// Build the realistic rewrite: `n` linear feature commits, then a rebase that
    /// replays them onto a new base.
    ///
    /// The commits are created here rather than taken from the fixture's tip
    /// because the fixture's recent history contains a merge, and a merge cannot be
    /// cherry-picked without choosing a parent. A rebase of a feature branch — which
    /// is the case this whole item is about — is linear by construction.
    ///
    /// The result is what a force-push looks like: the same work, new SHAs, plus one
    /// commit that is genuinely new.
    fn rebase_top(repo: &Path, n: usize) -> Result<RewriteFixture, String> {
        let base = git(repo, &["rev-parse", "HEAD"])?;

        // The "feature work" that will later be rebased.
        for index in 0..n {
            std::fs::write(
                repo.join(format!("feature_{index}.txt")),
                format!("work {index}\n"),
            )
            .map_err(|e| format!("write: {e}"))?;
            git(repo, &["add", "-A"])?;
            git(
                repo,
                &[
                    "commit",
                    "--quiet",
                    "--no-gpg-sign",
                    "-m",
                    &format!("Feature work {index}"),
                ],
            )?;
        }

        let old_tip = git(repo, &["rev-parse", "HEAD"])?;
        let replayed: Vec<String> =
            git(repo, &["rev-list", "--reverse", &format!("{base}..HEAD")])?
                .lines()
                .map(str::to_owned)
                .collect();

        // The rewrite: drop back to the base, insert a commit underneath, replay.
        git(repo, &["reset", "--hard", "--quiet", &base])?;
        std::fs::write(repo.join("inserted.txt"), "a commit inserted by the rebase")
            .map_err(|e| format!("write: {e}"))?;
        git(repo, &["add", "-A"])?;
        git(
            repo,
            &[
                "commit",
                "--quiet",
                "--no-gpg-sign",
                "-m",
                "Inserted before the replay",
            ],
        )?;

        for sha in &replayed {
            git(repo, &["cherry-pick", "--no-gpg-sign", sha])?;
        }

        let new_tip = git(repo, &["rev-parse", "HEAD"])?;
        Ok(RewriteFixture {
            old_tip,
            base,
            replayed,
            new_tip,
        })
    }

    struct RewriteFixture {
        old_tip: String,
        base: String,
        replayed: Vec<String>,
        new_tip: String,
    }

    #[tokio::test]
    async fn git_force_push_the_rewrite_is_detected_and_resets_to_the_merge_base() {
        let (_dir, repo) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let rewrite = rebase_top(&repo, 3).unwrap_or_else(|e| panic!("rebase: {e}"));
        let runner = GitRunner::new();

        assert_ne!(
            rewrite.old_tip, rewrite.new_tip,
            "the rewrite must have changed the tip"
        );

        let state = classify_cursor(&runner, &repo, "main", Some(&rewrite.old_tip))
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        match &state {
            CursorState::Rewritten {
                old_cursor,
                merge_base,
            } => {
                assert_eq!(old_cursor, &rewrite.old_tip);
                assert_eq!(
                    merge_base, &rewrite.base,
                    "the merge-base is the newest commit both histories still share"
                );
            }
            other => panic!("expected a rewrite, got {other:?}"),
        }
        assert_eq!(state.effective(), Some(rewrite.base.as_str()));
    }

    #[tokio::test]
    async fn git_force_push_writes_an_audit_row() {
        // REVL-33's second criterion. `revlocal-vcs` does not depend on
        // `revlocal-store` in production — recovery returns events and the caller
        // records them (ADR 0013) — so this test plays the caller, to prove the
        // event carries everything an audit row needs.
        let (_dir, repo) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let rewrite = rebase_top(&repo, 3).unwrap_or_else(|e| panic!("rebase: {e}"));
        let runner = GitRunner::new();

        let state = classify_cursor(&runner, &repo, "main", Some(&rewrite.old_tip))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let event = state
            .event("main")
            .unwrap_or_else(|| panic!("a rewrite must produce an event"));
        assert_eq!(event.audit_kind(), "history_rewritten");

        let db_dir = TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let pool = revlocal_store::open(&db_dir.path().join("rev-local.db"))
            .await
            .unwrap_or_else(|e| panic!("open db: {e}"));

        let detail = match &event {
            DiscoveryEvent::HistoryRewritten {
                branch,
                old_cursor,
                reset_to,
            } => {
                format!(
                    r#"{{"branch":"{branch}","old_cursor":"{old_cursor}","reset_to":"{reset_to}"}}"#
                )
            }
            other => panic!("wrong event: {other:?}"),
        };

        let at = chrono::DateTime::parse_from_rfc3339("2026-08-27T12:00:00Z")
            .map(|t| t.with_timezone(&chrono::Utc))
            .unwrap_or_default();

        revlocal_store::AuditStore::new(&pool)
            .append(&revlocal_core::AuditEntry {
                id: revlocal_core::AuditId::new(0),
                at,
                actor: "daemon".to_owned(),
                kind: event.audit_kind().to_owned(),
                repo_id: None,
                run_id: None,
                detail_json: detail,
            })
            .await
            .unwrap_or_else(|e| panic!("append audit: {e}"));

        let rows = revlocal_store::AuditStore::new(&pool)
            .recent(10)
            .await
            .unwrap_or_else(|e| panic!("recent: {e}"));

        assert_eq!(rows.len(), 1, "an audit row must exist");
        assert_eq!(rows[0].kind, "history_rewritten");
        assert!(
            rows[0].detail_json.contains(&rewrite.old_tip),
            "the row must record where the cursor WAS: {}",
            rows[0].detail_json
        );
        assert!(
            rows[0].detail_json.contains(&rewrite.base),
            "and where it was reset to: {}",
            rows[0].detail_json
        );
    }

    #[tokio::test]
    async fn git_force_push_rebased_commits_are_recognised_by_content_not_by_sha() {
        // The heart of it. Every replayed commit has a NEW sha and the SAME content.
        let (_dir, repo) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let rewrite = rebase_top(&repo, 3).unwrap_or_else(|e| panic!("rebase: {e}"));
        let runner = GitRunner::new();

        let new_shas: Vec<String> = git(&repo, &["rev-list", &format!("{}..HEAD", rewrite.base)])
            .unwrap_or_else(|e| panic!("{e}"))
            .lines()
            .map(str::to_owned)
            .collect();

        for sha in &rewrite.replayed {
            assert!(
                !new_shas.contains(sha),
                "a rebase must produce new SHAs, but {sha} survived"
            );
        }

        let old_ids = patch_ids(&runner, &repo, &rewrite.replayed)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let new_ids = patch_ids(&runner, &repo, &new_shas)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        let old_contents: std::collections::BTreeSet<&String> = old_ids.values().collect();
        let matched = new_ids
            .values()
            .filter(|id| old_contents.contains(id))
            .count();

        assert_eq!(
            matched,
            rewrite.replayed.len(),
            "every replayed commit should hash to the same content as before the rebase"
        );
    }

    #[tokio::test]
    async fn git_force_push_recovery_does_not_re_review_what_survived_the_rewrite() {
        // REVL-33's third criterion, and the failure the whole item exists to
        // prevent: a rebase re-filing every finding on every commit it moved.
        let (_dir, repo) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let rewrite = rebase_top(&repo, 3).unwrap_or_else(|e| panic!("rebase: {e}"));
        let runner = GitRunner::new();

        let state = classify_cursor(&runner, &repo, "main", Some(&rewrite.old_tip))
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        let mut discovered = discover_branch(&runner, &repo, "main", state.effective(), 1000)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        // Merge-base reset alone brings back everything above the base: the inserted
        // commit AND all three replays. That is the state this criterion rejects.
        assert_eq!(
            discovered.len(),
            rewrite.replayed.len() + 1,
            "merge-base reset alone re-discovers the replayed commits"
        );
        assert!(
            discovered.iter().all(|c| c.skip_reason.is_none()),
            "nothing is marked yet"
        );

        let events = mark_superseded_by_rewrite(
            &runner,
            &repo,
            &rewrite.old_tip,
            &rewrite.base,
            &mut discovered,
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            events.is_empty(),
            "the old commits are still reachable, so dedupe worked"
        );

        let superseded = discovered.iter().filter(|c| c.is_skipped()).count();
        let to_review = discovered.iter().filter(|c| !c.is_skipped()).count();

        assert_eq!(
            superseded,
            rewrite.replayed.len(),
            "every replayed commit must be recognised as already reviewed"
        );
        assert_eq!(to_review, 1, "only the genuinely new commit is reviewed");

        // A skip must SAY WHY (SPEC §18), and name the commit it duplicates — an
        // operator wondering why a rebased commit has no review needs that line.
        let reason = discovered
            .iter()
            .find(|c| c.is_skipped())
            .and_then(|c| c.skip_reason.clone())
            .unwrap_or_default();
        assert!(reason.starts_with("unchanged_after_rewrite:"), "{reason}");
        assert!(
            rewrite.replayed.iter().any(|sha| reason.contains(sha)),
            "the reason must name the already-reviewed commit: {reason}"
        );
    }

    #[tokio::test]
    async fn git_force_push_a_genuinely_new_commit_is_still_reviewed() {
        // The other half: dedupe must not swallow real work. A rewrite that also
        // introduced a change has to be reviewed, or a rebase becomes a way to land
        // code unreviewed.
        let (_dir, repo) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let rewrite = rebase_top(&repo, 3).unwrap_or_else(|e| panic!("rebase: {e}"));
        let runner = GitRunner::new();

        let mut discovered = discover_branch(&runner, &repo, "main", Some(&rewrite.base), 1000)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        mark_superseded_by_rewrite(
            &runner,
            &repo,
            &rewrite.old_tip,
            &rewrite.base,
            &mut discovered,
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));

        let reviewable: Vec<&str> = discovered
            .iter()
            .filter(|c| !c.is_skipped())
            .filter_map(|c| c.title.as_deref())
            .collect();

        assert_eq!(
            reviewable,
            ["Inserted before the replay"],
            "the commit the rebase introduced must still be reviewed"
        );
    }

    #[tokio::test]
    async fn git_force_push_dedupe_being_impossible_is_reported_not_assumed() {
        // If the pre-rewrite commits are gone there is nothing to compare against,
        // so everything above the merge-base is reviewed again. That is the right
        // behaviour and the wrong thing to do silently: an operator seeing a burst
        // of re-reviews needs the log line (SPEC §18).
        let (_dir, repo) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let rewrite = rebase_top(&repo, 3).unwrap_or_else(|e| panic!("rebase: {e}"));
        let runner = GitRunner::new();

        let mut discovered = discover_branch(&runner, &repo, "main", Some(&rewrite.base), 1000)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        // A cursor that no longer resolves stands in for a gc'd history.
        let events = mark_superseded_by_rewrite(
            &runner,
            &repo,
            "0123456789abcdef0123456789abcdef01234567",
            &rewrite.base,
            &mut discovered,
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(events.len(), 1, "the inability to dedupe must be recorded");
        assert_eq!(events[0].audit_kind(), "rewrite_dedupe_unavailable");
        assert!(
            discovered.iter().all(|c| c.skip_reason.is_none()),
            "with nothing to compare against, nothing may be skipped"
        );
    }

    #[tokio::test]
    async fn git_force_push_an_empty_diff_does_not_match_every_other_empty_diff() {
        // patch-id reports all-zeroes for an empty diff. Treating that as a content
        // match would suppress every empty commit and every merge as "already
        // reviewed", which is a silent drop of unrelated work.
        let (_dir, repo) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let runner = GitRunner::new();

        git(
            &repo,
            &[
                "commit",
                "--quiet",
                "--allow-empty",
                "--no-gpg-sign",
                "-m",
                "Empty one",
            ],
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let first_empty = git(&repo, &["rev-parse", "HEAD"]).unwrap_or_else(|e| panic!("{e}"));
        git(
            &repo,
            &[
                "commit",
                "--quiet",
                "--allow-empty",
                "--no-gpg-sign",
                "-m",
                "Empty two",
            ],
        )
        .unwrap_or_else(|e| panic!("{e}"));

        let base = git(&repo, &["rev-parse", "HEAD~2"]).unwrap_or_else(|e| panic!("{e}"));
        let mut discovered = discover_branch(&runner, &repo, "main", Some(&base), 1000)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        mark_superseded_by_rewrite(&runner, &repo, &first_empty, &base, &mut discovered)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        assert!(
            discovered.iter().all(|c| c.skip_reason.is_none()),
            "an empty diff must not be treated as matching another empty diff"
        );
    }
}
