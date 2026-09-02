//! The unattended loop (RL-1501, SPEC §4.2, §7, §14).
//!
//! # Why this is not in the CLI
//!
//! `revlocal watch` had the only working end-to-end pass — recover, discover,
//! enqueue, review — and it lived in `revlocal-cli`, which the desktop app cannot
//! depend on. So the app grew a *different* half of the loop: a button that drained
//! the queue, and a discovery call that passed `None` for the cursor and never wrote
//! one back. On the machine this was written for that produced 59 discovered
//! changes, 51 runs sitting in `queued`, zero cursor rows, and nothing published.
//!
//! One tick, in the crate both front ends already depend on, is the fix. The CLI's
//! `watch` and the app's background task now run the same code, so "it works from
//! the terminal" and "it works in the app" cannot drift apart again.
//!
//! # A tick reports what it did, including nothing
//!
//! §18: a pass that reviewed nothing because everything was over budget and a pass
//! that reviewed nothing because the repositories are quiet are different facts.
//! [`TickReport::line`] is one sentence of plain English, and it always says which.
//!
//! # Order matters
//!
//! Recovery first — a run left `reviewing` by a crash holds a slot the drain would
//! otherwise never get. Then discovery, then queueing, then reviewing, then
//! publishing: each step's output is the next step's input, so a single tick can
//! take a commit all the way from "just pushed" to "filed as an issue".

use std::collections::BTreeMap;
use std::path::Path;

use revlocal_core::{Cursor, GlobalConfig, Repo, RepoConfig, RepoId, Timestamp, TriggerSource};
use revlocal_store::{
    ChangeStore, CursorStore, Pool, PublishActionStore, RepoStore, RunStore, SettingStore,
};
use revlocal_vcs::{GitAdapter, VcsAdapter};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::budgets::{check, BudgetVerdict, DEFAULT_MAX_CONCURRENT_RUNS};
use crate::executor::RunOutcome;
use crate::scheduler::{Decision, Scheduler, WorldState};
use crate::state_machine::{recover_interrupted, RunEventSink};

/// How often the app ticks when nothing says otherwise.
///
/// Sixty seconds is short enough that a commit is reviewed while you still
/// remember making it, and long enough that a quiet machine is not spending its
/// day running `git log` over repositories nobody touched.
pub const DEFAULT_INTERVAL_SECS: u64 = 60;

/// Why a tick could not run at all.
///
/// Anything that only stops *one* repository is a note on the report instead: the
/// whole point of the loop is that one broken repository does not stop the others.
#[derive(Debug, thiserror::Error)]
pub enum AutopilotError {
    /// The database could not be read.
    #[error("could not read the local database: {source}\n  try: revlocal db migrate")]
    Store {
        /// Why.
        #[source]
        source: Box<revlocal_store::StoreError>,
    },

    /// The review pass itself failed.
    #[error("{detail}")]
    Execute {
        /// What went wrong.
        detail: String,
    },
}

fn boxed(source: revlocal_store::StoreError) -> AutopilotError {
    AutopilotError::Store {
        source: Box::new(source),
    }
}

/// What one repository's discovery pass did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoPass {
    /// Which repository.
    pub repo: String,
    /// Changes discovered.
    pub discovered: usize,
    /// Changes recorded for review.
    pub recorded: usize,
    /// Changes recorded and deliberately skipped (§9.4), with the reason.
    pub skipped: Vec<String>,
    /// Where the cursor now stands, when it moved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// What went wrong, if anything.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// What one tick did, in the order it did it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TickReport {
    /// When the tick started, RFC 3339.
    pub at: String,
    /// Enabled repositories considered.
    pub repos: usize,
    /// Discovery, per repository. Empty when nothing was due.
    pub passes: Vec<RepoPass>,
    /// Runs abandoned mid-stage by an earlier crash and re-queued.
    pub recovered: usize,
    /// Runs newly queued this tick.
    pub queued: usize,
    /// Reviews that finished this tick.
    pub reviewed: Vec<RunOutcome>,
    /// Publish actions delivered this tick.
    pub published: usize,
    /// Publish actions that failed to deliver this tick.
    pub publish_failed: usize,
    /// Publish actions sitting in the approvals inbox, right now.
    pub awaiting_approval: usize,
    /// Runs still queued after this tick.
    pub still_queued: u32,
    /// The kill switch is engaged.
    pub paused: bool,
    /// Why the loop did nothing at all, when something stopped it.
    ///
    /// Distinct from a quiet tick, which has no reason and needs none. §18: "we
    /// are stopped" and "there was nothing to do" must never render the same,
    /// and this is the field that keeps them apart. Carries the remedy, because
    /// the only useful thing to say about a kill switch is how to release it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped: Option<String>,
    /// Anything that did not happen, and why — never dropped in silence (§18).
    pub notes: Vec<String>,
}

impl TickReport {
    /// One sentence of plain English, for the status line.
    ///
    /// Written as a list of things that happened rather than a template with
    /// zeroes in it: "3 repos, 0 new, 0 reviewed, 0 filed" is a line people learn
    /// to stop reading.
    pub fn line(&self) -> String {
        if let Some(stopped) = &self.stopped {
            return stopped.clone();
        }
        if self.repos == 0 {
            return "No repositories are enabled yet.".to_owned();
        }

        let mut parts: Vec<String> = Vec::new();
        let recorded: usize = self.passes.iter().map(|pass| pass.recorded).sum();
        let skipped: usize = self.passes.iter().map(|pass| pass.skipped.len()).sum();
        if recorded > 0 {
            parts.push(format!("found {recorded} new change(s)"));
        }
        if skipped > 0 {
            parts.push(format!("skipped {skipped}"));
        }
        if self.recovered > 0 {
            parts.push(format!("restarted {} stuck run(s)", self.recovered));
        }
        if self.queued > 0 {
            parts.push(format!("queued {}", self.queued));
        }
        if !self.reviewed.is_empty() {
            parts.push(format!("reviewed {}", self.reviewed.len()));
        }
        if self.published > 0 {
            parts.push(format!("filed {}", self.published));
        }
        if self.publish_failed > 0 {
            parts.push(format!("{} failed to file", self.publish_failed));
        }
        if self.awaiting_approval > 0 {
            parts.push(format!("{} waiting for you", self.awaiting_approval));
        }
        if self.still_queued > 0 {
            parts.push(format!("{} still queued", self.still_queued));
        }

        if parts.is_empty() {
            return format!("Nothing new across {} repositories.", self.repos);
        }
        let mut line = parts.join(", ");
        // A capital and a full stop, because this is read as a sentence in the UI.
        if let Some(first) = line.get_mut(..1) {
            first.make_ascii_uppercase();
        }
        line.push('.');
        line
    }

    /// Whether this tick is worth telling somebody about.
    ///
    /// A quiet tick every minute would drown the one that matters.
    pub fn is_newsworthy(&self) -> bool {
        self.recovered > 0
            || self.queued > 0
            || !self.reviewed.is_empty()
            || self.published > 0
            || self.publish_failed > 0
            || !self.notes.is_empty()
    }
}

/// Run one pass of the whole loop.
///
/// `targets` are the ones only the caller can build — the app's MCP-backed Andare
/// target, say, which needs a bearer this crate must never read. The local report
/// target is always registered here rather than being passed in: it needs no
/// configuration, and making it the caller's job is how `revlocal watch` ended up
/// reviewing commits and writing nothing anywhere.
pub async fn tick(
    pool: &Pool,
    config: &GlobalConfig,
    sink: &dyn RunEventSink,
    data_dir: &Path,
    targets: &[std::sync::Arc<dyn revlocal_publish::PublishTarget>],
    at: Timestamp,
    cancel: &CancellationToken,
) -> Result<TickReport, AutopilotError> {
    let mut report = TickReport {
        at: at.to_rfc3339(),
        ..TickReport::default()
    };

    report.paused = SettingStore::new(pool).is_paused().await.map_err(boxed)?;
    let repos: Vec<Repo> = RepoStore::new(pool)
        .list()
        .await
        .map_err(boxed)?
        .into_iter()
        .filter(|repo| repo.enabled)
        .collect();
    report.repos = repos.len();

    // A checkout that is not there cannot be discovered, queued or reviewed, and
    // every attempt costs engine budget to learn what one `exists` call knows.
    // Two of the three repositories on the machine this was written for had been
    // deleted, were still enabled, and had 51 runs waiting against them.
    let (reachable, missing): (Vec<Repo>, Vec<Repo>) = repos
        .into_iter()
        .partition(|repo| repo.local_path.as_deref().is_none_or(path_exists));
    for repo in &missing {
        report.notes.push(format!(
            "{}: the checkout is gone — {}\n  try: put it back, point the repository at its new location, or disable it",
            repo.name,
            repo.local_path.as_deref().unwrap_or("no path recorded")
        ));
    }
    let mut repos = reachable;

    // Backfill a remote nobody recorded (RL-1514). Every repository on the live
    // install predated `add` asking for one, so the GitHub target held every
    // finding with "no recognisable GitHub remote" — correct behaviour on data
    // that was wrong. Done here rather than as a migration because it needs to
    // read the checkout, and this is the pass that already knows the checkout is
    // there.
    for repo in &mut repos {
        if repo.remote_url.is_some() || repo.kind != revlocal_core::RepoKind::Git {
            continue;
        }
        let Some(path) = repo.local_path.clone() else {
            continue;
        };
        let found = revlocal_vcs::origin_url(&revlocal_vcs::GitRunner::new(), Path::new(&path))
            .await
            .unwrap_or_default();
        let Some(url) = found else { continue };

        // Written down, not just used: the repository screen shows it, and a
        // value that only existed for the length of one pass would have to be
        // rediscovered every minute.
        match RepoStore::new(pool)
            .update(&Repo {
                remote_url: Some(url.clone()),
                updated_at: at,
                ..repo.clone()
            })
            .await
        {
            Ok(()) => repo.remote_url = Some(url),
            Err(error) => report.notes.push(format!(
                "{}: could not record its remote — {error}",
                repo.name
            )),
        }
    }

    if report.paused {
        // The scheduler owns this sentence, including the remedy: `watch` prints
        // it and the app shows it, and two wordings of "we are stopped" is one
        // too many.
        report.stopped = Some(crate::scheduler::Idle::Killed.summary_line());
        return Ok(report);
    }
    // Not `stopped`: an install with no repositories yet is somebody halfway
    // through setting up, not a system that has been halted. A pass where every
    // repository has vanished is not halted either — the notes above say what
    // happened, and saying it twice in different words helps nobody.
    if repos.is_empty() {
        return Ok(report);
    }

    // First: a run left mid-stage by a crash holds a concurrency slot, so the
    // drain below would never get one.
    let recovery = recover_interrupted(
        pool,
        sink,
        at,
        chrono::Duration::minutes(i64::from(config.global.stale_run_minutes)),
        config.global.max_attempts,
    )
    .await
    .map_err(boxed)?;
    report.recovered = recovery.re_enqueued.len();
    for (run, reason) in &recovery.given_up {
        report
            .notes
            .push(format!("run #{} was given up on — {reason}", run.get()));
    }

    if scheduler_says_discover(pool, &repos, at).await? {
        for repo in &repos {
            report.passes.push(discover_one(pool, repo, at).await);
        }
        for pass in &report.passes {
            if let Some(error) = &pass.error {
                report.notes.push(format!("{}: {error}", pass.repo));
            }
        }
    }

    for repo in &repos {
        match crate::executor::enqueue(pool, repo, at).await {
            Ok(queued) => {
                report.queued += queued.queued.len();
                if queued.more_waiting {
                    report.notes.push(format!(
                        "{}: more changes are waiting than one pass queues; the next tick takes the rest",
                        repo.name
                    ));
                }
            }
            Err(error) => report.notes.push(format!("{}: {error}", repo.name)),
        }
    }

    let drained = crate::executor::drain(
        pool,
        config,
        sink,
        data_dir,
        DEFAULT_MAX_CONCURRENT_RUNS,
        at,
        cancel,
    )
    .await
    .map_err(|error| AutopilotError::Execute {
        detail: error.to_string(),
    })?;
    report.reviewed = drained.finished;
    report.notes.extend(drained.held);

    // Last, and unconditional: an action left `pending` by an earlier tick is
    // still owed delivery even if this tick reviewed nothing.
    let mut queue =
        revlocal_publish::PublishQueue::new(pool.clone(), revlocal_publish::QueueConfig::default());
    queue.register(std::sync::Arc::new(revlocal_publish::ReportTarget::beside(
        data_dir,
    )));
    for target in targets {
        queue.register(std::sync::Arc::clone(target));
    }

    match queue.dispatch_pending(at).await {
        Ok(dispatch) => {
            report.published = dispatch.sent;
            report.publish_failed = dispatch.failed + dispatch.retryable;
            if dispatch.unroutable > 0 {
                // §18: an action nobody can route is not delivered, and a count
                // that said nothing would read as a queue that had caught up.
                report.notes.push(format!(
                    "{} action(s) name a target that is not configured, so nothing was sent\n  try: add the suite bearer in Settings",
                    dispatch.unroutable
                ));
            }
        }
        Err(error) => report.notes.push(format!("could not deliver: {error}")),
    }

    report.awaiting_approval = PublishActionStore::new(pool)
        .list_awaiting_approval()
        .await
        .map_err(boxed)?
        .len();
    report.still_queued = RunStore::new(pool)
        .count_matching(None, Some(revlocal_core::RunStatus::Queued))
        .await
        .map_err(boxed)?;

    Ok(report)
}

/// Whether a repository's checkout is still on disk.
///
/// A separate function only so the reason is written down once: `Path::exists`
/// answers `false` for a path that exists but cannot be read, and for this
/// purpose that is the same answer — rev-local cannot review it either way, and
/// the note tells somebody to go and look.
fn path_exists(path: &str) -> bool {
    !path.is_empty() && Path::new(path).exists()
}

/// Ask the scheduler whether this tick should poll remotes at all.
///
/// The decision lives in `Scheduler::tick` and its ordering rules are asserted
/// there; this gathers the world it needs and reads the answer.
async fn scheduler_says_discover(
    pool: &Pool,
    repos: &[Repo],
    at: Timestamp,
) -> Result<bool, AutopilotError> {
    let mut bus = crate::triggers::TriggerBus::default();
    for repo in repos {
        bus.admit(&crate::triggers::TriggerEvent::new(
            repo.id,
            TriggerSource::Poll,
            at,
        ));
    }
    let due = bus.due_passes(at + chrono::Duration::milliseconds(2_000));

    let mut budgets: BTreeMap<RepoId, BudgetVerdict> = BTreeMap::new();
    for repo in repos {
        let entry = revlocal_store::BudgetLedgerStore::new(pool)
            .get(repo.id, &crate::budgets::day_of(at))
            .await
            .map_err(boxed)?;
        budgets.insert(
            repo.id,
            check(entry.as_ref(), &revlocal_core::BudgetSettings::default()),
        );
    }

    let world = WorldState {
        killed: false,
        running: 0,
        slot_limit: DEFAULT_MAX_CONCURRENT_RUNS,
        due,
        backfill_waiting: Vec::new(),
        budgets,
    };

    Ok(matches!(
        Scheduler.tick(&world),
        Decision::Discover(_) | Decision::Backfill { .. }
    ))
}

/// Discover one repository, record what it found, and advance its cursor.
///
/// # The cursor advances last, and past skipped changes too
///
/// Last, because a crash between recording and advancing costs a re-read and a
/// crash the other way loses a change permanently. Past skipped ones, because a
/// change that was looked at and deliberately not reviewed is *finished* — leaving
/// the cursor behind it means re-deciding it on every poll forever.
pub async fn discover_one(pool: &Pool, repo: &Repo, at: Timestamp) -> RepoPass {
    let branch = repo.default_branch.as_deref().unwrap_or("main");
    let scope = Cursor::commits_scope(branch);

    let cursor = match CursorStore::new(pool).get(repo.id, &scope).await {
        Ok(cursor) => cursor,
        Err(error) => return failed_pass(repo, error.to_string()),
    };

    let changes = match GitAdapter::new().discover(repo, cursor.as_ref(), 50).await {
        Ok(changes) => changes,
        // One unreachable remote must not stop every other repository being
        // reviewed, so this is recorded rather than returned.
        Err(error) => return failed_pass(repo, error.to_string()),
    };

    let config = serde_json::from_str::<RepoConfig>(&repo.config_json).unwrap_or_default();
    let mut pass = RepoPass {
        repo: repo.name.clone(),
        discovered: changes.len(),
        recorded: 0,
        skipped: Vec::new(),
        cursor: None,
        error: None,
    };

    for change in &changes {
        let skip = revlocal_vcs::skip_rules::evaluate(change, &config);
        if let Err(error) = record(pool, repo, change, skip.as_ref(), at).await {
            pass.error = Some(error.to_string());
            return pass;
        }
        match skip {
            Some(skip) => pass
                .skipped
                .push(format!("{} — {}", change.external_id, skip.detail)),
            None => pass.recorded += 1,
        }
        pass.cursor = Some(change.cursor_value.clone());
    }

    if let Some(value) = &pass.cursor {
        if let Err(error) = CursorStore::new(pool)
            .advance(repo.id, &scope, value, at)
            .await
        {
            pass.cursor = None;
            pass.error = Some(error.to_string());
        }
    }

    pass
}

fn failed_pass(repo: &Repo, error: String) -> RepoPass {
    RepoPass {
        repo: repo.name.clone(),
        discovered: 0,
        recorded: 0,
        skipped: Vec::new(),
        cursor: None,
        error: Some(error),
    }
}

/// Write one discovered change down, and its skip decision if it has one.
///
/// `upsert`, not `insert`: discovery can legitimately see the same change twice —
/// a force-push, an overlapping poll — and a unique-constraint failure there would
/// stop a pass over something harmless.
///
/// A skipped change gets a `skipped` **run** as well as a change row. §9.4's rules
/// need a change's parents and paths, which only discovery has: the stored `change`
/// row carries neither, so the executor cannot re-derive the decision correctly.
async fn record(
    pool: &Pool,
    repo: &Repo,
    change: &revlocal_vcs::DetectedChange,
    skip: Option<&revlocal_vcs::Skip>,
    at: Timestamp,
) -> Result<(), AutopilotError> {
    let stored = ChangeStore::new(pool)
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
        .map_err(boxed)?;

    let Some(skip) = skip else {
        return Ok(());
    };

    // Only once. A repeat poll sees the same change again, and a second skipped
    // run per tick would fill the run table with the decision not to review.
    if !RunStore::new(pool)
        .list_for_change(stored.id)
        .await
        .map_err(boxed)?
        .is_empty()
    {
        return Ok(());
    }

    RunStore::new(pool)
        .insert(&revlocal_core::Run {
            id: revlocal_core::RunId::new(0),
            change_id: stored.id,
            attempt: 1,
            status: revlocal_core::RunStatus::Skipped,
            engine: repo.engine,
            depth: revlocal_core::Depth::Summary,
            trigger: TriggerSource::Poll,
            skip_reason: Some(skip.detail.clone()),
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
        .map_err(boxed)?;

    Ok(())
}
