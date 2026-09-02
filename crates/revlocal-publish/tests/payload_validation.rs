//! Payloads checked before somebody approves them (RL-1516, SPEC §12.4).
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use revlocal_core::Capability;
use revlocal_publish::validate_payload;

#[test]
fn the_payload_shape_an_older_version_wrote_is_caught_before_approval() {
    // Verbatim from the live install: three actions written by a build whose
    // Andare payload was the finding itself. `AndareTarget` wants
    // `{draft, context, recurrence_body}`, so all three would have failed
    // terminally the moment somebody pressed Approve.
    let legacy = r#"{"title":"Nested patch content","body":"...","rev-local-fingerprint":"a5ec"}"#;

    let refused = validate_payload("andare", Capability::CreateIssue, legacy)
        .expect_err("an unreadable payload must be refused");

    assert!(refused.contains("Andare issue"), "{refused}");
    // §18: a problem with no way out is a dead end, and there are exactly two.
    assert!(refused.contains("edit the payload"), "{refused}");
    assert!(refused.contains("reject"), "{refused}");
}

#[test]
fn a_payload_the_target_can_read_is_accepted() {
    let good = serde_json::json!({
        "draft": {
            "project": "ENG",
            "summary": "The query is built by concatenation",
            "description": "body",
            "fingerprint": "fp1",
        },
        "context": {},
        "recurrence_body": "rev-local saw this again.",
    })
    .to_string();

    assert!(validate_payload("andare", Capability::CreateIssue, &good).is_ok());
}

#[test]
fn one_target_s_payload_is_not_accepted_for_another() {
    // The shapes overlap enough to be confused by hand — both carry a title and
    // a body — and sending an Andare issue to GitHub would file the wrong thing
    // in the wrong place.
    let report = serde_json::json!({
        "repo": "acme",
        "title": "t",
        "body": "b",
        "fingerprint": "fp1",
    })
    .to_string();

    assert!(validate_payload("report", Capability::CreateIssue, &report).is_ok());
    assert!(validate_payload("andare", Capability::CreateIssue, &report).is_err());
}

#[test]
fn one_target_s_capabilities_are_told_apart() {
    // `andare` sends issues and outcome reports, and they are different shapes.
    // Dispatch picks by capability, so validation has to pick the same way or it
    // would condemn a payload that is perfectly sendable.
    let outcome = serde_json::json!({
        "key": "ENG-42",
        "comment": "rev-local reviewed this.",
        "transition": serde_json::Value::Null,
    })
    .to_string();

    assert!(validate_payload("andare", Capability::SetStatus, &outcome).is_ok());
    assert!(validate_payload("andare", Capability::Comment, &outcome).is_ok());
    assert!(validate_payload("andare", Capability::CreateIssue, &outcome).is_err());
}

#[test]
fn a_target_this_code_does_not_know_is_not_condemned() {
    // A payload it cannot check is not a payload it should refuse. Otherwise
    // adding a target becomes a breaking change to the approvals inbox.
    assert!(validate_payload("something-new", Capability::CreateIssue, "{}").is_ok());
}

#[test]
fn text_that_is_not_json_at_all_is_refused() {
    // The likeliest way a person breaks a payload: editing the body and leaving
    // a trailing comma.
    let broken = r#"{"draft": {"project": "ENG",}}"#;

    assert!(validate_payload("andare", Capability::CreateIssue, broken).is_err());
}
