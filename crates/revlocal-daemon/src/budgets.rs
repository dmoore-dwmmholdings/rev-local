//! Budgets and concurrency caps (RL-805, SPEC §13.1, §4.3).
//!
//! # Exhausting a budget never drops a change
//!
//! §13.1's `on_exhausted` has three values and none of them is "forget about it".
//! `pause` stops reviewing, `queue` holds the work, `skip` records the change with
//! a reason — and the third is the one that looks like a drop and is not. A
//! skipped change has a row saying it was skipped and why, which is what makes
//! tomorrow's operator able to answer "did rev-local look at this?".
//!
//! §18 states the general rule; a budget is the case where it is most tempting to
//! break, because a silent drop under budget pressure looks exactly like a quiet
//! system working normally.
//!
//! # A day that reported no cost is not a cheap day
//!
//! Decision D10 already shapes the ledger: a day with even one unpriced run
//! reports `cost_usd: None` rather than a total that happens to exclude it. The
//! budget check inherits that. An unmeasured day cannot be compared against a cost
//! ceiling, so it is reported as unmeasurable rather than as under budget — the
//! alternative is a cost budget that silently stops enforcing the moment an engine
//! stops reporting prices.

use revlocal_core::{BudgetLedgerEntry, BudgetSettings, OnExhausted, Timestamp};

/// The calendar day a timestamp falls in, as the ledger keys it.
pub fn day_of(at: Timestamp) -> String {
    at.format("%Y-%m-%d").to_string()
}

/// Which allowance ran out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exhausted {
    /// Daily token allowance.
    Tokens,
    /// Daily run allowance.
    Runs,
    /// Daily cost allowance.
    Cost,
}

impl Exhausted {
    /// How this reads in a skip reason and in the UI.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tokens => "daily token budget",
            Self::Runs => "daily run budget",
            Self::Cost => "daily cost budget",
        }
    }
}

/// What the budget check concluded.
///
/// `PartialEq` but not `Eq`: one variant carries dollars, and `f64` has no total
/// equality. Deriving `Eq` would mean rounding money to make a comparison work,
/// which is a worse trade than not having the trait.
#[derive(Debug, Clone, PartialEq)]
pub enum BudgetVerdict {
    /// There is room; proceed.
    WithinBudget,
    /// Out of allowance. What happens next is `on_exhausted`'s to say.
    Exhausted {
        /// Which allowance.
        which: Exhausted,
        /// What §13.1 says to do about it.
        action: OnExhausted,
        /// A sentence for the run row, the UI and the log.
        reason: String,
    },
    /// A cost ceiling is configured and the day's cost cannot be measured.
    ///
    /// Deliberately not `WithinBudget`. See the module docs: an unmeasured day
    /// read as a cheap one is a cost budget that stops enforcing exactly when an
    /// engine stops reporting prices.
    CostUnmeasurable {
        /// What is known to have been spent, as a lower bound.
        known_cost_usd: f64,
        /// The ceiling it could not be compared against.
        ceiling_usd: f64,
        /// A sentence explaining it.
        reason: String,
    },
}

impl BudgetVerdict {
    /// Whether a run may start.
    pub const fn allows_run(&self) -> bool {
        matches!(self, Self::WithinBudget)
    }

    /// The reason to record on a skipped or held change, if any.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::WithinBudget => None,
            Self::Exhausted { reason, .. } | Self::CostUnmeasurable { reason, .. } => Some(reason),
        }
    }
}

/// Check one repo's spend for one day against its budget.
///
/// `spent` is `None` for a day with no ledger row yet, which is not zero-spend
/// pedantry — it is the common case on the first run of the day and has to mean
/// "nothing spent" rather than "no data".
pub fn check(spent: Option<&BudgetLedgerEntry>, budget: &BudgetSettings) -> BudgetVerdict {
    let runs = spent.map_or(0, |entry| entry.runs);
    let tokens = spent.map_or(0, |entry| entry.usage.tokens_in + entry.usage.tokens_out);

    if budget.daily_runs_per_repo > 0 && runs >= budget.daily_runs_per_repo {
        return BudgetVerdict::Exhausted {
            which: Exhausted::Runs,
            action: budget.on_exhausted,
            reason: format!(
                "{} reached: {runs} of {} runs today",
                Exhausted::Runs.as_str(),
                budget.daily_runs_per_repo
            ),
        };
    }

    if budget.daily_tokens_per_repo > 0 && tokens >= budget.daily_tokens_per_repo {
        return BudgetVerdict::Exhausted {
            which: Exhausted::Tokens,
            action: budget.on_exhausted,
            reason: format!(
                "{} reached: {tokens} of {} tokens today",
                Exhausted::Tokens.as_str(),
                budget.daily_tokens_per_repo
            ),
        };
    }

    // §13.1: 0 means unlimited, so a cost ceiling only exists when one was set.
    if !budget.cost_is_unlimited() {
        let ceiling = budget.daily_cost_usd_per_repo;
        match spent.map(|entry| (entry.usage.cost_usd, entry.known_cost_usd)) {
            Some((Some(cost), _)) if cost >= ceiling => {
                return BudgetVerdict::Exhausted {
                    which: Exhausted::Cost,
                    action: budget.on_exhausted,
                    reason: format!(
                        "{} reached: ${cost:.2} of ${ceiling:.2} today",
                        Exhausted::Cost.as_str()
                    ),
                };
            }
            // A day with an unpriced run in it. Even the *known* portion may
            // already be over, which is worth saying separately from "we cannot
            // tell" — but either way this is not a green light.
            Some((None, known)) => {
                return BudgetVerdict::CostUnmeasurable {
                    known_cost_usd: known,
                    ceiling_usd: ceiling,
                    reason: format!(
                        "today's cost cannot be measured: at least ${known:.2} of \
                         ${ceiling:.2} is known, and at least one run reported no \
                         price (decision D10)"
                    ),
                };
            }
            _ => {}
        }
    }

    BudgetVerdict::WithinBudget
}

/// Whether a change detected under this verdict is still recorded.
///
/// Always. The three `on_exhausted` values differ in what happens to the *review*,
/// never in whether the change is written down — §18's rule, and the reason
/// `skip` is a status with a reason rather than an early return.
pub const fn records_the_change(_verdict: &BudgetVerdict) -> bool {
    true
}

/// Whether a change should be reviewed once budget returns.
///
/// `pause` and `queue` both come back; `skip` does not. A skipped change was
/// recorded with a reason and a decision was made about it — re-reviewing it
/// tomorrow would contradict the row that says it was skipped.
pub const fn resumes_after_reset(action: OnExhausted) -> bool {
    matches!(action, OnExhausted::Pause | OnExhausted::Queue)
}

/// The audit event for a budget stopping work.
pub const AUDIT_KIND_BUDGET_EXHAUSTED: &str = "budget_exhausted";

/// The audit detail for it.
pub fn exhausted_detail(repo: &str, day: &str, verdict: &BudgetVerdict) -> serde_json::Value {
    serde_json::json!({
        "repo": repo,
        "day": day,
        "reason": verdict.reason(),
        "resumes_after_reset": match verdict {
            BudgetVerdict::Exhausted { action, .. } => resumes_after_reset(*action),
            // Not a budget decision, so nothing to resume from — it clears when a
            // priced run lands or the day rolls over.
            _ => true,
        },
    })
}

// --- concurrency (SPEC §4.3, §13.1's `max_concurrent_runs`) ---------------

use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// SPEC §13.1's default.
pub const DEFAULT_MAX_CONCURRENT_RUNS: usize = 2;

/// The global cap on runs in flight.
///
/// A semaphore rather than a counter that callers check: a check-then-act pair has
/// a window between the two, and under exactly the load the cap exists for, that
/// window is when it is widest. Holding a permit for the duration of the run makes
/// the cap a property of the type rather than of everyone remembering to ask.
#[derive(Debug, Clone)]
pub struct RunSlots {
    permits: Arc<Semaphore>,
    limit: usize,
}

impl RunSlots {
    /// A cap of `limit` concurrent runs. Zero is treated as one — a cap of zero
    /// would mean nothing ever runs, which is a configuration mistake rather than
    /// an instruction.
    pub fn new(limit: usize) -> Self {
        let limit = limit.max(1);
        Self {
            permits: Arc::new(Semaphore::new(limit)),
            limit,
        }
    }

    /// The configured cap.
    pub const fn limit(&self) -> usize {
        self.limit
    }

    /// How many runs could start right now without waiting.
    pub fn available(&self) -> usize {
        self.permits.available_permits()
    }

    /// Wait for a slot. The run holds it until the permit is dropped.
    ///
    /// Returns `None` only if the semaphore has been closed, which nothing here
    /// does — the `Option` exists so a caller cannot be handed a permit that was
    /// never granted.
    pub async fn acquire(&self) -> Option<OwnedSemaphorePermit> {
        Arc::clone(&self.permits).acquire_owned().await.ok()
    }
}

impl Default for RunSlots {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_CONCURRENT_RUNS)
    }
}
