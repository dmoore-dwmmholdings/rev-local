//! Manual review and resumable backfill (RL-1007, SPEC §7.4).
//!
//! # Backfill is the one operation that can starve everything else
//!
//! A repository with four years of history is tens of thousands of commits. Every
//! other trigger source produces work at roughly the rate a human produces it;
//! backfill produces all of it at once. If backfill work sat in the same queue as
//! live work, the commit somebody just pushed would be reviewed after the other
//! twenty thousand — which is the same as not reviewing it.
//!
//! §7.4's answer is that backfill is enqueued *behind* live work. Not throttled,
//! not rate-limited: **strictly** behind. A single live trigger outranks the entire
//! backlog, and it does so at every decision point rather than at a periodic check,
//! because a fairness rule that only applies sometimes is a fairness rule that
//! fails under exactly the load it exists for.
//!
//! # Resumable means the cursor advances per item, not per run
//!
//! §7.4 gives backfill a distinct `backfill:` cursor, separate from the discovery
//! cursor, because the two move in opposite directions: discovery advances toward
//! HEAD, backfill walks away from it. Sharing one would make a backfill rewind
//! live discovery and re-review everything.
//!
//! The cursor advances after each item is *recorded*, never in a batch at the end.
//! A backfill interrupted after nine thousand of ten thousand items must resume at
//! nine thousand — and Ctrl-C during a long backfill is the expected way to stop
//! one, not an exceptional case.
//!
//! # `--dry-run` must cost nothing
//!
//! The reason to dry-run a backfill is to find out what it would cost before
//! spending it. A dry run that spends engine tokens to tell you what it would
//! spend is worse than useless, so enumeration and execution are separate
//! functions and the dry-run path cannot reach an engine — it does not take one.

use revlocal_core::{RepoId, Timestamp, TriggerSource};

use crate::budgets::BudgetVerdict;

/// The cursor scope §7.4 requires, distinct from discovery's.
pub fn backfill_scope(stream: &str) -> String {
    format!("backfill:{stream}")
}

/// Where a backfill starts (§7.4's `--since`).
///
/// Kept as the user's own words rather than resolved here: a date, a sha and a
/// revision number are interpreted by the adapter that knows the repository's
/// kind, and guessing between them in the CLI is how `--since 12345` becomes a
/// date on one repo and a revision on another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Since(pub String);

/// One historical change a backfill would review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackfillItem {
    /// The change's identity in its own system.
    pub external_id: String,
    /// A one-line description for the dry-run listing.
    pub summary: String,
}

/// What a backfill would do, without doing it (§7.4's `--dry-run`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackfillPlan {
    /// Which repository.
    pub repo_id: RepoId,
    /// The cursor scope this backfill advances.
    pub scope: String,
    /// Where it resumed from, if it did.
    pub resumed_from: Option<String>,
    /// The items it would review, in order.
    pub items: Vec<BackfillItem>,
    /// How many candidates `--limit` excluded.
    ///
    /// §18: "showing 50 of 3,000" and "there are 50" are different statements, and
    /// a plan that reported only the first would let somebody conclude their
    /// history was smaller than it is.
    pub excluded_by_limit: usize,
}

impl BackfillPlan {
    /// How many changes would be reviewed.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether there is nothing to do.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The lines a dry run prints.
    pub fn summary_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        match &self.resumed_from {
            Some(cursor) => lines.push(format!(
                "resuming {} after {cursor}: {} change(s) to review",
                self.scope,
                self.items.len()
            )),
            None => lines.push(format!(
                "{}: {} change(s) to review",
                self.scope,
                self.items.len()
            )),
        }
        if self.excluded_by_limit > 0 {
            lines.push(format!(
                "  {} more match --since and were excluded by --limit",
                self.excluded_by_limit
            ));
        }
        for item in &self.items {
            lines.push(format!("  {} {}", item.external_id, item.summary));
        }
        lines
    }
}

/// Enumerate what a backfill would review (§7.4).
///
/// Takes no engine and cannot reach one. `--dry-run` exists to find out what a
/// backfill would cost *before* spending it, and a dry run that spent tokens to
/// answer that would be worse than useless.
///
/// `candidates` is what the adapter found for `--since`, oldest first. `resume`
/// is the `backfill:` cursor's current value, if it has one.
pub fn plan(
    repo_id: RepoId,
    scope: &str,
    candidates: &[BackfillItem],
    resume: Option<&str>,
    limit: Option<usize>,
) -> BackfillPlan {
    // Resume means "everything after this one", so the cursor's own item is
    // excluded — it was already reviewed. Including it would re-review one change
    // on every resume, which over enough interruptions is a lot of duplicate work
    // and a lot of duplicate findings.
    let remaining: Vec<BackfillItem> = match resume {
        Some(cursor) => match candidates
            .iter()
            .position(|item| item.external_id == cursor)
        {
            Some(index) => candidates[index + 1..].to_vec(),
            // A cursor naming something not in the candidate list. History was
            // rewritten, or `--since` moved. Starting over is the safe reading:
            // re-reviewing is wasteful, skipping is wrong.
            None => candidates.to_vec(),
        },
        None => candidates.to_vec(),
    };

    let (items, excluded_by_limit) = match limit {
        Some(limit) if remaining.len() > limit => {
            (remaining[..limit].to_vec(), remaining.len() - limit)
        }
        _ => (remaining, 0),
    };

    BackfillPlan {
        repo_id,
        scope: scope.to_owned(),
        resumed_from: resume.map(str::to_owned),
        items,
        excluded_by_limit,
    }
}

/// Why a backfill step did not run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Yielded {
    /// Live work is waiting. Backfill goes behind it, always.
    LiveWorkPending,
    /// The repository's budget is spent.
    BudgetExhausted {
        /// What to record on the item.
        reason: String,
    },
}

/// What a backfill step decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Review this item, then advance the cursor to it.
    Review(BackfillItem),
    /// Stand aside. The plan is not abandoned; it resumes when the reason clears.
    Yield(Yielded),
    /// Nothing left.
    Done,
}

/// Drives one backfill, yielding to live work at every step.
#[derive(Debug)]
pub struct Backfill {
    plan: BackfillPlan,
    position: usize,
    /// The last item recorded, which is what the cursor holds.
    last_recorded: Option<String>,
}

impl Backfill {
    /// Start from a plan.
    pub fn new(plan: BackfillPlan) -> Self {
        let last_recorded = plan.resumed_from.clone();
        Self {
            plan,
            position: 0,
            last_recorded,
        }
    }

    /// Decide what to do next.
    ///
    /// `live_pending` is asked at **every** step rather than once at the start. A
    /// backfill of twenty thousand commits takes hours, and a fairness check that
    /// only ran at the beginning would let the whole backlog run ahead of a commit
    /// pushed a minute later.
    pub fn next_step(&self, live_pending: bool, budget: &BudgetVerdict) -> Step {
        if live_pending {
            return Step::Yield(Yielded::LiveWorkPending);
        }
        if !budget.allows_run() {
            return Step::Yield(Yielded::BudgetExhausted {
                reason: budget
                    .reason()
                    .unwrap_or("the repository's budget is spent")
                    .to_owned(),
            });
        }
        match self.plan.items.get(self.position) {
            Some(item) => Step::Review(item.clone()),
            None => Step::Done,
        }
    }

    /// Record that an item was reviewed, advancing the cursor to it.
    ///
    /// Per item, never in a batch at the end. A backfill interrupted after nine
    /// thousand of ten thousand must resume at nine thousand, and Ctrl-C during a
    /// long backfill is the expected way to stop one.
    pub fn recorded(&mut self, item: &BackfillItem) {
        self.position = self.position.saturating_add(1);
        self.last_recorded = Some(item.external_id.clone());
    }

    /// The value the `backfill:` cursor should hold right now.
    pub fn cursor_value(&self) -> Option<&str> {
        self.last_recorded.as_deref()
    }

    /// How many items are left.
    pub fn remaining(&self) -> usize {
        self.plan.items.len().saturating_sub(self.position)
    }

    /// How many have been recorded in this run.
    pub fn completed(&self) -> usize {
        self.position
    }

    /// The plan being executed.
    pub const fn plan(&self) -> &BackfillPlan {
        &self.plan
    }
}

/// The trigger source a backfill's runs are recorded under (§7.4, §5's CHECK).
pub const BACKFILL_TRIGGER: TriggerSource = TriggerSource::Backfill;

/// The trigger source a manual review is recorded under.
pub const MANUAL_TRIGGER: TriggerSource = TriggerSource::Manual;

/// A manual review request (§7.4's `revlocal review --rev`).
///
/// Manual is the one source that does **not** go through the trigger bus. §7 has
/// triggers schedule discovery, and discovery decides what changed; a human naming
/// a revision has already decided. Sending it through the bus would coalesce it
/// with whatever else was in flight and review something else instead — the one
/// case where coalescing is wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualReview {
    /// Which repository.
    pub repo_id: RepoId,
    /// The revision the user named, verbatim.
    pub rev: String,
    /// When it was asked for.
    pub requested_at: Timestamp,
}

impl ManualReview {
    /// A request for one change.
    pub fn new(repo_id: RepoId, rev: &str, requested_at: Timestamp) -> Self {
        Self {
            repo_id,
            rev: rev.to_owned(),
            requested_at,
        }
    }

    /// Whether a manual review yields to anything.
    ///
    /// It does not. A human is waiting for this one, which is the difference
    /// between it and every other source.
    pub const fn yields_to_live_work(&self) -> bool {
        false
    }
}
