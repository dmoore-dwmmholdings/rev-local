//! The GitHub review target (RL-703, SPEC §11.3).
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::TimeZone;
use revlocal_core::{
    Capability, Category, Finding, FindingId, FindingState, PublishAction, PublishActionId,
    PublishActionStatus, RiskClass, RunId, Severity, Timestamp, Verdict,
};
use revlocal_publish::{
    compose, event_for, idempotency_key, DiffAnchors, ExistingReview, GitHubTarget, GitHubWriter,
    PublishError, PublishTarget, ReviewEvent, ReviewOptions, ReviewPayload,
};

fn at(minute: u32) -> Timestamp {
    chrono::Utc
        .with_ymd_and_hms(2026, 8, 28, 12, minute, 0)
        .single()
        .unwrap_or_default()
}

fn finding(
    id: i64,
    severity: Severity,
    file: Option<&str>,
    line_start: Option<u32>,
    line_end: Option<u32>,
    title: &str,
) -> Finding {
    Finding {
        id: FindingId::new(id),
        run_id: RunId::new(1),
        fingerprint: format!("fp-{id}"),
        severity,
        category: Category::Correctness,
        confidence: 0.9,
        file: file.map(str::to_owned),
        line_start,
        line_end,
        title: title.to_owned(),
        body: "The loop reads one element past the end of the slice.".to_owned(),
        failure_scenario: None,
        suggested_fix: None,
        state: FindingState::Open,
        created_at: at(3),
    }
}

/// A diff that contains `src/main.rs` lines 10..20.
fn anchors() -> DiffAnchors {
    let mut anchors = DiffAnchors::none();
    anchors.add("src/main.rs", 10, 20);
    anchors
}

// --- criterion 3: the zero-findings body -----------------------------------

#[test]
fn github_review_a_clean_review_renders_as_a_result_not_an_empty_heading() {
    let draft = compose(
        Some("Reviewed 3 files. Nothing to flag."),
        &[],
        &anchors(),
        Verdict::Approve,
        ReviewOptions::default(),
    );

    assert!(draft.body.contains("## rev-local review"));
    assert!(draft.body.contains("Reviewed 3 files"));
    assert!(
        draft.body.contains("No findings."),
        "most reviews of a small change find nothing; a body that renders as a bare \
         heading reads like a broken tool rather than a clean result:\n{}",
        draft.body
    );
    assert!(draft.comments.is_empty());
    assert!(
        !draft.body.contains("Findings outside the diff"),
        "there is nothing to demote"
    );
}

#[test]
fn github_review_a_missing_summary_does_not_leave_a_gap() {
    let draft = compose(
        None,
        &[],
        &anchors(),
        Verdict::Comment,
        ReviewOptions::default(),
    );
    assert!(draft
        .body
        .starts_with("## rev-local review\n\nNo findings."));

    let blank = compose(
        Some("   "),
        &[],
        &anchors(),
        Verdict::Comment,
        ReviewOptions::default(),
    );
    assert_eq!(blank.body, draft.body, "a blank summary is no summary");
}

// --- criterion 2: unanchorable comments are demoted, not dropped ----------

#[test]
fn github_review_an_unanchorable_finding_appears_in_the_body() {
    let findings = vec![
        finding(
            1,
            Severity::High,
            Some("src/main.rs"),
            Some(12),
            Some(12),
            "Off-by-one in the bounds check",
        ),
        // Outside the diff's hunk.
        finding(
            2,
            Severity::Medium,
            Some("src/main.rs"),
            Some(400),
            Some(400),
            "Unvalidated input reaches the query",
        ),
        // A file the diff does not touch at all.
        finding(
            3,
            Severity::Low,
            Some("src/other.rs"),
            Some(5),
            Some(5),
            "Shadowed binding",
        ),
        // No location at all.
        finding(4, Severity::Info, None, None, None, "Consider a CHANGELOG"),
    ];

    let draft = compose(
        None,
        &findings,
        &anchors(),
        Verdict::RequestChanges,
        ReviewOptions::default(),
    );

    assert_eq!(draft.comments.len(), 1, "only one finding is in the diff");
    assert_eq!(draft.comments[0].path, "src/main.rs");
    assert_eq!(draft.comments[0].line, 12);

    assert!(draft.body.contains("## Findings outside the diff"));
    for title in [
        "Unvalidated input reaches the query",
        "Shadowed binding",
        "Consider a CHANGELOG",
    ] {
        assert!(
            draft.body.contains(title),
            "§18: a dropped comment makes the review look like it found less than it \
             did. `{title}` is missing from:\n{}",
            draft.body
        );
    }
    assert_eq!(draft.demoted_count(), 3);

    // And every finding is still in the table, anchored or not.
    for f in &findings {
        assert!(
            draft.body.contains(&f.title),
            "the table must list everything: {}",
            f.title
        );
    }
}

#[test]
fn github_review_a_range_that_starts_outside_the_diff_narrows_rather_than_demotes() {
    // 5..12 — the end is in the diff, the start is not.
    let findings = vec![finding(
        1,
        Severity::High,
        Some("src/main.rs"),
        Some(5),
        Some(12),
        "Range spilling above the hunk",
    )];

    let draft = compose(
        None,
        &findings,
        &anchors(),
        Verdict::Comment,
        ReviewOptions::default(),
    );

    assert_eq!(draft.comments.len(), 1);
    assert_eq!(draft.comments[0].line, 12);
    assert_eq!(
        draft.comments[0].start_line, None,
        "GitHub rejects a multi-line comment whose start is outside the diff, so it \
         narrows to the anchorable end — half the context beats none of the comment"
    );
    assert_eq!(draft.demoted_count(), 0);
}

#[test]
fn github_review_a_range_wholly_inside_the_diff_keeps_both_ends() {
    let findings = vec![finding(
        1,
        Severity::High,
        Some("src/main.rs"),
        Some(11),
        Some(14),
        "Multi-line finding",
    )];

    let draft = compose(
        None,
        &findings,
        &anchors(),
        Verdict::Comment,
        ReviewOptions::default(),
    );
    assert_eq!(draft.comments[0].start_line, Some(11));
    assert_eq!(draft.comments[0].line, 14);
}

// --- criterion 4: APPROVE is opt-in ---------------------------------------

#[test]
fn github_review_approve_is_never_submitted_unless_explicitly_allowed() {
    let default = ReviewOptions::default();
    assert!(!default.allow_approve, "the default must be off");

    assert_eq!(
        event_for(Verdict::Approve, default),
        ReviewEvent::Comment,
        "§10.2: an approving review can satisfy a branch protection rule, and \
         granting that by default decides something about somebody's merge process \
         that they did not ask for"
    );

    assert_eq!(
        event_for(
            Verdict::Approve,
            ReviewOptions {
                allow_approve: true,
                ..default
            }
        ),
        ReviewEvent::Approve
    );

    // And a composed review carries the same decision.
    let draft = compose(None, &[], &anchors(), Verdict::Approve, default);
    assert_eq!(draft.event, ReviewEvent::Comment);
    assert_eq!(draft.event.as_str(), "COMMENT");
}

#[test]
fn github_review_request_changes_only_blocks_when_the_repo_asked_for_it() {
    let default = ReviewOptions::default();
    assert!(!default.block_on_findings, "SPEC §11.3: default is false");

    assert_eq!(
        event_for(Verdict::RequestChanges, default),
        ReviewEvent::Comment
    );
    assert_eq!(
        event_for(
            Verdict::RequestChanges,
            ReviewOptions {
                block_on_findings: true,
                ..default
            }
        ),
        ReviewEvent::RequestChanges
    );
}

// --- criterion 1: a second publish edits rather than duplicates -----------

/// A fake GitHub that records what it was asked to do.
#[derive(Default)]
struct FakeGitHub {
    reviews: Mutex<Vec<(String, u64, String, String)>>,
    creates: Arc<AtomicUsize>,
    updates: Arc<AtomicUsize>,
}

#[async_trait]
impl GitHubWriter for FakeGitHub {
    async fn find_review(
        &self,
        repo: &str,
        pr: u64,
        head_sha: &str,
    ) -> Result<Option<ExistingReview>, PublishError> {
        let reviews = self.reviews.lock().map_err(|e| PublishError::Transport {
            target: "github".to_owned(),
            detail: e.to_string(),
        })?;
        Ok(reviews
            .iter()
            .position(|(r, p, sha, _)| r == repo && *p == pr && sha == head_sha)
            .map(|index| ExistingReview {
                id: index as u64 + 1,
                url: Some(format!(
                    "https://github.com/{repo}/pull/{pr}#review-{}",
                    index + 1
                )),
            }))
    }

    async fn create_review(&self, payload: &ReviewPayload) -> Result<ExistingReview, PublishError> {
        self.creates.fetch_add(1, Ordering::SeqCst);
        let mut reviews = self.reviews.lock().map_err(|e| PublishError::Transport {
            target: "github".to_owned(),
            detail: e.to_string(),
        })?;
        reviews.push((
            payload.repo.clone(),
            payload.pr,
            payload.head_sha.clone(),
            payload.body.clone(),
        ));
        let id = reviews.len() as u64;
        Ok(ExistingReview {
            id,
            url: Some(format!(
                "https://github.com/{}/pull/{}#review-{id}",
                payload.repo, payload.pr
            )),
        })
    }

    async fn update_review(
        &self,
        repo: &str,
        review_id: u64,
        body: &str,
    ) -> Result<ExistingReview, PublishError> {
        self.updates.fetch_add(1, Ordering::SeqCst);
        let mut reviews = self.reviews.lock().map_err(|e| PublishError::Transport {
            target: "github".to_owned(),
            detail: e.to_string(),
        })?;
        let index = review_id as usize - 1;
        if let Some(entry) = reviews.get_mut(index) {
            entry.3 = body.to_owned();
        }
        Ok(ExistingReview {
            id: review_id,
            url: Some(format!(
                "https://github.com/{repo}/pull/7#review-{review_id}"
            )),
        })
    }
}

fn review_action(payload: &ReviewPayload, key: &str) -> PublishAction {
    PublishAction {
        id: PublishActionId::new(1),
        run_id: RunId::new(1),
        finding_id: None,
        target: "github".to_owned(),
        capability: Capability::PostReview,
        risk: RiskClass::Low,
        idempotency_key: key.to_owned(),
        payload_json: serde_json::to_string(payload).unwrap_or_default(),
        status: PublishActionStatus::Pending,
        attempts: 0,
        response_json: None,
        external_ref: None,
        error: None,
        created_at: at(3),
        sent_at: None,
    }
}

#[tokio::test]
async fn github_review_a_second_publish_for_the_same_head_sha_edits_the_first() {
    let github = FakeGitHub::default();
    let creates = Arc::clone(&github.creates);
    let updates = Arc::clone(&github.updates);
    let target = GitHubTarget::new(github);

    let draft = compose(
        Some("First pass."),
        &[],
        &anchors(),
        Verdict::Comment,
        ReviewOptions::default(),
    );
    let payload = ReviewPayload::new("acme/widgets", 7, "abc123", &draft);
    let key = idempotency_key("acme/widgets", 7, "abc123", "pr");

    let first = target
        .execute(&review_action(&payload, &key))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(creates.load(Ordering::SeqCst), 1);
    assert_eq!(updates.load(Ordering::SeqCst), 0);
    assert!(first.external_ref.is_some());

    // The run is re-run against the same commit — a retry, or a manual re-review.
    let second_draft = compose(
        Some("Second pass, one more finding."),
        &[finding(
            1,
            Severity::High,
            Some("src/main.rs"),
            Some(12),
            Some(12),
            "Off-by-one in the bounds check",
        )],
        &anchors(),
        Verdict::Comment,
        ReviewOptions::default(),
    );
    let second_payload = ReviewPayload::new("acme/widgets", 7, "abc123", &second_draft);

    target
        .execute(&review_action(&second_payload, &key))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(
        creates.load(Ordering::SeqCst),
        1,
        "§11.3: on a re-run for the same head SHA, edit the existing review rather \
         than posting a second one"
    );
    assert_eq!(updates.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn github_review_a_new_head_sha_earns_a_new_review() {
    let github = FakeGitHub::default();
    let creates = Arc::clone(&github.creates);
    let target = GitHubTarget::new(github);

    let draft = compose(
        None,
        &[],
        &anchors(),
        Verdict::Comment,
        ReviewOptions::default(),
    );

    for sha in ["abc123", "def456"] {
        let payload = ReviewPayload::new("acme/widgets", 7, sha, &draft);
        let key = idempotency_key("acme/widgets", 7, sha, "pr");
        target
            .execute(&review_action(&payload, &key))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
    }

    assert_eq!(
        creates.load(Ordering::SeqCst),
        2,
        "a new push earns its own review; the old one stays attached to the commit \
         it was about"
    );
}

#[test]
fn github_review_the_idempotency_key_is_the_one_the_spec_names() {
    assert_eq!(
        idempotency_key("acme/widgets", 7, "abc123", "pr"),
        "gh:acme/widgets:7:abc123:pr"
    );
}

#[tokio::test]
async fn github_review_a_payload_that_is_not_a_review_is_terminal_not_retried() {
    let target = GitHubTarget::new(FakeGitHub::default());
    let mut action = review_action(
        &ReviewPayload::new(
            "acme/widgets",
            7,
            "abc",
            &compose(
                None,
                &[],
                &anchors(),
                Verdict::Comment,
                ReviewOptions::default(),
            ),
        ),
        "k",
    );
    action.payload_json = "{\"not\":\"a review\"}".to_owned();

    let error = target
        .execute(&action)
        .await
        .expect_err("a malformed payload cannot be delivered");

    assert!(
        !error.is_retryable(),
        "retrying will not make the stored payload parse: {error}"
    );
}

#[tokio::test]
async fn github_review_reports_the_capabilities_spec_11_3_names() {
    let target = GitHubTarget::new(FakeGitHub::default());
    let capabilities = target.discover().await.unwrap_or_else(|e| panic!("{e}"));

    for capability in [
        Capability::PostReview,
        Capability::Comment,
        Capability::SetCheck,
    ] {
        assert!(
            capabilities.supports(capability),
            "§11.3 lists {capability:?}"
        );
    }
}

// --- the table ------------------------------------------------------------

#[test]
fn github_review_the_table_reads_worst_first() {
    let findings = vec![
        finding(1, Severity::Low, Some("a.rs"), Some(1), Some(1), "Low one"),
        finding(
            2,
            Severity::Critical,
            Some("b.rs"),
            Some(1),
            Some(1),
            "Critical one",
        ),
        finding(
            3,
            Severity::Medium,
            Some("c.rs"),
            Some(1),
            Some(1),
            "Medium one",
        ),
    ];

    let draft = compose(
        None,
        &findings,
        &DiffAnchors::none(),
        Verdict::Comment,
        ReviewOptions::default(),
    );

    let critical = draft.body.find("Critical one").unwrap_or_default();
    let medium = draft.body.find("Medium one").unwrap_or_default();
    let low = draft.body.find("Low one").unwrap_or_default();
    assert!(
        critical < medium && medium < low,
        "the table is a priority list:\n{}",
        draft.body
    );
}

#[test]
fn github_review_a_pipe_in_a_title_does_not_break_the_table() {
    let findings = vec![finding(
        1,
        Severity::High,
        Some("a.rs"),
        Some(1),
        Some(1),
        "Prefer a || b over a | b",
    )];

    let draft = compose(
        None,
        &findings,
        &DiffAnchors::none(),
        Verdict::Comment,
        ReviewOptions::default(),
    );

    let row = draft
        .body
        .lines()
        .find(|line| line.contains("Prefer a"))
        .unwrap_or_default();
    assert_eq!(
        row.matches(" | ").count() + row.matches("| ").count().min(1),
        row.matches(" | ").count() + 1,
        "sanity"
    );
    assert!(
        row.contains("\\|"),
        "an unescaped pipe ends the cell early and the row renders as gibberish: {row}"
    );
}

// --- the `gh` requests rev-local sends ------------------------------------

use revlocal_publish::{
    find_own_review, gh_create_review, gh_list_reviews, gh_update_review, REVIEW_MARKER,
};

#[test]
fn github_review_a_create_request_carries_the_body_on_stdin_not_in_flags() {
    let draft = compose(
        Some("Summary with \"quotes\" and\nnewlines."),
        &[finding(
            1,
            Severity::High,
            Some("src/main.rs"),
            Some(11),
            Some(14),
            "Off-by-one",
        )],
        &anchors(),
        Verdict::Comment,
        ReviewOptions::default(),
    );
    let payload = ReviewPayload::new("acme/widgets", 7, "abc123", &draft);

    let request = gh_create_review(&payload).unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(
        request.args,
        vec![
            "api",
            "--method",
            "POST",
            "repos/acme/widgets/pulls/7/reviews",
            "--input",
            "-"
        ]
    );

    let body: serde_json::Value = serde_json::from_str(&request.stdin.clone().unwrap_or_default())
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(body["commit_id"], "abc123");
    assert_eq!(body["event"], "COMMENT");
    assert!(
        body["body"]
            .as_str()
            .is_some_and(|b| b.contains(REVIEW_MARKER)),
        "the marker is how rev-local finds its own review again"
    );

    let comment = &body["comments"][0];
    assert_eq!(comment["path"], "src/main.rs");
    assert_eq!(comment["line"], 14);
    assert_eq!(comment["start_line"], 11);
    assert_eq!(comment["side"], "RIGHT");
    assert_eq!(
        comment["start_side"], "RIGHT",
        "GitHub rejects start_line without start_side"
    );
}

#[test]
fn github_review_a_single_line_comment_omits_the_range_fields() {
    let draft = compose(
        None,
        &[finding(
            1,
            Severity::High,
            Some("src/main.rs"),
            Some(12),
            Some(12),
            "One line",
        )],
        &anchors(),
        Verdict::Comment,
        ReviewOptions::default(),
    );
    let request = gh_create_review(&ReviewPayload::new("a/b", 1, "sha", &draft))
        .unwrap_or_else(|e| panic!("{e}"));
    let body: serde_json::Value =
        serde_json::from_str(&request.stdin.unwrap_or_default()).unwrap_or_else(|e| panic!("{e}"));

    assert!(
        body["comments"][0].get("start_line").is_none(),
        "a single-line comment with start_line == line is rejected by GitHub"
    );
}

#[test]
fn github_review_an_update_request_targets_the_review_and_sends_only_a_body() {
    let request =
        gh_update_review("acme/widgets", 7, 42, "new body").unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(
        request.args,
        vec![
            "api",
            "--method",
            "PUT",
            "repos/acme/widgets/pulls/7/reviews/42",
            "--input",
            "-"
        ]
    );

    let body: serde_json::Value =
        serde_json::from_str(&request.stdin.unwrap_or_default()).unwrap_or_else(|e| panic!("{e}"));
    assert!(body["body"]
        .as_str()
        .is_some_and(|b| b.contains("new body")));
    assert!(
        body.get("event").is_none() && body.get("comments").is_none(),
        "GitHub's update endpoint takes only a body; sending more is a 422"
    );
}

#[test]
fn github_review_listing_is_paginated() {
    let request = gh_list_reviews("acme/widgets", 7);
    assert!(
        request.args.contains(&"--paginate".to_owned()),
        "a busy pull request has more reviews than one page, and missing ours means \
         posting a duplicate"
    );
    assert!(request.stdin.is_none());
}

#[test]
fn github_review_finds_its_own_review_and_not_somebody_else_s() {
    let listing = serde_json::json!([
        { "id": 1, "commit_id": "abc123", "body": "Looks good to me", "html_url": "u1" },
        { "id": 2, "commit_id": "abc123", "body": format!("{REVIEW_MARKER}\nours"), "html_url": "u2" },
    ])
    .to_string();

    let found = find_own_review(&listing, "abc123").expect("ours is there");
    assert_eq!(found.id, 2, "a human review must not be edited");
    assert_eq!(found.url.as_deref(), Some("u2"));
}

#[test]
fn github_review_does_not_edit_its_review_of_an_earlier_push() {
    let listing = serde_json::json!([
        { "id": 1, "commit_id": "old-sha", "body": format!("{REVIEW_MARKER}\nours"), "html_url": "u1" },
    ])
    .to_string();

    assert!(
        find_own_review(&listing, "new-sha").is_none(),
        "matching on the marker alone would edit the review of a previous commit; \
         a new push earns a new review"
    );
}

#[test]
fn github_review_an_unreadable_listing_is_no_match_rather_than_a_wrong_one() {
    assert!(find_own_review("not json", "abc").is_none());
    assert!(find_own_review("{}", "abc").is_none());
}
