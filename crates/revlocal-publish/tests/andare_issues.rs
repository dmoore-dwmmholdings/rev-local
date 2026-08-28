//! Filing findings into Andare with fingerprint dedupe (RL-705, SPEC §11.4).
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::TimeZone;
use revlocal_core::{
    Capability, Category, Finding, FindingId, FindingState, PublishAction, PublishActionId,
    PublishActionStatus, RiskClass, RunId, Severity, Timestamp,
};
use revlocal_publish::{
    compose_issue, filing_candidates, plan, search_query, AndareOptions, AndarePayload,
    AndareTarget, AndareWriter, FilingPlan, IssueContext, IssueDraft, PublishError, PublishTarget,
    SearchOutcome, FINGERPRINT_TRAILER,
};

fn at(minute: u32) -> Timestamp {
    chrono::Utc
        .with_ymd_and_hms(2026, 8, 28, 12, minute, 0)
        .single()
        .unwrap_or_default()
}

fn finding(id: i64, severity: Severity, title: &str) -> Finding {
    Finding {
        id: FindingId::new(id),
        run_id: RunId::new(1),
        fingerprint: format!("fp-{id}"),
        severity,
        category: Category::Security,
        confidence: 0.92,
        file: Some("src/login.rs".to_owned()),
        line_start: Some(42),
        line_end: Some(48),
        title: title.to_owned(),
        body: "The query is built by string concatenation.".to_owned(),
        failure_scenario: Some("A username of `' OR 1=1 --` returns every row.".to_owned()),
        suggested_fix: Some("Use a bound parameter.".to_owned()),
        state: FindingState::Open,
        created_at: at(3),
    }
}

fn options() -> AndareOptions {
    AndareOptions {
        project: "REVL".to_owned(),
        min_severity: Severity::High,
    }
}

fn context() -> IssueContext {
    IssueContext {
        change_ref: Some("https://github.com/acme/widgets/pull/7".to_owned()),
        trama_url: Some("https://trama.example.com/eng/review-12".to_owned()),
        code_excerpt: Some(
            "let q = format!(\"SELECT * FROM users WHERE name = '{name}'\");".to_owned(),
        ),
    }
}

// --- criterion 2: the body carries everything §11.4 lists -----------------

#[test]
fn andare_issues_the_body_carries_every_section_the_spec_lists() {
    let draft = compose_issue(
        &finding(1, Severity::High, "SQL injection in login"),
        &context(),
        &options(),
    );

    assert_eq!(draft.project, "REVL");
    assert_eq!(
        draft.summary, "SQL injection in login",
        "ADR 0028: Andare's field is `summary`, not `title`"
    );

    for expected in [
        "The query is built by string concatenation.",
        "## Failure scenario",
        "' OR 1=1 --",
        "## Code",
        "SELECT * FROM users",
        "## Suggested fix",
        "src/login.rs:42-48",
        "https://github.com/acme/widgets/pull/7",
        "https://trama.example.com/eng/review-12",
    ] {
        assert!(
            draft.description.contains(expected),
            "missing `{expected}` from:\n{}",
            draft.description
        );
    }

    assert!(
        draft
            .description
            .contains(&format!("{FINGERPRINT_TRAILER} fp-1")),
        "the trailer is the idempotency key:\n{}",
        draft.description
    );
}

#[test]
fn andare_issues_an_absent_section_says_so_rather_than_vanishing() {
    let mut bare = finding(2, Severity::High, "Something");
    bare.failure_scenario = None;
    bare.suggested_fix = None;

    let draft = compose_issue(&bare, &IssueContext::default(), &options());

    assert!(
        draft.description.contains("Change: not recorded"),
        "a body that omits the link reads as a finding with no change, which is a \
         different claim:\n{}",
        draft.description
    );
    assert!(draft.description.contains("Review page: not published"));
    assert!(!draft.description.contains("## Failure scenario"));
}

// --- criterion 3: below-threshold findings do not become issues -----------

#[test]
fn andare_issues_below_threshold_findings_do_not_create_issues() {
    let findings = vec![
        finding(1, Severity::Critical, "Critical"),
        finding(2, Severity::High, "High"),
        finding(3, Severity::Medium, "Medium"),
        finding(4, Severity::Low, "Low"),
        finding(5, Severity::Info, "Info"),
    ];

    let candidates = filing_candidates(&findings, &options());
    assert_eq!(
        candidates.len(),
        2,
        "§11.4's default threshold is `high`; medium and below stay in the review"
    );

    for f in [&findings[2], &findings[3], &findings[4]] {
        assert_eq!(
            plan(f, &context(), &options(), &SearchOutcome::NotFound),
            FilingPlan::BelowThreshold,
            "{} should not be filed",
            f.title
        );
    }
}

#[test]
fn andare_issues_the_threshold_is_configurable() {
    let permissive = AndareOptions {
        min_severity: Severity::Low,
        ..options()
    };
    let findings = vec![
        finding(1, Severity::Low, "Low"),
        finding(2, Severity::Info, "Info"),
    ];

    assert_eq!(filing_candidates(&findings, &permissive).len(), 1);
}

// --- criterion 1: a re-run comments rather than duplicating ---------------

#[test]
fn andare_issues_a_known_fingerprint_becomes_a_comment_not_a_second_issue() {
    let found = SearchOutcome::Found("REVL-42".to_owned());
    let outcome = plan(
        &finding(1, Severity::High, "SQL injection in login"),
        &context(),
        &options(),
        &found,
    );

    let FilingPlan::CommentOn { key, body } = outcome else {
        panic!("expected a comment, got {outcome:?}");
    };
    assert_eq!(key, "REVL-42");
    assert!(body.contains("saw this again"));
    assert!(
        body.contains("fp-1"),
        "the comment carries the fingerprint too, so the thread stays greppable"
    );
}

#[test]
fn andare_issues_the_search_query_is_scoped_to_the_project() {
    let query = search_query("REVL", "fp-1");
    assert!(query.contains("project = \"REVL\""));
    assert!(query.contains(FINGERPRINT_TRAILER));
    assert!(query.contains("fp-1"));
}

// --- criterion 4: without search, do not file -----------------------------

#[test]
fn andare_issues_an_unmapped_search_degrades_to_comment_only_and_says_so() {
    let outcome = plan(
        &finding(1, Severity::High, "SQL injection in login"),
        &context(),
        &options(),
        &SearchOutcome::Unavailable,
    );

    let FilingPlan::Degraded { reason } = outcome else {
        panic!("expected degradation, got {outcome:?}");
    };

    assert!(reason.contains("search"), "{reason}");
    assert!(
        reason.contains("duplicate"),
        "the reason must say what it is protecting against: {reason}"
    );
    assert!(
        reason.contains("revlocal targets map"),
        "§18: a user-visible message says what to do about it: {reason}"
    );
}

// --- the target end to end -------------------------------------------------

#[derive(Default)]
struct FakeAndare {
    issues: Mutex<Vec<(String, String)>>,
    comments: Mutex<Vec<(String, String)>>,
    creates: Arc<AtomicUsize>,
    searchable: bool,
}

#[async_trait]
impl AndareWriter for FakeAndare {
    fn can_search(&self) -> bool {
        self.searchable
    }

    async fn search(&self, query: &str) -> Result<Option<String>, PublishError> {
        let issues = self.issues.lock().map_err(|e| PublishError::Transport {
            target: "andare".to_owned(),
            detail: e.to_string(),
        })?;
        // Crude but faithful: the AQL asks for a trailer substring, so match on it.
        let needle = query
            .rsplit_once(FINGERPRINT_TRAILER)
            .map(|(_, rest)| rest.trim().trim_end_matches('"').to_owned())
            .unwrap_or_default();
        Ok(issues
            .iter()
            .find(|(_, body)| body.contains(&needle))
            .map(|(key, _)| key.clone()))
    }

    async fn create_issue(&self, draft: &IssueDraft) -> Result<String, PublishError> {
        self.creates.fetch_add(1, Ordering::SeqCst);
        let mut issues = self.issues.lock().map_err(|e| PublishError::Transport {
            target: "andare".to_owned(),
            detail: e.to_string(),
        })?;
        let key = format!("REVL-{}", issues.len() + 1);
        issues.push((key.clone(), draft.description.clone()));
        Ok(key)
    }

    async fn comment(&self, key: &str, body: &str) -> Result<(), PublishError> {
        let mut comments = self.comments.lock().map_err(|e| PublishError::Transport {
            target: "andare".to_owned(),
            detail: e.to_string(),
        })?;
        comments.push((key.to_owned(), body.to_owned()));
        Ok(())
    }

    async fn set_status(&self, _key: &str, _status: &str) -> Result<(), PublishError> {
        // RL-706's path; this fixture is about filing.
        Ok(())
    }
}

fn andare_action(payload: &AndarePayload) -> PublishAction {
    PublishAction {
        id: PublishActionId::new(1),
        run_id: RunId::new(1),
        finding_id: Some(FindingId::new(1)),
        target: "andare".to_owned(),
        capability: Capability::CreateIssue,
        risk: RiskClass::High,
        idempotency_key: format!("andare:{}", payload.draft.fingerprint),
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

fn payload_for(f: &Finding) -> AndarePayload {
    AndarePayload {
        draft: compose_issue(f, &context(), &options()),
        context: context(),
        recurrence_body: revlocal_publish::recurrence_comment(f, &context()),
    }
}

#[tokio::test]
async fn andare_issues_re_running_the_same_review_comments_instead_of_duplicating() {
    let writer = FakeAndare {
        searchable: true,
        ..FakeAndare::default()
    };
    let creates = Arc::clone(&writer.creates);
    let target = AndareTarget::new(writer);

    let f = finding(1, Severity::High, "SQL injection in login");
    let payload = payload_for(&f);

    let first = target
        .execute(&andare_action(&payload))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(first.external_ref.as_deref(), Some("REVL-1"));
    assert!(!first.deduplicated);
    assert_eq!(creates.load(Ordering::SeqCst), 1);

    // The same review runs again — a retry, a re-push, a manual re-review.
    let second = target
        .execute(&andare_action(&payload))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(
        creates.load(Ordering::SeqCst),
        1,
        "§11.4: a second run comments on the existing issue rather than filing a \
         second one"
    );
    assert_eq!(second.external_ref.as_deref(), Some("REVL-1"));
    assert!(
        second.deduplicated,
        "§11.6 wants a landing-on-existing distinguishable from a fresh filing in \
         the audit log"
    );
}

#[tokio::test]
async fn andare_issues_a_target_without_search_refuses_to_file() {
    let writer = FakeAndare {
        searchable: false,
        ..FakeAndare::default()
    };
    let creates = Arc::clone(&writer.creates);
    let target = AndareTarget::new(writer);

    let f = finding(1, Severity::High, "SQL injection in login");
    let error = target
        .execute(&andare_action(&payload_for(&f)))
        .await
        .expect_err("filing blind would duplicate on every run");

    assert!(
        matches!(error, PublishError::Unsupported { .. }),
        "{error:?}"
    );
    assert!(
        !error.is_retryable(),
        "retrying does not map a capability: {error}"
    );
    assert_eq!(creates.load(Ordering::SeqCst), 0, "nothing was filed");
}

#[tokio::test]
async fn andare_issues_health_says_why_filing_is_disabled() {
    let target = AndareTarget::new(FakeAndare {
        searchable: false,
        ..FakeAndare::default()
    });

    let health = target.health().await.unwrap_or_else(|e| panic!("{e}"));
    assert!(health.reachable, "the server answered; it is not down");
    assert!(
        !health.capabilities.supports(Capability::CreateIssue),
        "a capability that would duplicate must not be advertised"
    );
    assert!(
        health.capabilities.supports(Capability::Comment),
        "commenting is still safe without search"
    );
    assert!(
        health
            .detail
            .as_deref()
            .is_some_and(|d| d.contains("duplicate")),
        "{:?}",
        health.detail
    );
}

#[tokio::test]
async fn andare_issues_a_searchable_target_advertises_filing() {
    let target = AndareTarget::new(FakeAndare {
        searchable: true,
        ..FakeAndare::default()
    });
    let capabilities = target.discover().await.unwrap_or_else(|e| panic!("{e}"));

    for capability in [
        Capability::CreateIssue,
        Capability::SetStatus,
        Capability::Comment,
    ] {
        assert!(
            capabilities.supports(capability),
            "§11.4 lists {capability:?}"
        );
    }
}
