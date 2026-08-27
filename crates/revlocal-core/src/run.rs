//! The `run` table and token accounting (SPEC §5, §8).

use crate::{ChangeId, Depth, EngineKind, RunId, RunStatus, Timestamp, TriggerSource};
use serde::{Deserialize, Serialize};

/// One execution of the pipeline against one change (`run`, SPEC §3).
///
/// `(change_id, attempt)` is unique: a retry is a new run, not a mutated one, so
/// the history of what was tried survives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    /// Primary key.
    pub id: RunId,
    /// The change under review.
    pub change_id: ChangeId,
    /// 1 for the first attempt, incrementing per retry.
    pub attempt: u32,
    /// Where the run is in its lifecycle.
    pub status: RunStatus,
    /// Which engine ran. Recorded per run, since a repo's engine can change.
    pub engine: EngineKind,
    /// How thoroughly it was reviewed (SPEC §9.3).
    pub depth: Depth,
    /// What caused the run.
    pub trigger: TriggerSource,
    /// Why the run was skipped. Set exactly when `status` is
    /// [`RunStatus::Skipped`] — SPEC §18's "no silent caps" means a skip always
    /// says why.
    pub skip_reason: Option<String>,
    /// Why the run failed. Set exactly when `status` is [`RunStatus::Failed`].
    pub error: Option<String>,
    /// Token and cost accounting.
    pub usage: Usage,
    /// When the engine process started.
    pub started_at: Option<Timestamp>,
    /// When the run reached a terminal status.
    pub finished_at: Option<Timestamp>,
    /// Path to the raw engine stdout on disk. Pruned by retention (SPEC §5.1).
    pub transcript_path: Option<String>,
    /// Why the engine output had to be salvaged, when it did.
    ///
    /// `Some` exactly when a step of the §8.2 fallback ladder was used. Typed as a
    /// reason rather than a flag to match `EngineOutcome::degraded` in SPEC §8.1,
    /// and because "no silent caps" (§18) means a degraded run has to say what was
    /// degraded about it. A degraded run escalates every publish action to high
    /// risk (§12.3), so this is load-bearing, not diagnostic.
    pub degraded: Option<String>,
    /// When the row was created.
    pub created_at: Timestamp,
}

impl Run {
    /// Whether the run's output had to be salvaged (SPEC §8.2).
    pub const fn is_degraded(&self) -> bool {
        self.degraded.is_some()
    }

    /// Whether this run's state is self-consistent.
    ///
    /// SPEC §18 forbids silent caps: a skipped run must say why, and a failed run
    /// must carry its error. This is the invariant the store asserts on write.
    pub fn is_consistent(&self) -> bool {
        let skip_ok = (self.status == RunStatus::Skipped) == self.skip_reason.is_some();
        let error_ok = (self.status == RunStatus::Failed) == self.error.is_some();
        skip_ok && error_ok
    }
}

/// Token and cost accounting for one run (`run.tokens_*`, `run.cost_usd`).
///
/// `cost_usd` is optional because not every engine reports a price; a missing cost
/// is recorded as missing rather than as zero, so budget maths cannot silently
/// treat an unknown spend as free (SPEC §18).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    /// Input tokens consumed.
    pub tokens_in: u64,
    /// Output tokens produced.
    pub tokens_out: u64,
    /// Cost in USD, when the engine reports one.
    pub cost_usd: Option<f64>,
}

impl Usage {
    /// Total tokens, which is what per-repo budgets are denominated in (D10).
    pub const fn total_tokens(&self) -> u64 {
        self.tokens_in + self.tokens_out
    }

    /// Add another run's usage to this one.
    ///
    /// An unknown cost stays unknown rather than being read as zero: if either side
    /// reports a cost the sum is `Some`, and the unknown side contributes nothing —
    /// which is why [`cost_is_complete`](Self::cost_is_complete) exists to say
    /// whether the total can be trusted.
    pub fn add(&mut self, other: &Self) {
        self.tokens_in += other.tokens_in;
        self.tokens_out += other.tokens_out;
        self.cost_usd = match (self.cost_usd, other.cost_usd) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
        };
    }

    /// Whether `cost_usd` accounts for everything measured.
    pub const fn cost_is_complete(&self) -> bool {
        self.cost_usd.is_some()
    }
}
