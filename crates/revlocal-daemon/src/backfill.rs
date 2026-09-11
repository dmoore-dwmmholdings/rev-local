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

// --- the driver (REVL-209, §7.4) --------------------------------------------

/// What one backfill actually did.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BackfillOutcome {
    /// Reviews that finished, oldest first.
    pub reviewed: Vec<crate::executor::RunOutcome>,
    /// Items recorded this run — the cursor has advanced past each of them.
    pub completed: usize,
    /// Items still ahead of the cursor when this stopped.
    pub remaining: usize,
    /// Why it stopped before finishing, when it did.
    ///
    /// §18: standing aside for live work, running out of budget and finishing the
    /// plan are three different outcomes, and a command that reported the item
    /// count alone would make the first two look like the third.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped: Option<String>,
    /// Where the `backfill:` cursor now points.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// Reviews that did not go ahead, and why — never dropped in silence (§18).
    pub held: Vec<String>,
    /// Changes §9.4 said not to review, with the reason.
    ///
    /// Separate from `held`: a skipped change is a *decision* that was made and
    /// recorded, and a held one is work that still wants doing. Collapsing them
    /// would make "we deliberately ignored 400 merge commits" read like "400
    /// reviews failed".
    pub skipped: Vec<String>,
}

impl BackfillOutcome {
    /// Whether the plan was worked to its end.
    pub const fn finished_the_plan(&self) -> bool {
        self.stopped.is_none()
    }

    /// The lines a terminal prints after a backfill.
    pub fn summary_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for outcome in &self.reviewed {
            lines.push(format!(
                "  reviewed {} — {} ({} finding(s), {} action(s), {})",
                outcome.change,
                outcome.verdict.as_deref().unwrap_or("no verdict"),
                outcome.findings,
                outcome.actions,
                outcome.status
            ));
        }
        for skipped in &self.skipped {
            lines.push(format!("  skipped: {skipped}"));
        }
        for held in &self.held {
            lines.push(format!("  held: {held}"));
        }
        if let Some(stopped) = &self.stopped {
            lines.push(format!("  stopped: {stopped}"));
            // The resume instruction, next to the reason it is needed. A backfill
            // that stood aside for live work is *not* abandoned, and somebody who
            // is not told that will re-run it with a fresh `--since` and review
            // everything twice.
            lines.push(format!(
                "  {} item(s) left; run the same command again to continue from the cursor",
                self.remaining
            ));
        }
        lines
    }
}

/// Whether this repository has live work waiting.
///
/// "Live" is everything a human or a trigger produced — anything whose source is
/// not a backfill. §7.4 puts backfill strictly behind it, so this is asked before
/// **every** item rather than once at the start: a sweep of twenty thousand
/// commits takes hours, and a commit pushed during hour two must not wait for
/// hour six.
///
/// Runs already in flight count, not only queued ones. A run that is `reviewing`
/// is live work in progress, and a backfill that started an engine beside it
/// would put two engines on one repository — which `max_concurrent_runs` exists
/// to prevent and which this would route around.
pub async fn live_work_pending(
    pool: &revlocal_store::Pool,
    repo_id: RepoId,
) -> Result<bool, revlocal_store::StoreError> {
    use revlocal_core::RunStatus;

    let store = revlocal_store::RunStore::new(pool);
    for status in [
        RunStatus::Queued,
        RunStatus::Preparing,
        RunStatus::Reviewing,
        RunStatus::Synthesizing,
    ] {
        let runs = store.list_recent(Some(repo_id), Some(status), 200).await?;
        if runs.iter().any(|run| run.trigger != BACKFILL_TRIGGER) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Review a planned backfill, oldest first, yielding to live work at every step.
///
/// # Why this drives reviews rather than filling the ordinary queue
///
/// Recording the changes and letting the normal loop enqueue them is smaller —
/// `enqueue` already picks up changes with no run. It is also wrong: the executor
/// takes the **oldest** queued run first, so history would run *ahead* of live
/// work, which is precisely what §7.4 and [`Yielded::LiveWorkPending`] exist to
/// prevent. The cheaper design quietly inverts the guarantee, which is worse than
/// not having it, because the code would still contain a fairness check that
/// looks like it is holding.
///
/// So the sweep owns its own loop and asks [`Backfill::next_step`] between items.
///
/// # `changes` is what the adapter found, not what the plan says
///
/// A [`BackfillItem`] carries an id and a summary — enough to *list*, not enough
/// to review. The adapter's own [`DetectedChange`](revlocal_vcs::DetectedChange)
/// carries the branch, the refs, the parents and the paths that materialisation
/// and §9.4's skip rules need, so they are passed alongside and matched by
/// `external_id`. An item with no matching change is reported rather than
/// skipped: it means enumeration and planning disagree, and silently reviewing
/// fewer commits than were listed is the failure §18 is about.
///
/// # A backfill obeys the same skip rules as discovery
///
/// §9.4 is about the change, not about when it was found. A merge commit is a
/// merge commit whether it landed this morning or in 2019, and a sweep that
/// reviewed the ones discovery skips would spend real tokens producing findings
/// the live loop had already decided nobody wants. The skipped run is recorded
/// with its reason, exactly as discovery records it, so `--since` twice over the
/// same history does not re-decide anything.
#[allow(clippy::too_many_arguments)]
pub async fn execute(
    pool: &revlocal_store::Pool,
    config: &revlocal_core::GlobalConfig,
    sink: &dyn crate::state_machine::RunEventSink,
    data_dir: &std::path::Path,
    repo: &revlocal_core::Repo,
    plan: BackfillPlan,
    changes: &[revlocal_vcs::DetectedChange],
    at: Timestamp,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<BackfillOutcome, crate::executor::ExecutorError> {
    let scope = plan.scope.clone();
    let repo_config =
        serde_json::from_str::<revlocal_core::RepoConfig>(&repo.config_json).unwrap_or_default();
    let mut driver = Backfill::new(plan);
    let mut outcome = BackfillOutcome::default();

    loop {
        if cancel.is_cancelled() {
            // Ctrl-C during a long backfill is the expected way to stop one, not
            // an exceptional case. The cursor is already at the last recorded
            // item, so this is a pause rather than a loss.
            outcome.stopped = Some("cancelled; the cursor is at the last item reviewed".to_owned());
            break;
        }

        let live = live_work_pending(pool, repo.id).await.map_err(|source| {
            crate::executor::ExecutorError::Store {
                source: Box::new(source),
            }
        })?;
        let verdict = repo_budget(pool, config, repo.id, at).await?;

        match driver.next_step(live, &verdict) {
            Step::Done => break,
            Step::Yield(Yielded::LiveWorkPending) => {
                outcome.stopped =
                    Some("live work is waiting, and backfill goes behind it (§7.4)".to_owned());
                break;
            }
            Step::Yield(Yielded::BudgetExhausted { reason }) => {
                outcome.stopped = Some(reason);
                break;
            }
            Step::Review(item) => {
                let Some(change) = changes
                    .iter()
                    .find(|change| change.external_id == item.external_id)
                else {
                    // Reported, never skipped past: this means the plan and the
                    // enumeration disagree, and a sweep that quietly reviewed
                    // fewer commits than it listed would be indistinguishable
                    // from one that reviewed them all.
                    outcome.held.push(format!(
                        "{}: planned but not among the enumerated changes, so there is nothing to review",
                        item.external_id
                    ));
                    driver.recorded(&item);
                    continue;
                };

                let stored = record_change(pool, repo, change, at).await?;

                // §9.4, evaluated here for the same reason discovery evaluates it
                // there: the rules need the parents and paths that only the
                // adapter's row carries, and re-deriving the answer later from
                // less information is how two answers to one question start
                // disagreeing.
                if let Some(skip) = revlocal_vcs::skip_rules::evaluate(change, &repo_config) {
                    record_skipped(pool, repo, stored.id, &skip.detail, at).await?;
                    outcome
                        .skipped
                        .push(format!("{}: {}", item.external_id, skip.detail));
                    driver.recorded(&item);
                    advance(pool, repo, &scope, &driver, &mut outcome, at).await?;
                    continue;
                }

                let run =
                    crate::executor::enqueue_one(pool, repo, &stored, BACKFILL_TRIGGER, at).await?;

                match crate::executor::execute_run(pool, config, sink, data_dir, run.id, at, cancel)
                    .await?
                {
                    Ok(finished) => outcome.reviewed.push(finished),
                    // A refusal, not a crash: the checkout is gone, the kill
                    // switch went on, the repository was disabled. The run row
                    // carries the reason and so does this report.
                    Err(reason) => outcome.held.push(reason),
                }

                // Per item, after the attempt, never batched at the end. An
                // interrupt at nine thousand of ten thousand resumes at nine
                // thousand.
                //
                // Advanced even when the attempt was held or failed, because the
                // run row now exists and carries the outcome: the change is
                // *covered*. Not advancing would make one broken commit a wall
                // the sweep could never get past, re-reviewing it on every run
                // and never reaching the ten thousand behind it.
                driver.recorded(&item);
                advance(pool, repo, &scope, &driver, &mut outcome, at).await?;
            }
        }
    }

    outcome.completed = driver.completed();
    outcome.remaining = driver.remaining();
    Ok(outcome)
}

/// This repository's budget verdict right now (§13.1).
///
/// Read before every item rather than once, for the same reason `live_pending` is:
/// a sweep long enough to matter is long enough to cross the line, and a budget
/// checked at the start is a budget that bounds nothing.
async fn repo_budget(
    pool: &revlocal_store::Pool,
    config: &revlocal_core::GlobalConfig,
    repo_id: RepoId,
    at: Timestamp,
) -> Result<BudgetVerdict, crate::executor::ExecutorError> {
    let day = crate::budgets::day_of(at);
    let spent = revlocal_store::BudgetLedgerStore::new(pool)
        .get(repo_id, &day)
        .await
        .map_err(|source| crate::executor::ExecutorError::Store {
            source: Box::new(source),
        })?;
    Ok(crate::budgets::check(spent.as_ref(), &config.budgets))
}

/// Store the adapter's row for one change, returning the stored form.
///
/// The same upsert discovery does, for the same reason: a backfill and a live
/// pass can reach the same commit, and two rows for one change would give it two
/// review histories.
async fn record_change(
    pool: &revlocal_store::Pool,
    repo: &revlocal_core::Repo,
    change: &revlocal_vcs::DetectedChange,
    at: Timestamp,
) -> Result<revlocal_core::Change, crate::executor::ExecutorError> {
    revlocal_store::ChangeStore::new(pool)
        .upsert(&revlocal_core::Change {
            id: revlocal_core::ChangeId::new(0),
            repo_id: repo.id,
            kind: change.kind,
            external_id: change.external_id.clone(),
            title: change.title.clone(),
            author_name: change.author_name.clone(),
            author_email: change.author_email.clone(),
            authored_at: change.authored_at,
            branch: change.branch.clone(),
            base_ref: change.base_ref.clone(),
            head_ref: change.head_ref.clone(),
            url: change.url.clone(),
            diff_stat: change.diff_stat,
            detected_at: at,
        })
        .await
        .map_err(|source| crate::executor::ExecutorError::Store {
            source: Box::new(source),
        })
}

/// Record the decision not to review one change (§9.4).
///
/// Only once. A change discovery already skipped keeps that run rather than
/// gaining a second identical one, so a backfill over history the live loop has
/// already walked adds nothing to the run table.
async fn record_skipped(
    pool: &revlocal_store::Pool,
    repo: &revlocal_core::Repo,
    change_id: revlocal_core::ChangeId,
    reason: &str,
    at: Timestamp,
) -> Result<(), crate::executor::ExecutorError> {
    let runs = revlocal_store::RunStore::new(pool);
    let existing = runs.list_for_change(change_id).await.map_err(|source| {
        crate::executor::ExecutorError::Store {
            source: Box::new(source),
        }
    })?;
    if !existing.is_empty() {
        return Ok(());
    }

    runs.insert(&revlocal_core::Run {
        id: revlocal_core::RunId::new(0),
        change_id,
        attempt: 1,
        status: revlocal_core::RunStatus::Skipped,
        engine: repo.engine,
        depth: revlocal_core::Depth::Summary,
        trigger: BACKFILL_TRIGGER,
        skip_reason: Some(reason.to_owned()),
        error: None,
        error_detail: None,
        degraded: None,
        usage: revlocal_core::Usage::default(),
        started_at: None,
        finished_at: Some(at),
        transcript_path: None,
        truncated: false,
        omitted_files: Vec::new(),
        verdict: None,
        summary: None,
        created_at: at,
    })
    .await
    .map(|_| ())
    .map_err(|source| crate::executor::ExecutorError::Store {
        source: Box::new(source),
    })
}

/// Move the `backfill:` cursor to the last recorded item.
///
/// Called after **every** item, reviewed or skipped, never batched at the end.
/// §7.4's resumability is the whole reason the scope exists, and a cursor written
/// once at the end would make an interrupted sweep of ten thousand commits resume
/// at zero.
async fn advance(
    pool: &revlocal_store::Pool,
    repo: &revlocal_core::Repo,
    scope: &str,
    driver: &Backfill,
    outcome: &mut BackfillOutcome,
    at: Timestamp,
) -> Result<(), crate::executor::ExecutorError> {
    outcome.completed = driver.completed();
    let Some(value) = driver.cursor_value() else {
        return Ok(());
    };
    revlocal_store::CursorStore::new(pool)
        .advance(repo.id, scope, value, at)
        .await
        .map_err(|source| crate::executor::ExecutorError::Store {
            source: Box::new(source),
        })?;
    outcome.cursor = Some(value.to_owned());
    Ok(())
}
