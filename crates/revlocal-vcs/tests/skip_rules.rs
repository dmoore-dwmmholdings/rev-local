//! Acceptance tests for `RL-305` — the skip table of SPEC §9.4.
//!
//! The theme, and the reason this item exists at all: **a skipped change is not an
//! invisible one**. Skipping avoids engine spend, not accountability. Every rule
//! records a reason, the change is still stored, and a `skipped` run still carries
//! that reason — a user whose lockfile bump has no review must be able to find out
//! why without reading the source.

mod skip_rules {
    use revlocal_core::RepoConfig;
    use revlocal_vcs::git::{discover_branch, merge_discoveries, resolve_branches};
    use revlocal_vcs::skip_rules::{evaluate, reviewable_paths, SkipReason};
    use revlocal_vcs::{DetectedChange, GitRunner};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use tempfile::TempDir;

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
    }

    /// A change with nothing remarkable about it, to vary one field at a time.
    fn a_change() -> DetectedChange {
        DetectedChange {
            kind: revlocal_core::ChangeKind::Commit,
            external_id: "deadbeef".to_owned(),
            title: Some("Fix the thing".to_owned()),
            author_name: Some("A Human".to_owned()),
            author_email: Some("human@example.invalid".to_owned()),
            authored_at: None,
            branch: Some("main".to_owned()),
            base_ref: Some("cafebabe".to_owned()),
            parents: vec!["cafebabe".to_owned()],
            paths: vec!["src/main.rs".to_owned()],
            head_ref: Some("deadbeef".to_owned()),
            url: None,
            diff_stat: revlocal_core::DiffStat::default(),
            skip_reason: None,
            cursor_value: "deadbeef".to_owned(),
        }
    }

    /// Build the fixture and return its repo path and manifest roles by sha.
    fn fixture() -> Result<(TempDir, PathBuf, serde_json::Value), String> {
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
        let text = std::fs::read_to_string(repo.join(".manifest.json"))
            .map_err(|e| format!("manifest: {e}"))?;
        let manifest: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("manifest json: {e}"))?;
        Ok((dir, repo, manifest))
    }

    fn sha_for_role(manifest: &serde_json::Value, role: &str) -> String {
        manifest["commits"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|c| c["role"] == role)
            .and_then(|c| c["sha"].as_str())
            .map_or_else(|| format!("<no role {role}>"), str::to_owned)
    }

    /// Every change on the fixture's watched branches.
    async fn discovered(repo: &Path) -> Result<Vec<DetectedChange>, String> {
        let runner = GitRunner::new();
        let patterns = RepoConfig::default().branches;
        let branches = resolve_branches(&runner, repo, &patterns)
            .await
            .map_err(|e| format!("resolve: {e}"))?;
        let mut per_branch = Vec::new();
        for branch in &branches {
            per_branch.push(
                discover_branch(&runner, repo, branch, None, 1000)
                    .await
                    .map_err(|e| format!("discover: {e}"))?,
            );
        }
        Ok(merge_discoveries(per_branch))
    }

    // --- one fixture commit per category -------------------------------------

    #[tokio::test]
    async fn skip_rules_the_lockfile_commit_is_skipped_as_ignored_paths() {
        let (_dir, repo, manifest) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let changes = discovered(&repo).await.unwrap_or_else(|e| panic!("{e}"));
        let config = RepoConfig::default();

        let sha = sha_for_role(&manifest, "lockfile_only");
        let change = changes
            .iter()
            .find(|c| c.external_id == sha)
            .unwrap_or_else(|| panic!("the lockfile commit was not discovered"));

        let skip = evaluate(change, &config)
            .unwrap_or_else(|| panic!("a lockfile-only change must be skipped"));
        assert_eq!(skip.reason, SkipReason::IgnoredPaths);
        assert!(
            skip.to_skip_reason().starts_with("ignored_paths: "),
            "{}",
            skip.to_skip_reason()
        );
        assert!(
            skip.detail.contains("ignore_globs"),
            "the detail must name the rule that fired: {}",
            skip.detail
        );
    }

    #[tokio::test]
    async fn skip_rules_the_bot_commit_is_skipped_as_ignored_author() {
        let (_dir, repo, manifest) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let changes = discovered(&repo).await.unwrap_or_else(|e| panic!("{e}"));
        let config = RepoConfig::default();

        let sha = sha_for_role(&manifest, "bot");
        let change = changes
            .iter()
            .find(|c| c.external_id == sha)
            .unwrap_or_else(|| panic!("the bot commit was not discovered"));

        let skip =
            evaluate(change, &config).unwrap_or_else(|| panic!("a bot commit must be skipped"));
        assert_eq!(skip.reason, SkipReason::IgnoredAuthor);
        assert!(
            skip.detail.contains("dependabot[bot]"),
            "the detail must name the author that matched: {}",
            skip.detail
        );
    }

    #[tokio::test]
    async fn skip_rules_the_merge_commit_is_skipped_as_merge_commit() {
        let (_dir, repo, manifest) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let changes = discovered(&repo).await.unwrap_or_else(|e| panic!("{e}"));
        let config = RepoConfig::default();
        assert!(
            !config.review_merge_commits,
            "the default is off (SPEC §13.2)"
        );

        let sha = sha_for_role(&manifest, "merge");
        let change = changes
            .iter()
            .find(|c| c.external_id == sha)
            .unwrap_or_else(|| panic!("the merge commit was not discovered"));

        assert_eq!(
            change.parents.len(),
            2,
            "discovery must carry the parent count"
        );
        let skip = evaluate(change, &config).unwrap_or_else(|| panic!("a merge must be skipped"));
        assert_eq!(skip.reason, SkipReason::MergeCommit);
    }

    #[tokio::test]
    async fn skip_rules_a_real_change_is_not_skipped() {
        // The guard that keeps the rest of this file honest: if everything were
        // skipped, every assertion above would pass and rev-local would review
        // nothing.
        let (_dir, repo, manifest) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let changes = discovered(&repo).await.unwrap_or_else(|e| panic!("{e}"));
        let config = RepoConfig::default();

        for role in [
            "planted_bug_off_by_one",
            "planted_bug_sql_injection",
            "clean",
        ] {
            let sha = sha_for_role(&manifest, role);
            let change = changes
                .iter()
                .find(|c| c.external_id == sha)
                .unwrap_or_else(|| panic!("{role} was not discovered"));
            assert_eq!(
                evaluate(change, &config),
                None,
                "{role} must be reviewed, not skipped"
            );
        }
    }

    #[tokio::test]
    async fn skip_rules_exactly_the_expected_fixture_commits_are_skipped() {
        // M4's gate: "skips the lockfile + bot + merge commits with correct
        // skip_reason". Asserted as a set, so a rule that started skipping something
        // else fails here rather than silently reducing coverage.
        let (_dir, repo, manifest) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let changes = discovered(&repo).await.unwrap_or_else(|e| panic!("{e}"));
        let config = RepoConfig::default();

        let mut skipped: Vec<(String, String)> = Vec::new();
        for change in &changes {
            if let Some(skip) = evaluate(change, &config) {
                let role = manifest["commits"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|c| c["sha"] == change.external_id.as_str())
                    .and_then(|c| c["role"].as_str())
                    .unwrap_or("<unknown>")
                    .to_owned();
                skipped.push((role, skip.reason.as_str().to_owned()));
            }
        }
        skipped.sort();

        assert_eq!(
            skipped,
            vec![
                ("bot".to_owned(), "ignored_author".to_owned()),
                ("lockfile_only".to_owned(), "ignored_paths".to_owned()),
                ("merge".to_owned(), "merge_commit".to_owned()),
            ],
            "exactly three fixture commits should be skipped, each for its own reason"
        );
    }

    // --- categories the fixture deliberately does not contain -----------------

    #[test]
    fn skip_rules_an_empty_diff_is_skipped_rather_than_sent_to_an_engine() {
        // Acceptance criterion 3. An engine invoked on an empty diff spends tokens
        // to report nothing.
        let mut change = a_change();
        change.paths.clear();

        let skip = evaluate(&change, &RepoConfig::default())
            .unwrap_or_else(|| panic!("an empty change must be skipped"));
        assert_eq!(skip.reason, SkipReason::EmptyDiff);
    }

    #[test]
    fn skip_rules_a_vendor_only_change_is_skipped() {
        let mut change = a_change();
        change.paths = vec![
            "vendor/github.com/x/y.go".to_owned(),
            "node_modules/left-pad/index.js".to_owned(),
        ];
        let skip = evaluate(&change, &RepoConfig::default())
            .unwrap_or_else(|| panic!("vendored paths must be skipped"));
        assert_eq!(skip.reason, SkipReason::IgnoredPaths);
    }

    #[test]
    fn skip_rules_one_reviewable_path_is_enough_to_review_the_change() {
        // §9.4 skips a change that touches ONLY ignored paths. A commit that bumps a
        // lockfile *and* changes source is a real change, and skipping it because
        // most of it is noise would hide the part that matters.
        let mut change = a_change();
        change.paths = vec![
            "Cargo.lock".to_owned(),
            "node_modules/x/index.js".to_owned(),
            "src/db.rs".to_owned(),
        ];
        assert_eq!(evaluate(&change, &RepoConfig::default()), None);

        let remaining = reviewable_paths(&change.paths, &RepoConfig::default());
        assert_eq!(
            remaining,
            ["src/db.rs"],
            "and only the real path is reviewable"
        );
    }

    // --- the rules themselves -------------------------------------------------

    #[test]
    fn skip_rules_a_merge_is_reviewed_when_the_repo_asks_for_it() {
        let mut change = a_change();
        change.parents = vec!["a".to_owned(), "b".to_owned()];

        let off = RepoConfig::default();
        assert_eq!(
            evaluate(&change, &off).map(|s| s.reason),
            Some(SkipReason::MergeCommit)
        );

        let on = RepoConfig {
            review_merge_commits: true,
            ..RepoConfig::default()
        };
        assert_eq!(
            evaluate(&change, &on),
            None,
            "review_merge_commits must be honoured"
        );
    }

    #[test]
    fn skip_rules_an_author_is_matched_on_name_or_email() {
        // A bot is spelled one way in one field and another in the other. A rule that
        // checked only the name would miss a bot that sets a plain display name.
        let mut by_name = a_change();
        by_name.author_name = Some("renovate[bot]".to_owned());
        by_name.author_email = Some("bot@renovateapp.invalid".to_owned());
        assert_eq!(
            evaluate(&by_name, &RepoConfig::default()).map(|s| s.reason),
            Some(SkipReason::IgnoredAuthor)
        );

        let mut by_email = a_change();
        by_email.author_name = Some("Renovate".to_owned());
        by_email.author_email = Some("dependabot[bot]".to_owned());
        let config = RepoConfig::default();
        assert_eq!(
            evaluate(&by_email, &config).map(|s| s.reason),
            Some(SkipReason::IgnoredAuthor),
            "matching only the display name would miss this one"
        );
    }

    #[test]
    fn skip_rules_an_author_pattern_does_not_match_by_substring() {
        // An `ignore_authors` entry of `bot` matching by substring would skip every
        // commit by anyone called Abbott.
        let mut change = a_change();
        change.author_name = Some("Abbott".to_owned());
        change.author_email = Some("abbott@example.invalid".to_owned());

        let config = RepoConfig {
            ignore_authors: vec!["bot".to_owned()],
            ..RepoConfig::default()
        };
        assert_eq!(
            evaluate(&change, &config),
            None,
            "a human named Abbott is not a bot"
        );
    }

    #[test]
    fn skip_rules_an_author_pattern_may_glob() {
        let mut change = a_change();
        change.author_name = Some("ci-runner-42".to_owned());
        let config = RepoConfig {
            ignore_authors: vec!["ci-runner-*".to_owned()],
            ..RepoConfig::default()
        };
        assert_eq!(
            evaluate(&change, &config).map(|s| s.reason),
            Some(SkipReason::IgnoredAuthor)
        );
    }

    #[test]
    fn skip_rules_a_merge_is_reported_as_a_merge_even_when_the_author_is_a_bot() {
        // Both rules fire. Reporting the merge is more useful, because that is the
        // property the user configured and the one they would change.
        let mut change = a_change();
        change.parents = vec!["a".to_owned(), "b".to_owned()];
        change.author_name = Some("dependabot[bot]".to_owned());

        assert_eq!(
            evaluate(&change, &RepoConfig::default()).map(|s| s.reason),
            Some(SkipReason::MergeCommit)
        );
    }

    #[test]
    fn skip_rules_a_malformed_ignore_glob_reviews_more_rather_than_less() {
        // A glob that does not compile must not be read as "matches everything".
        // Doing so would skip every change in the repository and look exactly like
        // rev-local silently doing nothing.
        let change = a_change();
        let config = RepoConfig {
            ignore_globs: vec!["[unclosed".to_owned()],
            ..RepoConfig::default()
        };
        assert_eq!(
            evaluate(&change, &config),
            None,
            "a broken ignore rule must fail towards reviewing, not towards silence"
        );
    }

    #[test]
    fn skip_rules_globs_cross_directories_the_way_the_defaults_assume() {
        // `**/*.lock` has to match a lockfile at any depth, or the default config
        // would only ignore a top-level one.
        let config = RepoConfig::default();
        for path in [
            "Cargo.lock",
            "crates/x/Cargo.lock",
            "a/b/c/node_modules/d/index.js",
            "vendor/x/y.go",
            "dist/bundle.js",
            "target/debug/thing",
            "web/app.min.js",
        ] {
            assert!(
                reviewable_paths(&[path.to_owned()], &config).is_empty(),
                "{path} should have matched a default ignore_glob"
            );
        }
        for path in ["src/main.rs", "docs/README.md", "lockfiles.rs"] {
            assert_eq!(
                reviewable_paths(&[path.to_owned()], &config),
                [path.to_owned()],
                "{path} must stay reviewable"
            );
        }
    }

    #[tokio::test]
    async fn skip_rules_a_skipped_change_is_still_recorded_and_still_has_a_run() {
        // Acceptance criterion 2, and the point of the whole item: skipping avoids
        // ENGINE SPEND, not accountability. A user whose lockfile bump has no review
        // must be able to find out why without reading the source.
        //
        // `revlocal-vcs` does not depend on `revlocal-store` in production (ADR
        // 0013); this test plays the pipeline to prove a skip survives the round
        // trip with its reason intact.
        let (_dir, repo, manifest) = fixture().unwrap_or_else(|e| panic!("{e}"));
        let changes = discovered(&repo).await.unwrap_or_else(|e| panic!("{e}"));
        let config = RepoConfig::default();

        let db = TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let pool = revlocal_store::open(&db.path().join("rev-local.db"))
            .await
            .unwrap_or_else(|e| panic!("open db: {e}"));

        let at = chrono::DateTime::parse_from_rfc3339("2026-08-27T12:00:00Z")
            .map(|t| t.with_timezone(&chrono::Utc))
            .unwrap_or_default();

        let stored_repo = revlocal_store::RepoStore::new(&pool)
            .insert(&revlocal_core::Repo {
                id: revlocal_core::RepoId::new(0),
                name: "git-basic".to_owned(),
                kind: revlocal_core::RepoKind::Git,
                local_path: Some(repo.display().to_string()),
                remote_url: None,
                default_branch: Some("main".to_owned()),
                engine: revlocal_core::EngineKind::Mock,
                autonomy: revlocal_core::AutonomyMode::DryRun,
                enabled: true,
                config_json: "{}".to_owned(),
                created_at: at,
                updated_at: at,
            })
            .await
            .unwrap_or_else(|e| panic!("insert repo: {e}"));

        let lockfile_sha = sha_for_role(&manifest, "lockfile_only");
        let detected = changes
            .iter()
            .find(|c| c.external_id == lockfile_sha)
            .unwrap_or_else(|| panic!("the lockfile commit was not discovered"));
        let skip = evaluate(detected, &config).unwrap_or_else(|| panic!("it should be skipped"));

        // The change is recorded even though it will not be reviewed.
        let stored_change = revlocal_store::ChangeStore::new(&pool)
            .upsert(&revlocal_core::Change {
                id: revlocal_core::ChangeId::new(0),
                repo_id: stored_repo.id,
                kind: detected.kind,
                external_id: detected.external_id.clone(),
                title: detected.title.clone(),
                author_name: detected.author_name.clone(),
                author_email: detected.author_email.clone(),
                authored_at: detected.authored_at,
                branch: detected.branch.clone(),
                base_ref: detected.base_ref.clone(),
                head_ref: detected.head_ref.clone(),
                url: None,
                diff_stat: detected.diff_stat,
                detected_at: at,
            })
            .await
            .unwrap_or_else(|e| panic!("upsert change: {e}"));

        let run = revlocal_store::RunStore::new(&pool)
            .insert(&revlocal_core::Run {
                id: revlocal_core::RunId::new(0),
                change_id: stored_change.id,
                attempt: 1,
                status: revlocal_core::RunStatus::Skipped,
                engine: revlocal_core::EngineKind::Mock,
                depth: revlocal_core::Depth::Summary,
                trigger: revlocal_core::TriggerSource::Poll,
                skip_reason: Some(skip.to_skip_reason()),
                error: None,
                degraded: None,
                usage: revlocal_core::Usage::default(),
                started_at: None,
                finished_at: Some(at),
                transcript_path: None,
                truncated: false,
                omitted_files: Vec::new(),
                verdict: None,
                summary: None,
                created_at: at,
            })
            .await
            .unwrap_or_else(|e| panic!("insert run: {e}"));

        // Read it all back the way the UI would.
        let runs = revlocal_store::RunStore::new(&pool)
            .list_for_change(stored_change.id)
            .await
            .unwrap_or_else(|e| panic!("list runs: {e}"));

        assert_eq!(runs.len(), 1, "a skipped change must still have a run");
        assert_eq!(runs[0].status, revlocal_core::RunStatus::Skipped);
        assert!(
            runs[0].is_consistent(),
            "a skipped run must carry its reason"
        );

        let reason = runs[0].skip_reason.clone().unwrap_or_default();
        assert!(
            reason.starts_with("ignored_paths: "),
            "the stored reason must be machine-groupable: {reason}"
        );
        assert!(
            reason.contains("ignore_globs"),
            "and human-readable enough to act on: {reason}"
        );
        assert_eq!(
            runs[0].usage.total_tokens(),
            0,
            "a skip must not have spent anything on an engine"
        );
        let _ = run;
    }

    #[tokio::test]
    async fn skip_rules_github_transport_is_recorded_on_the_repo_row() {
        // Not a skip rule, but it needs the same dev-dependency on the store and the
        // same "prove it round-trips" treatment. SPEC §6.3: the selected transport is
        // stored on the repo row. RL-307 added the column.
        use revlocal_vcs::GitHubTransport;

        let db = TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let pool = revlocal_store::open(&db.path().join("rev-local.db"))
            .await
            .unwrap_or_else(|e| panic!("open db: {e}"));
        let at = chrono::DateTime::parse_from_rfc3339("2026-08-27T12:00:00Z")
            .map(|t| t.with_timezone(&chrono::Utc))
            .unwrap_or_default();

        let repo = revlocal_store::RepoStore::new(&pool)
            .insert(&revlocal_core::Repo {
                id: revlocal_core::RepoId::new(0),
                name: "owner/repo".to_owned(),
                kind: revlocal_core::RepoKind::GitHub,
                local_path: None,
                remote_url: Some("https://github.com/owner/repo".to_owned()),
                default_branch: Some("main".to_owned()),
                engine: revlocal_core::EngineKind::Mock,
                autonomy: revlocal_core::AutonomyMode::DryRun,
                enabled: true,
                config_json: "{}".to_owned(),
                created_at: at,
                updated_at: at,
            })
            .await
            .unwrap_or_else(|e| panic!("insert repo: {e}"));

        let store = revlocal_store::RepoStore::new(&pool);

        // Not probed yet is distinguishable from probed-and-unauthenticated: one
        // means nobody looked, the other means we looked and this is as good as it
        // gets.
        assert_eq!(
            store
                .github_transport(repo.id)
                .await
                .unwrap_or_else(|e| panic!("{e}")),
            None,
            "a fresh repo has not been probed"
        );

        let chosen = GitHubTransport::GhCli {
            account: Some("octocat".to_owned()),
        };
        store
            .set_github_transport(repo.id, Some(chosen.name()))
            .await
            .unwrap_or_else(|e| panic!("set: {e}"));

        assert_eq!(
            store
                .github_transport(repo.id)
                .await
                .unwrap_or_else(|e| panic!("{e}")),
            Some("gh_cli".to_owned())
        );

        // The CHECK constraint keeps a typo out of the column.
        assert!(
            store
                .set_github_transport(repo.id, Some("gh-cli"))
                .await
                .is_err(),
            "a transport name the ladder cannot produce must be refused"
        );
    }

    #[tokio::test]
    async fn skip_rules_probing_the_real_gh_reports_something_actionable() {
        // Runs against whatever `gh` this machine has. The assertion is not "gh is
        // authenticated" — it will not be, in CI or here — but that the probe
        // produces a ladder that SAYS what is wrong and how to fix it, whichever
        // state the machine is in.
        use revlocal_vcs::github::{probe, select};

        let runner =
            revlocal_vcs::GitRunner::new().with_timeout(std::time::Duration::from_secs(20));
        let probes = probe(&runner, None, false, None).await;
        let selection = select(&probes);

        let report = selection.doctor_lines().join("\n");
        assert_eq!(
            selection.doctor_lines().len(),
            3,
            "all three rungs are reported"
        );
        assert!(
            report.contains("try:"),
            "every failing rung carries remediation:\n{report}"
        );

        if probes.gh_installed && !probes.gh_authenticated {
            assert!(
                report.contains("gh auth login"),
                "an installed-but-unauthenticated gh must point at the right fix:\n{report}"
            );
        }
    }

    #[test]
    fn skip_rules_every_reason_is_either_decided_here_or_documented_as_elsewhere() {
        // §9.4's rules, split by what each needs to decide. Asserting the split
        // keeps a rule from being quietly forgotten rather than deliberately
        // deferred — it caught `generated_only` being added, which is what it is
        // for.
        //
        // `generated_only` is decided by this crate but not by `evaluate`: it needs
        // file *content*, so it lives in `evaluate_with_generated`, which the
        // caller reaches after materialising. Still "decided here" in the sense
        // this test means — no other layer knows about it.
        let decided: Vec<&str> = SkipReason::ALL
            .iter()
            .filter(|r| r.is_decided_by_vcs())
            .map(|r| r.as_str())
            .collect();
        assert_eq!(
            decided,
            [
                "ignored_paths",
                "empty_diff",
                "ignored_author",
                "merge_commit",
                "generated_only"
            ]
        );

        // draft_pr and covered_by_pr are decided by the GitHub adapter, which is
        // the only layer that knows a change is a pull request at all;
        // already_reviewed needs the store. This assertion is why adding DraftPr in
        // RL-308 failed here first rather than being quietly forgotten.
        let deferred: Vec<&str> = SkipReason::ALL
            .iter()
            .filter(|r| !r.is_decided_by_vcs())
            .map(|r| r.as_str())
            .collect();
        assert_eq!(deferred, ["draft_pr", "covered_by_pr", "already_reviewed"]);
    }
}

// --- generated-file markers (RL-305b, ADR 0014) ----------------------------

mod generated_markers {
    use std::collections::BTreeMap;
    use std::path::Path;

    use revlocal_core::RepoConfig;
    use revlocal_vcs::generated::{marker_in_header, scan, HEADER_BYTES, HEADER_LINES};
    use revlocal_vcs::skip_rules::{evaluate_with_generated, SkipReason};

    /// A change touching exactly `paths`, with nothing else remarkable.
    ///
    /// Its own copy rather than reaching into `mod skip_rules`: these tests vary
    /// paths and nothing else, and a shared builder that grew a field would make
    /// them fail for reasons unrelated to what they assert.
    fn change_with_paths(paths: &[String]) -> revlocal_vcs::DetectedChange {
        revlocal_vcs::DetectedChange {
            kind: revlocal_core::ChangeKind::Commit,
            external_id: "deadbeef".to_owned(),
            title: Some("Regenerate".to_owned()),
            author_name: Some("A Human".to_owned()),
            author_email: Some("human@example.invalid".to_owned()),
            authored_at: None,
            branch: Some("main".to_owned()),
            base_ref: Some("cafebabe".to_owned()),
            parents: vec!["cafebabe".to_owned()],
            paths: paths.to_vec(),
            head_ref: Some("deadbeef".to_owned()),
            url: None,
            diff_stat: revlocal_core::DiffStat::default(),
            skip_reason: None,
            cursor_value: "deadbeef".to_owned(),
        }
    }

    /// Write `body` to `path` under `dir`, creating parents.
    fn write(dir: &Path, path: &str, body: &str) -> Result<(), String> {
        let full = dir.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(full, body).map_err(|e| e.to_string())
    }

    #[test]
    fn a_marker_in_the_header_is_recognised() {
        // Criterion 1. The forms the tools actually emit, not forms I imagined:
        // `@generated` is the cross-tool convention (Prettier, bazel, protobuf)
        // and Go's is a sentence rather than a tag.
        assert_eq!(
            marker_in_header("// Code generated by protoc-gen-go. DO NOT EDIT.\n"),
            Some("Code generated by")
        );
        assert_eq!(
            marker_in_header("/* @generated by prettier */\nconst x = 1;\n"),
            Some("@generated")
        );
        // A licence block before the marker is ordinary and must not hide it.
        let with_licence = format!("{}\n// @generated\n", "// Copyright 2026\n".repeat(10));
        assert_eq!(marker_in_header(&with_licence), Some("@generated"));
    }

    #[test]
    fn a_file_merely_mentioning_the_marker_further_down_is_not_generated() {
        // Criterion 3, and the reason this is a header check rather than a
        // substring search. Matching anywhere would suppress any file that
        // *documents* the convention — including the module implementing it, and
        // any linter config listing the markers it recognises.
        let mut body = String::from("// A hand-written file.\n");
        body.push_str(&"let x = 1;\n".repeat(HEADER_LINES + 5));
        body.push_str("// We treat files marked @generated as generated.\n");

        assert_eq!(
            marker_in_header(&body),
            None,
            "a mention below the header is documentation, not a marker"
        );
    }

    #[test]
    fn do_not_edit_alone_is_not_a_marker() {
        // It appears in hand-written files often enough to be dangerous: a config
        // with a "DO NOT EDIT below this line" divider is hand-written and worth
        // reviewing. Go's full form is specific because they had this problem.
        assert_eq!(
            marker_in_header("# DO NOT EDIT below this line\nfoo = 1\n"),
            None
        );
    }

    #[test]
    fn the_read_is_bounded() -> Result<(), String> {
        // Criterion 4. Generated files are routinely megabytes, and a minified
        // bundle is one line and several of them — so "read the first 40 lines"
        // is not a bound on its own.
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let huge = format!("const x = 1;{}", "/* padding */".repeat(400_000));
        write(dir.path(), "bundle.min.js", &huge)?;

        let full = std::fs::metadata(dir.path().join("bundle.min.js"))
            .map_err(|e| e.to_string())?
            .len();
        assert!(full > 1_000_000, "the fixture must be genuinely large");

        let header = revlocal_vcs::generated::read_header(&dir.path().join("bundle.min.js"))
            .ok_or("the header must be readable")?;
        assert!(
            header.len() <= HEADER_BYTES,
            "read {} bytes of a {full}-byte file; the cap is {HEADER_BYTES}",
            header.len()
        );
        Ok(())
    }

    #[test]
    fn a_change_touching_only_generated_files_is_skipped_naming_the_marker() -> Result<(), String> {
        // Criterion 2.
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        write(
            dir.path(),
            "api/pb.go",
            "// Code generated by protoc. DO NOT EDIT.\n",
        )?;
        write(dir.path(), "web/schema.ts", "/* @generated */\n")?;

        let paths = vec!["api/pb.go".to_owned(), "web/schema.ts".to_owned()];
        let found = scan(dir.path(), &paths);
        assert_eq!(found.len(), 2, "both files carry a marker");

        let change = change_with_paths(&paths);
        let skip = evaluate_with_generated(&change, &RepoConfig::default(), &found)
            .ok_or("a wholly generated change must be skipped")?;

        assert_eq!(skip.reason, SkipReason::GeneratedOnly);
        // §18: the reason names the marker, so an operator can check the claim
        // rather than take it on trust.
        assert!(
            skip.detail.contains("@generated") || skip.detail.contains("Code generated by"),
            "the reason must name the marker: {}",
            skip.detail
        );
        Ok(())
    }

    #[test]
    fn a_change_mixing_generated_and_handwritten_files_is_still_reviewed() -> Result<(), String> {
        // Criterion 5, and consistent with
        // `skip_rules_one_reviewable_path_is_enough_to_review_the_change`. A commit
        // that regenerates a schema *and* changes the code using it is a commit
        // somebody wrote, and it is exactly the kind worth reading.
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        write(
            dir.path(),
            "api/pb.go",
            "// Code generated by protoc. DO NOT EDIT.\n",
        )?;
        write(
            dir.path(),
            "src/handler.go",
            "package main\n\nfunc main() {}\n",
        )?;

        let paths = vec!["api/pb.go".to_owned(), "src/handler.go".to_owned()];
        let found = scan(dir.path(), &paths);
        assert_eq!(found.len(), 1, "only the generated file carries a marker");

        let change = change_with_paths(&paths);
        assert!(
            evaluate_with_generated(&change, &RepoConfig::default(), &found).is_none(),
            "one hand-written file is enough to review the change"
        );
        Ok(())
    }

    #[test]
    fn an_unreadable_file_is_reviewed_rather_than_skipped() -> Result<(), String> {
        // The safe direction. Reviewing something unnecessarily costs tokens;
        // skipping a hand-written file because it happened to be unreadable loses
        // a review and says the wrong reason for it.
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let paths = vec!["does/not/exist.go".to_owned()];

        let found = scan(dir.path(), &paths);
        assert!(found.is_empty(), "a missing file is not a generated one");

        let change = change_with_paths(&paths);
        assert!(evaluate_with_generated(&change, &RepoConfig::default(), &found).is_none());
        Ok(())
    }

    #[test]
    fn the_generated_reason_is_distinct_from_ignored_paths() {
        // The decision this item left open. "Ignored" would send somebody hunting
        // through `ignore_globs` for a pattern that cannot exist — §9.4's
        // "generated-file markers" is not a glob. Two reasons, two wire spellings.
        assert_ne!(SkipReason::GeneratedOnly, SkipReason::IgnoredPaths);
        assert_eq!(SkipReason::GeneratedOnly.as_str(), "generated_only");
        assert!(SkipReason::GeneratedOnly.is_decided_by_vcs());
        assert!(SkipReason::ALL.contains(&SkipReason::GeneratedOnly));
    }

    #[test]
    fn an_empty_map_never_skips() {
        // A caller that forgot to scan must get "review it", not "skip it". The
        // failure mode of a missing scan should cost tokens, not coverage.
        let change = change_with_paths(&["src/main.rs".to_owned()]);
        let empty: BTreeMap<String, &'static str> = BTreeMap::new();

        assert!(evaluate_with_generated(&change, &RepoConfig::default(), &empty).is_none());
    }
}
