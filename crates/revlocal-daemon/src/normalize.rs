//! Finding normalization, suppression and dedupe (SPEC §9.5, §10).
//!
//! Everything an engine reported passes through here on its way to a publish plan.
//! Four things happen, in §9.5's order: severity is clamped, out-of-diff findings are
//! capped, suppressions are applied, and fingerprints decide what is a repeat.
//!
//! # Nothing is discarded, only labelled
//!
//! §9.5's original wording said to *drop* out-of-diff findings and *drop* suppressed
//! ones. This module retains both and marks them instead — [`FindingState`] already
//! has `Suppressed` for exactly this — and the publish plan filters on the label.
//!
//! The difference matters because a dropped finding is unanswerable. A user who asks
//! "why didn't it mention the thing in `src/other.rs`?" gets an answer from a
//! suppressed row and silence from a discarded one, and §18 exists to prevent
//! precisely that silence. It also costs nothing: these are rows in a local database,
//! not tokens in a prompt.
//!
//! See ADR 0021, which amended §9.5 to say so.

use revlocal_core::{
    fingerprint, Category, Depth, Finding, FindingId, FindingState, RunId, Severity, Suppression,
    Timestamp,
};
use revlocal_engine::{DroppedFinding, RawFinding};

/// The severity an unparseable one becomes (SPEC §9.5).
///
/// `medium` rather than `low`, because an engine that could not spell its severity
/// gives no evidence the finding is unimportant — only that its output was sloppy.
/// Rounding down would let a formatting bug quietly demote a real defect.
pub const UNKNOWN_SEVERITY: Severity = Severity::Medium;

/// The ceiling an out-of-diff finding is capped at (SPEC §9.5).
pub const OUT_OF_DIFF_CEILING: Severity = Severity::Medium;

/// Whether a depth permits out-of-diff findings at full severity (SPEC §9.5).
///
/// `true` for `deep` only. A deep review is explicitly asked to look beyond the
/// diff — refuting each finding means reading the code around it — so its
/// out-of-diff observations are the ones it was sent to find.
pub const fn allow_out_of_diff_findings(depth: Depth) -> bool {
    matches!(depth, Depth::Deep)
}

/// Why a finding is not going to be published as reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalizationNote {
    /// The engine's severity did not parse and became [`UNKNOWN_SEVERITY`].
    SeveritySalvaged {
        /// What the engine wrote.
        reported: String,
    },
    /// The finding names a file the change does not touch.
    OutOfDiff {
        /// The file as reported.
        file: String,
        /// Whether the severity was capped as a result.
        capped: bool,
    },
    /// An active suppression matched.
    Suppressed {
        /// What matched: a fingerprint or a glob.
        matched: String,
        /// The reason the user gave, if any.
        reason: Option<String>,
    },
    /// The same fingerprint is already published on this change.
    Superseded {
        /// The fingerprint that was already filed.
        fingerprint: String,
    },
}

impl NormalizationNote {
    /// A line for the run record and the UI.
    pub fn describe(&self) -> String {
        match self {
            Self::SeveritySalvaged { reported } => {
                format!("severity {reported:?} is not a known level; treated as medium")
            }
            Self::OutOfDiff { file, capped: true } => {
                format!("`{file}` is not in this change; severity capped at medium")
            }
            Self::OutOfDiff {
                file,
                capped: false,
            } => format!("`{file}` is not in this change"),
            Self::Suppressed { matched, reason } => match reason {
                Some(reason) => format!("suppressed by {matched} ({reason})"),
                None => format!("suppressed by {matched}"),
            },
            Self::Superseded { fingerprint } => {
                format!("already published on this change as {fingerprint}")
            }
        }
    }
}

/// One finding, normalized, with everything that happened to it.
#[derive(Debug, Clone, PartialEq)]
pub struct NormalizedFinding {
    /// The finding as it will be stored.
    pub finding: Finding,
    /// Whether it names a file outside the change.
    ///
    /// Carried alongside rather than stored: §9.5 asks the *publish* layer to render
    /// these inline as `info` because GitHub cannot anchor a comment to a line it
    /// cannot see, and that layer needs to know. If a later item needs this to
    /// survive a restart it becomes a column; nothing needs it yet.
    pub out_of_diff: bool,
    /// What normalization did, in order.
    pub notes: Vec<NormalizationNote>,
}

impl NormalizedFinding {
    /// Whether this finding belongs in the publish plan (SPEC §9.5).
    ///
    /// The single place the question is answered, so "suppressed things are not
    /// published" is one rule rather than a condition repeated at every target.
    pub const fn is_publishable(&self) -> bool {
        matches!(self.finding.state, FindingState::Open)
    }
}

/// Everything normalization produced.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Normalized {
    /// Every finding, publishable or not, in the order the engine reported them.
    pub findings: Vec<NormalizedFinding>,
    /// Findings that stayed dropped: their schema violations were not just severity.
    pub still_dropped: Vec<DroppedFinding>,
}

impl Normalized {
    /// The findings that reach the publish plan.
    pub fn publishable(&self) -> impl Iterator<Item = &NormalizedFinding> {
        self.findings.iter().filter(|f| f.is_publishable())
    }

    /// Findings held back, with the reason for each.
    pub fn withheld(&self) -> impl Iterator<Item = (&NormalizedFinding, String)> {
        self.findings
            .iter()
            .filter(|f| !f.is_publishable())
            .map(|f| {
                let reason = f
                    .notes
                    .iter()
                    .map(NormalizationNote::describe)
                    .collect::<Vec<_>>()
                    .join("; ");
                (f, reason)
            })
    }
}

/// What normalization needs to know about the change being reviewed.
#[derive(Debug, Clone)]
pub struct NormalizeContext<'a> {
    /// The run these findings came from.
    pub run_id: RunId,
    /// The repo name, which is part of the fingerprint (§10.3).
    pub repo_name: &'a str,
    /// Paths the change touches. A finding outside this set is out-of-diff.
    pub changed_paths: &'a [String],
    /// The depth this run was reviewed at.
    pub depth: Depth,
    /// Active suppressions for this repo.
    pub suppressions: &'a [Suppression],
    /// Fingerprints already `published` on this change (SPEC §9.5).
    pub published_fingerprints: &'a [String],
    /// When the run finished.
    pub now: Timestamp,
}

/// Recover findings the schema dropped only because of their severity (SPEC §9.5).
///
/// §8.3 is a decision of record and is not relaxed: the schema still rejects these,
/// the drop is still recorded, and a finding with *any other* violation stays dropped.
/// This reads the recorded drops and re-admits the ones whose sole defect was a word.
///
/// Returns the salvaged findings paired with what the engine actually wrote, and the
/// drops that stand.
fn salvage_severities(
    dropped: &[DroppedFinding],
) -> (Vec<(RawFinding, String)>, Vec<DroppedFinding>) {
    let mut salvaged = Vec::new();
    let mut still_dropped = Vec::new();

    for drop in dropped {
        let only_severity = drop.violations.len() == 1
            && drop.violations[0].contains("severity")
            && drop.raw.get("severity").is_some_and(|s| s.is_string());

        if !only_severity {
            still_dropped.push(drop.clone());
            continue;
        }

        let reported = drop
            .raw
            .get("severity")
            .and_then(|s| s.as_str())
            .unwrap_or_default()
            .to_owned();

        let mut repaired = drop.raw.clone();
        if let Some(object) = repaired.as_object_mut() {
            object.insert(
                "severity".to_owned(),
                serde_json::Value::String(UNKNOWN_SEVERITY.as_str().to_owned()),
            );
        }

        match serde_json::from_value::<RawFinding>(repaired) {
            Ok(finding) => salvaged.push((finding, reported)),
            // The severity was the only *schema* violation but the document still
            // will not deserialize — a type mismatch the schema did not describe.
            // It stays dropped rather than being guessed at.
            Err(_) => still_dropped.push(drop.clone()),
        }
    }

    (salvaged, still_dropped)
}

/// Whether a suppression matches a finding.
fn suppression_match(
    suppression: &Suppression,
    finding_fingerprint: &str,
    file: Option<&str>,
) -> Option<String> {
    if !suppression.is_actionable() {
        return None;
    }

    if suppression
        .fingerprint
        .as_ref()
        .is_some_and(|f| f == finding_fingerprint)
    {
        return Some(format!("fingerprint {finding_fingerprint}"));
    }

    let glob = suppression.glob.as_ref()?;
    let path = file?;
    let compiled = globset::Glob::new(glob).ok()?.compile_matcher();

    compiled.is_match(path).then(|| format!("glob {glob}"))
}

/// Normalize one run's findings (SPEC §9.5).
pub fn normalize(
    findings: &[RawFinding],
    dropped: &[DroppedFinding],
    context: &NormalizeContext<'_>,
) -> Normalized {
    let (salvaged, still_dropped) = salvage_severities(dropped);

    let all: Vec<(RawFinding, Option<String>)> = findings
        .iter()
        .map(|f| (f.clone(), None))
        .chain(salvaged.into_iter().map(|(f, r)| (f, Some(r))))
        .collect();

    // Fingerprints published *earlier in this same run* also supersede, so an engine
    // reporting the same defect twice files it once.
    let mut seen_this_run: Vec<String> = Vec::new();
    let mut out = Vec::with_capacity(all.len());

    for (raw, salvaged_from) in all {
        let mut notes = Vec::new();
        let mut severity = raw.severity;

        if let Some(reported) = salvaged_from {
            notes.push(NormalizationNote::SeveritySalvaged { reported });
        }

        // --- out of diff ---
        let out_of_diff = raw
            .file
            .as_ref()
            .is_some_and(|f| !context.changed_paths.iter().any(|p| p == f));

        if out_of_diff {
            let capped =
                !allow_out_of_diff_findings(context.depth) && severity > OUT_OF_DIFF_CEILING;
            if capped {
                severity = OUT_OF_DIFF_CEILING;
            }
            notes.push(NormalizationNote::OutOfDiff {
                file: raw.file.clone().unwrap_or_default(),
                capped,
            });
        }

        // --- fingerprint (§10.3) ---
        let print = fingerprint(
            context.repo_name,
            raw.file.as_deref(),
            raw.category,
            &raw.title,
        );

        // --- state ---
        let suppression = context
            .suppressions
            .iter()
            .find_map(|s| suppression_match(s, &print, raw.file.as_deref()).map(|m| (s, m)));

        let already_published =
            context.published_fingerprints.contains(&print) || seen_this_run.contains(&print);

        let state = if let Some((suppression, matched)) = suppression {
            notes.push(NormalizationNote::Suppressed {
                matched,
                reason: suppression.reason.clone(),
            });
            FindingState::Suppressed
        } else if already_published {
            notes.push(NormalizationNote::Superseded {
                fingerprint: print.clone(),
            });
            FindingState::Superseded
        } else {
            seen_this_run.push(print.clone());
            FindingState::Open
        };

        out.push(NormalizedFinding {
            finding: Finding {
                // Assigned by the store on insert; normalization does not know it.
                id: FindingId::new(0),
                run_id: context.run_id,
                fingerprint: print,
                severity,
                category: raw.category,
                confidence: raw.confidence.unwrap_or(1.0),
                file: raw.file.clone(),
                line_start: raw.line_start,
                line_end: raw.line_end,
                title: raw.title.clone(),
                body: raw.body.clone(),
                failure_scenario: raw.failure_scenario.clone(),
                suggested_fix: raw.suggested_fix.clone(),
                state,
                created_at: context.now,
            },
            out_of_diff,
            notes,
        });
    }

    Normalized {
        findings: out,
        still_dropped,
    }
}

/// The category an out-of-diff finding is published inline as (SPEC §9.5).
///
/// GitHub cannot anchor a comment to a line that is not in the diff, so the publish
/// layer renders these in the summary body instead. Exposed here so the rule lives
/// with the rest of §9.5 rather than being rediscovered per target.
pub const fn inline_category_for_out_of_diff() -> Category {
    Category::Other
}

#[cfg(test)]
mod tests {
    use super::*;
    use revlocal_core::SuppressionId;

    fn raw(severity: Severity, file: Option<&str>, title: &str) -> RawFinding {
        RawFinding {
            severity,
            category: Category::Correctness,
            confidence: Some(0.9),
            file: file.map(str::to_owned),
            line_start: Some(3),
            line_end: Some(4),
            title: title.to_owned(),
            body: "why".to_owned(),
            failure_scenario: Some("inputs".to_owned()),
            suggested_fix: None,
        }
    }

    fn context<'a>(
        changed: &'a [String],
        depth: Depth,
        suppressions: &'a [Suppression],
        published: &'a [String],
    ) -> NormalizeContext<'a> {
        NormalizeContext {
            run_id: RunId::new(1),
            repo_name: "rev-local",
            changed_paths: changed,
            depth,
            suppressions,
            published_fingerprints: published,
            now: Timestamp::default(),
        }
    }

    fn suppression(fingerprint: Option<&str>, glob: Option<&str>) -> Suppression {
        Suppression {
            id: SuppressionId::new(1),
            repo_id: None,
            fingerprint: fingerprint.map(str::to_owned),
            glob: glob.map(str::to_owned),
            reason: Some("known, accepted".to_owned()),
            created_at: Timestamp::default(),
        }
    }

    fn dropped(index: usize, violations: &[&str], raw: serde_json::Value) -> DroppedFinding {
        DroppedFinding {
            index,
            violations: violations.iter().map(|v| (*v).to_owned()).collect(),
            raw,
        }
    }

    // --- criterion 1: out-of-diff is retained but capped ---

    /// §9.5 as amended (ADR 0021). The original wording said to *drop* these, which
    /// contradicted §18 and the section's own next clause.
    #[test]
    fn normalize_an_out_of_diff_finding_is_retained_and_capped() {
        let changed = ["src/a.rs".to_owned()];
        let result = normalize(
            &[raw(Severity::Critical, Some("src/elsewhere.rs"), "boom")],
            &[],
            &context(&changed, Depth::Standard, &[], &[]),
        );

        let finding = &result.findings[0];
        assert_eq!(finding.finding.severity, Severity::Medium);
        assert!(finding.out_of_diff);
        assert!(finding.is_publishable(), "retained, not dropped");
        assert_eq!(result.findings.len(), 1);
        assert!(finding.notes.contains(&NormalizationNote::OutOfDiff {
            file: "src/elsewhere.rs".to_owned(),
            capped: true,
        }));
    }

    /// A deep review is sent to look beyond the diff — refuting a finding means
    /// reading the code around it — so its out-of-diff observations keep their weight.
    #[test]
    fn normalize_a_deep_run_keeps_out_of_diff_severity() {
        let changed = ["src/a.rs".to_owned()];
        let result = normalize(
            &[raw(Severity::Critical, Some("src/elsewhere.rs"), "boom")],
            &[],
            &context(&changed, Depth::Deep, &[], &[]),
        );

        assert_eq!(result.findings[0].finding.severity, Severity::Critical);
        assert!(result.findings[0].out_of_diff);
        assert!(result.findings[0]
            .notes
            .contains(&NormalizationNote::OutOfDiff {
                file: "src/elsewhere.rs".to_owned(),
                capped: false,
            }));
    }

    #[test]
    fn normalize_an_out_of_diff_finding_below_the_ceiling_is_not_raised() {
        let changed = ["src/a.rs".to_owned()];
        let result = normalize(
            &[raw(Severity::Low, Some("src/elsewhere.rs"), "small")],
            &[],
            &context(&changed, Depth::Standard, &[], &[]),
        );

        assert_eq!(result.findings[0].finding.severity, Severity::Low);
    }

    #[test]
    fn normalize_an_in_diff_finding_keeps_its_severity() {
        let changed = ["src/a.rs".to_owned()];
        let result = normalize(
            &[raw(Severity::Critical, Some("src/a.rs"), "boom")],
            &[],
            &context(&changed, Depth::Standard, &[], &[]),
        );

        assert_eq!(result.findings[0].finding.severity, Severity::Critical);
        assert!(!result.findings[0].out_of_diff);
        assert!(result.findings[0].notes.is_empty());
    }

    /// A finding with no file is repo-wide, not out-of-diff. Treating it as
    /// out-of-diff would cap every architectural observation at medium.
    #[test]
    fn normalize_a_fileless_finding_is_not_out_of_diff() {
        let changed = ["src/a.rs".to_owned()];
        let result = normalize(
            &[raw(Severity::High, None, "no tests anywhere")],
            &[],
            &context(&changed, Depth::Standard, &[], &[]),
        );

        assert!(!result.findings[0].out_of_diff);
        assert_eq!(result.findings[0].finding.severity, Severity::High);
    }

    #[test]
    fn normalize_out_of_diff_publishes_inline_as_a_non_anchored_category() {
        // GitHub cannot anchor a comment to a line it cannot see; the rule lives
        // here so each target does not rediscover it.
        assert_eq!(inline_category_for_out_of_diff(), Category::Other);
        assert!(!allow_out_of_diff_findings(Depth::Standard));
        assert!(!allow_out_of_diff_findings(Depth::Summary));
        assert!(allow_out_of_diff_findings(Depth::Deep));
    }

    // --- criterion 4: unknown severity maps to medium, not dropped ---

    #[test]
    fn normalize_an_unknown_severity_is_salvaged_as_medium() {
        let result = normalize(
            &[],
            &[dropped(
                0,
                &["/severity: \"warning\" is not one of the allowed values"],
                serde_json::json!({
                    "severity": "warning",
                    "category": "correctness",
                    "title": "off by one",
                    "body": "why",
                }),
            )],
            &context(&[], Depth::Standard, &[], &[]),
        );

        assert_eq!(result.findings.len(), 1, "salvaged, not dropped");
        assert!(result.still_dropped.is_empty());
        assert_eq!(result.findings[0].finding.severity, Severity::Medium);
        assert_eq!(result.findings[0].finding.title, "off by one");
        assert!(result.findings[0].is_publishable());
        assert!(result.findings[0]
            .notes
            .contains(&NormalizationNote::SeveritySalvaged {
                reported: "warning".to_owned()
            }));
    }

    /// Medium, not low. An engine that could not spell its severity gives no evidence
    /// the finding is unimportant — only that its output was sloppy. Rounding down
    /// would let a formatting bug quietly demote a real defect.
    #[test]
    fn normalize_the_unknown_severity_default_is_medium() {
        assert_eq!(UNKNOWN_SEVERITY, Severity::Medium);
        assert!(UNKNOWN_SEVERITY > Severity::Low);
    }

    /// §8.3 is a decision of record and is not relaxed here. A finding with any
    /// violation beyond its severity stays dropped rather than being guessed at.
    #[test]
    fn normalize_a_finding_with_other_violations_stays_dropped() {
        let result = normalize(
            &[],
            &[dropped(
                0,
                &[
                    "/severity: \"warning\" is not one of the allowed values",
                    "/title: is a required property",
                ],
                serde_json::json!({"severity": "warning", "category": "correctness", "body": "b"}),
            )],
            &context(&[], Depth::Standard, &[], &[]),
        );

        assert!(result.findings.is_empty());
        assert_eq!(result.still_dropped.len(), 1);
    }

    #[test]
    fn normalize_a_non_severity_violation_stays_dropped() {
        let result = normalize(
            &[],
            &[dropped(
                0,
                &["/title: is a required property"],
                serde_json::json!({"severity": "high", "category": "correctness", "body": "b"}),
            )],
            &context(&[], Depth::Standard, &[], &[]),
        );

        assert!(result.findings.is_empty());
        assert_eq!(result.still_dropped.len(), 1);
    }

    /// The violation named severity but the document still will not deserialize —
    /// a type mismatch the schema did not describe. It stays dropped rather than
    /// being guessed at.
    #[test]
    fn normalize_an_unsalvageable_document_stays_dropped() {
        let result = normalize(
            &[],
            &[dropped(
                0,
                &["/severity: 7 is not of type \"string\""],
                serde_json::json!({"severity": 7, "category": "correctness", "title": "t"}),
            )],
            &context(&[], Depth::Standard, &[], &[]),
        );

        assert!(result.findings.is_empty());
        assert_eq!(result.still_dropped.len(), 1);
    }

    /// **Cross-check, and the reason criterion 4 is not decorative.**
    ///
    /// The salvage recognises a severity-only failure by looking for "severity" in
    /// the violation text the *engine crate* produced. That is an assumption about
    /// another crate's wording, and if it were wrong the salvage would never fire in
    /// production while every hand-written test above still passed — a silent cap of
    /// exactly the kind §18 forbids, hidden behind a green suite.
    ///
    /// So this runs the real validator over a real document and feeds its real
    /// output in.
    #[test]
    fn normalize_salvages_what_the_real_validator_actually_drops() {
        let document = serde_json::json!({
            "schema_version": 1,
            "verdict": "comment",
            "summary": "s",
            "findings": [{
                "severity": "warning",
                "category": "correctness",
                "title": "off by one",
                "body": "why",
            }],
        })
        .to_string();

        let validated = revlocal_engine::validate(&document).expect("envelope parses");
        assert_eq!(validated.outcome.findings.len(), 0, "the schema rejects it");
        assert_eq!(validated.dropped.len(), 1);

        let result = normalize(
            &validated.outcome.findings,
            &validated.dropped,
            &context(&[], Depth::Standard, &[], &[]),
        );

        assert_eq!(
            result.findings.len(),
            1,
            "salvage did not fire on the real violation text: {:?}",
            validated.dropped[0].violations
        );
        assert_eq!(result.findings[0].finding.severity, Severity::Medium);
        assert_eq!(result.findings[0].finding.title, "off by one");
    }

    /// The other half of the cross-check: a document the validator accepts must pass
    /// through untouched, with nothing salvaged and nothing dropped.
    #[test]
    fn normalize_leaves_a_valid_document_alone() {
        let document = serde_json::json!({
            "schema_version": 1,
            "verdict": "comment",
            "summary": "s",
            "findings": [{
                "severity": "high",
                "category": "correctness",
                "title": "real",
                "body": "why",
                "file": "src/a.rs",
            }],
        })
        .to_string();

        let validated = revlocal_engine::validate(&document).expect("parses");
        assert!(validated.is_complete());

        let changed = ["src/a.rs".to_owned()];
        let result = normalize(
            &validated.outcome.findings,
            &validated.dropped,
            &context(&changed, Depth::Standard, &[], &[]),
        );

        assert_eq!(result.publishable().count(), 1);
        assert_eq!(result.findings[0].finding.severity, Severity::High);
        assert!(result.findings[0].notes.is_empty());
    }

    // --- criterion 2: a suppressed fingerprint never reaches the publish plan ---

    #[test]
    fn normalize_a_suppressed_fingerprint_never_reaches_the_publish_plan() {
        let changed = ["src/a.rs".to_owned()];
        let print = fingerprint("rev-local", Some("src/a.rs"), Category::Correctness, "boom");
        let suppressions = [suppression(Some(&print), None)];

        let result = normalize(
            &[raw(Severity::Critical, Some("src/a.rs"), "boom")],
            &[],
            &context(&changed, Depth::Standard, &suppressions, &[]),
        );

        assert_eq!(result.publishable().count(), 0);
        // Retained and labelled, not discarded: a user asking why it was not
        // mentioned gets an answer.
        assert_eq!(result.findings.len(), 1);
        assert_eq!(result.findings[0].finding.state, FindingState::Suppressed);

        let (_, reason) = result.withheld().next().expect("withheld with a reason");
        assert!(reason.contains("known, accepted"), "{reason}");
    }

    #[test]
    fn normalize_a_glob_suppression_matches_by_path() {
        let changed = ["vendor/gen/api.rs".to_owned()];
        let suppressions = [suppression(None, Some("vendor/**"))];

        let result = normalize(
            &[raw(Severity::High, Some("vendor/gen/api.rs"), "style")],
            &[],
            &context(&changed, Depth::Standard, &suppressions, &[]),
        );

        assert_eq!(result.publishable().count(), 0);
        assert_eq!(result.findings[0].finding.state, FindingState::Suppressed);
    }

    #[test]
    fn normalize_a_glob_suppression_does_not_match_other_paths() {
        let changed = ["src/a.rs".to_owned()];
        let suppressions = [suppression(None, Some("vendor/**"))];

        let result = normalize(
            &[raw(Severity::High, Some("src/a.rs"), "real")],
            &[],
            &context(&changed, Depth::Standard, &suppressions, &[]),
        );

        assert_eq!(result.publishable().count(), 1);
    }

    /// A suppression with neither a fingerprint nor a glob matches nothing. Matching
    /// everything would silence the whole repo from one malformed row.
    #[test]
    fn normalize_an_inert_suppression_matches_nothing() {
        let changed = ["src/a.rs".to_owned()];
        let suppressions = [suppression(None, None)];

        let result = normalize(
            &[raw(Severity::High, Some("src/a.rs"), "real")],
            &[],
            &context(&changed, Depth::Standard, &suppressions, &[]),
        );

        assert_eq!(result.publishable().count(), 1);
    }

    /// An uncompilable glob suppresses nothing rather than everything. The safe
    /// direction here is the opposite of `sensitive_globs`: a broken suppression must
    /// not silence findings the user never asked to silence.
    #[test]
    fn normalize_an_invalid_suppression_glob_suppresses_nothing() {
        let changed = ["src/a.rs".to_owned()];
        let suppressions = [suppression(None, Some("[unclosed"))];

        let result = normalize(
            &[raw(Severity::High, Some("src/a.rs"), "real")],
            &[],
            &context(&changed, Depth::Standard, &suppressions, &[]),
        );

        assert_eq!(result.publishable().count(), 1);
    }

    // --- criterion 3: a repeat fingerprint is superseded, not duplicated ---

    #[test]
    fn normalize_a_repeat_of_a_published_fingerprint_is_superseded() {
        let changed = ["src/a.rs".to_owned()];
        let print = fingerprint("rev-local", Some("src/a.rs"), Category::Correctness, "boom");

        let result = normalize(
            &[raw(Severity::High, Some("src/a.rs"), "boom")],
            &[],
            &context(&changed, Depth::Standard, &[], std::slice::from_ref(&print)),
        );

        assert_eq!(result.findings[0].finding.state, FindingState::Superseded);
        assert_eq!(result.publishable().count(), 0, "not re-filed");
        assert!(result.findings[0]
            .notes
            .contains(&NormalizationNote::Superseded { fingerprint: print }));
    }

    /// §10.3's fingerprint is line-number independent, so the same defect restated
    /// after a rebase is the same finding.
    #[test]
    fn normalize_a_repeat_at_a_different_line_is_still_superseded() {
        let changed = ["src/a.rs".to_owned()];
        let print = fingerprint("rev-local", Some("src/a.rs"), Category::Correctness, "boom");

        let mut moved = raw(Severity::High, Some("src/a.rs"), "boom");
        moved.line_start = Some(400);
        moved.line_end = Some(402);

        let result = normalize(
            &[moved],
            &[],
            &context(&changed, Depth::Standard, &[], &[print]),
        );

        assert_eq!(result.findings[0].finding.state, FindingState::Superseded);
    }

    /// An engine reporting the same defect twice in one result files it once.
    #[test]
    fn normalize_a_repeat_within_one_run_is_superseded_too() {
        let changed = ["src/a.rs".to_owned()];
        let result = normalize(
            &[
                raw(Severity::High, Some("src/a.rs"), "boom"),
                raw(Severity::High, Some("src/a.rs"), "boom"),
            ],
            &[],
            &context(&changed, Depth::Standard, &[], &[]),
        );

        assert_eq!(result.publishable().count(), 1);
        assert_eq!(result.findings[0].finding.state, FindingState::Open);
        assert_eq!(result.findings[1].finding.state, FindingState::Superseded);
    }

    #[test]
    fn normalize_a_different_finding_on_the_same_file_is_not_superseded() {
        let changed = ["src/a.rs".to_owned()];
        let print = fingerprint("rev-local", Some("src/a.rs"), Category::Correctness, "boom");

        let result = normalize(
            &[raw(Severity::High, Some("src/a.rs"), "a different problem")],
            &[],
            &context(&changed, Depth::Standard, &[], &[print]),
        );

        assert_eq!(result.findings[0].finding.state, FindingState::Open);
        assert_eq!(result.publishable().count(), 1);
    }

    /// Suppression is checked before supersession: a user who asked never to hear
    /// something again should see "suppressed", not "already told you".
    #[test]
    fn normalize_suppression_wins_over_supersession() {
        let changed = ["src/a.rs".to_owned()];
        let print = fingerprint("rev-local", Some("src/a.rs"), Category::Correctness, "boom");
        let suppressions = [suppression(Some(&print), None)];

        let result = normalize(
            &[raw(Severity::High, Some("src/a.rs"), "boom")],
            &[],
            &context(
                &changed,
                Depth::Standard,
                &suppressions,
                std::slice::from_ref(&print),
            ),
        );

        assert_eq!(result.findings[0].finding.state, FindingState::Suppressed);
    }

    // --- general ---

    #[test]
    fn normalize_preserves_the_reported_order() {
        let changed = ["src/a.rs".to_owned()];
        let result = normalize(
            &[
                raw(Severity::Low, Some("src/a.rs"), "first"),
                raw(Severity::High, Some("src/a.rs"), "second"),
            ],
            &[],
            &context(&changed, Depth::Standard, &[], &[]),
        );

        assert_eq!(result.findings[0].finding.title, "first");
        assert_eq!(result.findings[1].finding.title, "second");
    }

    /// A missing confidence is 1.0, not 0.0. Absent evidence about confidence is not
    /// evidence of no confidence, and 0.0 would sort every such finding below the
    /// low-confidence threshold and out of sight.
    #[test]
    fn normalize_a_missing_confidence_is_full_confidence() {
        let changed = ["src/a.rs".to_owned()];
        let mut without = raw(Severity::High, Some("src/a.rs"), "t");
        without.confidence = None;

        let result = normalize(
            &[without],
            &[],
            &context(&changed, Depth::Standard, &[], &[]),
        );

        assert!((result.findings[0].finding.confidence - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn normalize_every_withheld_finding_states_a_reason() {
        let changed = ["src/a.rs".to_owned()];
        let print = fingerprint("rev-local", Some("src/a.rs"), Category::Correctness, "boom");
        let suppressions = [suppression(None, Some("other/**"))];

        let result = normalize(
            &[
                raw(Severity::High, Some("src/a.rs"), "boom"),
                raw(Severity::High, Some("other/b.rs"), "elsewhere"),
            ],
            &[],
            &context(&changed, Depth::Standard, &suppressions, &[print]),
        );

        for (finding, reason) in result.withheld() {
            assert!(
                !reason.is_empty(),
                "{} was withheld with no reason",
                finding.finding.title
            );
        }
        assert_eq!(result.withheld().count(), 2);
    }

    #[test]
    fn normalize_an_empty_result_is_empty() {
        let result = normalize(&[], &[], &context(&[], Depth::Standard, &[], &[]));

        assert!(result.findings.is_empty());
        assert!(result.still_dropped.is_empty());
        assert_eq!(result.publishable().count(), 0);
    }
}
