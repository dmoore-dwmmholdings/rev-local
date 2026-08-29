//! The daemon's decision loop (RL-1201, SPEC §4.2, §4.3, §7, §12.1, §13.1).
//!
//! # The gap this closes
//!
//! M8 through M12 built the parts and nothing ran them. `gate` was never invoked,
//! no target was constructed from config, `RunSlots` bounded nothing, and the kill
//! switch was engaged by no caller. Each piece had tests; the composition had
//! none, which is the shape of assembly that looks finished and does nothing.
//!
//! # A decision function, not a loop
//!
//! [`Scheduler::tick`] takes the world's state and returns what to do about it. It
//! does not sleep, spawn, or touch a database — which is what lets the ordering
//! rules below be asserted directly instead of inferred from timing.
//!
//! `revlocal watch` is then a thin wrapper: read state, call `tick`, act, repeat.
//! The loop is the easy part; the ordering is not.
//!
//! # The ordering is the whole design
//!
//! Four things can stop work starting, and they are checked in an order that is
//! not arbitrary:
//!
//! 1. **The kill switch** (§12.1) — a human said stop. Nothing outranks it, and
//!    checking it second would mean a budget error surfacing when somebody has
//!    just hit the emergency control.
//! 2. **Concurrency** (§4.3) — a slot is a physical limit; without one there is
//!    nowhere for work to go, and asking about budgets first would burn a budget
//!    check on work that cannot start.
//! 3. **Budget** (§13.1) — this is where "cannot tell" matters. An unmeasured day
//!    pauses rather than proceeding (D10, RL-409).
//! 4. **Precedence** (§7.4) — live work before backfill, always.
//!
//! Reordering any pair changes what an operator is told when two things are true
//! at once, and being told the wrong reason is worse than being told nothing.

use revlocal_core::RepoId;

use crate::backfill::Yielded;
use crate::budgets::BudgetVerdict;
use crate::triggers::DiscoveryPass;

/// Why the scheduler is not starting work right now.
///
/// Named rather than collapsed to a boolean because §18's rule applies most where
/// a system goes quiet: "rev-local is doing nothing" has five different meanings
/// and only one of them is "there is nothing to do".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Idle {
    /// Nothing is waiting.
    NothingToDo,
    /// A human engaged the kill switch (§12.1).
    Killed,
    /// Every run slot is occupied (§4.3).
    AtCapacity {
        /// How many are in flight.
        running: usize,
        /// The configured ceiling.
        limit: usize,
    },
    /// A budget stopped it (§13.1).
    Budget {
        /// Which repository.
        repo_id: RepoId,
        /// What to record and show.
        reason: String,
    },
}

impl Idle {
    /// A line for `revlocal watch` and the UI.
    ///
    /// Every variant says something. A daemon that prints nothing while doing
    /// nothing is indistinguishable from a daemon that has crashed.
    pub fn summary_line(&self) -> String {
        match self {
            Self::NothingToDo => "idle: no repository has work waiting".to_owned(),
            Self::Killed => {
                "stopped: the kill switch is engaged; `revlocal resume` releases it".to_owned()
            }
            Self::AtCapacity { running, limit } => format!(
                "waiting: {running} of {limit} run slots in use (§13.1 \
                 max_concurrent_runs)"
            ),
            Self::Budget { repo_id, reason } => {
                format!("holding repo {}: {reason}", repo_id.get())
            }
        }
    }

    /// Whether this is a state somebody should look at.
    ///
    /// Capacity and an empty queue are the system working. A kill switch and a
    /// budget are the system *not* doing what it was asked to.
    pub const fn needs_attention(&self) -> bool {
        matches!(self, Self::Killed | Self::Budget { .. })
    }
}

/// What the scheduler decided to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Run this discovery pass.
    Discover(DiscoveryPass),
    /// Review the next backfill item for this repository.
    Backfill {
        /// Which repository.
        repo_id: RepoId,
    },
    /// Do nothing, for a stated reason.
    Idle(Idle),
}

impl Decision {
    /// Whether this starts work.
    pub const fn starts_work(&self) -> bool {
        matches!(self, Self::Discover(_) | Self::Backfill { .. })
    }
}

/// Everything `tick` needs to know, gathered by the caller.
///
/// A struct rather than eight arguments: the order of the checks is the design,
/// and eight positional booleans is how a caller silently swaps two of them.
#[derive(Debug, Clone)]
pub struct WorldState {
    /// Whether a human engaged the kill switch (§12.1).
    pub killed: bool,
    /// Runs currently executing (§4.3).
    pub running: usize,
    /// The configured ceiling.
    pub slot_limit: usize,
    /// Discovery passes whose coalescing window has closed (§7).
    pub due: Vec<DiscoveryPass>,
    /// Repositories with backfill work waiting (§7.4).
    pub backfill_waiting: Vec<RepoId>,
    /// The budget verdict per repository, for those with work.
    pub budgets: std::collections::BTreeMap<RepoId, BudgetVerdict>,
}

impl WorldState {
    /// An idle world with capacity.
    pub fn idle(slot_limit: usize) -> Self {
        Self {
            killed: false,
            running: 0,
            slot_limit,
            due: Vec::new(),
            backfill_waiting: Vec::new(),
            budgets: std::collections::BTreeMap::new(),
        }
    }

    /// Whether any live work is waiting — what backfill yields to (§7.4).
    pub fn has_live_work(&self) -> bool {
        !self.due.is_empty()
    }
}

/// Decides what the daemon does next.
#[derive(Debug, Default)]
pub struct Scheduler;

impl Scheduler {
    /// Decide one step.
    ///
    /// See the module docs for why the checks are in this order.
    pub fn tick(&self, world: &WorldState) -> Decision {
        // 1. §12.1. A human said stop, and nothing outranks that. Checked before
        //    capacity so an operator who just hit the switch is told about the
        //    switch rather than about a queue.
        if world.killed {
            return Decision::Idle(Idle::Killed);
        }

        // 2. §4.3. A slot is a physical limit; without one there is nowhere for
        //    work to go, and a budget check on work that cannot start is a budget
        //    check nobody needed.
        if world.running >= world.slot_limit.max(1) {
            return Decision::Idle(Idle::AtCapacity {
                running: world.running,
                limit: world.slot_limit.max(1),
            });
        }

        // 3. Live work first (§7.4), and only then its budget — so the reason
        //    reported is about the work that would actually have run.
        if let Some(pass) = world.due.first() {
            if let Some(idle) = budget_block(world, pass.repo_id) {
                return Decision::Idle(idle);
            }
            return Decision::Discover(pass.clone());
        }

        // 4. Backfill, strictly behind live work — which the branch above already
        //    guarantees by returning before this is reached.
        if let Some(repo_id) = world.backfill_waiting.first().copied() {
            if let Some(idle) = budget_block(world, repo_id) {
                return Decision::Idle(idle);
            }
            return Decision::Backfill { repo_id };
        }

        Decision::Idle(Idle::NothingToDo)
    }

    /// Why a backfill would stand aside, in [`Yielded`]'s terms.
    ///
    /// The same question RL-1007 answers, asked from the scheduler's side so the
    /// two cannot disagree about what "yield" means.
    pub fn backfill_yield(&self, world: &WorldState, repo_id: RepoId) -> Option<Yielded> {
        if world.has_live_work() {
            return Some(Yielded::LiveWorkPending);
        }
        let verdict = world.budgets.get(&repo_id)?;
        (!verdict.allows_run()).then(|| Yielded::BudgetExhausted {
            reason: verdict
                .reason()
                .unwrap_or("the repository's budget is spent")
                .to_owned(),
        })
    }
}

/// Whether this repository's budget stops work, and what to say about it.
///
/// A repository with no verdict is one nobody has checked, which is not the same
/// as one that passed. Absence means proceed — the ledger is consulted per run
/// anyway — and this exists so that reading is deliberate rather than incidental.
fn budget_block(world: &WorldState, repo_id: RepoId) -> Option<Idle> {
    let verdict = world.budgets.get(&repo_id)?;
    (!verdict.allows_run()).then(|| Idle::Budget {
        repo_id,
        reason: verdict
            .reason()
            .unwrap_or("the repository's budget is spent")
            .to_owned(),
    })
}
