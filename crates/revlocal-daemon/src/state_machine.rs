//! Run stage transitions and crash recovery (SPEC §9.1).
//!
//! Two responsibilities that look separate and are not.
//!
//! **Transitions are persisted and then announced.** In that order. Announcing
//! first and persisting second would let the UI show a stage the database does not
//! have; a crash between the two would leave a run that looks `reviewing` forever
//! with nothing behind it. Persisting first means a crash loses an *event* — the UI
//! is stale until it refreshes — but never lies about what happened.
//!
//! **Recovery is the same problem seen from the other side.** A daemon that dies
//! mid-review leaves a run stuck in a non-terminal stage. Nothing will ever move it,
//! because the thing that would have moved it is gone. So on startup, runs that have
//! been sitting in a live stage longer than they plausibly could be are failed as
//! `interrupted` and re-enqueued.
//!
//! # Why re-enqueueing needs a cap
//!
//! A change that crashes the daemon crashes it again on the next attempt. Without a
//! ceiling, every startup would recover it, every recovery would crash, and
//! rev-local would spend its life re-reviewing one poisonous commit and never reach
//! the rest. The cap is what turns a poison pill into a recorded failure.

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use revlocal_core::{Run, RunId, RunStatus, Timestamp};
use revlocal_store::{Pool, RunStore, StoreError};

/// How many attempts one change gets before rev-local stops re-enqueueing it.
///
/// Not in SPEC §13.1, which has `stale_run_minutes` but no attempt ceiling. Chosen
/// here and surfaced as a parameter so it can become config without a rewrite.
///
/// Three, because the failures worth retrying are transient — a machine that slept,
/// an engine that was mid-update — and a third identical failure is evidence rather
/// than bad luck.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 3;

/// The attempt ceiling to use, given a repository's global settings.
///
/// RL-1305 added `max_attempts` to §13.1, so config is the source of truth and
/// this is how a caller reaches it. [`DEFAULT_MAX_ATTEMPTS`] remains for callers
/// with no config — a test harness, or the recovery pass that runs before config
/// is loaded — and `state_machine_the_config_default_matches_the_constant` fails
/// if the two ever disagree.
pub const fn max_attempts_from(global: &revlocal_core::GlobalSettings) -> u32 {
    // Zero would mean "give up before trying", which is a configuration mistake
    // rather than an instruction. One attempt is the least that can mean anything.
    if global.max_attempts == 0 {
        1
    } else {
        global.max_attempts
    }
}

/// The `run.error` recorded for a run the daemon abandoned by dying.
pub const INTERRUPTED: &str = "interrupted";

/// Something that happened to a run, for the UI's live feed (SPEC §4.2, §15).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunEvent {
    /// A run moved between stages.
    StageChanged {
        /// Which run.
        run: RunId,
        /// Where it was.
        from: RunStatus,
        /// Where it is now.
        to: RunStatus,
    },
    /// A run was found abandoned and failed as `interrupted`.
    Interrupted {
        /// Which run.
        run: RunId,
        /// The stage it was stuck in — the most useful field for diagnosis, since
        /// it says *where* the daemon died.
        stuck_in: RunStatus,
    },
    /// An interrupted run was re-enqueued as a new attempt.
    ReEnqueued {
        /// The run that was interrupted.
        previous: RunId,
        /// Its successor.
        run: RunId,
        /// Which attempt the successor is.
        attempt: u32,
    },
    /// An interrupted run was **not** re-enqueued, and why.
    ///
    /// SPEC §18: giving up is a decision, and a change that stops being reviewed
    /// with no record is indistinguishable from one that was reviewed and found
    /// clean.
    GivenUp {
        /// Which run.
        run: RunId,
        /// Why no further attempt was made.
        reason: String,
    },
}

/// Where run events go.
///
/// A trait rather than a concrete channel so the daemon can fan out to the Tauri
/// event bridge and the CLI's stdout without this module knowing about either — and
/// so a test can simply collect them.
pub trait RunEventSink: Send + Sync {
    /// Record one event. Must not block or fail the transition that produced it.
    fn emit(&self, event: RunEvent);
}

/// A sink that drops everything, for callers with no UI.
pub struct NullSink;

impl RunEventSink for NullSink {
    fn emit(&self, _event: RunEvent) {}
}

/// Move a run to `next`, persisting first and announcing second.
///
/// `from` is the stage the caller believes the run is in; the store's
/// compare-and-swap refuses the move if it is not (`RL-109b`), so two callers racing
/// on one run cannot both succeed.
pub async fn transition(
    pool: &Pool,
    sink: &dyn RunEventSink,
    run: RunId,
    from: RunStatus,
    to: RunStatus,
) -> Result<(), StoreError> {
    RunStore::new(pool).transition(run, from, to).await?;

    // Only after the store agreed. An event for a transition that did not happen is
    // worse than a missing one: the UI would show a stage nothing can move it out of.
    sink.emit(RunEvent::StageChanged { run, from, to });
    Ok(())
}

/// What a recovery pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Runs failed as `interrupted`.
    pub interrupted: Vec<RunId>,
    /// New runs created to retry them.
    pub re_enqueued: Vec<RunId>,
    /// Runs abandoned, with the reason.
    pub given_up: Vec<(RunId, String)>,
}

impl RecoveryReport {
    /// Whether the pass found anything to do.
    pub fn is_empty(&self) -> bool {
        self.interrupted.is_empty() && self.re_enqueued.is_empty() && self.given_up.is_empty()
    }
}

/// Fail and re-enqueue runs abandoned by a previous process (SPEC §9.1).
///
/// A run is considered abandoned when it is in a non-terminal stage and has not been
/// touched for `stale_after`. `now` is a parameter rather than read from the clock so
/// the staleness boundary can be tested exactly instead of by sleeping.
pub async fn recover_interrupted(
    pool: &Pool,
    sink: &dyn RunEventSink,
    now: Timestamp,
    stale_after: ChronoDuration,
    max_attempts: u32,
) -> Result<RecoveryReport, StoreError> {
    let store = RunStore::new(pool);
    let mut report = RecoveryReport::default();

    for run in store.list_stale(now, stale_after).await? {
        let stuck_in = run.status;

        // Failed first. If the process dies again mid-recovery, the next pass sees a
        // terminal run and does not re-enqueue a second successor — which is what
        // makes recovery idempotent rather than merely usually-correct.
        store.mark_interrupted(run.id, INTERRUPTED).await?;
        report.interrupted.push(run.id);
        sink.emit(RunEvent::Interrupted {
            run: run.id,
            stuck_in,
        });

        if run.attempt >= max_attempts {
            let reason = format!(
                "attempt {} of {max_attempts} was interrupted in `{stuck_in}`; not \
                 retrying, because a change that keeps interrupting the daemon will \
                 keep interrupting it",
                run.attempt
            );
            report.given_up.push((run.id, reason.clone()));
            sink.emit(RunEvent::GivenUp {
                run: run.id,
                reason,
            });
            continue;
        }

        let successor = Run {
            id: RunId::new(0),
            attempt: run.attempt + 1,
            status: RunStatus::Queued,
            // A retry starts clean: it has spent nothing, seen nothing and salvaged
            // nothing. Carrying the previous attempt's usage forward would
            // double-charge the budget for work that was thrown away.
            usage: revlocal_core::Usage::default(),
            skip_reason: None,
            error: None,
            degraded: None,
            started_at: None,
            finished_at: None,
            transcript_path: None,
            truncated: false,
            omitted_files: Vec::new(),
            verdict: None,
            summary: None,
            created_at: now,
            ..run.clone()
        };

        match store.insert(&successor).await {
            Ok(created) => {
                report.re_enqueued.push(created.id);
                sink.emit(RunEvent::ReEnqueued {
                    previous: run.id,
                    run: created.id,
                    attempt: created.attempt,
                });
            }
            // `(change_id, attempt)` is unique, so a successor already existing means
            // a previous recovery got this far. That is a success, not a failure —
            // and it is the second thing keeping recovery from looping.
            Err(e) if e.is_already_exists() => {
                let reason = format!(
                    "attempt {} already exists; a previous recovery pass created it",
                    successor.attempt
                );
                report.given_up.push((run.id, reason.clone()));
                sink.emit(RunEvent::GivenUp {
                    run: run.id,
                    reason,
                });
            }
            Err(other) => return Err(other),
        }
    }

    Ok(report)
}

/// The staleness cutoff for `now`.
///
/// Exposed because the daemon logs it at startup: "recovering runs untouched since
/// X" is a far more useful line than "recovering stale runs".
pub fn stale_before(now: Timestamp, stale_after: ChronoDuration) -> DateTime<Utc> {
    now - stale_after
}
