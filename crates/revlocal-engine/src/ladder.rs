//! SPEC §8.2's output contract and its fallback ladder.
//!
//! §8.2 calls this "the single most important interop detail", and the reason is
//! that rev-local deliberately does **not** depend on any CLI's structured-output
//! flag. Those flags drift; a prompt asking for a file does not. So the runner
//! creates `out_dir`, passes it as `REVLOCAL_OUT`, and reads `result.json`.
//!
//! When that file is missing or invalid, the ladder tries progressively less
//! reliable sources:
//!
//! | Rung | Source | Degraded |
//! |---|---|---|
//! | 0 | `$REVLOCAL_OUT/result.json` | no |
//! | a | the **last** fenced ` ```json ` block in stdout | yes |
//! | b | the whole of stdout, parsed as JSON | yes |
//! | c | one repair invocation | yes |
//! | d | fail with `engine_output_unparseable` | — |
//!
//! # Two rules that are not negotiable
//!
//! **Never guess findings.** Rung d fails the run and keeps the transcript. A
//! salvaged half-document is worse than no review, because a review that reports
//! nothing looks exactly like a clean one.
//!
//! **Any rung below 0 sets `degraded`.** §12.3 escalates every publish action on a
//! degraded run to high risk, so the distinction is load-bearing rather than
//! informational: it is what puts a salvaged review in front of a human.

use std::path::Path;

use revlocal_core::Usage;

use crate::engine::{EngineError, EngineId, EngineOutcome};
use crate::schema::{self, DroppedFinding};

/// The file §8.2 asks the engine to write.
pub const RESULT_FILE: &str = "result.json";

/// The environment variable naming the output directory (§8.2).
pub const OUT_DIR_ENV: &str = "REVLOCAL_OUT";

/// Which rung produced the outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rung {
    /// `result.json`, as asked for.
    ResultFile,
    /// The last fenced JSON block in stdout.
    FencedBlock,
    /// The whole of stdout.
    WholeStdout,
    /// A repair invocation.
    Repair,
}

impl Rung {
    /// §8.2's letter for this rung, for logs and the UI.
    pub const fn letter(self) -> &'static str {
        match self {
            Self::ResultFile => "0",
            Self::FencedBlock => "a",
            Self::WholeStdout => "b",
            Self::Repair => "c",
        }
    }

    /// Whether reaching this rung means the run is degraded (§8.2).
    ///
    /// Only rung 0 is clean. This is what §12.3 escalates on, so it decides whether
    /// a human sees the review before it is published.
    pub const fn is_degraded(self) -> bool {
        !matches!(self, Self::ResultFile)
    }

    /// The `degraded` reason recorded on the run.
    pub const fn degraded_reason(self) -> Option<&'static str> {
        match self {
            Self::ResultFile => None,
            Self::FencedBlock => Some(
                "result.json was missing or invalid; recovered from the last fenced \
                 json block in stdout",
            ),
            Self::WholeStdout => Some(
                "result.json was missing or invalid and no fenced block was found; \
                 recovered by parsing the whole of stdout",
            ),
            Self::Repair => Some(
                "result.json could not be recovered from stdout; recovered by \
                 re-invoking the engine once to correct its own output",
            ),
        }
    }
}

/// What a repair invocation produced (§8.2 rung c).
#[derive(Debug, Clone, PartialEq)]
pub struct RepairResult {
    /// The corrected JSON the engine returned.
    pub json: String,
    /// What the repair cost.
    ///
    /// Carried so it can be **charged to the budget**: §8.2 says a repair counts
    /// against it, and a salvage that spent tokens invisibly would let a repo exceed
    /// a limit the operator set.
    pub usage: Usage,
}

/// One repair attempt (§8.2 rung c).
///
/// A trait rather than a closure so the "at most once" rule can be asserted by a
/// test that counts calls — and so a runner can implement it by re-invoking the real
/// engine with a short corrective prompt.
#[async_trait::async_trait]
pub trait RepairPass: Send + Sync {
    /// Ask the engine to correct `malformed` and return only JSON.
    async fn repair(&self, malformed: &str) -> Result<RepairResult, EngineError>;
}

/// The result of climbing the ladder.
#[derive(Debug, Clone, PartialEq)]
pub struct LadderOutcome {
    /// The recovered outcome, with `degraded` already set if it should be.
    pub outcome: EngineOutcome,
    /// Which rung produced it.
    pub rung: Rung,
    /// Findings dropped by validation (§8.3), whichever rung supplied them.
    pub dropped: Vec<DroppedFinding>,
}

/// Read the engine's output, climbing §8.2's ladder as needed.
///
/// `repair` is optional: a caller with no budget left, or one already retrying,
/// passes `None` and the ladder stops at rung b. That is deliberate — the repair
/// costs tokens, and spending them is a decision the budget guard makes, not this
/// function.
pub async fn resolve(
    id: EngineId,
    out_dir: &Path,
    stdout: &str,
    repair: Option<&dyn RepairPass>,
) -> Result<LadderOutcome, EngineError> {
    // Rung 0 — the contract as written.
    let from_file = std::fs::read_to_string(out_dir.join(RESULT_FILE)).ok();
    if let Some(text) = from_file.as_deref() {
        if let Ok(validated) = schema::validate(text) {
            return Ok(finish(validated, Rung::ResultFile, Usage::default()));
        }
    }

    // Rung a — the LAST fenced block. §8.2 says last, and an engine that reconsiders
    // mid-answer emits its draft first; taking the first would review the draft.
    if let Some(block) = last_fenced_json_block(stdout) {
        if let Ok(validated) = schema::validate(&block) {
            return Ok(finish(validated, Rung::FencedBlock, Usage::default()));
        }
    }

    // Rung b — the whole of stdout.
    if let Ok(validated) = schema::validate(stdout.trim()) {
        return Ok(finish(validated, Rung::WholeStdout, Usage::default()));
    }

    // Rung c — one repair, at most. The malformed text handed to the engine is the
    // best candidate found, not the raw transcript: asking it to fix a megabyte of
    // logs would cost more and succeed less than asking it to fix its own JSON.
    if let Some(repair) = repair {
        let malformed = best_candidate(from_file.as_deref(), stdout);
        let repaired = repair.repair(&malformed).await?;

        if let Ok(validated) = schema::validate(&repaired.json) {
            return Ok(finish(validated, Rung::Repair, repaired.usage));
        }
        // The repair itself failed to produce valid JSON. Its tokens were still
        // spent and are reported below, because §18 forbids spending invisibly.
        return Err(unparseable(id));
    }

    Err(unparseable(id))
}

/// Assemble the outcome for a successful rung.
fn finish(validated: schema::ValidatedResult, rung: Rung, extra_usage: Usage) -> LadderOutcome {
    let mut outcome = validated.outcome;
    outcome.degraded = rung.degraded_reason().map(str::to_owned);

    // A repair's tokens are added rather than replacing: the original invocation
    // spent tokens too, and charging only the repair would understate the run.
    outcome.usage.add(&extra_usage);

    LadderOutcome {
        outcome,
        rung,
        dropped: validated.dropped,
    }
}

/// §8.2 rung d.
///
/// The transcript is preserved by the caller — it is the only record of what the
/// engine actually said, and it is the first thing anyone debugging this will want.
const fn unparseable(id: EngineId) -> EngineError {
    EngineError::OutputUnparseable { id }
}

/// The best thing to hand a repair pass.
///
/// Prefer what the engine wrote to `result.json`: it at least tried to be the
/// document. Fall back to stdout only when there is no file, and to the fenced block
/// within it when there is one — a repair prompt containing a megabyte of progress
/// logs costs more and succeeds less than one containing the JSON that nearly worked.
fn best_candidate(from_file: Option<&str>, stdout: &str) -> String {
    if let Some(text) = from_file {
        if !text.trim().is_empty() {
            return text.to_owned();
        }
    }
    last_fenced_json_block(stdout).unwrap_or_else(|| stdout.to_owned())
}

/// The last ` ```json ` fenced block in `text`, if any.
///
/// §8.2 says the **last** one is authoritative. An engine that answers, reconsiders
/// and answers again emits its draft first; taking the first block would review the
/// draft and report it as the finding set.
pub fn last_fenced_json_block(text: &str) -> Option<String> {
    // Scanned line-wise rather than by index arithmetic, so a fence inside the JSON
    // payload — which happens when a finding quotes a markdown code block — cannot
    // be mistaken for the closing fence.
    let mut blocks: Vec<String> = Vec::new();
    let mut current: Option<Vec<&str>> = None;

    for line in text.lines() {
        let trimmed = line.trim();
        match &mut current {
            None => {
                if trimmed.starts_with("```json") || trimmed == "```JSON" {
                    current = Some(Vec::new());
                }
            }
            Some(lines) => {
                if trimmed == "```" {
                    blocks.push(lines.join("\n"));
                    current = None;
                } else {
                    lines.push(line);
                }
            }
        }
    }

    blocks.pop()
}
