//! The `audit` and `budget_ledger` tables (SPEC §5, §12, decision D10).

use crate::{AuditId, RepoId, RunId, Timestamp, Usage};
use serde::{Deserialize, Serialize};

/// One entry in the audit log (`audit`, SPEC §5, decision D7).
///
/// The audit log is append-only and is the record of what rev-local did on a
/// user's behalf, so `actor` distinguishes the daemon from a human from a specific
/// engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    /// Primary key.
    pub id: AuditId,
    /// When it happened.
    pub at: Timestamp,
    /// `daemon` | `user` | `engine:<name>`.
    pub actor: String,
    /// Event name.
    pub kind: String,
    /// The repo involved, when there is one.
    pub repo_id: Option<RepoId>,
    /// The run involved, when there is one.
    pub run_id: Option<RunId>,
    /// Event-specific payload, as JSON.
    pub detail_json: String,
}

/// One day's spend against one repo's budget (`budget_ledger`, decision D10).
///
/// The primary key is `(repo_id, day)`, and `day` is a local `YYYY-MM-DD` date, not
/// an instant — budgets are a human-facing daily allowance, so they roll over on
/// the user's midnight rather than UTC's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BudgetLedgerEntry {
    /// Which repo spent it.
    pub repo_id: RepoId,
    /// Local calendar day, `YYYY-MM-DD`.
    pub day: String,
    /// How many runs executed that day.
    pub runs: u32,
    /// Tokens and cost spent that day.
    ///
    /// `usage.cost_usd` is `Some` **only when every run that day reported a
    /// price**. A day with even one unpriced run reports `None`, so a cost budget
    /// cannot read an unmeasured day as a cheap one (decision D10, SPEC §18). The
    /// costs that *were* reported are still available as
    /// [`known_cost_usd`](Self::known_cost_usd) — the number is not discarded,
    /// it just is not allowed to masquerade as the total.
    pub usage: Usage,

    /// The sum of the costs that were actually reported that day.
    ///
    /// Equal to `usage.cost_usd` when the day is complete; a lower bound on the
    /// real spend when it is not.
    pub known_cost_usd: f64,
}

impl BudgetLedgerEntry {
    /// Whether this day's spend has reached `limit` runs.
    ///
    /// Exhaustion pauses the repo; it never silently drops a change (decision D10,
    /// SPEC §18).
    pub const fn runs_exhausted(&self, limit: u32) -> bool {
        self.runs >= limit
    }

    /// Whether this day's token spend has reached `limit`.
    ///
    /// Returns `None` when the day contains a run whose tokens nobody measured and
    /// the known portion has not already passed the limit. The honest answer is
    /// "cannot tell", and D10 says an exhausted-or-unknown budget pauses rather
    /// than proceeding.
    ///
    /// This was a plain `bool` until RL-409, on ADR 0010's stated grounds that
    /// "token counts are always known". They are not: §8.3's `result.json` carries
    /// no usage field, so a real engine's runner had no counts to report and
    /// returned zero. A caller treating a missing count as "not exhausted" is doing
    /// exactly what §18 forbids, which is why this is no longer a `bool`.
    pub const fn tokens_exhausted(&self, limit: u64) -> Option<bool> {
        if self.usage.total_tokens() >= limit {
            // Already over on the tokens we do know about; an unmeasured remainder
            // can only make that more true.
            return Some(true);
        }
        if self.usage.tokens_are_known() {
            Some(false)
        } else {
            None
        }
    }

    /// Whether this day's token count is fully known.
    pub const fn tokens_are_complete(&self) -> bool {
        self.usage.tokens_are_known()
    }

    /// Whether this day's cost is fully known.
    pub const fn cost_is_complete(&self) -> bool {
        self.usage.cost_is_complete()
    }

    /// Whether this day's spend has reached a cost `limit`.
    ///
    /// Returns `None` when the day's cost is incomplete and the known portion has
    /// not already passed the limit: the honest answer is "cannot tell", and D10
    /// says an exhausted-or-unknown budget pauses rather than proceeding. A caller
    /// that treated `None` as "not exhausted" would be doing exactly what §18
    /// forbids, which is why this is not a `bool`.
    pub fn cost_exhausted(&self, limit: f64) -> Option<bool> {
        if self.known_cost_usd >= limit {
            // Already over on the costs we do know about; the unknown remainder
            // can only make that more true.
            return Some(true);
        }
        if self.cost_is_complete() {
            Some(false)
        } else {
            None
        }
    }
}
