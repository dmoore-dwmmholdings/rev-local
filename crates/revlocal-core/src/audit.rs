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
    pub usage: Usage,
}

impl BudgetLedgerEntry {
    /// Whether this day's spend has reached `limit` runs.
    ///
    /// Exhaustion pauses the repo; it never silently drops a change (decision D10,
    /// SPEC §18).
    pub const fn runs_exhausted(&self, limit: u32) -> bool {
        self.runs >= limit
    }

    /// Whether this day's spend has reached `limit` total tokens.
    pub const fn tokens_exhausted(&self, limit: u64) -> bool {
        self.usage.total_tokens() >= limit
    }
}
