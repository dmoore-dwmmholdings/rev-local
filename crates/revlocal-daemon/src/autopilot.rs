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

/// The database key holding when retention last swept.
///
/// Stored rather than kept in memory so a machine that is shut down every
/// evening still sweeps: an in-process "once a day" timer on an app that runs
/// for six hours a day never fires.
pub const SETTING_LAST_SWEEP: &str = "retention.last_sweep";

/// Whether the background loop is switched on.
///
/// Written by the desktop app's toggle and read by `doctor`, which is why it
/// lives here rather than in the binary: two definitions of one key is how the
/// diagnosis ends up reading a setting nothing writes (RL-1551).
pub const SETTING_AUTOPILOT: &str = "autopilot";

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
    /// Something worth saying that is not a fault (REVL-201).
    ///
    /// A repository with no commits yet will never be reviewed and nobody needs
    /// to do anything about it. Saying nothing hides why it is always empty;
    /// saying it under the word FAILED puts an alarm against a repository that
    /// is perfectly fine, and a screen that does that stops being read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
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
    /// Finished runs removed by retention this tick (§13.1).
    pub pruned: u64,
    /// Approvals that ran out of time waiting for a human (§12.4).
    pub expired: usize,
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
        if self.pruned > 0 {
            parts.push(format!("cleared {} old run(s)", self.pruned));
        }
        // Named separately from the inbox count: something that expired
        // unattended is news, and folding it into "waiting for you" would make it
        // disappear at the moment it stopped waiting (§18).
        if self.expired > 0 {
            parts.push(format!("{} approval(s) expired unanswered", self.expired));
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
/// `targets` is what `revlocal_publish::targets_from_config` made of the config —
/// the destinations that need credentials this crate must never read, plus the
/// reason each one that could not be built could not be built. Both halves are
/// load-bearing: the built targets are what deliver, and the reasons are what
/// turn the queue's `unroutable` count into a sentence somebody can act on.
///
/// The local report target is always registered here rather than being passed in:
/// it needs no configuration, and making it the caller's job is how
/// `revlocal watch` ended up reviewing commits and writing nothing anywhere.
pub async fn tick(
    pool: &Pool,
    config: &GlobalConfig,
    sink: &dyn RunEventSink,
    data_dir: &Path,
    targets: &revlocal_publish::TargetSet,
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
        .partition(crate::repos::checkout_is_present);
    // Recorded, not yet reported. The drain below reaches the same conclusion
    // about the same repositories from the other side — its queued runs cannot
    // proceed — and a reader needs the repository, the cause and the remedy once.
    // That the loop learned it in two places is rev-local's business (RL-1536).
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
    // There is deliberately no early return for "no reachable repository". There
    // used to be, and it skipped everything below — including `dispatch_pending`,
    // whose own comment promises an action left `pending` by an earlier tick is
    // still owed delivery "even if this tick reviewed nothing" (RL-1547).
    //
    // The trigger is not exotic: an external drive, a network share, a checkout
    // being moved. If every enabled repository sat on it, findings a human had
    // already approved stopped going out, approvals stopped ageing, retention
    // stopped, and a run stuck mid-stage stayed stuck — all reported as nothing
    // but "the checkout is gone".
    //
    // Nothing below needs a reachable checkout to be correct. Discovery and the
    // enqueue loop iterate `repos` and so do nothing when it is empty; the drain
    // holds what it cannot run and says why, which is how the missing ones get
    // reported at all. Deleting the special case is the fix, rather than adding
    // a second one.

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

    if scheduler_says_discover(pool, config, &repos, at).await? {
        for repo in &repos {
            report.passes.push(discover_one(pool, repo, at).await);
        }
        // Deliberately not copied into `notes` (REVL-202). `TickReport` carries
        // `passes` and `notes` together, so every consumer already has the
        // error; the copy only made `watch` print each discovery failure twice,
        // once as `repo — FAILED: …` and once as `held: repo: …`. Teaching each
        // renderer to de-duplicate instead is how two lists come to disagree
        // about which one is authoritative — RL-1536 was the same shape.
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

    let drained =
        crate::executor::drain(pool, config, sink, data_dir, slot_limit(config), at, cancel)
            .await
            .map_err(|error| AutopilotError::Execute {
                detail: error.to_string(),
            })?;
    report.reviewed = drained.finished;
    // The drain's held lines already name the repository and the remedy, so a
    // repository it mentioned needs nothing further from discovery. One that it
    // did not — because it had no queued runs at all — still does, or its absence
    // would go unsaid entirely.
    let held = drained.held;
    for repo in &missing {
        if !held.iter().any(|line| line.contains(repo.name.as_str())) {
            report
                .notes
                .push(crate::repos::checkout_missing_detail(repo));
        }
    }
    report.notes.extend(held);

    // Last, and unconditional: an action left `pending` by an earlier tick is
    // still owed delivery even if this tick reviewed nothing.
    let mut queue =
        revlocal_publish::PublishQueue::new(pool.clone(), revlocal_publish::QueueConfig::default());
    queue.register(std::sync::Arc::new(revlocal_publish::ReportTarget::beside(
        data_dir,
    )));
    for target in &targets.targets {
        queue.register(std::sync::Arc::clone(target));
    }

    match queue.dispatch_pending(at).await {
        Ok(dispatch) => {
            report.published = dispatch.sent;
            report.publish_failed = dispatch.failed + dispatch.retryable;
            if dispatch.unroutable > 0 {
                // §18: an action nobody can route is not delivered, and a count
                // that said nothing would read as a queue that had caught up.
                //
                // The count alone sent people to Settings whatever the cause —
                // including the cause that was really "this binary never built
                // any target at all" (REVL-223). `TargetSet` carries the reason
                // per destination, so the note names the destination and what to
                // do about it, and falls back to the count only when the config
                // has nothing to say about why.
                let mut note = format!(
                    "{} action(s) name a target that is not configured, so nothing was sent",
                    dispatch.unroutable
                );
                for line in targets.notes() {
                    note.push_str(&format!("\n  {line}"));
                }
                if targets.unavailable.is_empty() {
                    note.push_str("\n  try: add the suite bearer in Settings");
                }
                report.notes.push(note);
            }
        }
        Err(error) => report.notes.push(format!("could not deliver: {error}")),
    }

    // Housekeeping last: it is the least urgent thing a pass does, and doing it
    // before the review would delay work for a sweep nobody is waiting on.
    match expire_approvals(pool, config, at).await {
        Ok(expired) => report.expired = expired,
        Err(error) => report
            .notes
            .push(format!("could not expire old approvals — {error}")),
    }

    match sweep(pool, config, at).await {
        Ok(pruned) => report.pruned = pruned,
        // §18: a sweep that failed is not a pass that failed, but it is also not
        // nothing — a disk filling up quietly is the failure this prevents.
        Err(error) => report
            .notes
            .push(format!("could not clear old runs — {error}")),
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

/// Reject approvals nobody answered in time (§12.4).
///
/// # Why an expiry is a rejection with its own reason
///
/// `REASON_EXPIRED` exists precisely so this is distinguishable from somebody
/// declining the action. They mean opposite things about the finding: one is a
/// judgement, the other is that nobody made one. Recording both as "rejected"
/// would make the inbox's history unreadable.
///
/// # Why it runs unattended
///
/// The inbox is the one place work piles up while nobody is watching, and after
/// RL-1519 it holds *only* the things that need judgement. A proposal from six
/// weeks ago against code that has since been rewritten is not something anybody
/// should approve, and it currently looks exactly like this morning's.
async fn expire_approvals(
    pool: &Pool,
    config: &GlobalConfig,
    at: Timestamp,
) -> Result<usize, AutopilotError> {
    let ttl = i64::from(config.global.approval_ttl_hours);
    // Zero means "wait forever", which is a legitimate choice for somebody who
    // reviews their inbox on their own schedule. Reading it as "expire
    // everything immediately" would throw away every pending approval.
    if ttl <= 0 {
        return Ok(0);
    }

    let store = PublishActionStore::new(pool);
    let waiting = store.list_awaiting_approval().await.map_err(boxed)?;

    let mut expired = 0_usize;
    for action in waiting {
        let deadline = crate::approvals::expires_at(action.created_at, ttl);
        if at < deadline {
            continue;
        }

        store
            .reject(action.id, crate::approvals::REASON_EXPIRED)
            .await
            .map_err(boxed)?;

        // §5: the audit log is what makes an unattended decision reviewable
        // afterwards. An expiry with no record is indistinguishable from an
        // action that was never queued.
        let waited = (at - action.created_at).num_hours();
        revlocal_store::AuditStore::new(pool)
            .append(&revlocal_core::AuditEntry {
                id: revlocal_core::AuditId::new(0),
                at,
                actor: "daemon".to_owned(),
                kind: crate::approvals::AUDIT_KIND_EXPIRED.to_owned(),
                repo_id: None,
                run_id: Some(action.run_id),
                detail_json: crate::approvals::expiry_detail(&action, waited).to_string(),
            })
            .await
            .map_err(boxed)?;

        expired += 1;
    }

    Ok(expired)
}

/// Remove finished runs past their retention window, at most once a day.
///
/// # Why the store hands back transcript paths
///
/// A run row and its transcript are two records of the same thing in two places.
/// Deleting the row and leaving the file is how a data directory grows without
/// anything in the database to explain it, so `delete_finished_before` returns
/// the paths and this removes them in the same pass.
///
/// # Why once a day rather than every tick
///
/// The delete scans, the window is thirty days, and the loop ticks every minute.
/// Sweeping every tick would be a table scan a minute to remove nothing.
async fn sweep(pool: &Pool, config: &GlobalConfig, at: Timestamp) -> Result<u64, AutopilotError> {
    let settings = SettingStore::new(pool);
    let last = settings
        .get(SETTING_LAST_SWEEP)
        .await
        .map_err(boxed)?
        .and_then(|raw| chrono::DateTime::parse_from_rfc3339(&raw).ok())
        .map(|parsed| parsed.with_timezone(&chrono::Utc));

    // A stored value that will not parse is treated as "never swept" rather than
    // as an error: the remedy is to sweep, which is what this does anyway.
    if last.is_some_and(|last| at - last < chrono::Duration::days(1)) {
        return Ok(0);
    }

    let days = i64::from(config.global.transcript_retention_days);
    // Zero means keep everything. Reading it as "delete every finished run" would
    // turn an unset field into data loss.
    if days <= 0 {
        settings
            .set(SETTING_LAST_SWEEP, &at.to_rfc3339(), at)
            .await
            .map_err(boxed)?;
        return Ok(0);
    }

    let (deleted, transcripts) = RunStore::new(pool)
        .delete_finished_before(at - chrono::Duration::days(days))
        .await
        .map_err(boxed)?;

    for path in transcripts {
        // A transcript already gone is not a problem — the row it belonged to is
        // the thing being removed, and failing the sweep over a missing file
        // would mean it never completes.
        let _ = std::fs::remove_file(path);
    }

    settings
        .set(SETTING_LAST_SWEEP, &at.to_rfc3339(), at)
        .await
        .map_err(boxed)?;

    Ok(deleted)
}

/// §4.3's concurrency ceiling, as configured.
///
/// `DEFAULT_MAX_CONCURRENT_RUNS` is the *default* the config falls back to, which
/// is what the constant was always for. Every call site used the constant
/// directly instead, so setting `max_concurrent_runs` did nothing at all — and
/// the tests did not catch it because they assert the constant equals its
/// documented value rather than that a configured value is honoured (RL-1524).
///
/// Zero would mean a loop that reviews nothing while reporting itself healthy, so
/// it is read as "unset" and falls back rather than being obeyed.
fn slot_limit(config: &GlobalConfig) -> usize {
    let configured = usize::try_from(config.global.max_concurrent_runs).unwrap_or(usize::MAX);
    if configured == 0 {
        DEFAULT_MAX_CONCURRENT_RUNS
    } else {
        configured
    }
}

/// Ask the scheduler whether this tick should poll remotes at all.
///
/// The decision lives in `Scheduler::tick` and its ordering rules are asserted
/// there; this gathers the world it needs and reads the answer.
async fn scheduler_says_discover(
    pool: &Pool,
    config: &GlobalConfig,
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
    // §7.1's window, from the config. This was a hardcoded 2000ms copied out of
    // `watch.rs` when the loop moved here, which silently duplicated a setting
    // that already existed (RL-1524).
    let window = i64::try_from(config.global.coalesce_window_ms).unwrap_or(i64::MAX);
    let due = bus.due_passes(at + chrono::Duration::milliseconds(window));

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
        slot_limit: slot_limit(config),
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

    // Chosen by kind rather than hardcoded to git (RL-1559). One selector, so
    // wiring an adapter reaches every surface at once.
    let discovered = match revlocal_vcs::adapter_for(repo) {
        Err(error) => return failed_pass(repo, error.to_string()),
        Ok(adapter) => adapter.discover(repo, cursor.as_ref(), 50).await,
    };

    let changes = match discovered {
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
        note: None,
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

    // Nothing found, and nothing ever found: the case where a repository is
    // configured for branches it does not have (REVL-200).
    //
    // `branches` defaults to `["main", "release/*"]`, so a checkout on `master`
    // discovered nothing and said "0 discovered, 0 recorded" — which is exactly
    // what a repository nobody has committed to says. The adapter's own `probe`
    // already knows the difference and nothing in the loop was asking it.
    //
    // Only when the cursor has never moved. A repository that has discovered
    // something before is quiet for ordinary reasons, and probing it every tick
    // would be a git invocation per repository per pass to re-learn that.
    //
    // The known hole in that condition (REVL-212): a repository that worked and
    // then had its branch renamed has a cursor, matches nothing, and goes silent
    // again — this same bug by another route. The fix is not to drop the
    // condition, which would probe every quiet repository every tick, but to
    // have discovery say it matched no branches, which it already knows and
    // currently throws away.
    if changes.is_empty() && cursor.is_none() {
        match nothing_to_discover(repo).await {
            Some((problem, true)) => pass.error = Some(problem),
            Some((problem, false)) => pass.note = Some(problem),
            None => {}
        }
    }

    pass
}

/// Why a repository that has never discovered anything found nothing, and
/// whether anybody should be alarmed about it.
///
/// Returns `None` for a repository that is merely quiet — a checkout with
/// nothing new on it is not a fault, and reporting one would make the loop cry
/// wolf on every fresh install.
///
/// The boolean is the difference between "your branch patterns match nothing
/// here", which somebody can fix, and "this has no commits yet", which nobody
/// needs to (REVL-201). Both mean nothing will be reviewed; only one is wrong.
async fn nothing_to_discover(repo: &Repo) -> Option<(String, bool)> {
    let adapter = revlocal_vcs::adapter_for(repo).ok()?;
    let probe = adapter.probe(repo).await.ok()?;
    let problem = probe.problems.first()?;

    Some((
        format!("{}\n  try: {}", problem.problem, problem.remediation),
        problem.fault,
    ))
}

fn failed_pass(repo: &Repo, error: String) -> RepoPass {
    RepoPass {
        repo: repo.name.clone(),
        discovered: 0,
        recorded: 0,
        skipped: Vec::new(),
        cursor: None,
        note: None,
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
