//! The review pipeline, end to end (SPEC §9).
//!
//! One materialized change in, one [`ReviewReport`] out. The stages are §9's, in
//! §9's order: skip rules, ignore filtering, truncation, depth selection, prompt
//! assembly, the engine, schema validation, normalization, and — when a standard run
//! turns up something serious — one escalated re-run at `deep`.
//!
//! # The report is stable on purpose
//!
//! §9's acceptance asks for "stable JSON suitable for test assertions", which is a
//! constraint on what may appear in it, not a formatting preference. Two reviews of
//! the same commit with the same config must produce byte-identical documents.
//!
//! That rules out more than it looks like. The worktree is a scratch directory whose
//! path changes every run, so **no report field carries an absolute path** — a single
//! leaked `cwd` would destroy the guarantee while every other assertion still passed.
//! Timestamps and database ids are likewise absent: they belong to the run record,
//! not to the description of what the review found.
//!
//! `report_is_byte_stable_across_runs` in `tests/pipeline_e2e.rs` is the guard.
//!
//! # Nothing here reaches the network
//!
//! The pipeline holds an [`Engine`] trait object and a filesystem path. It opens no
//! sockets, and the engines that do exist run local binaries. The e2e suite asserts
//! this rather than assuming it — see `no_network` there.

use std::path::{Path, PathBuf};

use revlocal_core::{Change, Depth, Finding, RepoConfig, Suppression, Timestamp, Usage, Verdict};
use revlocal_engine::{Engine, EngineError, EngineTask};
use tokio_util::sync::CancellationToken;

use crate::{depth, normalize, prompt, truncation};

/// The version of the report document.
///
/// Bumped when a field is removed or changes meaning — never when one is added, so a
/// consumer that reads what it recognises keeps working.
pub const REPORT_SCHEMA_VERSION: u32 = 1;

/// How a review ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    /// The engine ran and its output was understood.
    Done,
    /// A §9.4 skip rule fired. No engine spend.
    Skipped,
    /// The engine could not produce a usable review.
    Failed,
}

/// Everything the pipeline needs about one change.
#[derive(Debug, Clone)]
pub struct ReviewInputs<'a> {
    /// Repository name — part of the fingerprint (§10.3), so it is the *stable*
    /// name, not a filesystem path.
    pub repo_name: &'a str,
    /// What kind of repository it is, for the prompt's metadata section.
    pub repo_kind: &'a str,
    /// The change under review.
    pub change: &'a Change,
    /// The effective per-repo config.
    pub config: &'a RepoConfig,
    /// The materialized worktree. Never appears in the report.
    pub worktree: &'a Path,
    /// The unified diff, before truncation.
    pub diff_unified: &'a str,
    /// Per-file summary of the same diff.
    pub diff_files: &'a [revlocal_core::FileDiff],
    /// PR labels, where the concept applies (§9.3).
    pub labels: &'a [String],
    /// Active suppressions for this repo (§9.5).
    pub suppressions: &'a [Suppression],
    /// Fingerprints already published on this change (§9.5).
    pub published_fingerprints: &'a [String],
    /// Findings from earlier runs, for the prompt's prior-context section (§9.2).
    pub prior_findings: &'a [Finding],
    /// A §9.4 skip decision already taken by the caller, if any.
    ///
    /// Taken as input rather than recomputed: the skip rules need a `DetectedChange`,
    /// which belongs to the VCS layer, and re-deriving it here would mean two places
    /// that could disagree about whether a change is reviewable.
    pub skip: Option<&'a str>,
    /// When the run happened. Used for the finding rows, never for the report.
    pub now: Timestamp,
}

/// One finding, as it appears in the report.
///
/// Deliberately not a [`Finding`]: that carries a database id and a timestamp, both
/// of which vary between runs and would break the stability guarantee.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReportFinding {
    /// Stable dedupe key (§10.3).
    pub fingerprint: String,
    /// How bad it is.
    pub severity: String,
    /// What kind of problem.
    pub category: String,
    /// Path, when file-scoped.
    pub file: Option<String>,
    /// First implicated line.
    pub line_start: Option<u32>,
    /// Last implicated line.
    pub line_end: Option<u32>,
    /// The claim.
    pub title: String,
    /// Whether it names a file the change does not touch (§9.5).
    pub out_of_diff: bool,
}

/// A finding that was held back, and why.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WithheldFinding {
    /// Its fingerprint.
    pub fingerprint: String,
    /// `suppressed` or `superseded`.
    pub state: String,
    /// The claim, so a user can recognise it.
    pub title: String,
    /// Why it is not being published.
    pub reason: String,
}

/// The result of one review, as stable JSON (SPEC §9).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReviewReport {
    /// Document version.
    pub schema_version: u32,
    /// Repository name.
    pub repo: String,
    /// Which engine actually ran (§8.4, decision D3).
    ///
    /// Stated rather than assumed, because `mock` and `claude` produce documents
    /// of the same shape and a mock result that read like a real one would be the
    /// most convincing wrong answer this program can give. Added rather than
    /// replacing a field, so `REPORT_SCHEMA_VERSION` stays at 1.
    pub engine: String,
    /// The change's identity in its own system.
    pub change: String,
    /// How it ended.
    pub status: ReviewStatus,
    /// Which §9.4 rule skipped it, when `status` is `skipped`.
    pub skip_reason: Option<String>,
    /// The depth actually reviewed at (§9.3).
    pub depth: String,
    /// Every rule that argued for that depth, deepest first (§9.3, §18).
    pub depth_reasons: Vec<String>,
    /// Whether a standard run was re-run at `deep` (§9.3).
    pub escalated: bool,
    /// How many engine invocations this review took.
    pub attempts: u32,
    /// Whether the diff was reduced (§9.4).
    pub truncated: bool,
    /// Files dropped entirely — complete, never itself truncated (§9.4, §18).
    pub omitted_files: Vec<String>,
    /// Files shown only as a stat line (§9.4).
    pub reduced_files: Vec<String>,
    /// The stance this review takes (§10.2).
    pub verdict: Option<String>,
    /// The engine's summary.
    pub summary: String,
    /// Findings that reach the publish plan.
    pub findings: Vec<ReportFinding>,
    /// Findings held back, with reasons (§9.5, §18).
    pub withheld: Vec<WithheldFinding>,
    /// Findings the schema rejected and normalization could not salvage (§8.3).
    pub dropped_findings: u32,
    /// Why the output had to be salvaged, if it did (§8.2).
    pub degraded: Option<String>,
    /// What the engine said it could not review (§8.3, §18).
    pub coverage_notes: Option<String>,
    /// Why the review failed, when `status` is `failed`.
    pub failure: Option<String>,
    /// Tokens and cost. An unmeasured cost stays absent (D10, ADR 0010).
    pub usage: Usage,
}

impl ReviewReport {
    /// Whether the report is self-consistent (SPEC §18).
    ///
    /// Every claim that something was reduced must be accompanied by what was
    /// reduced, and a failed review must say why. Both are the same class of bug: a
    /// document that looks complete and is not.
    pub fn is_consistent(&self) -> bool {
        let truncation_ok =
            !self.truncated || !self.omitted_files.is_empty() || !self.reduced_files.is_empty();
        let failure_ok = (self.status != ReviewStatus::Failed) || self.failure.is_some();
        let skip_ok = (self.status != ReviewStatus::Skipped) || self.skip_reason.is_some();

        truncation_ok && failure_ok && skip_ok
    }

    /// Serialize deterministically.
    ///
    /// `serde_json` preserves struct field order and this report holds no maps, so
    /// the bytes are a function of the values alone.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

/// What can stop a review before it produces a report.
#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    /// The prompt could not be assembled or persisted.
    #[error(transparent)]
    Prompt(#[from] prompt::PromptError),

    /// The scratch output directory could not be prepared.
    #[error("could not prepare the engine's output directory {path}: {source}")]
    OutDir {
        /// Where.
        path: String,
        /// Why.
        #[source]
        source: std::io::Error,
    },
}

/// A skipped change, reported without spending an engine (SPEC §9.4).
fn skipped_report(
    inputs: &ReviewInputs<'_>,
    engine_id: revlocal_engine::EngineId,
    reason: &str,
) -> ReviewReport {
    ReviewReport {
        schema_version: REPORT_SCHEMA_VERSION,
        repo: inputs.repo_name.to_owned(),
        engine: engine_id.as_str().to_owned(),
        change: inputs.change.external_id.clone(),
        status: ReviewStatus::Skipped,
        skip_reason: Some(reason.to_owned()),
        depth: Depth::Summary.to_string(),
        depth_reasons: Vec::new(),
        escalated: false,
        attempts: 0,
        truncated: false,
        omitted_files: Vec::new(),
        reduced_files: Vec::new(),
        verdict: None,
        summary: String::new(),
        findings: Vec::new(),
        withheld: Vec::new(),
        dropped_findings: 0,
        degraded: None,
        coverage_notes: None,
        failure: None,
        usage: Usage::default(),
    }
}

/// Everything one review produced.
///
/// The report is the *document* — what `--json` prints and what a consumer reads.
/// The findings are rows, ready for the store: full bodies, confidences and
/// suggested fixes, which the report deliberately does not carry because a
/// summary that included every field would not be a summary.
///
/// Returned together because a caller that persists a review needs both, and the
/// alternative was the pipeline dropping the rows on the floor and something
/// downstream reconstructing them from the summary — which is exactly how two
/// representations of one review start disagreeing.
#[derive(Debug, Clone)]
pub struct ReviewOutcome {
    /// The report.
    pub report: ReviewReport,
    /// Every finding, publishable or not, ready to store.
    pub findings: Vec<normalize::NormalizedFinding>,
}

/// Run one review (SPEC §9).
pub async fn review(
    inputs: &ReviewInputs<'_>,
    engine: &dyn Engine,
    scratch: &Path,
    cancel: &CancellationToken,
) -> Result<ReviewOutcome, PipelineError> {
    // --- §9.4 skip ---
    if let Some(reason) = inputs.skip {
        return Ok(ReviewOutcome {
            report: skipped_report(inputs, engine.id(), reason),
            findings: Vec::new(),
        });
    }

    // --- §9.4 ignore filtering, then truncation ---
    let all_paths: Vec<String> = inputs.diff_files.iter().map(|f| f.path.clone()).collect();
    let reviewable = revlocal_vcs::reviewable_paths(&all_paths, inputs.config);

    // A change with nothing reviewable left is a skip, not an empty review. Sending
    // an engine an empty diff spends a budget to be told nothing is wrong.
    if reviewable.is_empty() {
        return Ok(ReviewOutcome {
            report: skipped_report(
                inputs,
                engine.id(),
                "every path matched ignore_globs, or the diff was empty",
            ),
            findings: Vec::new(),
        });
    }

    let kept: Vec<revlocal_core::FileDiff> = inputs
        .diff_files
        .iter()
        .filter(|f| reviewable.contains(&f.path))
        .cloned()
        .collect();

    let cut = truncation::truncate(inputs.diff_unified, &kept, inputs.config);

    // --- §9.3 depth ---
    let decision = depth::select(
        &reviewable,
        &inputs.change.diff_stat,
        inputs.labels,
        inputs.config,
    );

    let conventions = prompt::read_conventions(inputs.worktree, inputs.config);
    let out_dir = scratch.join("engine-out");

    let changed_paths: Vec<String> = inputs.diff_files.iter().map(|f| f.path.clone()).collect();

    let mut attempt = Attempt {
        depth: decision.depth,
        escalated: false,
    };
    let mut attempts = 0_u32;

    let (outcome, normalized) = loop {
        attempts += 1;
        std::fs::create_dir_all(&out_dir).map_err(|source| PipelineError::OutDir {
            path: out_dir.display().to_string(),
            source,
        })?;

        let context = prompt::build_context(
            inputs.repo_name,
            inputs.repo_kind,
            inputs.change,
            inputs.config,
            &cut.diff,
            cut.truncated,
            &cut.omitted_files,
            conventions.clone(),
            inputs.prior_findings,
            &suppressed_fingerprints(inputs.suppressions),
        );
        let rendered = prompt::render_to(&context, &out_dir)?;

        let task = EngineTask {
            cwd: inputs.worktree.to_path_buf(),
            out_dir: out_dir.clone(),
            prompt: rendered,
            attachments: Vec::new(),
            timeout: revlocal_engine::timeout_for(attempt.depth),
            depth: attempt.depth,
        };

        let outcome = engine.run(task, cancel.clone()).await;

        let Ok(ref engine_outcome) = outcome else {
            break (outcome, None);
        };

        let normalized = normalize::normalize(
            &engine_outcome.findings,
            &[],
            &normalize::NormalizeContext {
                run_id: revlocal_core::RunId::new(0),
                repo_name: inputs.repo_name,
                changed_paths: &changed_paths,
                depth: attempt.depth,
                suppressions: inputs.suppressions,
                published_fingerprints: inputs.published_fingerprints,
                now: inputs.now,
            },
        );

        // §9.3's escalation, asked of the findings that **survived normalization**
        // rather than of what the engine said. A critical finding the user has
        // suppressed must not buy them a 25-minute re-run they asked not to have,
        // and a superseded one is a repeat of something already filed.
        //
        // `depth::escalate` refuses from any tier but `standard`, which is what makes
        // "exactly once" structural rather than a counter kept here.
        let publishable: Vec<Finding> = normalized
            .publishable()
            .map(|f| f.finding.clone())
            .collect();

        if depth::escalate(attempt.depth, &publishable).is_none() {
            break (outcome, Some(normalized));
        }

        attempt = Attempt {
            depth: Depth::Deep,
            escalated: true,
        };
    };

    let findings = normalized
        .as_ref()
        .map(|n| n.findings.clone())
        .unwrap_or_default();

    Ok(ReviewOutcome {
        report: build_report(
            inputs,
            &decision,
            &cut,
            EngineRun {
                engine_id: engine.id(),
                attempt: &attempt,
                attempts,
                outcome,
                normalized,
            },
        ),
        findings,
    })
}

/// Everything one pass at the engine produced.
///
/// Grouped rather than passed as five parameters: they are one thing — what came
/// back from the engine — and a signature long enough to trip clippy's limit is
/// one where two arguments of the same type can be swapped without the compiler
/// noticing.
struct EngineRun<'a> {
    engine_id: revlocal_engine::EngineId,
    attempt: &'a Attempt,
    attempts: u32,
    outcome: Result<revlocal_engine::EngineOutcome, EngineError>,
    normalized: Option<normalize::Normalized>,
}

/// Which depth an attempt ran at, and whether it was the escalated one.
struct Attempt {
    depth: Depth,
    escalated: bool,
}

/// Fingerprints a user asked never to see again, for the prompt (§9.2).
fn suppressed_fingerprints(suppressions: &[Suppression]) -> Vec<String> {
    let mut prints: Vec<String> = suppressions
        .iter()
        .filter_map(|s| s.fingerprint.clone())
        .collect();
    // Sorted so the prompt — and therefore any transcript diff — is stable.
    prints.sort_unstable();
    prints
}

/// Assemble the report from whatever the engine produced.
fn build_report(
    inputs: &ReviewInputs<'_>,
    decision: &depth::DepthDecision,
    cut: &truncation::TruncationOutcome,
    run: EngineRun<'_>,
) -> ReviewReport {
    let EngineRun {
        engine_id,
        attempt,
        attempts,
        outcome,
        normalized,
    } = run;
    let mut report = ReviewReport {
        schema_version: REPORT_SCHEMA_VERSION,
        repo: inputs.repo_name.to_owned(),
        engine: engine_id.as_str().to_owned(),
        change: inputs.change.external_id.clone(),
        status: ReviewStatus::Done,
        skip_reason: None,
        depth: attempt.depth.to_string(),
        depth_reasons: decision.explain(),
        escalated: attempt.escalated,
        attempts,
        truncated: cut.truncated,
        omitted_files: cut.omitted_files.clone(),
        reduced_files: cut.reduced_files.clone(),
        verdict: None,
        summary: String::new(),
        findings: Vec::new(),
        withheld: Vec::new(),
        dropped_findings: 0,
        degraded: None,
        coverage_notes: None,
        failure: None,
        usage: Usage::default(),
    };

    let (Ok(engine_outcome), Some(normalized)) = (outcome, normalized) else {
        report.status = ReviewStatus::Failed;
        // A code, not a message: §8.2's failure reasons are a fixed set the UI and
        // the audit log both key on, and prose would be a different string every
        // time an error's wording changed.
        report.failure = Some("engine_failed".to_owned());
        return report;
    };

    report.verdict = Some(engine_outcome.verdict.to_string());
    report.summary = engine_outcome.summary.clone();
    report.degraded = engine_outcome.degraded.clone();
    report.coverage_notes = engine_outcome.coverage_notes.clone();
    report.usage = engine_outcome.usage;
    report.dropped_findings = u32::try_from(normalized.still_dropped.len()).unwrap_or(u32::MAX);

    report.findings = normalized
        .publishable()
        .map(|f| ReportFinding {
            fingerprint: f.finding.fingerprint.clone(),
            severity: f.finding.severity.to_string(),
            category: f.finding.category.to_string(),
            file: f.finding.file.clone(),
            line_start: f.finding.line_start,
            line_end: f.finding.line_end,
            title: f.finding.title.clone(),
            out_of_diff: f.out_of_diff,
        })
        .collect();

    report.withheld = normalized
        .withheld()
        .map(|(f, reason)| WithheldFinding {
            fingerprint: f.finding.fingerprint.clone(),
            state: f.finding.state.to_string(),
            title: f.finding.title.clone(),
            reason,
        })
        .collect();

    report
}

/// Where the pipeline writes the engine's output, relative to a scratch directory.
///
/// Exposed so a caller archiving a run knows where to look without guessing.
pub fn engine_out_dir(scratch: &Path) -> PathBuf {
    scratch.join("engine-out")
}

/// Whether a verdict blocks (SPEC §10.2), for callers building a publish plan.
pub const fn blocks(verdict: Verdict) -> bool {
    matches!(verdict, Verdict::RequestChanges)
}
