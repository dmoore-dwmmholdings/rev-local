//! Acceptance tests for `RL-308` — pull-request discovery keyed by head SHA.
//!
//! The property everything here turns on: **a pull request is not one change**.
//! `external_id = "{number}:{head_sha}"`, so a re-push produces a *new* Change
//! rather than mutating the old one. Keying on the number alone would let a re-push
//! change the row a finding points at, and a finding filed against reviewed code
//! would silently start describing code nobody reviewed.

mod github_pr_discover {
    use revlocal_core::{ChangeKind, RepoConfig, Timestamp};
    use revlocal_vcs::github::{
        discover_pull_requests, mark_covered_by_pr, parse_gh_pr_list, superseded_fingerprints,
        PullRequest, PullRequestSource, GH_PR_FIELDS,
    };
    use revlocal_vcs::DetectedChange;
    use std::collections::BTreeSet;

    fn at(minute: u32) -> Timestamp {
        chrono::DateTime::parse_from_rfc3339(&format!("2026-08-27T12:{minute:02}:00Z"))
            .map(|t| t.with_timezone(&chrono::Utc))
            .unwrap_or_default()
    }

    fn a_pr(number: u64, head_sha: &str) -> PullRequest {
        PullRequest {
            number,
            title: format!("Feature {number}"),
            head_sha: head_sha.to_owned(),
            base_ref: "main".to_owned(),
            head_ref: format!("feature/{number}"),
            author: "octocat".to_owned(),
            is_draft: false,
            updated_at: Some(at(0)),
            url: format!("https://github.com/owner/repo/pull/{number}"),
            commit_shas: Vec::new(),
        }
    }

    /// A source that returns whatever it was given.
    struct Stub(Vec<PullRequest>);

    #[async_trait::async_trait]
    impl PullRequestSource for Stub {
        async fn open_pull_requests(
            &self,
            _base_branches: &[String],
        ) -> Result<Vec<PullRequest>, revlocal_vcs::GitError> {
            Ok(self.0.clone())
        }
    }

    /// Discover through the stub.
    ///
    /// Returns `Result`; helpers are not `#[test]` fns, so the unwrap/expect/panic
    /// ban applies to them (ADR 0003).
    async fn discover(
        prs: Vec<PullRequest>,
        config: &RepoConfig,
    ) -> Result<Vec<DetectedChange>, String> {
        discover_pull_requests(&Stub(prs), &["main".to_owned()], config)
            .await
            .map_err(|e| format!("discover: {e}"))
    }

    // --- a re-push is a new change -------------------------------------------

    #[tokio::test]
    async fn github_pr_a_re_push_produces_a_second_change_not_a_mutated_first() {
        // Acceptance criterion 1. If the identity were the PR number alone, the
        // second discovery would upsert onto the first row (the store keys on
        // `(repo_id, kind, external_id)`), and a finding filed against the reviewed
        // head would start describing code nobody reviewed.
        let before = discover(vec![a_pr(7, "aaaaaaa")], &RepoConfig::default())
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let after = discover(vec![a_pr(7, "bbbbbbb")], &RepoConfig::default())
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(before[0].external_id, "7:aaaaaaa");
        assert_eq!(after[0].external_id, "7:bbbbbbb");
        assert_ne!(
            before[0].external_id, after[0].external_id,
            "a re-push must not land on the same change row"
        );
        assert_eq!(before[0].kind, ChangeKind::Pr);
    }

    #[tokio::test]
    async fn github_pr_the_cursor_advances_on_activity_that_does_not_move_the_head() {
        // §6.3 orders PR discovery by `updated_at`, and a PR can change without its
        // head moving — a label, a base change, a comment. If the cursor were the
        // head SHA, those would be re-reported forever.
        let mut pr = a_pr(7, "aaaaaaa");
        pr.updated_at = Some(at(5));
        let changes = discover(vec![pr], &RepoConfig::default())
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        assert!(
            changes[0].cursor_value.starts_with("2026-08-27T12:05"),
            "the PR cursor is updated_at, not the head sha: {}",
            changes[0].cursor_value
        );
        assert_ne!(changes[0].cursor_value, changes[0].external_id);
    }

    #[tokio::test]
    async fn github_pr_discovery_is_ordered_by_activity_oldest_first() {
        // Same reason commit discovery is oldest-first: reviews publish in the order
        // things happened, and newest-first would put a fix's review above the bug's.
        let mut old = a_pr(1, "aaa");
        old.updated_at = Some(at(1));
        let mut recent = a_pr(2, "bbb");
        recent.updated_at = Some(at(9));

        let changes = discover(vec![recent, old], &RepoConfig::default())
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let ids: Vec<&str> = changes.iter().map(|c| c.external_id.as_str()).collect();
        assert_eq!(ids, ["1:aaa", "2:bbb"]);
    }

    // --- drafts ---------------------------------------------------------------

    #[tokio::test]
    async fn github_pr_drafts_are_skipped_by_default_but_still_visible() {
        // Acceptance criterion 3. Skipped, not omitted: a user who wonders why their
        // draft has no review must be able to see the reason (SPEC §18).
        let mut draft = a_pr(7, "aaa");
        draft.is_draft = true;

        let changes = discover(vec![draft], &RepoConfig::default())
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(changes.len(), 1, "the draft must still appear as a change");

        let reason = changes[0].skip_reason.clone().unwrap_or_default();
        assert!(reason.starts_with("draft_pr: "), "{reason}");
        assert!(
            reason.contains("#7"),
            "the reason must name the PR: {reason}"
        );
        assert!(
            reason.contains("review_draft_prs"),
            "and the setting that would change it: {reason}"
        );
    }

    #[tokio::test]
    async fn github_pr_drafts_are_reviewed_when_the_repo_asks() {
        let mut draft = a_pr(7, "aaa");
        draft.is_draft = true;
        let config = RepoConfig {
            review_draft_prs: true,
            ..RepoConfig::default()
        };

        let changes = discover(vec![draft], &config)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(changes[0].skip_reason, None);
    }

    // --- covered_by_pr --------------------------------------------------------

    #[tokio::test]
    async fn github_pr_commits_covered_by_an_open_pr_are_skipped() {
        // Acceptance criterion 4. Without this, a ten-commit PR with both review
        // modes on would be reviewed eleven times and file the same findings from
        // each.
        let mut pr = a_pr(7, "ccc");
        pr.commit_shas = vec!["aaa".to_owned(), "bbb".to_owned(), "ccc".to_owned()];

        let mut commits: Vec<DetectedChange> = ["aaa", "bbb", "ccc", "zzz"]
            .iter()
            .map(|sha| DetectedChange {
                kind: ChangeKind::Commit,
                external_id: (*sha).to_owned(),
                title: None,
                author_name: None,
                author_email: None,
                authored_at: None,
                branch: Some("main".to_owned()),
                base_ref: None,
                parents: vec!["parent".to_owned()],
                paths: vec!["src/main.rs".to_owned()],
                head_ref: Some((*sha).to_owned()),
                url: None,
                diff_stat: revlocal_core::DiffStat::default(),
                skip_reason: None,
                cursor_value: (*sha).to_owned(),
            })
            .collect();

        let marked = mark_covered_by_pr(&mut commits, &[pr]);
        assert_eq!(marked, 3);

        for change in commits.iter().take(3) {
            let reason = change.skip_reason.clone().unwrap_or_default();
            assert!(reason.starts_with("covered_by_pr: "), "{reason}");
            assert!(
                reason.contains("#7"),
                "the reason must name the PR doing the covering: {reason}"
            );
        }
        assert_eq!(
            commits[3].skip_reason, None,
            "a commit no open PR contains must still be reviewed"
        );
    }

    #[tokio::test]
    async fn github_pr_covering_does_not_overwrite_an_existing_skip_reason() {
        // A merge commit inside a PR is already skipped as a merge. Overwriting that
        // would lose the more specific reason and change what the user is told.
        let mut pr = a_pr(7, "aaa");
        pr.commit_shas = vec!["aaa".to_owned()];

        let mut commits = vec![DetectedChange {
            kind: ChangeKind::Commit,
            external_id: "aaa".to_owned(),
            title: None,
            author_name: None,
            author_email: None,
            authored_at: None,
            branch: None,
            base_ref: None,
            parents: vec!["p1".to_owned(), "p2".to_owned()],
            paths: Vec::new(),
            head_ref: None,
            url: None,
            diff_stat: revlocal_core::DiffStat::default(),
            skip_reason: Some("merge_commit: 2 parents".to_owned()),
            cursor_value: "aaa".to_owned(),
        }];

        assert_eq!(mark_covered_by_pr(&mut commits, &[pr]), 0);
        assert!(
            commits[0]
                .skip_reason
                .as_deref()
                .unwrap_or_default()
                .starts_with("merge_commit"),
            "the first reason wins"
        );
    }

    // --- supersession ---------------------------------------------------------

    #[test]
    fn github_pr_findings_that_do_not_recur_on_the_new_head_are_superseded() {
        // Acceptance criterion 2. A re-push makes a new Change, so the previous
        // head's findings hang off a run nobody will look at again. The ones that
        // recur are re-reported naturally; the ones that do not are superseded —
        // NOT deleted. The finding still happened, and an audit trail that quietly
        // loses findings is not one.
        let previous: BTreeSet<String> = ["fp-fixed".to_owned(), "fp-still-there".to_owned()]
            .into_iter()
            .collect();
        let current: BTreeSet<String> = ["fp-still-there".to_owned(), "fp-new".to_owned()]
            .into_iter()
            .collect();

        let superseded = superseded_fingerprints(&previous, &current);
        assert_eq!(
            superseded,
            ["fp-fixed"],
            "only the one that stopped recurring"
        );
    }

    #[test]
    fn github_pr_a_finding_that_recurs_is_not_superseded_even_after_a_rebase() {
        // §10.3's fingerprint is line-number independent on purpose, so a finding
        // that survived a rebase keeps its fingerprint and must not be re-filed.
        let previous: BTreeSet<String> = ["fp-a".to_owned()].into_iter().collect();
        let current: BTreeSet<String> = ["fp-a".to_owned()].into_iter().collect();
        assert!(superseded_fingerprints(&previous, &current).is_empty());
    }

    #[test]
    fn github_pr_a_head_with_no_findings_supersedes_everything_from_the_previous_one() {
        // The "they fixed it all" case: every previous finding stops recurring.
        let previous: BTreeSet<String> = ["a".to_owned(), "b".to_owned()].into_iter().collect();
        let superseded = superseded_fingerprints(&previous, &BTreeSet::new());
        assert_eq!(superseded.len(), 2);
    }

    // --- parsing what `gh` actually emits -------------------------------------

    #[test]
    fn github_pr_parses_a_real_gh_pr_list_payload() {
        // The part that breaks when `gh` changes its output, and the part that can
        // be tested without a network.
        let payload = r#"[
          {
            "number": 42,
            "title": "Add pagination",
            "headRefOid": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "baseRefName": "main",
            "headRefName": "feature/pagination",
            "author": { "login": "octocat", "is_bot": false },
            "isDraft": false,
            "updatedAt": "2026-08-27T12:00:00Z",
            "url": "https://github.com/owner/repo/pull/42",
            "commits": [
              { "oid": "1111111111111111111111111111111111111111" },
              { "oid": "2222222222222222222222222222222222222222" }
            ]
          }
        ]"#;

        let parsed = parse_gh_pr_list(payload).unwrap_or_else(|e| panic!("parse: {e}"));
        assert_eq!(parsed.len(), 1);

        let pr = &parsed[0];
        assert_eq!(pr.number, 42);
        assert_eq!(pr.author, "octocat", "author is an object with a login");
        assert!(!pr.is_draft);
        assert_eq!(pr.commit_shas.len(), 2);
        assert_eq!(
            pr.external_id(),
            "42:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert!(pr.updated_at.is_some());
    }

    #[test]
    fn github_pr_an_unknown_field_does_not_stop_discovery() {
        // A newer `gh` adding a key must not break the adapter. Rejecting unknown
        // fields would turn a GitHub CLI upgrade into an outage.
        let payload = r#"[{
          "number": 1, "title": "t", "headRefOid": "abc", "baseRefName": "main",
          "headRefName": "f", "author": {"login": "x"}, "isDraft": false,
          "updatedAt": "2026-08-27T12:00:00Z", "url": "u", "commits": [],
          "somethingGhAddedLater": {"nested": true}
        }]"#;
        let parsed = parse_gh_pr_list(payload).unwrap_or_else(|e| panic!("parse: {e}"));
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn github_pr_an_older_gh_with_a_string_author_still_parses() {
        let payload = r#"[{
          "number": 1, "title": "t", "headRefOid": "abc", "baseRefName": "main",
          "headRefName": "f", "author": "octocat", "isDraft": true,
          "updatedAt": "2026-08-27T12:00:00Z", "url": "u", "commits": []
        }]"#;
        let parsed = parse_gh_pr_list(payload).unwrap_or_else(|e| panic!("parse: {e}"));
        assert_eq!(parsed[0].author, "octocat");
        assert!(parsed[0].is_draft);
    }

    #[test]
    fn github_pr_a_pr_missing_its_head_sha_is_dropped_not_guessed() {
        // Without a head SHA there is no identity. Inventing one would create a
        // change that can never be matched again.
        let payload = r#"[{"number": 1, "title": "t", "baseRefName": "main"}]"#;
        let parsed = parse_gh_pr_list(payload).unwrap_or_else(|e| panic!("parse: {e}"));
        assert!(parsed.is_empty());
    }

    #[test]
    fn github_pr_the_requested_fields_cover_everything_the_parser_reads() {
        // Asking for a field the parser ignores is harmless. Parsing a field that
        // was never requested yields null for every PR and looks like GitHub
        // returning nothing — so the request list is asserted against the parser.
        for field in [
            "number",
            "title",
            "headRefOid",
            "baseRefName",
            "headRefName",
            "author",
            "isDraft",
            "updatedAt",
            "url",
            "commits",
        ] {
            assert!(
                GH_PR_FIELDS.split(',').any(|f| f == field),
                "the parser reads `{field}` but `gh` is never asked for it"
            );
        }
    }

    #[tokio::test]
    async fn github_pr_a_change_carries_no_invented_file_list() {
        // A PR listing does not include files. Guessing an empty one and letting the
        // skip rules act on it would report every PR as an empty diff.
        let changes = discover(vec![a_pr(7, "aaa")], &RepoConfig::default())
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(changes[0].paths.is_empty());
        assert_eq!(
            changes[0].diff_stat,
            revlocal_core::DiffStat::default(),
            "the stat is filled by materialization, not guessed here"
        );
    }
}
