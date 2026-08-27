//! Acceptance tests for `RL-303` — git commit discovery.
//!
//! Every commit is referenced **by manifest role**, never by SHA. That is what
//! lets the fixture gain a commit, or change one's content, without these tests
//! being rewritten — and it is why the RL-201 refactor could rebuild the whole
//! fixture without touching a single test.

mod git_discover {
    use revlocal_vcs::git::{discover_branch, merge_discoveries, resolve_branches};
    use revlocal_vcs::GitRunner;
    use serde::Deserialize;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use tempfile::TempDir;

    #[derive(Debug, Clone, Deserialize)]
    struct CommitEntry {
        role: String,
        sha: String,
    }

    #[derive(Debug, Clone, Deserialize)]
    struct Manifest {
        commits: Vec<CommitEntry>,
    }

    impl Manifest {
        /// The sha for a role.
        ///
        /// Returns a sentinel rather than panicking: helpers are not `#[test]`
        /// fns, so the unwrap/expect/panic ban applies to them (ADR 0003). A
        /// missing role fails the assertion that used it, naming the role.
        fn sha(&self, role: &str) -> String {
            self.commits.iter().find(|c| c.role == role).map_or_else(
                || format!("<no commit with role {role}>"),
                |c| c.sha.clone(),
            )
        }

        fn role_of(&self, sha: &str) -> String {
            self.commits
                .iter()
                .find(|c| c.sha == sha)
                .map_or_else(|| format!("<unknown sha {sha}>"), |c| c.role.clone())
        }
    }

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
    }

    /// Build a fresh fixture and return its directory and manifest.
    ///
    /// Returns `Result`; helpers are not `#[test]` fns (ADR 0003).
    fn fixture() -> Result<(TempDir, PathBuf, Manifest), String> {
        let dir = TempDir::new().map_err(|e| format!("temp dir: {e}"))?;
        let root = workspace_root();

        let output = Command::new("bash")
            .arg(root.join("fixtures/build.sh"))
            .arg("--out")
            .arg(dir.path())
            .current_dir(&root)
            .output()
            .map_err(|e| format!("build.sh: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "build.sh failed: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        let repo = dir.path().join("git-basic");
        let text = std::fs::read_to_string(repo.join(".manifest.json"))
            .map_err(|e| format!("reading manifest: {e}"))?;
        let manifest: Manifest =
            serde_json::from_str(&text).map_err(|e| format!("parsing manifest: {e}"))?;

        Ok((dir, repo, manifest))
    }

    /// SPEC §13.2's default watched branches.
    fn default_patterns() -> Vec<String> {
        vec!["main".to_owned(), "release/*".to_owned()]
    }

    /// Resolve every watched branch and merge their discoveries.
    ///
    /// Returns `Result`; helpers are not `#[test]` fns (ADR 0003).
    async fn discover_all(
        repo: &Path,
        patterns: &[String],
    ) -> Result<Vec<revlocal_vcs::DetectedChange>, String> {
        let runner = GitRunner::new();
        let branches = resolve_branches(&runner, repo, patterns)
            .await
            .map_err(|e| format!("resolve_branches: {e}"))?;

        let mut per_branch = Vec::new();
        for branch in &branches {
            per_branch.push(
                discover_branch(&runner, repo, branch, None, 1000)
                    .await
                    .map_err(|e| format!("discover {branch}: {e}"))?,
            );
        }
        Ok(merge_discoveries(per_branch))
    }

    // --- branch resolution ---------------------------------------------------

    #[tokio::test]
    async fn git_discover_branch_globs_resolve() {
        let (_dir, repo, _manifest) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let runner = GitRunner::new();

        let branches = resolve_branches(&runner, &repo, &default_patterns())
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            branches,
            ["main", "release/pager-tweak"],
            "the default `release/*` glob must pick up the fixture's release branch"
        );
    }

    #[tokio::test]
    async fn git_discover_a_pattern_matching_nothing_is_not_an_error() {
        // A repo that has not created its release/* branches yet is normal.
        // Failing discovery over it would stop reviewing `main` as well.
        let (_dir, repo, _manifest) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let runner = GitRunner::new();

        let branches = resolve_branches(&runner, &repo, &["nothing/*".to_owned()])
            .await
            .unwrap_or_else(|e| panic!("an unmatched pattern must not fail: {e}"));
        assert!(branches.is_empty());
    }

    #[tokio::test]
    async fn git_discover_overlapping_patterns_do_not_list_a_branch_twice() {
        let (_dir, repo, _manifest) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let runner = GitRunner::new();

        let branches = resolve_branches(
            &runner,
            &repo,
            &["main".to_owned(), "*".to_owned(), "ma*".to_owned()],
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(branches.iter().filter(|b| *b == "main").count(), 1);
    }

    // --- discovery -----------------------------------------------------------

    #[tokio::test]
    async fn git_discover_finds_exactly_the_expected_changes_by_role() {
        let (_dir, repo, manifest) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let found = discover_all(&repo, &default_patterns())
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        let mut roles: Vec<String> = found
            .iter()
            .map(|c| manifest.role_of(&c.external_id))
            .collect();
        roles.sort_unstable();

        let mut expected: Vec<String> = manifest.commits.iter().map(|c| c.role.clone()).collect();
        expected.sort_unstable();

        assert_eq!(
            roles, expected,
            "under SPEC §13.2's default branches, every commit in the fixture should \
             be discovered exactly once"
        );
        assert_eq!(found.len(), 12, "M4's gate expects exactly 12 changes");
    }

    #[tokio::test]
    async fn git_discover_returns_changes_oldest_first() {
        // Reviews publish in the order changes happened. Newest-first would put a
        // fix's review above the bug's.
        let (_dir, repo, manifest) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let runner = GitRunner::new();

        let found = discover_branch(&runner, &repo, "main", None, 1000)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        let first = manifest.role_of(&found[0].external_id);
        let last = manifest.role_of(&found[found.len() - 1].external_id);
        assert_eq!(first, "initial");
        assert_eq!(last, "clean_final");
    }

    #[tokio::test]
    async fn git_discover_a_merged_branch_commit_is_not_reported_twice() {
        // `branch_work` is reachable from `main` through the merge and directly from
        // the release branch. It is one change. The store's UNIQUE constraint would
        // catch a duplicate, but only after a second review had been queued and paid
        // for.
        let (_dir, repo, manifest) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let found = discover_all(&repo, &default_patterns())
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        let branch_work = manifest.sha("branch_work");
        assert_eq!(
            found
                .iter()
                .filter(|c| c.external_id == branch_work)
                .count(),
            1,
            "a commit reachable from two watched branches is one change"
        );
    }

    #[tokio::test]
    async fn git_discover_first_parent_does_not_re_report_what_a_merge_brought_in() {
        // Without --first-parent, `branch_work` would appear again on `main` the
        // moment the merge landed, and every already-reviewed commit on a long-lived
        // branch would be re-reviewed at merge time.
        let (_dir, repo, manifest) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let runner = GitRunner::new();

        let on_main = discover_branch(&runner, &repo, "main", None, 1000)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let ids: Vec<&str> = on_main.iter().map(|c| c.external_id.as_str()).collect();

        assert!(
            !ids.contains(&manifest.sha("branch_work").as_str()),
            "--first-parent must not walk into the merged branch"
        );
        assert!(
            ids.contains(&manifest.sha("merge").as_str()),
            "but the merge itself is a change"
        );
    }

    #[tokio::test]
    async fn git_discover_carries_the_metadata_the_skip_rules_will_need() {
        // RL-305 matches on author (ignore_authors) and on the file list. Discovery
        // has to supply the author, or the bot rule would have to shell out again
        // per commit.
        let (_dir, repo, manifest) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let found = discover_all(&repo, &default_patterns())
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        let bot_sha = manifest.sha("bot");
        let bot = found
            .iter()
            .find(|c| c.external_id == bot_sha)
            .unwrap_or_else(|| panic!("the bot commit was not discovered"));

        assert_eq!(bot.author_name.as_deref(), Some("dependabot[bot]"));
        assert!(
            bot.authored_at.is_some(),
            "authored_at is needed for ordering and display"
        );
        assert_eq!(bot.branch.as_deref(), Some("main"));
        assert!(bot.base_ref.is_some(), "the first parent is the diff base");
        assert_eq!(
            bot.cursor_value, bot.external_id,
            "SPEC §6.2: a git cursor is the last reviewed SHA"
        );
        assert!(
            bot.skip_reason.is_none(),
            "discovery reports the change; deciding not to review it is RL-305's job, \
             and §18 wants the skip recorded rather than the change omitted"
        );
    }

    #[tokio::test]
    async fn git_discover_reports_a_diff_stat_big_enough_to_drive_depth_selection() {
        let (_dir, repo, manifest) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let found = discover_all(&repo, &default_patterns())
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        let large = manifest.sha("large_200_files");
        let change = found
            .iter()
            .find(|c| c.external_id == large)
            .unwrap_or_else(|| panic!("the 200-file commit was not discovered"));

        assert_eq!(
            change.diff_stat.files, 200,
            "SPEC §9.3 selects depth on file count"
        );
        assert!(change.diff_stat.insertions > 0);
    }

    #[tokio::test]
    async fn git_discover_a_subject_containing_a_newline_does_not_split_a_commit() {
        // The parser uses NUL and RS as separators precisely because `\n` and `|`
        // can occur in a subject. A commit whose subject broke the format would be
        // silently reported as two commits, one of them garbage.
        let (dir, repo, _manifest) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let _keep = dir;

        std::fs::write(repo.join("tricky.txt"), "x").unwrap_or_else(|e| panic!("write: {e}"));
        for args in [
            vec!["add", "-A"],
            vec![
                "commit",
                "-m",
                "subject with | pipe and \"quotes\"",
                "--no-gpg-sign",
            ],
        ] {
            let status = Command::new("git")
                .args(&args)
                .current_dir(&repo)
                .env("GIT_AUTHOR_NAME", "T")
                .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
                .env("GIT_COMMITTER_NAME", "T")
                .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
                .output()
                .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
            assert!(
                status.status.success(),
                "{}",
                String::from_utf8_lossy(&status.stderr)
            );
        }

        let runner = GitRunner::new();
        let found = discover_branch(&runner, &repo, "main", None, 1000)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(
            found.len(),
            12,
            "the tricky subject must be one commit, not two"
        );
        assert_eq!(
            found[found.len() - 1].title.as_deref(),
            Some("subject with | pipe and \"quotes\""),
            "and its subject must survive intact"
        );
    }

    // --- cursors -------------------------------------------------------------

    #[tokio::test]
    async fn git_discover_a_cursor_bounds_discovery_to_what_is_new() {
        let (_dir, repo, manifest) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let runner = GitRunner::new();

        let cursor = manifest.sha("lockfile_only");
        let found = discover_branch(&runner, &repo, "main", Some(&cursor), 1000)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        let roles: Vec<String> = found
            .iter()
            .map(|c| manifest.role_of(&c.external_id))
            .collect();
        assert_eq!(
            roles,
            [
                "bot",
                "filler_modules",
                "large_200_files",
                "merge",
                "clean_final"
            ]
        );
        assert!(
            !roles.contains(&"lockfile_only".to_owned()),
            "the cursor commit itself is already reviewed"
        );
    }

    #[tokio::test]
    async fn git_discover_re_running_after_a_crash_finds_the_same_change_again() {
        // Acceptance criterion 2. Discovery never advances a cursor; the caller
        // records the change durably and only then stores the cursor. A crash in
        // between must re-discover, not skip — losing a change is unrecoverable
        // while seeing one twice costs nothing.
        let (_dir, repo, manifest) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let runner = GitRunner::new();

        let cursor = manifest.sha("lockfile_only");

        // First pass: discovers the bot commit. Imagine the process dying here,
        // after the change was seen but before the cursor moved.
        let first = discover_branch(&runner, &repo, "main", Some(&cursor), 1)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(first.len(), 1);
        assert_eq!(manifest.role_of(&first[0].external_id), "bot");

        // Second pass with the SAME cursor, because it was never advanced.
        let second = discover_branch(&runner, &repo, "main", Some(&cursor), 1)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(
            first[0].external_id, second[0].external_id,
            "the same change must be re-discovered after a crash"
        );
        assert_eq!(
            first[0].cursor_value, second[0].cursor_value,
            "and it must name the same cursor value, or the retry would advance wrongly"
        );

        // Deduplication is by external_id, which is the same on both passes — so the
        // store's upsert lands on one row rather than creating a second change.
        let merged = merge_discoveries(vec![first.clone(), second]);
        assert_eq!(
            merged.len(),
            1,
            "re-discovery must not duplicate the change"
        );
    }

    #[tokio::test]
    async fn git_discover_an_up_to_date_cursor_finds_nothing() {
        let (_dir, repo, manifest) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let runner = GitRunner::new();

        let tip = manifest.sha("clean_final");
        let found = discover_branch(&runner, &repo, "main", Some(&tip), 1000)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(found.is_empty(), "nothing has happened since the tip");
    }

    #[tokio::test]
    async fn git_discover_a_limit_takes_the_oldest_not_the_newest() {
        // Pins a bug this code actually had. `git log --reverse --max-count=N`
        // applies the limit during traversal, BEFORE reversing, so it yields the
        // NEWEST n. A discovery that returned those would review them, advance the
        // cursor past them, and never come back for the older ones — changes lost
        // silently, which is the one failure this layer must not have.
        let (_dir, repo, manifest) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let runner = GitRunner::new();

        let found = discover_branch(&runner, &repo, "main", None, 3)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        let roles: Vec<String> = found
            .iter()
            .map(|c| manifest.role_of(&c.external_id))
            .collect();
        assert_eq!(
            roles,
            ["initial", "clean", "planted_bug_off_by_one"],
            "a limited discovery must return the OLDEST changes, so the cursor \
             advances forward through history without skipping anything"
        );
    }

    #[tokio::test]
    async fn git_discover_a_limit_of_zero_asks_git_for_nothing() {
        let (_dir, repo, _manifest) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let runner = GitRunner::new();

        let found = discover_branch(&runner, &repo, "main", None, 0)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(found.is_empty());
    }

    #[tokio::test]
    async fn git_discover_limiting_then_resuming_covers_the_history_exactly_once() {
        // The property that matters more than either test above: walking a branch in
        // limited batches must visit every commit exactly once, in order. Off-by-one
        // in the range arithmetic would either skip a change or review one twice.
        let (_dir, repo, manifest) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let runner = GitRunner::new();

        let mut cursor: Option<String> = None;
        let mut visited: Vec<String> = Vec::new();

        loop {
            let batch = discover_branch(&runner, &repo, "main", cursor.as_deref(), 2)
                .await
                .unwrap_or_else(|e| panic!("{e}"));
            if batch.is_empty() {
                break;
            }
            for change in &batch {
                visited.push(manifest.role_of(&change.external_id));
            }
            // The caller advances only after recording, which is why cursor_value
            // comes from the change rather than being inferred here.
            cursor = batch.last().map(|c| c.cursor_value.clone());
        }

        let expected = discover_branch(&runner, &repo, "main", None, 1000)
            .await
            .unwrap_or_else(|e| panic!("{e}"))
            .iter()
            .map(|c| manifest.role_of(&c.external_id))
            .collect::<Vec<_>>();

        assert_eq!(
            visited, expected,
            "batched discovery must cover the same history, in the same order, once"
        );
    }

    #[tokio::test]
    async fn git_discover_leaves_the_fixture_working_tree_untouched() {
        // M4's gate asserts this after a full review. Discovery is a pure read and
        // must not be the thing that breaks it.
        let (_dir, repo, _manifest) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let _ = discover_all(&repo, &default_patterns()).await;

        let status = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&repo)
            .output()
            .unwrap_or_else(|e| panic!("git status: {e}"));
        assert!(
            String::from_utf8_lossy(&status.stdout).trim().is_empty(),
            "discovery dirtied the repository under review"
        );
    }
}
