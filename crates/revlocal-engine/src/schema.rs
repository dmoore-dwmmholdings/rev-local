//! Validating engine output against `result.v1.json` (SPEC §8.3).
//!
//! §8.3: *"Findings failing validation are dropped individually with an audit
//! event; the run still succeeds if at least the envelope parsed."*
//!
//! That sentence describes two different failure appetites in one document, and the
//! whole module exists to keep them apart:
//!
//! - **The envelope is all-or-nothing.** If it does not parse, there is no review —
//!   and §8.2 is explicit that the run fails with `engine_output_unparseable` rather
//!   than guessing. Inventing a verdict from a broken document would be the worst
//!   possible failure: a confident review of nothing.
//! - **A finding is individually droppable.** One malformed finding out of twelve
//!   should not throw away the other eleven, because an engine that got eleven right
//!   did useful work. But a drop is *recorded*, never silent (§18).
//!
//! # The schema is embedded, not read from disk
//!
//! `include_str!` rather than a runtime file read. A packaged desktop app has no
//! `crates/` directory next to the binary, so a runtime read would work in every
//! test and fail on every install — the worst possible place to find out.

use revlocal_core::{Usage, Verdict};

use crate::engine::{EngineOutcome, RawFinding};

/// The schema itself, compiled into the binary.
pub const RESULT_SCHEMA_V1: &str = include_str!("../schema/result.v1.json");

/// The only `schema_version` this build understands (SPEC §8.3).
pub const SUPPORTED_SCHEMA_VERSION: u64 = 1;

/// A finding that did not validate, and why.
///
/// Returned as data rather than written to the audit log here: this crate does not
/// depend on `revlocal-store`, and the layer that owns the run writes the event
/// (ADR 0013). It also makes the drops assertable without a database.
#[derive(Debug, Clone, PartialEq)]
pub struct DroppedFinding {
    /// Position in the original `findings` array, so the transcript can be read
    /// alongside this.
    pub index: usize,
    /// Every schema violation, in full.
    ///
    /// Not just the first: an engine emitting findings with two problems each would
    /// otherwise need as many round trips as it has mistakes.
    pub violations: Vec<String>,
    /// The finding as the engine wrote it.
    ///
    /// Kept so the audit event can show what was thrown away. A drop that cannot
    /// show its subject is unauditable.
    pub raw: serde_json::Value,
}

impl DroppedFinding {
    /// The audit log `kind` for this event (SPEC §5, §8.3).
    pub const AUDIT_KIND: &'static str = "finding_dropped_invalid";

    /// A one-line summary for a log.
    pub fn message(&self) -> String {
        format!(
            "finding {} failed validation and was dropped: {}",
            self.index,
            self.violations.join("; ")
        )
    }
}

/// What validation produced.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidatedResult {
    /// The outcome, carrying only findings that validated.
    pub outcome: EngineOutcome,
    /// Findings that were dropped. Empty on a clean document.
    pub dropped: Vec<DroppedFinding>,
}

impl ValidatedResult {
    /// Whether anything was thrown away.
    pub fn is_complete(&self) -> bool {
        self.dropped.is_empty()
    }
}

/// Why engine output could not be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SchemaError {
    /// The output is not JSON at all.
    #[error(
        "the engine's result.json is not valid JSON: {detail}\n  \
         try: this is usually the engine writing prose where JSON was asked for; \
         the transcript is kept, and §8.2's fallback ladder tries stdout next"
    )]
    NotJson {
        /// The parse error.
        detail: String,
    },

    /// The document parsed but is not a `result.json`.
    #[error(
        "the engine's result.json does not match the schema:\n  {}\n  \
         try: check the engine's prompt still asks for SPEC §8.3's shape; the \
         transcript is kept",
        violations.join("\n  ")
    )]
    InvalidEnvelope {
        /// Every violation, in full.
        violations: Vec<String>,
    },

    /// The document declares a `schema_version` this build does not know.
    #[error(
        "the engine produced result.json schema_version {found}, but this build of \
         rev-local understands version {supported}\n  \
         try: {remediation}"
    )]
    UnsupportedVersion {
        /// What the document declared.
        found: u64,
        /// What this build supports.
        supported: u64,
        /// What the user should do.
        remediation: String,
    },

    /// The embedded schema itself is broken. A build error, surfaced at runtime.
    #[error("rev-local's own result schema failed to compile: {detail}")]
    BadSchema {
        /// What is wrong with it.
        detail: String,
    },
}

/// Validate an engine's `result.json` (SPEC §8.3).
///
/// The envelope is validated without its findings' item constraints, then each
/// finding is validated on its own. That ordering is the point: validating the whole
/// document at once would make one malformed finding fail the envelope, throwing
/// away eleven good findings because of a twelfth.
pub fn validate(json: &str) -> Result<ValidatedResult, SchemaError> {
    let document: serde_json::Value =
        serde_json::from_str(json).map_err(|e| SchemaError::NotJson {
            detail: e.to_string(),
        })?;

    // Version first, and before schema validation, so a future document gets a
    // message about versions rather than a list of violations it cannot act on.
    check_version(&document)?;

    let schema: serde_json::Value =
        serde_json::from_str(RESULT_SCHEMA_V1).map_err(|e| SchemaError::BadSchema {
            detail: e.to_string(),
        })?;

    let envelope_validator =
        jsonschema::validator_for(&envelope_only(&schema)).map_err(|e| SchemaError::BadSchema {
            detail: e.to_string(),
        })?;

    let violations: Vec<String> = envelope_validator
        .iter_errors(&document)
        .map(|e| format!("{} at {}", e, e.instance_path))
        .collect();
    if !violations.is_empty() {
        return Err(SchemaError::InvalidEnvelope { violations });
    }

    let finding_validator = jsonschema::validator_for(&finding_schema(&schema)).map_err(|e| {
        SchemaError::BadSchema {
            detail: e.to_string(),
        }
    })?;

    let mut findings = Vec::new();
    let mut dropped = Vec::new();

    for (index, raw) in document
        .get("findings")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
    {
        let violations: Vec<String> = finding_validator
            .iter_errors(raw)
            .map(|e| format!("{} at {}", e, e.instance_path))
            .collect();

        if !violations.is_empty() {
            dropped.push(DroppedFinding {
                index,
                violations,
                raw: raw.clone(),
            });
            continue;
        }

        match serde_json::from_value::<RawFinding>(raw.clone()) {
            Ok(finding) => findings.push(finding),
            // Schema-valid but not deserializable means the schema and the Rust type
            // disagree — a bug in rev-local, not in the engine. Dropping it with the
            // reason is still better than failing the whole run over it.
            Err(e) => dropped.push(DroppedFinding {
                index,
                violations: vec![format!(
                    "schema-valid but rev-local could not read it ({e}); this is a \
                     rev-local bug, not an engine one"
                )],
                raw: raw.clone(),
            }),
        }
    }

    let outcome = EngineOutcome {
        findings,
        summary: string_field(&document, "summary"),
        verdict: verdict_of(&document),
        usage: Usage::default(),
        transcript: String::new(),
        degraded: None,
        coverage_notes: document
            .get("coverage_notes")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
    };

    Ok(ValidatedResult { outcome, dropped })
}

/// Check `schema_version` before anything else.
fn check_version(document: &serde_json::Value) -> Result<(), SchemaError> {
    let Some(found) = document
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
    else {
        // A missing version is an envelope problem, not a version problem — let the
        // schema report it in context rather than guessing at intent here.
        return Ok(());
    };

    if found == SUPPORTED_SCHEMA_VERSION {
        return Ok(());
    }

    let remediation = if found > SUPPORTED_SCHEMA_VERSION {
        "the engine is newer than this rev-local; upgrade rev-local, or pin the \
         engine's prompt template to the older schema"
            .to_owned()
    } else {
        "the engine is producing an older format; check the prompt template in \
         SPEC §9.2 is the one being sent"
            .to_owned()
    };

    Err(SchemaError::UnsupportedVersion {
        found,
        supported: SUPPORTED_SCHEMA_VERSION,
        remediation,
    })
}

/// The schema with `findings` reduced to "an array".
///
/// So the envelope can be judged on its own. Without this, a single bad finding
/// fails the whole document and eleven good ones go with it.
fn envelope_only(schema: &serde_json::Value) -> serde_json::Value {
    let mut relaxed = schema.clone();
    if let Some(findings) = relaxed
        .get_mut("properties")
        .and_then(|p| p.get_mut("findings"))
    {
        *findings = serde_json::json!({ "type": "array" });
    }
    relaxed
}

/// Just the finding subschema, resolvable on its own.
fn finding_schema(schema: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$defs": schema.get("$defs").cloned().unwrap_or(serde_json::Value::Null),
        "$ref": "#/$defs/finding"
    })
}

fn string_field(document: &serde_json::Value, key: &str) -> String {
    document
        .get(key)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_owned()
}

/// The verdict, defaulting to `comment`.
///
/// Only reachable once the envelope validated, which requires a `verdict` from the
/// enum — so the default is unreachable in practice and exists to avoid a panic
/// path rather than to paper over a missing field.
fn verdict_of(document: &serde_json::Value) -> Verdict {
    document
        .get("verdict")
        .and_then(|v| v.as_str())
        .and_then(|v| v.parse().ok())
        .unwrap_or(Verdict::Comment)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A document that should validate cleanly.
    fn valid() -> serde_json::Value {
        serde_json::json!({
            "schema_version": 1,
            "verdict": "request_changes",
            "summary": "Two defects.",
            "findings": [
                {
                    "severity": "high",
                    "category": "correctness",
                    "confidence": 0.9,
                    "file": "src/pager.rs",
                    "line_start": 6,
                    "line_end": 6,
                    "title": "Inclusive range walks one past the last index",
                    "body": "`start..=(start + per_page)` yields one index too many."
                },
                {
                    "severity": "critical",
                    "category": "security",
                    "file": "src/db.rs",
                    "title": "User input is interpolated into SQL",
                    "body": "`name` is formatted into the query."
                }
            ]
        })
    }

    fn validate_value(document: &serde_json::Value) -> Result<ValidatedResult, SchemaError> {
        validate(&document.to_string())
    }

    // --- the happy path -------------------------------------------------------

    #[test]
    fn schema_a_valid_document_passes() {
        let result = validate_value(&valid()).unwrap_or_else(|e| panic!("{e}"));

        assert!(result.is_complete(), "nothing should have been dropped");
        assert_eq!(result.outcome.findings.len(), 2);
        assert_eq!(result.outcome.verdict, Verdict::RequestChanges);
        assert_eq!(result.outcome.summary, "Two defects.");
        assert_eq!(
            result.outcome.findings[0].confidence,
            Some(0.9),
            "an optional field that WAS present must survive"
        );
        assert_eq!(
            result.outcome.findings[1].confidence, None,
            "and one that was absent must not be invented"
        );
    }

    #[test]
    fn schema_a_document_with_no_findings_is_valid() {
        // "Nothing wrong here" is a real review, and the commonest one on a healthy
        // repository. Treating an empty findings array as suspect would make every
        // clean review look like a failure.
        let mut document = valid();
        document["findings"] = serde_json::json!([]);
        document["verdict"] = serde_json::json!("approve");

        let result = validate_value(&document).unwrap_or_else(|e| panic!("{e}"));
        assert!(result.outcome.findings.is_empty());
        assert_eq!(result.outcome.verdict, Verdict::Approve);
    }

    #[test]
    fn schema_coverage_notes_survive_because_18_depends_on_them() {
        // §18: a review that saw 60% of the diff must never look like one that saw
        // all of it. `coverage_notes` is how the engine says so.
        let mut document = valid();
        document["coverage_notes"] = serde_json::json!("Could not read src/generated/.");

        let result = validate_value(&document).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            result.outcome.coverage_notes.as_deref(),
            Some("Could not read src/generated/.")
        );
    }

    // --- one bad finding does not sink the rest -------------------------------

    #[test]
    fn schema_a_single_malformed_finding_is_dropped_and_the_rest_survive() {
        // Acceptance criterion 2, and the whole reason the envelope and the findings
        // are validated separately. An engine that got eleven findings right did
        // useful work; throwing them away over a twelfth would waste a real review.
        let mut document = valid();
        document["findings"]
            .as_array_mut()
            .into_iter()
            .flatten()
            .count();
        document["findings"] = serde_json::json!([
            {
                "severity": "high", "category": "correctness",
                "title": "A good finding", "body": "with a body"
            },
            {
                "severity": "high", "category": "correctness",
                "title": "This one has no body"
            },
            {
                "severity": "low", "category": "tests",
                "title": "Another good one", "body": "also with a body"
            }
        ]);

        let result = validate_value(&document).unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(result.outcome.findings.len(), 2, "the good ones survive");
        assert_eq!(result.dropped.len(), 1);
        assert!(
            !result.is_complete(),
            "and the result knows it is incomplete"
        );

        let dropped = &result.dropped[0];
        assert_eq!(
            dropped.index, 1,
            "the position must be recoverable from the transcript"
        );
        assert!(
            dropped.violations.iter().any(|v| v.contains("body")),
            "the reason must name the field: {:?}",
            dropped.violations
        );
        assert_eq!(
            dropped.raw["title"], "This one has no body",
            "a drop that cannot show what it threw away is unauditable"
        );
    }

    #[test]
    fn schema_a_dropped_finding_carries_every_violation_not_just_the_first() {
        // An engine emitting findings with two problems each would otherwise need as
        // many round trips as it has mistakes.
        let mut document = valid();
        document["findings"] = serde_json::json!([{
            "severity": "catastrophic",
            "category": "vibes",
            "title": "x".repeat(200),
            "body": "b"
        }]);

        let result = validate_value(&document).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(result.dropped.len(), 1);
        assert!(
            result.dropped[0].violations.len() >= 3,
            "expected severity, category and title violations: {:?}",
            result.dropped[0].violations
        );
    }

    #[test]
    fn schema_a_drop_has_an_audit_kind_and_a_readable_message() {
        // §8.3 requires an audit event. The engine crate does not write it — it does
        // not depend on the store (ADR 0013) — so it hands back everything the row
        // needs.
        let mut document = valid();
        document["findings"] = serde_json::json!([{ "severity": "high" }]);

        let result = validate_value(&document).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(DroppedFinding::AUDIT_KIND, "finding_dropped_invalid");

        let message = result.dropped[0].message();
        assert!(message.contains("finding 0"), "{message}");
        assert!(message.contains("dropped"), "{message}");
    }

    #[test]
    fn schema_every_finding_being_bad_still_leaves_a_usable_run() {
        // §8.3: "the run still succeeds if at least the envelope parsed." A review
        // that found nothing valid is a real outcome — with a verdict and a summary
        // — and it is not the same as a run that failed.
        let mut document = valid();
        document["findings"] = serde_json::json!([{ "severity": "high" }, { "body": "x" }]);

        let result = validate_value(&document).unwrap_or_else(|e| panic!("{e}"));
        assert!(result.outcome.findings.is_empty());
        assert_eq!(result.dropped.len(), 2);
        assert_eq!(
            result.outcome.summary, "Two defects.",
            "the envelope survives intact"
        );
    }

    // --- the envelope is all or nothing ---------------------------------------

    #[test]
    fn schema_a_malformed_envelope_fails_rather_than_inventing_findings() {
        // Acceptance criterion 3. §8.2 fails the run with
        // `engine_output_unparseable` rather than guessing — a confident review of
        // nothing is the worst possible failure.
        let mut document = valid();
        document.as_object_mut().into_iter().for_each(|o| {
            o.remove("verdict");
        });

        let error = validate_value(&document).expect_err("a missing verdict must fail");
        assert!(
            matches!(error, SchemaError::InvalidEnvelope { .. }),
            "{error:?}"
        );
        assert!(
            error.to_string().contains("try:"),
            "errors carry remediation: {error}"
        );
    }

    #[test]
    fn schema_an_unknown_verdict_fails_the_envelope() {
        let mut document = valid();
        document["verdict"] = serde_json::json!("lgtm");
        assert!(matches!(
            validate_value(&document),
            Err(SchemaError::InvalidEnvelope { .. })
        ));
    }

    #[test]
    fn schema_output_that_is_not_json_says_what_the_fallback_ladder_will_do() {
        let error = validate("I reviewed the code and it looks fine to me!")
            .expect_err("prose is not a result document");
        assert!(matches!(error, SchemaError::NotJson { .. }), "{error:?}");
        assert!(
            error.to_string().contains("fallback"),
            "the message should say the ladder tries stdout next: {error}"
        );
    }

    #[test]
    fn schema_a_summary_over_the_cap_fails_the_envelope() {
        // §8.3 caps it at 1200. A summary is posted verbatim to a PR, and a
        // multi-megabyte one would be a review nobody can read.
        let mut document = valid();
        document["summary"] = serde_json::json!("x".repeat(1201));
        assert!(matches!(
            validate_value(&document),
            Err(SchemaError::InvalidEnvelope { .. })
        ));
    }

    // --- version ---------------------------------------------------------------

    #[test]
    fn schema_a_newer_schema_version_is_a_typed_error_that_says_what_to_do() {
        // Acceptance criterion 4. Reported before schema validation, so a future
        // document gets a message about versions rather than a list of violations
        // nobody can act on.
        let mut document = valid();
        document["schema_version"] = serde_json::json!(2);

        let error = validate_value(&document).expect_err("version 2 is not understood");
        match &error {
            SchemaError::UnsupportedVersion {
                found, supported, ..
            } => {
                assert_eq!(*found, 2);
                assert_eq!(*supported, SUPPORTED_SCHEMA_VERSION);
            }
            other => panic!("expected a version error, got {other:?}"),
        }
        assert!(
            error.to_string().contains("upgrade rev-local"),
            "a newer engine means upgrade: {error}"
        );
    }

    #[test]
    fn schema_an_older_schema_version_points_at_the_prompt_instead() {
        // Different cause, different remedy: an old version means the prompt is
        // asking for the wrong shape, not that rev-local is behind.
        let mut document = valid();
        document["schema_version"] = serde_json::json!(0);

        let error = validate_value(&document).expect_err("version 0 is not understood");
        assert!(error.to_string().contains("prompt template"), "{error}");
        assert!(!error.to_string().contains("upgrade rev-local"), "{error}");
    }

    #[test]
    fn schema_a_missing_version_is_an_envelope_problem_not_a_version_one() {
        // Guessing at intent would produce a version error for a document that never
        // claimed a version. The schema reports it in context instead.
        let mut document = valid();
        document.as_object_mut().into_iter().for_each(|o| {
            o.remove("schema_version");
        });
        assert!(matches!(
            validate_value(&document),
            Err(SchemaError::InvalidEnvelope { .. })
        ));
    }

    // --- the fixture engine's own output --------------------------------------

    #[test]
    fn schema_the_fixture_engines_output_validates_for_real() {
        // The fixture and this validator are the two halves of one contract, written
        // in different languages in different files. If they drift, every M5 test
        // using the fixture is testing a document rev-local would reject in
        // production — and the drift would surface as a mysterious M5 failure rather
        // than here.
        //
        // So this runs the fixture and validates what it actually wrote, rather than
        // grepping its source for shapes that look right.
        let out = tempfile::TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));

        let status = std::process::Command::new(crate::mock_engine_program())
            .env("MOCK_ENGINE_MODE", "valid")
            .env("REVLOCAL_OUT", out.path())
            .output()
            .unwrap_or_else(|e| panic!("running the fixture engine: {e}"));
        assert!(
            status.status.success(),
            "{}",
            String::from_utf8_lossy(&status.stderr)
        );

        let written = std::fs::read_to_string(out.path().join("result.json"))
            .unwrap_or_else(|e| panic!("reading result.json: {e}"));

        let result = validate(&written)
            .unwrap_or_else(|e| panic!("the fixture engine's own output was rejected: {e}"));

        assert!(
            result.is_complete(),
            "the fixture's happy path must drop nothing: {:?}",
            result.dropped
        );
        assert_eq!(result.outcome.findings.len(), 2);
        assert_eq!(result.outcome.verdict, Verdict::RequestChanges);
    }

    #[test]
    fn schema_the_fixtures_partial_findings_mode_exercises_the_drop_path() {
        // The fixture has a mode built specifically for §8.3's "drop individually,
        // keep the run" behaviour. If that mode stopped producing invalid findings,
        // the drop path would silently stop being tested anywhere.
        let out = tempfile::TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));

        let status = std::process::Command::new(crate::mock_engine_program())
            .env("MOCK_ENGINE_MODE", "partial_findings")
            .env("REVLOCAL_OUT", out.path())
            .output()
            .unwrap_or_else(|e| panic!("running the fixture engine: {e}"));
        assert!(
            status.status.success(),
            "{}",
            String::from_utf8_lossy(&status.stderr)
        );

        let written = std::fs::read_to_string(out.path().join("result.json"))
            .unwrap_or_else(|e| panic!("reading result.json: {e}"));

        let result =
            validate(&written).unwrap_or_else(|e| panic!("the envelope must still parse: {e}"));

        assert!(
            !result.dropped.is_empty(),
            "partial_findings must produce findings that fail validation, or the \
             drop path is untested"
        );
        assert!(
            !result.outcome.findings.is_empty(),
            "and it must ALSO produce valid ones, or 'the rest survive' is untested"
        );
        assert_eq!(
            result.outcome.verdict,
            Verdict::RequestChanges,
            "§8.3: the run still succeeds if the envelope parsed"
        );
    }
}
