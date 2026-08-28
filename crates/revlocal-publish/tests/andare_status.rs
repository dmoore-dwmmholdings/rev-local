//! Reporting a review outcome onto a linked work item (RL-706, SPEC §11.4).
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use std::collections::BTreeMap;

use revlocal_core::{ActionIntent, RiskClass, Verdict};
use revlocal_publish::{outcome_comment, plan_outcomes, transition_for, KeyPattern};

/// Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.
fn pattern() -> Result<KeyPattern, String> {
    KeyPattern::default_pattern().map_err(|e| e.to_string())
}

fn transitions() -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    map.insert("request_changes".to_owned(), "In Review".to_owned());
    map
}

// --- criterion 1: key extraction ------------------------------------------

#[test]
fn andare_status_extracts_a_single_key() {
    assert_eq!(
        pattern()
            .unwrap_or_else(|e| panic!("{e}"))
            .keys("REVL-42: fix the bounds check"),
        vec!["REVL-42".to_owned()]
    );
}

#[test]
fn andare_status_extracts_multiple_keys_in_order_without_repeats() {
    let text = "REVL-42 and ENG-7: partial fix for REVL-42, follow-up in ENG-7";
    assert_eq!(
        pattern().unwrap_or_else(|e| panic!("{e}")).keys(text),
        vec!["REVL-42".to_owned(), "ENG-7".to_owned()],
        "one ticket mentioned twice is one ticket; commenting twice is noise a \
         person has to read"
    );
}

#[test]
fn andare_status_finds_a_key_inside_a_url() {
    let text = "Fixes https://andare.example.com/browse/REVL-42 and adds a test";
    assert_eq!(
        pattern().unwrap_or_else(|e| panic!("{e}")).keys(text),
        vec!["REVL-42".to_owned()],
        "pasting the ticket URL is one of the two common ways people reference a \
         ticket; skipping it would miss the case most worth handling"
    );
}

#[test]
fn andare_status_no_key_is_no_report_rather_than_an_error() {
    assert!(pattern()
        .unwrap_or_else(|e| panic!("{e}"))
        .keys("tidy up the readme")
        .is_empty());

    let reports = plan_outcomes(
        "tidy up the readme",
        &pattern().unwrap_or_else(|e| panic!("{e}")),
        Verdict::Comment,
        0,
        &transitions(),
        None,
        None,
    );
    assert!(
        reports.is_empty(),
        "a change that names no ticket is ordinary, not a failure"
    );
}

#[test]
fn andare_status_a_lowercase_or_malformed_key_is_not_a_key() {
    for text in ["revl-42", "REVL42", "REVL-", "-42", "R-42"] {
        assert!(
            pattern()
                .unwrap_or_else(|e| panic!("{e}"))
                .keys(text)
                .is_empty(),
            "`{text}` should not match the default pattern"
        );
    }
}

#[test]
fn andare_status_a_broken_pattern_says_what_the_default_is() {
    let error = KeyPattern::new("[unclosed").expect_err("that is not a regex");
    let message = error.to_string();
    assert!(message.contains("andare_key_regex"), "{message}");
    assert!(message.contains("try:"), "§18: {message}");
}

#[test]
fn andare_status_a_custom_pattern_is_honoured() {
    let custom = KeyPattern::new(r"#\d+").unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        custom.keys("closes #17 and #18"),
        vec!["#17".to_owned(), "#18".to_owned()]
    );
}

// --- criterion 2: a comment always, a transition only when configured -----

#[test]
fn andare_status_a_comment_is_always_planned() {
    for verdict in [Verdict::Approve, Verdict::Comment, Verdict::RequestChanges] {
        let reports = plan_outcomes(
            "REVL-42: something",
            &pattern().unwrap_or_else(|e| panic!("{e}")),
            verdict,
            2,
            &BTreeMap::new(),
            None,
            None,
        );
        assert_eq!(reports.len(), 1);
        assert!(
            !reports[0].comment.is_empty(),
            "§11.4: a comment always, whatever the verdict"
        );
        assert_eq!(
            reports[0].transition, None,
            "an empty andare_transition_on means do not move anything"
        );
    }
}

#[test]
fn andare_status_a_transition_happens_only_for_a_mapped_verdict() {
    let map = transitions();

    assert_eq!(
        transition_for(Verdict::RequestChanges, &map),
        Some("In Review")
    );
    assert_eq!(
        transition_for(Verdict::Approve, &map),
        None,
        "a verdict absent from the map moves nothing; a tool that moved tickets \
         unasked would be rearranging somebody's board"
    );
    assert_eq!(transition_for(Verdict::Comment, &map), None);

    let reports = plan_outcomes(
        "REVL-42: something",
        &pattern().unwrap_or_else(|e| panic!("{e}")),
        Verdict::RequestChanges,
        1,
        &map,
        None,
        None,
    );
    assert_eq!(reports[0].transition.as_deref(), Some("In Review"));
}

#[test]
fn andare_status_every_named_ticket_gets_its_own_report() {
    let reports = plan_outcomes(
        "REVL-42 and ENG-7 both",
        &pattern().unwrap_or_else(|e| panic!("{e}")),
        Verdict::RequestChanges,
        1,
        &transitions(),
        Some("https://github.com/acme/widgets/pull/7"),
        Some("https://trama.example.com/eng/review-12"),
    );

    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0].key, "REVL-42");
    assert_eq!(reports[1].key, "ENG-7");
    for report in &reports {
        assert!(report.comment.contains("pull/7"));
        assert!(report.comment.contains("review-12"));
        assert_eq!(report.transition.as_deref(), Some("In Review"));
    }
}

// --- criterion 4: a transition is high risk -------------------------------

#[test]
fn andare_status_a_transition_is_classified_high_risk() {
    assert_eq!(
        ActionIntent::SetStatus.baseline_risk(),
        RiskClass::High,
        "§12.3: moving somebody's ticket is a visible change to shared state that \
         other people's work queues are built on"
    );

    let reports = plan_outcomes(
        "REVL-42",
        &pattern().unwrap_or_else(|e| panic!("{e}")),
        Verdict::RequestChanges,
        1,
        &transitions(),
        None,
        None,
    );
    assert_eq!(reports[0].transition_risk(), Some(RiskClass::High));
}

#[test]
fn andare_status_a_report_with_no_transition_has_no_transition_risk() {
    let reports = plan_outcomes(
        "REVL-42",
        &pattern().unwrap_or_else(|e| panic!("{e}")),
        Verdict::Approve,
        0,
        &transitions(),
        None,
        None,
    );
    assert_eq!(
        reports[0].transition_risk(),
        None,
        "a comment is a comment; it should not inherit a transition's risk"
    );
}

// --- what the comment says -------------------------------------------------

#[test]
fn andare_status_the_comment_distinguishes_clean_from_non_blocking_from_blocking() {
    let clean = outcome_comment(Verdict::Approve, 0, None, None);
    assert!(clean.contains("found nothing"), "{clean}");

    let non_blocking = outcome_comment(Verdict::Comment, 3, None, None);
    assert!(non_blocking.contains("none blocking"), "{non_blocking}");

    let blocking = outcome_comment(Verdict::RequestChanges, 2, None, None);
    assert!(
        blocking.contains("including blocking"),
        "someone triaging their board needs these three to read differently: {blocking}"
    );
}

// --- criterion 3: an invalid transition is recorded, not fatal ------------

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::TimeZone;
use revlocal_core::{
    Capability, PublishAction, PublishActionId, PublishActionStatus, RunId, Timestamp,
};
use revlocal_publish::{
    AndareTarget, AndareWriter, IssueDraft, OutcomePayload, PublishError, PublishTarget,
};

fn at(minute: u32) -> Timestamp {
    chrono::Utc
        .with_ymd_and_hms(2026, 8, 28, 12, minute, 0)
        .single()
        .unwrap_or_default()
}

/// An Andare whose workflow refuses the transition.
struct StubbornWorkflow {
    comments: Arc<Mutex<Vec<(String, String)>>>,
    allow_transition: bool,
}

#[async_trait]
impl AndareWriter for StubbornWorkflow {
    fn can_search(&self) -> bool {
        true
    }

    async fn search(&self, _query: &str) -> Result<Option<String>, PublishError> {
        Ok(None)
    }

    async fn create_issue(&self, _draft: &IssueDraft) -> Result<String, PublishError> {
        Ok("REVL-1".to_owned())
    }

    async fn comment(&self, key: &str, body: &str) -> Result<(), PublishError> {
        let mut comments = self.comments.lock().map_err(|e| PublishError::Transport {
            target: "andare".to_owned(),
            detail: e.to_string(),
        })?;
        comments.push((key.to_owned(), body.to_owned()));
        Ok(())
    }

    async fn set_status(&self, key: &str, status: &str) -> Result<(), PublishError> {
        if self.allow_transition {
            return Ok(());
        }
        Err(PublishError::Rejected {
            target: "andare".to_owned(),
            status: Some(422),
            detail: format!("`{status}` is not reachable from {key}'s current state"),
        })
    }
}

fn outcome_action(payload: &OutcomePayload) -> PublishAction {
    PublishAction {
        id: PublishActionId::new(1),
        run_id: RunId::new(1),
        finding_id: None,
        target: "andare".to_owned(),
        capability: Capability::SetStatus,
        risk: revlocal_core::RiskClass::High,
        idempotency_key: format!("andare:status:{}", payload.key),
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
async fn andare_status_a_refused_transition_is_recorded_and_the_comment_still_landed() {
    let comments = Arc::new(Mutex::new(Vec::new()));
    let target = AndareTarget::new(StubbornWorkflow {
        comments: Arc::clone(&comments),
        allow_transition: false,
    });

    let payload = OutcomePayload {
        key: "REVL-42".to_owned(),
        comment: "rev-local reviewed this change.".to_owned(),
        transition: Some("In Review".to_owned()),
    };

    let error = target
        .execute(&outcome_action(&payload))
        .await
        .expect_err("a refused transition is a failure of the action");

    // Not a panic, and not a retry loop.
    assert!(
        !error.is_retryable(),
        "a workflow that does not allow the move will not allow it next time: {error}"
    );

    let message = error.to_string();
    assert!(
        message.contains("the comment was posted"),
        "an operator reading this must not have to open the ticket to find out what \
         actually happened: {message}"
    );
    assert!(message.contains("In Review"), "{message}");

    let posted = comments.lock().unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        posted.len(),
        1,
        "§11.4: the comment is unconditional, and it goes first so a refused \
         transition cannot take it with it"
    );
    assert_eq!(posted[0].0, "REVL-42");
}

#[tokio::test]
async fn andare_status_a_successful_transition_reports_the_ticket() {
    let comments = Arc::new(Mutex::new(Vec::new()));
    let target = AndareTarget::new(StubbornWorkflow {
        comments: Arc::clone(&comments),
        allow_transition: true,
    });

    let receipt = target
        .execute(&outcome_action(&OutcomePayload {
            key: "REVL-42".to_owned(),
            comment: "rev-local reviewed this change.".to_owned(),
            transition: Some("In Review".to_owned()),
        }))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(receipt.external_ref.as_deref(), Some("REVL-42"));
    assert_eq!(comments.lock().unwrap_or_else(|e| panic!("{e}")).len(), 1);
}

#[tokio::test]
async fn andare_status_an_outcome_with_no_transition_only_comments() {
    let comments = Arc::new(Mutex::new(Vec::new()));
    let target = AndareTarget::new(StubbornWorkflow {
        comments: Arc::clone(&comments),
        // Would refuse if asked — so a pass here proves it was not asked.
        allow_transition: false,
    });

    target
        .execute(&outcome_action(&OutcomePayload {
            key: "REVL-42".to_owned(),
            comment: "rev-local reviewed this change.".to_owned(),
            transition: None,
        }))
        .await
        .unwrap_or_else(|e| panic!("an unmapped verdict must not attempt a move: {e}"));

    assert_eq!(comments.lock().unwrap_or_else(|e| panic!("{e}")).len(), 1);
}

#[tokio::test]
async fn andare_status_a_malformed_outcome_payload_is_terminal() {
    let target = AndareTarget::new(StubbornWorkflow {
        comments: Arc::new(Mutex::new(Vec::new())),
        allow_transition: true,
    });

    let mut action = outcome_action(&OutcomePayload {
        key: "REVL-42".to_owned(),
        comment: "x".to_owned(),
        transition: None,
    });
    action.payload_json = "{\"not\":\"an outcome\"}".to_owned();

    let error = target
        .execute(&action)
        .await
        .expect_err("a malformed payload cannot be delivered");
    assert!(!error.is_retryable(), "{error}");
}
