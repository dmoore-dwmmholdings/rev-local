//! Acceptance tests for `RL-306` — materializing must not mutate the source repo.
//!
//! This is the safety property M4's exit gate turns on, and it is not a formality.
//! rev-local watches repositories people are actively working in. A review that
//! stashed someone's uncommitted work, moved HEAD, or left a half-checked-out index
//! behind would be worse than no review at all — and it would happen at 3am, on a
//! poll, with nobody watching.
//!
//! So the checks here are not "did materialize succeed". They are: what did the
//! source repository look like before, and is it byte-identical now.

mod git_materialize_is_readonly {
    use revlocal_core::{Change, ChangeId, ChangeKind, DiffStat, FileStatus, RepoId, RunId};
    use revlocal_vcs::git::{is_bare, materialize, prune_worktrees, release_worktree};
    use revlocal_vcs::{GitRunner, ScratchDir};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use tempfile::TempDir;

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
    }

    /// Read git state from `dir`. Test-local: this inspects a repository rather than
    /// reviewing one, which is the exemption the choke-point guard documents.
    fn git(dir: &Path, args: &[&str]) -> String {
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_default()
    }

    /// Everything about a repository that materializing must leave alone.
    #[derive(Debug, PartialEq, Eq)]
    struct RepoState {
        head: String,
        tree_hash: String,
        status: String,
        branches: String,
        stash: String,
        worktrees: String,
        index: String,
    }

    fn capture(dir: &Path) -> RepoState {
        RepoState {
            head: git(dir, &["rev-parse", "HEAD"]),
            // The tree hash is the content of the checkout as git sees it. A file
            // changed on disk without being staged would not move HEAD, but it does
            // move this once written — and `status` catches it before that.
            tree_hash: git(dir, &["rev-parse", "HEAD^{tree}"]),
            status: git(dir, &["status", "--porcelain"]),
            branches: git(dir, &["branch", "--format=%(refname:short) %(objectname)"]),
            stash: git(dir, &["stash", "list"]),
            worktrees: git(dir, &["worktree", "list"]),
            // The index specifically: a checkout that touched it would show here
            // even if the working tree happened to end up matching.
            index: git(dir, &["ls-files", "-s"]),
        }
    }

    /// Build the fixture, returning the temp dir, the repo, and the manifest.
    fn fixture() -> Result<(TempDir, PathBuf, serde_json::Value), String> {
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
                "build.sh: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        let repo = dir.path().join("git-basic");
        let manifest: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(repo.join(".manifest.json"))
                .map_err(|e| format!("manifest: {e}"))?,
        )
        .map_err(|e| format!("manifest json: {e}"))?;
        Ok((dir, repo, manifest))
    }

    fn shas(manifest: &serde_json::Value) -> Vec<(String, String)> {
        manifest["commits"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| {
                Some((
                    c["role"].as_str()?.to_owned(),
                    c["sha"].as_str()?.to_owned(),
                ))
            })
            .collect()
    }

    fn a_change(sha: &str) -> Change {
        Change {
            id: ChangeId::new(1),
            repo_id: RepoId::new(1),
            kind: ChangeKind::Commit,
            external_id: sha.to_owned(),
            title: None,
            author_name: None,
            author_email: None,
            authored_at: None,
            branch: Some("main".to_owned()),
            base_ref: None,
            head_ref: Some(sha.to_owned()),
            url: None,
            diff_stat: DiffStat::default(),
            detected_at: chrono::DateTime::parse_from_rfc3339("2026-08-27T12:00:00Z")
                .map(|t| t.with_timezone(&chrono::Utc))
                .unwrap_or_default(),
        }
    }

    // --- the safety property --------------------------------------------------

    #[tokio::test]
    async fn materialize_every_fixture_commit_leaves_the_repo_byte_identical() {
        // Acceptance criteria 1 and 2, over EVERY commit rather than a
        // representative one: the merge and the 200-file commit take different code
        // paths from a one-file change, and a checkout bug could hide in either.
        let (dir, repo, manifest) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let runner = GitRunner::new();
        let before = capture(&repo);

        for (index, (role, sha)) in shas(&manifest).into_iter().enumerate() {
            let scratch = ScratchDir::create(
                dir.path(),
                #[allow(clippy::cast_possible_wrap)]
                RunId::new(index as i64 + 1),
                false,
            )
            .unwrap_or_else(|e| panic!("scratch for {role}: {e}"));

            let context = materialize(&runner, &repo, &a_change(&sha), scratch.path())
                .await
                .unwrap_or_else(|e| panic!("materialize {role}: {e}"));

            assert!(context.worktree.is_dir(), "{role}: no tree was produced");

            release_worktree(&runner, &repo, &context.worktree)
                .await
                .unwrap_or_else(|e| panic!("release {role}: {e}"));
        }

        let after = capture(&repo);
        assert_eq!(
            before, after,
            "materializing changed the repository under review"
        );
        assert!(
            after.status.is_empty(),
            "git status --porcelain must be empty"
        );
    }

    #[tokio::test]
    async fn materialize_does_not_disturb_uncommitted_work() {
        // The case that would actually hurt someone. rev-local reviews repositories
        // people are working in; a poll that stashed their work-in-progress to check
        // out a commit would be worse than no review at all.
        let (dir, repo, manifest) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let runner = GitRunner::new();

        std::fs::write(
            repo.join("src/main.rs"),
            "// half-finished work\nfn main() {}\n",
        )
        .unwrap_or_else(|e| panic!("write: {e}"));
        std::fs::write(repo.join("untracked.txt"), "not added yet\n")
            .unwrap_or_else(|e| panic!("write: {e}"));
        let _ = git(&repo, &["add", "src/main.rs"]);

        let before = capture(&repo);
        assert!(
            !before.status.is_empty(),
            "the repo should be dirty for this test"
        );
        let wip = std::fs::read_to_string(repo.join("src/main.rs")).unwrap_or_default();

        let (_role, sha) = shas(&manifest)
            .into_iter()
            .find(|(role, _)| role == "planted_bug_off_by_one")
            .unwrap_or_else(|| panic!("no such role"));

        let scratch = ScratchDir::create(dir.path(), RunId::new(99), false)
            .unwrap_or_else(|e| panic!("scratch: {e}"));
        let context = materialize(&runner, &repo, &a_change(&sha), scratch.path())
            .await
            .unwrap_or_else(|e| panic!("materialize: {e}"));
        release_worktree(&runner, &repo, &context.worktree)
            .await
            .unwrap_or_else(|e| panic!("release: {e}"));

        assert_eq!(capture(&repo), before, "uncommitted work was disturbed");
        assert_eq!(
            std::fs::read_to_string(repo.join("src/main.rs")).unwrap_or_default(),
            wip,
            "the user's in-progress file was overwritten"
        );
        assert!(
            repo.join("untracked.txt").is_file(),
            "an untracked file was removed"
        );
        assert!(
            git(&repo, &["stash", "list"]).is_empty(),
            "materializing must never stash the user's work"
        );
    }

    #[tokio::test]
    async fn materialize_returns_worktree_list_to_its_prior_state() {
        // Acceptance criterion 3. `worktree add` writes metadata into the source
        // repo's .git; leaving it behind would accumulate an entry per review.
        let (dir, repo, manifest) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let runner = GitRunner::new();
        let before = git(&repo, &["worktree", "list"]);

        let (_role, sha) = shas(&manifest).into_iter().next().unwrap_or_default();
        let scratch = ScratchDir::create(dir.path(), RunId::new(1), false)
            .unwrap_or_else(|e| panic!("scratch: {e}"));
        let context = materialize(&runner, &repo, &a_change(&sha), scratch.path())
            .await
            .unwrap_or_else(|e| panic!("materialize: {e}"));

        let during = git(&repo, &["worktree", "list"]);
        assert_ne!(
            during, before,
            "the worktree should be registered while in use"
        );

        release_worktree(&runner, &repo, &context.worktree)
            .await
            .unwrap_or_else(|e| panic!("release: {e}"));

        assert_eq!(git(&repo, &["worktree", "list"]), before);
    }

    #[tokio::test]
    async fn materialize_pruning_recovers_from_a_run_that_died() {
        // The scratch directory removes itself on a panic (RL-301), which leaves a
        // worktree entry pointing at nothing. Without pruning, a crashing repo would
        // accumulate one per run forever.
        let (dir, repo, manifest) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let runner = GitRunner::new();
        let before = git(&repo, &["worktree", "list"]);

        let (_role, sha) = shas(&manifest).into_iter().next().unwrap_or_default();
        {
            let scratch = ScratchDir::create(dir.path(), RunId::new(7), false)
                .unwrap_or_else(|e| panic!("scratch: {e}"));
            materialize(&runner, &repo, &a_change(&sha), scratch.path())
                .await
                .unwrap_or_else(|e| panic!("materialize: {e}"));
            // Scratch drops here without release_worktree ever being called.
        }

        assert_ne!(
            git(&repo, &["worktree", "list"]),
            before,
            "a stale entry should remain"
        );

        prune_worktrees(&runner, &repo)
            .await
            .unwrap_or_else(|e| panic!("prune: {e}"));
        assert_eq!(
            git(&repo, &["worktree", "list"]),
            before,
            "pruning must clean it up"
        );
    }

    // --- the bare mirror ------------------------------------------------------

    #[tokio::test]
    async fn materialize_works_against_a_bare_mirror() {
        // Acceptance criterion 4. SPEC §6.1 prescribes `git archive` here.
        // `worktree add` happens to work on a bare repository, but it would write
        // metadata into a mirror rev-local does not own; `archive` writes nothing,
        // which is the stronger guarantee and the reason the spec names it.
        let (dir, repo, manifest) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let bare = dir.path().join("git-bare");
        let runner = GitRunner::new();

        assert!(is_bare(&runner, &bare)
            .await
            .unwrap_or_else(|e| panic!("{e}")));
        assert!(!is_bare(&runner, &repo)
            .await
            .unwrap_or_else(|e| panic!("{e}")));

        let before_worktrees = git(&bare, &["worktree", "list"]);
        let before_head = git(&bare, &["rev-parse", "HEAD"]);

        // The tip, so both a normal file and a dotfile exist at this commit —
        // `.github/` is not added until the bot commit, and asserting on it at an
        // earlier revision would be asserting the fixture is wrong.
        let (_role, sha) = shas(&manifest)
            .into_iter()
            .find(|(role, _)| role == "clean_final")
            .unwrap_or_else(|| panic!("no such role"));

        let scratch = ScratchDir::create(dir.path(), RunId::new(1), false)
            .unwrap_or_else(|e| panic!("scratch: {e}"));
        let context = materialize(&runner, &bare, &a_change(&sha), scratch.path())
            .await
            .unwrap_or_else(|e| panic!("materialize from bare: {e}"));

        assert!(
            context.worktree.join("src/db.rs").is_file(),
            "the tree was not extracted"
        );
        assert!(
            context.worktree.join(".github/dependabot.yml").is_file(),
            "dotfiles must survive extraction — tar does not include them by accident"
        );
        assert!(
            !context.diff_unified.is_empty(),
            "a diff must be produced from a bare repo too"
        );

        assert_eq!(
            git(&bare, &["worktree", "list"]),
            before_worktrees,
            "archive must not register a worktree in the mirror"
        );
        assert_eq!(git(&bare, &["rev-parse", "HEAD"]), before_head);

        // The tar is an implementation detail and must not be left where the engine
        // would see it as repository content.
        let leftovers: Vec<String> = std::fs::read_dir(scratch.path())
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tar"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "an archive was left behind: {leftovers:?}"
        );
    }

    // --- the context itself ---------------------------------------------------

    #[tokio::test]
    async fn materialize_produces_a_usable_review_context() {
        let (dir, repo, manifest) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let runner = GitRunner::new();

        let (_role, sha) = shas(&manifest)
            .into_iter()
            .find(|(role, _)| role == "planted_bug_off_by_one")
            .unwrap_or_else(|| panic!("no such role"));

        let scratch = ScratchDir::create(dir.path(), RunId::new(1), false)
            .unwrap_or_else(|e| panic!("scratch: {e}"));
        let context = materialize(&runner, &repo, &a_change(&sha), scratch.path())
            .await
            .unwrap_or_else(|e| panic!("materialize: {e}"));

        // The tree is checked out AT the change, not at whatever HEAD was.
        let pager = std::fs::read_to_string(context.worktree.join("src/pager.rs"))
            .unwrap_or_else(|e| panic!("reading pager.rs: {e}"));
        assert!(
            pager.contains("BUG (planted)"),
            "the planted bug should be present"
        );

        assert!(
            context.diff_unified.contains("src/pager.rs"),
            "the diff names the file"
        );
        assert!(context.diff_unified.contains("+++"), "it is a unified diff");
        assert_eq!(context.diff_files.len(), 1);
        assert_eq!(context.diff_files[0].path, "src/pager.rs");
        assert_eq!(context.diff_files[0].status, FileStatus::Added);
        assert!(context.diff_files[0].insertions > 0);
        assert!(!context.diff_files[0].binary);
        assert_eq!(context.stat.files, 1);
        assert_eq!(context.message, "Add pagination helper");
        assert_eq!(context.parents.len(), 1);

        assert!(
            context.is_consistent(),
            "a freshly materialized context claims nothing was omitted, and nothing was"
        );
        assert!(!context.truncated);

        release_worktree(&runner, &repo, &context.worktree)
            .await
            .unwrap_or_else(|e| panic!("release: {e}"));
    }

    #[tokio::test]
    async fn materialize_a_merge_diffs_against_its_first_parent() {
        // Without --first-parent, `git show` on a merge prints nothing, and the
        // review would see an empty diff for a commit that changed things.
        let (dir, repo, manifest) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let runner = GitRunner::new();

        let (_role, sha) = shas(&manifest)
            .into_iter()
            .find(|(role, _)| role == "merge")
            .unwrap_or_else(|| panic!("no merge commit"));

        let scratch = ScratchDir::create(dir.path(), RunId::new(1), false)
            .unwrap_or_else(|e| panic!("scratch: {e}"));
        let context = materialize(&runner, &repo, &a_change(&sha), scratch.path())
            .await
            .unwrap_or_else(|e| panic!("materialize: {e}"));

        assert_eq!(context.parents.len(), 2, "it is a merge");
        assert!(
            !context.diff_unified.is_empty(),
            "a merge must still produce a diff against its first parent"
        );
        assert!(context.stat.files > 0);

        release_worktree(&runner, &repo, &context.worktree)
            .await
            .unwrap_or_else(|e| panic!("release: {e}"));
    }

    #[tokio::test]
    async fn materialize_two_concurrent_runs_get_separate_trees() {
        // SPEC §4.3 allows two concurrent runs by default, and both may be on the
        // same repository. Two worktrees on one repo is exactly what `worktree add`
        // is for; this asserts they do not collide.
        let (dir, repo, manifest) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let runner = GitRunner::new();
        let commits = shas(&manifest);

        let (_, first_sha) = commits
            .iter()
            .find(|(role, _)| role == "planted_bug_off_by_one")
            .cloned()
            .unwrap_or_default();
        let (_, second_sha) = commits
            .iter()
            .find(|(role, _)| role == "planted_bug_sql_injection")
            .cloned()
            .unwrap_or_default();

        let first_scratch = ScratchDir::create(dir.path(), RunId::new(1), false)
            .unwrap_or_else(|e| panic!("scratch: {e}"));
        let second_scratch = ScratchDir::create(dir.path(), RunId::new(2), false)
            .unwrap_or_else(|e| panic!("scratch: {e}"));

        let first = materialize(&runner, &repo, &a_change(&first_sha), first_scratch.path())
            .await
            .unwrap_or_else(|e| panic!("first: {e}"));
        let second = materialize(
            &runner,
            &repo,
            &a_change(&second_sha),
            second_scratch.path(),
        )
        .await
        .unwrap_or_else(|e| panic!("second: {e}"));

        assert_ne!(first.worktree, second.worktree);
        assert!(
            !first.worktree.join("src/db.rs").is_file(),
            "the earlier commit's tree must not contain the later commit's file"
        );
        assert!(second.worktree.join("src/db.rs").is_file());

        for context in [&first, &second] {
            release_worktree(&runner, &repo, &context.worktree)
                .await
                .unwrap_or_else(|e| panic!("release: {e}"));
        }
    }
}
