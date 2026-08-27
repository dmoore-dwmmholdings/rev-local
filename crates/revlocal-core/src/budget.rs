//! The per-repo daily budget decision (SPEC §12, decision D10).
//!
//! [`check`] is pure: it takes the limits and a day's accumulated spend and says
//! whether a run may start. The ledger arithmetic lives in `revlocal-store`; this
//! is only the judgement.
//!
//! D10: exhaustion **pauses, queues or skips — it never silently drops**. The
//! return type reflects that. There is no `false`-shaped answer that a caller
//! could read as "carry on quietly"; an exhausted budget names which limit was
//! hit and what the repo asked to happen about it.

use crate::{BudgetLedgerEntry, OnExhausted};
use serde::{Deserialize, Serialize};

/// One repo's daily allowances (SPEC §13.1, `[budgets]`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BudgetLimits {
    /// Runs per day. `0` means unlimited.
    pub daily_runs: u32,
    /// Total tokens per day. `0` means unlimited.
    pub daily_tokens: u64,
    /// USD per day. `0` means unlimited (SPEC §13.1).
    pub daily_cost_usd: f64,
    /// What to do when an allowance runs out.
    pub on_exhausted: OnExhausted,
}

impl BudgetLimits {
    /// Whether a cost ceiling is actually configured.
    ///
    /// SPEC §13.1 spells "no cost limit" as `0`. This matters more than it looks:
    /// with no cost limit, an unmeasured cost is simply irrelevant, and blocking
    /// on it would stop runs for a reason the operator never asked about.
    pub fn has_cost_limit(&self) -> bool {
        self.daily_cost_usd > 0.0
    }
}

string_enum! {
    /// Which allowance stopped a run, or why it could not be checked.
    pub enum ExhaustedLimit {
        /// The daily run count.
        Runs => "runs",
        /// The daily token allowance.
        Tokens => "tokens",
        /// The daily cost allowance.
        Cost => "cost",
        /// A cost limit is configured, but the day's cost is not fully known.
        ///
        /// Distinct from [`Cost`](Self::Cost) because the operator response
        /// differs: a real overspend means the budget worked, while this means an
        /// engine reported no price and the budget cannot be enforced at all.
        CostUnknown => "cost_unknown",
    }
}

/// Whether a run may start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BudgetDecision {
    /// Within every configured allowance.
    Proceed,
    /// An allowance is spent. Carries what to do and why, so nothing is dropped
    /// without a recorded reason (SPEC §18).
    Exhausted {
        /// Which allowance.
        limit: ExhaustedLimit,
        /// What the repo configured for this case.
        action: OnExhausted,
        /// Human-readable, for the audit log and the UI.
        reason: String,
    },
}

impl BudgetDecision {
    /// Whether a run may start now.
    pub const fn may_run(&self) -> bool {
        matches!(self, Self::Proceed)
    }

    /// The reason a run may not start, if it may not.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Proceed => None,
            Self::Exhausted { reason, .. } => Some(reason),
        }
    }
}

/// Decide whether a run may start against `limits`, given `today`'s spend.
///
/// `today` is `None` when nothing has been spent yet, which always proceeds.
///
/// Checked in order runs → tokens → cost, so the reported limit is the cheapest
/// one to explain. Every limit of `0` is unlimited (SPEC §13.1).
///
/// The cost check has three outcomes rather than two, per ADR 0010: over,
/// under, or **not knowable** because some run that day reported no price. Only
/// the last needs comment — a configured cost ceiling that cannot be evaluated is
/// treated as exhausted, because the alternative is running an unbounded number
/// of unpriced reviews against a budget the operator believed was enforced.
pub fn check(limits: &BudgetLimits, today: Option<&BudgetLedgerEntry>) -> BudgetDecision {
    let Some(today) = today else {
        return BudgetDecision::Proceed;
    };

    if limits.daily_runs > 0 && today.runs_exhausted(limits.daily_runs) {
        return BudgetDecision::Exhausted {
            limit: ExhaustedLimit::Runs,
            action: limits.on_exhausted,
            reason: format!(
                "{} of {} daily runs used for {}",
                today.runs, limits.daily_runs, today.day
            ),
        };
    }

    if limits.daily_tokens > 0 && today.tokens_exhausted(limits.daily_tokens) {
        return BudgetDecision::Exhausted {
            limit: ExhaustedLimit::Tokens,
            action: limits.on_exhausted,
            reason: format!(
                "{} of {} daily tokens used for {}",
                today.usage.total_tokens(),
                limits.daily_tokens,
                today.day
            ),
        };
    }

    if limits.has_cost_limit() {
        match today.cost_exhausted(limits.daily_cost_usd) {
            Some(true) => {
                return BudgetDecision::Exhausted {
                    limit: ExhaustedLimit::Cost,
                    action: limits.on_exhausted,
                    reason: format!(
                        "${:.2} of ${:.2} daily cost used for {}",
                        today.known_cost_usd, limits.daily_cost_usd, today.day
                    ),
                };
            }
            Some(false) => {}
            None => {
                return BudgetDecision::Exhausted {
                    limit: ExhaustedLimit::CostUnknown,
                    action: limits.on_exhausted,
                    reason: format!(
                        "cost limit ${:.2} cannot be enforced for {}: at least one run \
                         reported no price, so the ${:.2} recorded is a lower bound",
                        limits.daily_cost_usd, today.day, today.known_cost_usd
                    ),
                };
            }
        }
    }

    BudgetDecision::Proceed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RepoId, Usage};

    fn limits() -> BudgetLimits {
        BudgetLimits {
            daily_runs: 10,
            daily_tokens: 1_000,
            daily_cost_usd: 5.0,
            on_exhausted: OnExhausted::Pause,
        }
    }

    fn spent(runs: u32, tokens: u64, cost: Option<f64>) -> BudgetLedgerEntry {
        BudgetLedgerEntry {
            repo_id: RepoId::new(1),
            day: "2026-08-27".to_owned(),
            runs,
            usage: Usage {
                tokens_in: tokens,
                tokens_out: 0,
                cost_usd: cost,
            },
            known_cost_usd: cost.unwrap_or(0.0),
        }
    }

    #[test]
    fn a_day_with_nothing_spent_proceeds() {
        assert_eq!(check(&limits(), None), BudgetDecision::Proceed);
    }

    #[test]
    fn spend_within_every_allowance_proceeds() {
        let decision = check(&limits(), Some(&spent(1, 100, Some(0.5))));
        assert!(decision.may_run());
        assert_eq!(decision.reason(), None);
    }

    #[test]
    fn each_allowance_stops_a_run_and_says_which() {
        for (entry, expected) in [
            (spent(10, 0, Some(0.0)), ExhaustedLimit::Runs),
            (spent(0, 1_000, Some(0.0)), ExhaustedLimit::Tokens),
            (spent(0, 0, Some(5.0)), ExhaustedLimit::Cost),
        ] {
            match check(&limits(), Some(&entry)) {
                BudgetDecision::Exhausted { limit, reason, .. } => {
                    assert_eq!(limit, expected, "reason was {reason}");
                    assert!(!reason.is_empty(), "an exhausted budget must say why");
                }
                BudgetDecision::Proceed => panic!("{expected} should have been exhausted"),
            }
        }
    }

    #[test]
    fn a_limit_of_zero_is_unlimited() {
        // SPEC §13.1 spells "no limit" as 0. Reading it as "zero allowed" would
        // stop every run on a repo with a default config.
        let unlimited = BudgetLimits {
            daily_runs: 0,
            daily_tokens: 0,
            daily_cost_usd: 0.0,
            on_exhausted: OnExhausted::Pause,
        };
        assert!(check(&unlimited, Some(&spent(9_999, 9_999_999, Some(1_000.0)))).may_run());
    }

    #[test]
    fn an_unmeasured_cost_stops_a_run_when_a_cost_limit_is_set() {
        // ADR 0010. The alternative is running an unbounded number of unpriced
        // reviews against a budget the operator believed was enforced.
        let decision = check(&limits(), Some(&spent(1, 10, None)));
        match decision {
            BudgetDecision::Exhausted { limit, reason, .. } => {
                assert_eq!(limit, ExhaustedLimit::CostUnknown);
                assert!(reason.contains("no price"), "{reason}");
                assert!(reason.contains("lower bound"), "{reason}");
            }
            BudgetDecision::Proceed => panic!("an unenforceable cost limit must not proceed"),
        }
    }

    #[test]
    fn an_unmeasured_cost_is_irrelevant_when_no_cost_limit_is_set() {
        // The other half, and the one that keeps this from being useless: the mock
        // engine never reports a price, so every inner-loop day is incomplete. With
        // no cost ceiling configured, that must not stop anything.
        let no_cost_limit = BudgetLimits {
            daily_cost_usd: 0.0,
            ..limits()
        };
        assert!(check(&no_cost_limit, Some(&spent(1, 10, None))).may_run());
    }

    #[test]
    fn known_overspend_beats_incomplete_measurement() {
        // If the costs we DO know about already passed the limit, the answer is
        // "over", not "cannot tell" — the unknown remainder only makes it more so.
        let mut entry = spent(1, 10, None);
        entry.known_cost_usd = 99.0;
        match check(&limits(), Some(&entry)) {
            BudgetDecision::Exhausted { limit, .. } => assert_eq!(limit, ExhaustedLimit::Cost),
            BudgetDecision::Proceed => panic!("known overspend must stop the run"),
        }
    }

    #[test]
    fn the_repos_configured_action_travels_with_the_decision() {
        // D10: exhaustion pauses, queues or skips — the caller must not have to
        // look the policy up separately and risk defaulting it.
        for action in OnExhausted::ALL {
            let limits = BudgetLimits {
                on_exhausted: *action,
                ..limits()
            };
            match check(&limits, Some(&spent(10, 0, Some(0.0)))) {
                BudgetDecision::Exhausted {
                    action: carried, ..
                } => {
                    assert_eq!(carried, *action);
                }
                BudgetDecision::Proceed => panic!("should be exhausted"),
            }
        }
    }

    #[test]
    fn there_is_no_decision_that_silently_drops_a_change() {
        // The type has two shapes and one of them always carries a reason, so a
        // caller cannot end up dropping a change with nothing recorded (§18).
        let decision = check(&limits(), Some(&spent(10, 0, Some(0.0))));
        assert!(!decision.may_run());
        assert!(decision.reason().is_some_and(|r| !r.is_empty()));
    }

    #[test]
    fn the_cheapest_limit_to_explain_is_the_one_reported() {
        // Everything is over at once; runs is reported because it is the simplest
        // thing to tell a user.
        let entry = spent(99, 99_999, Some(99.0));
        match check(&limits(), Some(&entry)) {
            BudgetDecision::Exhausted { limit, .. } => assert_eq!(limit, ExhaustedLimit::Runs),
            BudgetDecision::Proceed => panic!("should be exhausted"),
        }
    }

    #[test]
    fn a_decision_round_trips_for_the_audit_log() {
        let decision = check(&limits(), Some(&spent(10, 0, Some(0.0))));
        let json = serde_json::to_string(&decision).unwrap_or_default();
        let back: BudgetDecision = serde_json::from_str(&json).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(back, decision);
    }
}
