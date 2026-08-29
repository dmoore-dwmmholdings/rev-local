//! `revlocal approvals list` and `revlocal budget show` (RL-1201, SPEC §12.4, §13.1, §14).
//!
//! Both are reads, and both answer a question somebody asks when the system has
//! gone quiet: *what is waiting on me*, and *have I run out*. Those are the two
//! reasons rev-local stops doing anything without being broken, and neither is
//! visible from the outside.
//!
//! Neither command writes. `approvals approve` and `budget reset` are separate,
//! because a command that shows you a queue should not be able to empty it.

use revlocal_core::{BudgetLedgerEntry, BudgetSettings, RepoId, Timestamp};
use revlocal_daemon::budgets::{check, BudgetVerdict};
use revlocal_store::{BudgetLedgerStore, Pool, PublishActionStore};
use serde::{Deserialize, Serialize};

/// Why an inspection could not complete.
#[derive(Debug, thiserror::Error)]
pub enum InspectError {
    /// The database could not be read.
    #[error("could not read the local database: {source}\n  try: revlocal db migrate")]
    Store {
        /// Why.
        #[source]
        source: Box<revlocal_store::StoreError>,
    },

    /// The report could not be serialised.
    #[error("could not render the report: {source}")]
    Unrenderable {
        /// Why.
        #[source]
        source: serde_json::Error,
    },
}

fn boxed(source: revlocal_store::StoreError) -> InspectError {
    InspectError::Store {
        source: Box::new(source),
    }
}

/// One action waiting for a human (§12.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitingAction {
    /// The action's id, for `revlocal approvals approve <id>`.
    pub id: i64,
    /// The run it belongs to.
    pub run_id: i64,
    /// Where it would be sent.
    pub target: String,
    /// What it would do.
    pub capability: String,
}

/// The approvals inbox (§12.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalsReport {
    /// Everything waiting, oldest first.
    pub waiting: Vec<WaitingAction>,
}

impl ApprovalsReport {
    /// The human output.
    pub fn render_human(&self) -> String {
        if self.waiting.is_empty() {
            // Said explicitly. An empty list rendered as nothing is
            // indistinguishable from a command that failed to read anything.
            return "Nothing is waiting for approval.\n".to_owned();
        }

        let mut out = format!("{} action(s) waiting for approval\n", self.waiting.len());
        for item in &self.waiting {
            // §15's rule, applied to the CLI: a pending outbound action names its
            // target, so approving it is not a leap of faith.
            out.push_str(&format!(
                "  #{:<5} run {:<5} {} → {}\n",
                item.id, item.run_id, item.capability, item.target
            ));
        }
        out.push_str("\nApprove with: revlocal approvals approve <id>\n");
        out
    }
}

/// Read the approvals inbox.
pub async fn approvals(pool: &Pool) -> Result<ApprovalsReport, InspectError> {
    let actions = PublishActionStore::new(pool)
        .list_awaiting_approval()
        .await
        .map_err(boxed)?;

    Ok(ApprovalsReport {
        waiting: actions
            .into_iter()
            .map(|action| WaitingAction {
                id: action.id.get(),
                run_id: action.run_id.get(),
                target: action.target.clone(),
                capability: action.capability.to_string(),
            })
            .collect(),
    })
}

/// One repository's spend for one day (§13.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BudgetReport {
    /// Which repository.
    pub repo_id: i64,
    /// The day, `YYYY-MM-DD`.
    pub day: String,
    /// Runs executed.
    pub runs: u32,
    /// The run ceiling; `0` means unlimited.
    pub daily_runs: u32,
    /// Tokens known to have been spent.
    pub tokens: u64,
    /// Whether that token count is the whole story (RL-409).
    pub tokens_known: bool,
    /// The token ceiling; `0` means unlimited.
    pub daily_tokens: u64,
    /// Cost known to have been spent.
    pub known_cost_usd: f64,
    /// Whether that cost is the whole story (D10).
    pub cost_known: bool,
    /// Whether a run may start right now, and why not if not.
    pub may_run: bool,
    /// The reason, when something is stopping it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl BudgetReport {
    /// The human output.
    pub fn render_human(&self) -> String {
        let ceiling = |limit: u64| {
            if limit == 0 {
                "unlimited".to_owned()
            } else {
                limit.to_string()
            }
        };

        let mut out = format!("repo {} on {}\n", self.repo_id, self.day);
        out.push_str(&format!(
            "  runs    {} of {}\n",
            self.runs,
            ceiling(u64::from(self.daily_runs))
        ));
        // §18, and RL-409's whole point: a number that might be a lower bound must
        // not be printed as though it were a total.
        out.push_str(&format!(
            "  tokens  {}{} of {}\n",
            self.tokens,
            if self.tokens_known {
                ""
            } else {
                " (at least — one run reported no count)"
            },
            ceiling(self.daily_tokens)
        ));
        out.push_str(&format!(
            "  cost    ${:.2}{}\n",
            self.known_cost_usd,
            if self.cost_known {
                ""
            } else {
                " (at least — one run reported no price)"
            }
        ));
        out.push_str(&format!(
            "\n{}\n",
            if self.may_run {
                "A run may start.".to_owned()
            } else {
                format!(
                    "Holding: {}",
                    self.reason.as_deref().unwrap_or("the budget is spent")
                )
            }
        ));
        out
    }
}

/// Read one repository's budget for a day (§13.1).
pub async fn budget(
    pool: &Pool,
    repo_id: RepoId,
    at: Timestamp,
    settings: &BudgetSettings,
) -> Result<BudgetReport, InspectError> {
    let day = revlocal_daemon::budgets::day_of(at);
    let entry: Option<BudgetLedgerEntry> = BudgetLedgerStore::new(pool)
        .get(repo_id, &day)
        .await
        .map_err(boxed)?;

    let verdict = check(entry.as_ref(), settings);

    // A day with no ledger row is a day nothing ran — which is *not* the same as a
    // day whose spend nobody measured. `Usage::default()` means "unmeasured"
    // (RL-409, deliberately), and using it here made a fresh install report
    // "0 tokens (at least — one run reported no count)" when no run had happened
    // at all. Found by running the command, not by reading it.
    let (usage, measured) = match entry.as_ref() {
        Some(entry) => (entry.usage, None),
        None => (revlocal_core::Usage::default(), Some(true)),
    };
    let tokens_known = measured.unwrap_or_else(|| usage.tokens_are_known());
    let cost_known = measured.unwrap_or_else(|| usage.cost_is_complete());

    Ok(BudgetReport {
        repo_id: repo_id.get(),
        day,
        runs: entry.as_ref().map_or(0, |e| e.runs),
        daily_runs: settings.daily_runs_per_repo,
        tokens: usage.total_tokens(),
        tokens_known,
        daily_tokens: settings.daily_tokens_per_repo,
        known_cost_usd: entry.as_ref().map_or(0.0, |e| e.known_cost_usd),
        cost_known,
        may_run: verdict.allows_run(),
        reason: verdict.reason().map(str::to_owned),
    })
}

/// Render whichever the caller asked for.
pub fn render<T: Serialize>(report: &T, human: String, json: bool) -> Result<String, InspectError> {
    if json {
        return serde_json::to_string_pretty(report)
            .map_err(|source| InspectError::Unrenderable { source });
    }
    Ok(human)
}

/// Whether the verdict is one an operator should look at.
pub const fn needs_attention(verdict: &BudgetVerdict) -> bool {
    !verdict.allows_run()
}
