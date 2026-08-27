//! The `Engine` trait and its task/outcome types (SPEC §8.1).
//!
//! One trait covers Claude Code, Codex and the offline mock. The pipeline never
//! branches on which engine is configured — decision D3 makes that a per-repository
//! setting, and a pipeline that knew the difference would have to be changed every
//! time a CLI did.
//!
//! Two things are deliberately in the *types* rather than in a convention:
//!
//! - **`out_dir` is the only writable path.** §8.5 spawns the engine with `cwd` set
//!   to a scratch worktree and expects findings in `out_dir`. Naming both on the
//!   task means a runner cannot forget to pass one, and a reader can see the
//!   boundary without reading the runner.
//! - **`degraded` is a reason, not a flag.** §8.2's fallback ladder sets it, and
//!   §12.3 escalates every publish action on a degraded run to high risk. An
//!   escalation nobody can explain is worse than none.

use std::path::PathBuf;
use std::time::Duration;

use revlocal_core::{Category, Depth, EngineKind, Severity, Usage, Verdict};

/// Which engine an implementation is (SPEC §8.1).
///
/// An alias rather than a second enum: `repo.engine` already stores exactly these
/// three values with a `CHECK` constraint (§5), and a parallel type would be one
/// more thing to keep in step for no gain.
pub type EngineId = EngineKind;

/// What `revlocal doctor` learned about an engine (SPEC §8.1, §8.4).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EngineProbe {
    /// Which engine this describes.
    pub id: EngineId,
    /// Whether the binary is on PATH.
    pub installed: bool,
    /// The version string, when it could be obtained.
    pub version: Option<String>,
    /// Whether the CLI reports itself as logged in.
    ///
    /// Decision D9: engines authenticate via the user's existing CLI logins and the
    /// app stores no API keys, so this is something to *report*, never to fix.
    pub authenticated: bool,
    /// Whether a smoke task produced a `result.json` (§8.4).
    ///
    /// Distinct from `installed` and `authenticated` because an engine can be both
    /// and still not honour the output contract — a CLI whose flags changed, for
    /// instance. That is the failure `doctor` exists to catch before a real review
    /// spends tokens discovering it.
    pub honours_output_contract: Option<bool>,
    /// Everything wrong, each with remediation (SPEC §18).
    pub problems: Vec<EngineProblem>,
}

impl EngineProbe {
    /// Whether this engine can be used for a review right now.
    pub fn is_usable(&self) -> bool {
        self.installed && self.authenticated && self.honours_output_contract != Some(false)
    }

    /// A probe for an engine that is not installed.
    pub fn missing(id: EngineId, remediation: impl Into<String>) -> Self {
        Self {
            id,
            installed: false,
            version: None,
            authenticated: false,
            honours_output_contract: None,
            problems: vec![EngineProblem {
                problem: format!("`{id}` is not on PATH"),
                remediation: remediation.into(),
            }],
        }
    }
}

/// One reason an engine is not usable, and what to do about it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EngineProblem {
    /// What is wrong.
    pub problem: String,
    /// What the user should do.
    pub remediation: String,
}

/// One review, handed to an engine (SPEC §8.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineTask {
    /// The materialized worktree. **Read-only intent** — it is a scratch copy, but
    /// the engine has no business editing it either.
    pub cwd: PathBuf,
    /// The only path the engine may write to. `result.json` lands here (§8.2).
    pub out_dir: PathBuf,
    /// The rendered prompt (§9.2).
    pub prompt: String,
    /// Files the prompt refers to: the diff, prior findings, repo conventions.
    pub attachments: Vec<PathBuf>,
    /// Hard wall-clock limit (§8.5). Depth-scaled by the caller.
    pub timeout: Duration,
    /// How thorough this review should be (§9.3).
    pub depth: Depth,
}

impl EngineTask {
    /// Whether the task is self-consistent enough to run.
    ///
    /// Checked because each of these fails *silently* otherwise: an empty prompt
    /// gets a review of nothing, and `cwd == out_dir` makes the worktree writable
    /// and lets the engine edit the code it is reviewing — which §8.5 forbids and
    /// which no test of the engine's output would notice.
    pub fn is_runnable(&self) -> Result<(), EngineError> {
        if self.prompt.trim().is_empty() {
            return Err(EngineError::InvalidTask {
                detail: "the prompt is empty; the engine would review nothing".to_owned(),
            });
        }
        if self.cwd == self.out_dir {
            return Err(EngineError::InvalidTask {
                detail: "out_dir must not be the worktree: §8.5 makes out_dir the \
                         only writable path, and this would let the engine edit the \
                         code it is reviewing"
                    .to_owned(),
            });
        }
        if self.timeout.is_zero() {
            return Err(EngineError::InvalidTask {
                detail: "the timeout is zero; the engine would be killed before it started"
                    .to_owned(),
            });
        }
        Ok(())
    }
}

/// A finding as the engine reported it, before normalization (§9.5).
///
/// Deliberately not [`revlocal_core::Finding`]: it has no id, no fingerprint and no
/// run, its severity may be a value this build does not know, and its file may not
/// exist in the change. §9.5 turns one into the other, and conflating them would
/// mean unvalidated engine output flowing straight into the store.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RawFinding {
    /// Severity as reported. §9.5 clamps an unknown value to `medium`.
    pub severity: Severity,
    /// Category as reported.
    pub category: Category,
    /// The engine's confidence, if it gave one.
    #[serde(default)]
    pub confidence: Option<f64>,
    /// Path relative to the repository root, as the engine wrote it.
    #[serde(default)]
    pub file: Option<String>,
    /// First line of the implicated range.
    #[serde(default)]
    pub line_start: Option<u32>,
    /// Last line of the implicated range.
    #[serde(default)]
    pub line_end: Option<u32>,
    /// The claim alone.
    pub title: String,
    /// Markdown: what is wrong and why.
    pub body: String,
    /// Concrete inputs or state leading to wrong output.
    #[serde(default)]
    pub failure_scenario: Option<String>,
    /// Optional markdown or diff.
    #[serde(default)]
    pub suggested_fix: Option<String>,
}

/// What an engine produced (SPEC §8.1).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EngineOutcome {
    /// Findings, unnormalized.
    pub findings: Vec<RawFinding>,
    /// Markdown, at most 1200 characters (§8.3).
    pub summary: String,
    /// The stance this review takes (§10.2).
    pub verdict: Verdict,
    /// Tokens and cost. An unreported cost stays `None` (D10, ADR 0010).
    pub usage: Usage,
    /// Raw stdout, for the archive.
    pub transcript: String,
    /// Why the output had to be salvaged, if it did (§8.2).
    ///
    /// `Some` exactly when a rung of the fallback ladder was used. A reason rather
    /// than a flag: §12.3 escalates every action on a degraded run to high risk, and
    /// an inbox full of escalations that cannot say why is unusable.
    pub degraded: Option<String>,
    /// What the engine could not review, and why (§8.3 `coverage_notes`).
    ///
    /// SPEC §18: a review that saw 60% of the diff must never look like one that
    /// saw all of it.
    #[serde(default)]
    pub coverage_notes: Option<String>,
}

impl EngineOutcome {
    /// Whether a rung of the §8.2 fallback ladder was used.
    pub const fn is_degraded(&self) -> bool {
        self.degraded.is_some()
    }
}

/// What can go wrong running an engine.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EngineError {
    /// The engine binary is not installed.
    #[error("`{id}` is not installed\n  try: {remediation}")]
    NotInstalled {
        /// Which engine.
        id: EngineId,
        /// How to install it.
        remediation: String,
    },

    /// The task itself is malformed.
    #[error("the review task is not runnable: {detail}")]
    InvalidTask {
        /// What is wrong with it.
        detail: String,
    },

    /// The engine exceeded its wall-clock limit and was killed (§8.5).
    #[error("`{id}` did not finish within {timeout:?} and was killed")]
    Timeout {
        /// Which engine.
        id: EngineId,
        /// The limit it exceeded.
        timeout: Duration,
    },

    /// The run was cancelled — by the kill switch (§12.1) or by a user.
    ///
    /// Distinct from a timeout because it is not a failure of the engine, and the
    /// run's terminal status differs (`cancelled`, not `failed`).
    #[error("`{id}` was cancelled")]
    Cancelled {
        /// Which engine.
        id: EngineId,
    },

    /// Every rung of §8.2's ladder failed.
    #[error("`{id}` produced no parseable output; the transcript was kept")]
    OutputUnparseable {
        /// Which engine.
        id: EngineId,
    },

    /// The engine ran but failed.
    #[error("`{id}` failed: {detail}")]
    Failed {
        /// Which engine.
        id: EngineId,
        /// What went wrong.
        detail: String,
    },
}

impl EngineError {
    /// The `run.error` value for this failure (SPEC §8.2).
    ///
    /// Stable strings, because they are stored and the UI groups by them.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NotInstalled { .. } => "engine_not_installed",
            Self::InvalidTask { .. } => "engine_invalid_task",
            Self::Timeout { .. } => "engine_timeout",
            Self::Cancelled { .. } => "engine_cancelled",
            Self::OutputUnparseable { .. } => "engine_output_unparseable",
            Self::Failed { .. } => "engine_failed",
        }
    }

    /// Whether this failure means the run was cancelled rather than failed.
    ///
    /// The two lead to different terminal statuses, and treating a kill-switch
    /// cancellation as a failure would fill the UI with errors when a user
    /// deliberately stopped everything.
    pub const fn is_cancellation(&self) -> bool {
        matches!(self, Self::Cancelled { .. })
    }
}

/// The engine layer's result alias.
pub type Result<T, E = EngineError> = std::result::Result<T, E>;

/// One review engine (SPEC §8.1).
///
/// Object-safe: the daemon holds `Box<dyn Engine>` chosen per repository, so the
/// set of engines is a runtime property rather than a compile-time one. There is a
/// test asserting that, because losing object safety is easy — one generic method
/// would do it — and the failure appears far from the cause.
#[async_trait::async_trait]
pub trait Engine: Send + Sync {
    /// Which engine this is.
    fn id(&self) -> EngineId;

    /// Is the binary present, what version, is it authenticated (§8.4)?
    ///
    /// Never spends tokens beyond §8.4's smoke task, and never fails the caller for
    /// an unusable engine — an unusable engine is a *report*, not an error, because
    /// `doctor` needs to show every engine's state at once.
    async fn probe(&self) -> Result<EngineProbe>;

    /// Run one review.
    ///
    /// `cancel` is the kill switch's token (§12.1). An implementation must return
    /// [`EngineError::Cancelled`] promptly rather than finishing the work.
    async fn run(
        &self,
        task: EngineTask,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<EngineOutcome>;
}
