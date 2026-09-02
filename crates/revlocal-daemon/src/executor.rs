//! The join between discovery and review (RL-1207, SPEC §4.2, §4.3, §9.1).
//!
//! # What was missing
//!
//! Every piece of the loop existed and was tested. Nothing connected them.
//! `watch` recorded changes; `pipeline::review` reviewed one change; the publish
//! queue, risk gating, approvals, budgets and the kill switch all worked on runs.
//! The only production caller of the pipeline was `revlocal review`, which takes a
//! filesystem path rather than a stored repository — so rev-local could be
//! configured, would notice your commits, and would never review one.
//!
//! To its credit `watch` said so on every tick rather than looking successful.
//! That is the difference between a gap and a lie, and it is probably why this
//! survived as long as it did.
//!
//! # Queueing and running are separate passes
//!
//! [`enqueue`] writes a `queued` run for every change that has none; [`drain`]
//! executes them. Splitting them is not ceremony: a queued run is the record that
//! rev-local *intends* to review something, and it has to survive a crash between
//! noticing and starting. §5's run row is that record, and the recovery pass
//! (RL-501) already knows how to find runs that were left mid-stage.
//!
//! # Every ceiling applies here, and none of them is this module's rule
//!
//! The kill switch, the daily budget, the concurrency cap and the autonomy mode
//! are all consulted through the functions that own them. This module decides
//! *nothing* about them — it is the place they finally get applied to real work,
//! which is why it is worth being explicit that a run held back is reported rather
//! than skipped in silence (§18).

use std::path::Path;

use revlocal_core::{
    ActionIntent, AutonomyMode, Capability, Change, Depth, GlobalConfig, PublishAction,
    PublishActionId, Repo, RepoConfig, Run, RunId, RunStatus, Timestamp, TriggerSource, Usage,
};
use revlocal_store::{
    BudgetLedgerStore, ChangeStore, FindingStore, Pool, PublishActionStore, RepoStore, RunStore,
    SettingStore, SuppressionStore,
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::state_machine::{transition, RunEventSink};
use crate::{budgets, engines, gating, pipeline};

/// How many changes are queued per repository per pass.
///
/// Not a silent cap: [`EnqueueReport::more_waiting`] says when it was hit, and a
/// second pass takes the next batch. A backfill of ten thousand commits should not
/// become ten thousand rows in one transaction, and it should not look finished
/// when it is not.
pub const ENQUEUE_BATCH: u32 = 50;

/// Why the executor could not do its work.
#[derive(Debug, thiserror::Error)]
pub enum ExecutorError {
    /// The database could not be read or written.
    #[error("could not reach the local database: {source}\n  try: revlocal db migrate")]
    Store {
        /// Why.
        #[source]
        source: Box<revlocal_store::StoreError>,
    },

    /// The engine named by a repository could not be built (§8.4).
    #[error("{source}")]
    Engine {
        /// Why.
        #[source]
        source: engines::EngineError,
    },

    /// An explicitly requested review was held before it could start.
    #[error("{detail}")]
    ManualHeld {
        /// Why the selected run could not begin.
        detail: String,
    },

    /// Publishing cannot be configured safely for this repository.
    #[error("{detail}")]
    PublishConfig {
        /// The setting that must be corrected.
        detail: String,
    },
}

fn boxed(source: revlocal_store::StoreError) -> ExecutorError {
    ExecutorError::Store {
        source: Box::new(source),
    }
}

/// What a queueing pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnqueueReport {
    /// Runs created, oldest change first.
    pub queued: Vec<i64>,
    /// Whether [`ENQUEUE_BATCH`] was reached, so more are waiting (§18).
    pub more_waiting: bool,
}

/// Queue a run for every change in this repository that has none.
///
/// Skip rules are **not** re-evaluated here, and that is not an omission. §9.4's
/// rules need a change's parents and paths — merge detection, `ignore_globs` — and
/// the stored `change` row carries neither: they belong to the moment of
/// discovery, which is where the rules already run. Discovery writes a `skipped`
/// run with its reason, so a skipped change is *covered* and this pass does not
/// pick it up. Re-deriving the answer from less information than the first
/// evaluation had is how two answers to one question start disagreeing.
pub async fn enqueue(
    pool: &Pool,
    repo: &Repo,
    at: Timestamp,
) -> Result<EnqueueReport, ExecutorError> {
    let changes = ChangeStore::new(pool)
        .without_runs(repo.id, ENQUEUE_BATCH)
        .await
        .map_err(boxed)?;
    let more_waiting = u32::try_from(changes.len()).unwrap_or(u32::MAX) >= ENQUEUE_BATCH;

    let runs = RunStore::new(pool);
    let mut queued = Vec::with_capacity(changes.len());

    for change in &changes {
        let run = runs
            .insert(&Run {
                id: RunId::new(0),
                change_id: change.id,
                attempt: 1,
                status: RunStatus::Queued,
                engine: repo.engine,
                depth: Depth::Standard,
                trigger: TriggerSource::Poll,
                skip_reason: None,
                error: None,
                error_detail: None,
                degraded: None,
                usage: Usage::default(),
                started_at: None,
                finished_at: None,
                transcript_path: None,
                truncated: false,
                omitted_files: Vec::new(),
                verdict: None,
                summary: None,
                created_at: at,
            })
            .await
            .map_err(boxed)?;

        queued.push(run.id.get());
    }

    Ok(EnqueueReport {
        queued,
        more_waiting,
    })
}

/// Queue one explicitly requested review, without running it.
///
/// Queueing and executing are separate because the caller is a UI. A manual
/// review takes as long as the engine takes, and a command that only returns
/// once the engine has finished gives the person who clicked no run to look at,
/// no stage events, and a window that appears to have hung. This returns the run
/// as soon as it exists; [`execute_run`] is what the caller then drives in the
/// background.
///
/// A manual request must not be coalesced with discovery: the caller named this
/// revision, and `enqueue` deliberately skips changes that already have a run.
/// Reusing it would make a second click review nothing at all.
pub async fn enqueue_manual(
    pool: &Pool,
    repo: &Repo,
    change: &Change,
    at: Timestamp,
) -> Result<Run, ExecutorError> {
    let change = ChangeStore::new(pool).upsert(change).await.map_err(boxed)?;
    let runs = RunStore::new(pool);
    let attempt = runs
        .list_for_change(change.id)
        .await
        .map_err(boxed)?
        .iter()
        .map(|run| run.attempt)
        .max()
        .unwrap_or_default()
        .checked_add(1)
        .ok_or_else(|| ExecutorError::ManualHeld {
            detail: format!(
                "change {} has exhausted its run-attempt counter",
                change.external_id
            ),
        })?;
    runs.insert(&Run {
        id: RunId::new(0),
        change_id: change.id,
        attempt,
        status: RunStatus::Queued,
        engine: repo.engine,
        depth: Depth::Standard,
        trigger: TriggerSource::Manual,
        skip_reason: None,
        error: None,
        error_detail: None,
        degraded: None,
        usage: Usage::default(),
        started_at: None,
        finished_at: None,
        transcript_path: None,
        truncated: false,
        omitted_files: Vec::new(),
        verdict: None,
        summary: None,
        created_at: at,
    })
    .await
    .map_err(boxed)
}

/// Run one already-queued run, by id.
///
/// [`drain`] takes whatever is at the head of the queue; this takes the one the
/// caller named. The desktop needs the second: somebody who asked to review
/// *this* commit is owed that commit's run, not the next one in line.
///
/// The nested `Result` matches [`drain`]'s: the outer is "the executor broke",
/// the inner is "this run did not go ahead, and here is why".
pub async fn execute_run(
    pool: &Pool,
    config: &GlobalConfig,
    sink: &dyn RunEventSink,
    data_dir: &Path,
    run_id: RunId,
    at: Timestamp,
    cancel: &CancellationToken,
) -> Result<Result<RunOutcome, String>, ExecutorError> {
    if SettingStore::new(pool).is_paused().await.map_err(boxed)? {
        // §12.1: the kill switch holds work rather than failing it. The run stays
        // queued, which is what makes resuming it a matter of un-pausing.
        return Ok(Err(format!(
            "run #{}: the kill switch is engaged, so it stays queued",
            run_id.get()
        )));
    }

    let run = RunStore::new(pool).get(run_id).await.map_err(boxed)?;

    execute_one(pool, config, sink, data_dir, &run, at, cancel).await
}

/// What happened to one run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunOutcome {
    /// Which run.
    pub run_id: i64,
    /// Which repository.
    pub repo: String,
    /// The change, in its own system's terms.
    pub change: String,
    /// Where it ended up.
    pub status: String,
    /// The engine that actually ran (§8.4, D3).
    pub engine: String,
    /// What it concluded, when it concluded anything.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    /// How many findings were stored.
    pub findings: usize,
    /// How many publish actions were queued.
    pub actions: usize,
    /// Why it failed or was held, when it was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// What one executor pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutorReport {
    /// Runs that finished, in the order they were taken.
    pub finished: Vec<RunOutcome>,
    /// Runs left queued, and why — never dropped in silence (§18).
    pub held: Vec<String>,
    /// Whether the kill switch is engaged (§12.1).
    pub paused: bool,
}

impl ExecutorReport {
    /// The line `watch` prints when nothing ran.
    pub fn idle_line(&self) -> Option<String> {
        if self.paused {
            return Some(
                "Paused: the kill switch is engaged, so nothing is being reviewed.".to_owned(),
            );
        }
        (!self.held.is_empty()).then(|| self.held.join("\n"))
    }
}

/// Execute up to `limit` queued runs.
///
/// Sequential rather than concurrent, deliberately for now: §4.3's semaphore
/// (`budgets::RunSlots`) is what bounds concurrency, and wiring it here before
/// anything runs at all would be two new things at once. `limit` is that bound
/// applied by the caller, and a pass that ran everything in the queue would ignore
/// §4.3 entirely.
pub async fn drain(
    pool: &Pool,
    config: &GlobalConfig,
    sink: &dyn RunEventSink,
    data_dir: &Path,
    limit: usize,
    at: Timestamp,
    cancel: &CancellationToken,
) -> Result<ExecutorReport, ExecutorError> {
    let paused = SettingStore::new(pool).is_paused().await.map_err(boxed)?;
    if paused {
        // §12.1: the kill switch stops work rather than queueing it differently.
        // The queued runs stay queued, which is what makes it reversible.
        return Ok(ExecutorReport {
            paused: true,
            ..ExecutorReport::default()
        });
    }

    let queued = RunStore::new(pool)
        .list_recent(None, Some(RunStatus::Queued), 200)
        .await
        .map_err(boxed)?;

    let mut report = ExecutorReport::default();

    for run in queued.iter().rev().take(limit) {
        if cancel.is_cancelled() {
            report.held.push(format!(
                "run #{}: cancelled before it started",
                run.id.get()
            ));
            continue;
        }

        match execute_one(pool, config, sink, data_dir, run, at, cancel).await? {
            Ok(outcome) => report.finished.push(outcome),
            Err(held) => report.held.push(held),
        }
    }

    Ok(report)
}

/// Run one queued review, or say why it was held.
///
/// The nested `Result` is deliberate: the outer one is "the executor broke", the
/// inner is "this run did not go ahead, and here is why". Collapsing them would
/// make a repository over its daily budget indistinguishable from a database that
/// will not open.
async fn execute_one(
    pool: &Pool,
    config: &GlobalConfig,
    sink: &dyn RunEventSink,
    data_dir: &Path,
    run: &Run,
    at: Timestamp,
    cancel: &CancellationToken,
) -> Result<Result<RunOutcome, String>, ExecutorError> {
    let change = ChangeStore::new(pool)
        .get(run.change_id)
        .await
        .map_err(boxed)?;
    let Some(repo) = RepoStore::new(pool)
        .list()
        .await
        .map_err(boxed)?
        .into_iter()
        .find(|r| r.id == change.repo_id)
    else {
        return Ok(Err(format!(
            "run #{}: its repository has been removed",
            run.id.get()
        )));
    };

    if !repo.enabled {
        return Ok(Err(format!(
            "run #{}: {} is disabled",
            run.id.get(),
            repo.name
        )));
    }

    // Before the budget check and before an engine is chosen, because a run
    // against a checkout that is not there costs a model call to learn what one
    // `exists` call knows.
    //
    // Here rather than in the loop's discovery pass, which is where it went
    // first: that stopped new runs being queued and left the fifty-one already
    // waiting to run and fail. Every path into a review goes through this
    // function — the loop, the desktop's queue button, `revlocal watch` — and a
    // guard in one caller is a guard the other two do not have.
    if !crate::repos::checkout_is_present(&repo) {
        return Ok(Err(format!(
            "run #{}: {}",
            run.id.get(),
            crate::repos::checkout_missing_detail(&repo)
        )));
    }

    // §13.1's budget, checked before anything is spent rather than after.
    let day = budgets::day_of(at);
    let spent = BudgetLedgerStore::new(pool)
        .get(repo.id, &day)
        .await
        .map_err(boxed)?;
    let verdict = budgets::check(spent.as_ref(), &config.budgets);
    if let Some(reason) = verdict.reason() {
        return Ok(Err(format!("run #{}: {reason}", run.id.get())));
    }

    let engine = engines::for_kind(repo.engine, config)
        .map_err(|source| ExecutorError::Engine { source })?;

    let runs = RunStore::new(pool);
    transition(pool, sink, run.id, RunStatus::Queued, RunStatus::Preparing)
        .await
        .map_err(boxed)?;

    // §6.1's scratch, which knows about `keep_scratch_on_failure` — a run that
    // failed is the one whose worktree somebody wants to look at.
    //
    // `data_dir` is passed in rather than invented here. §4.1 puts scratch at
    // `{data_dir}/scratch/{run_id}`, and a module-local guess at that path means
    // two rev-local instances with different databases collide on run id 1 —
    // which `ScratchDir::create` correctly refuses, turning somebody else's
    // installation into this one's failed review.
    let mut scratch = match revlocal_vcs::ScratchDir::create(
        data_dir,
        run.id,
        config.global.keep_scratch_on_failure,
    ) {
        Ok(dir) => dir,
        Err(error) => {
            return Ok(Err(fail(
                pool,
                sink,
                run.id,
                RunStatus::Preparing,
                &format!(
                    "could not create a scratch directory under {}: {error}",
                    data_dir.display()
                ),
                None,
            )
            .await?));
        }
    };

    let context = match materialize(&repo, &change, scratch.path()).await {
        Ok(context) => context,
        Err(detail) => {
            scratch.mark_failed();
            return Ok(Err(fail(
                pool,
                sink,
                run.id,
                RunStatus::Preparing,
                &detail,
                None,
            )
            .await?));
        }
    };

    transition(
        pool,
        sink,
        run.id,
        RunStatus::Preparing,
        RunStatus::Reviewing,
    )
    .await
    .map_err(boxed)?;

    // Point the run at the live file before starting the engine. It is later
    // copied out of scratch, but while reviewing this is what the UI tails.
    let live_transcript = scratch
        .path()
        .join("engine-out")
        .join(revlocal_engine::TRANSCRIPT_FILE);
    if let Some(parent) = live_transcript.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::File::create(&live_transcript);
    let live_transcript = live_transcript.display().to_string();
    RunStore::new(pool)
        .set_transcript_path(run.id, Some(&live_transcript))
        .await
        .map_err(boxed)?;

    let repo_config = serde_json::from_str::<RepoConfig>(&repo.config_json).unwrap_or_default();
    let suppressions = SuppressionStore::new(pool)
        .list_for_repo(repo.id)
        .await
        .map_err(boxed)?;

    let change_with_stat = Change {
        diff_stat: context.stat,
        ..change.clone()
    };

    // Record the engine's pid while its process is alive (RL-1521).
    //
    // `Engine::run` returns the pid only inside `EngineOutcome`, which arrives
    // once the process has exited — exactly when it stops being useful to
    // anything trying to stop it. The sink is fed from inside `supervise` at
    // spawn, and this task writes it down.
    //
    // A task rather than an inline write because reporting is synchronous and
    // storing is not. It holds its own pool handle so it cannot borrow anything
    // the review needs.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<u32>();
    let pids = revlocal_engine::PidSink::to(tx);
    let recorder = {
        let pool = pool.clone();
        let run_id = run.id;
        tokio::spawn(async move {
            while let Some(pid) = rx.recv().await {
                // A pid that cannot be stored is not a reason to fail a review
                // that is already running; it costs the ability to reap that one
                // process, which is what the kill switch reports.
                let _ = RunStore::new(&pool).set_engine_pid(run_id, Some(pid)).await;
            }
        })
    };

    let outcome = pipeline::review(
        &pipeline::ReviewInputs {
            repo_name: &repo.name,
            repo_kind: repo.kind.as_str(),
            change: &change_with_stat,
            config: &repo_config,
            worktree: &context.worktree,
            diff_unified: &context.diff_unified,
            diff_files: &context.diff_files,
            labels: &[],
            suppressions: &suppressions,
            published_fingerprints: &[],
            prior_findings: &[],
            skip: None,
            now: at,
        },
        engine.as_ref(),
        scratch.path(),
        cancel,
        &pids,
    )
    .await;

    // The recorder outlives the review only long enough to drain what is left.
    // Dropping the sender first is what lets it finish: the loop ends on a closed
    // channel rather than on a flag somebody has to remember to set.
    drop(pids);
    let _ = recorder.await;

    // Cleared once the process is gone. A pid left recorded against a finished
    // run is what `orphan_pids` reports, and reporting a dead process as an
    // orphan on every kill from now on is worse than never recording it.
    let _ = RunStore::new(pool).set_engine_pid(run.id, None).await;

    let transcript = retain_transcript(data_dir, run.id, scratch.path());

    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            scratch.mark_failed();
            return Ok(Err(fail(
                pool,
                sink,
                run.id,
                RunStatus::Reviewing,
                &error.to_string(),
                transcript.as_deref(),
            )
            .await?));
        }
    };
    scratch.mark_succeeded();

    transition(
        pool,
        sink,
        run.id,
        RunStatus::Reviewing,
        RunStatus::Synthesizing,
    )
    .await
    .map_err(boxed)?;

    // Findings are stored before any publish action is created. An action whose
    // finding is not in the store is one the approvals inbox cannot render and a
    // suppression can never match.
    let findings = FindingStore::new(pool);
    let mut stored = Vec::new();
    for candidate in &outcome.findings {
        let row = findings
            .insert(&revlocal_core::Finding {
                run_id: run.id,
                ..candidate.finding.clone()
            })
            .await
            .map_err(boxed)?;
        stored.push((row, candidate.is_publishable()));
    }

    transition(
        pool,
        sink,
        run.id,
        RunStatus::Synthesizing,
        RunStatus::Publishing,
    )
    .await
    .map_err(boxed)?;

    let actions = queue_actions(pool, config, &repo, run.id, &stored, &outcome, at).await?;
    let statuses = actions.statuses;

    // The run's own record, before the terminal transition: a run that says `done`
    // and carries no usage would make §13's budget ledger disagree with itself.
    //
    // The pipeline's own status decides the run's. The first version of this wrote
    // `Done` unconditionally, and a review whose engine could not run at all was
    // recorded as a clean run with no findings — the exact "looks successful and
    // found nothing" failure §18 exists to prevent. A test caught it, which is the
    // only reason it is not still here.
    let mut finished = run.clone();
    finished.error = outcome.report.failure.clone();
    // The code groups; this is the half a person acts on (RL-1512). Stored on the
    // run rather than only reported, so a failure is still explicable tomorrow.
    finished.error_detail = outcome.report.failure_detail.clone();
    finished.skip_reason = outcome.report.skip_reason.clone();
    finished.usage = outcome.report.usage;
    finished.verdict = outcome
        .report
        .verdict
        .as_deref()
        .and_then(|v| v.parse().ok());
    finished.summary = Some(outcome.report.summary.clone());
    finished.transcript_path = transcript;
    finished.truncated = outcome.report.truncated;
    finished
        .omitted_files
        .clone_from(&outcome.report.omitted_files);
    finished.degraded.clone_from(&outcome.report.degraded);
    finished.started_at = Some(at);
    finished.finished_at = Some(at);
    runs.record_result(&finished).await.map_err(boxed)?;

    // §13: what was spent is recorded whether or not anybody looks, and an
    // unmeasured run is recorded as unmeasured rather than as free (ADR 0010).
    BudgetLedgerStore::new(pool)
        .add_run(repo.id, &day, 1, &outcome.report.usage)
        .await
        .map_err(boxed)?;

    let awaiting = statuses.contains(&revlocal_core::PublishActionStatus::AwaitingApproval);
    let terminal = match outcome.report.status {
        // §8.2: an engine that could not produce a usable review is a failed run,
        // not an empty one. The two look identical in a findings count and mean
        // opposite things.
        pipeline::ReviewStatus::Failed => RunStatus::Failed,
        pipeline::ReviewStatus::Skipped => RunStatus::Skipped,
        pipeline::ReviewStatus::Done if awaiting => RunStatus::AwaitingApproval,
        pipeline::ReviewStatus::Done => RunStatus::Done,
    };
    transition(pool, sink, run.id, RunStatus::Publishing, terminal)
        .await
        .map_err(boxed)?;

    Ok(Ok(RunOutcome {
        run_id: run.id.get(),
        repo: repo.name.clone(),
        change: change.external_id.clone(),
        status: terminal.as_str().to_owned(),
        engine: outcome.report.engine.clone(),
        verdict: outcome.report.verdict.clone(),
        findings: stored.len(),
        actions: statuses.len(),
        // The failure first: a run that failed and a run that was salvaged are
        // both worth a line, and only one of them produced a review. A publish
        // that was held comes last, because it is the least serious of the three
        // and the only one that leaves a usable review behind.
        detail: failure_line(&outcome.report)
            .or_else(|| outcome.report.degraded.clone())
            // Every held reason, not the first. Two targets can each be missing
            // something different, and showing one would hide the other.
            .or_else(|| (!actions.held.is_empty()).then(|| actions.held.join("\n"))),
    }))
}

/// The failure as one line: the code, and what the engine actually said.
///
/// Both, not either. The code is what the UI groups by and what somebody greps
/// for; the message is the only half that says what to do. Reporting the code
/// alone is how three runs sat `engine_failed` for a week with nothing to act on.
fn failure_line(report: &pipeline::ReviewReport) -> Option<String> {
    let code = report.failure.as_ref()?;
    Some(match &report.failure_detail {
        Some(detail) => format!("{code} — {detail}"),
        None => code.clone(),
    })
}

/// Mark a run failed with a reason, and return the line the report shows.
///
/// §18: a run that stopped being reviewed with no record is indistinguishable
/// from one that was reviewed and found clean.
async fn fail(
    pool: &Pool,
    sink: &dyn RunEventSink,
    run: RunId,
    from: RunStatus,
    detail: &str,
    transcript_path: Option<&str>,
) -> Result<String, ExecutorError> {
    let runs = RunStore::new(pool);
    runs.set_transcript_path(run, transcript_path)
        .await
        .map_err(boxed)?;
    runs.mark_interrupted(run, detail).await.map_err(boxed)?;
    // The store already moved it; the event is what the UI needs.
    sink.emit(crate::state_machine::RunEvent::StageChanged {
        run,
        from,
        to: RunStatus::Failed,
    });
    Ok(format!("run #{}: {detail}", run.get()))
}

/// Copy the engine's raw output out of the disposable scratch directory.
fn retain_transcript(data_dir: &Path, run: RunId, scratch: &Path) -> Option<String> {
    let source = scratch
        .join("engine-out")
        .join(revlocal_engine::TRANSCRIPT_FILE);
    let text = std::fs::read(&source).ok()?;
    let directory = data_dir.join("transcripts");
    std::fs::create_dir_all(&directory).ok()?;
    let path = directory.join(format!("{}.log", run.get()));
    std::fs::write(&path, text).ok()?;
    Some(path.display().to_string())
}

/// Materialize the change with the adapter its repository kind needs (§6).
async fn materialize(
    repo: &Repo,
    change: &Change,
    into: &Path,
) -> Result<revlocal_vcs::ChangeContext, String> {
    match repo.kind {
        revlocal_core::RepoKind::Git | revlocal_core::RepoKind::GitHub => {
            use revlocal_vcs::VcsAdapter as _;
            revlocal_vcs::GitAdapter::new()
                .materialize(repo, change, into)
                .await
                .map_err(|e| e.to_string())
        }
        // §6.4's SVN path materialises through its own adapter, which needs an
        // `svn` binary. Reported rather than silently reviewed as git — the diff
        // would be empty and the review would look clean.
        revlocal_core::RepoKind::Svn => Err(
            "SVN repositories are not executed by this pass yet; `revlocal review` \
             reviews one revision at a time"
                .to_owned(),
        ),
    }
}

/// What queueing this run's publish actions produced.
struct QueuedActions {
    /// The status each queued action was given.
    statuses: Vec<revlocal_core::PublishActionStatus>,
    /// Why work was not queued, when the review itself was fine.
    ///
    /// §18: nothing is dropped in silence. A repository missing a setting is not
    /// a failed review, and returning it as one used to leave the run stuck in
    /// `publishing` and abort the whole executor pass — one repository's missing
    /// `andare_project` stopped every other repository's queue.
    ///
    /// A `Vec`, not an `Option`, because targets fail independently and a
    /// repository can be missing what two of them need. While this held one
    /// reason, adding the GitHub target made its message shadow Andare's — which
    /// is the same silent drop in a new place.
    held: Vec<String>,
}

/// Turn publishable findings into gated publish actions (§11, §12).
///
/// The gate is `gating::gate`, the same one every other path uses. This module
/// does not decide what is risky; it decides nothing at all, which is the point.
async fn queue_actions(
    pool: &Pool,
    config: &GlobalConfig,
    repo: &Repo,
    run: RunId,
    stored: &[(revlocal_core::Finding, bool)],
    outcome: &pipeline::ReviewOutcome,
    at: Timestamp,
) -> Result<QueuedActions, ExecutorError> {
    let mode = AutonomyMode::effective(config.global.mode, repo.autonomy);
    if mode == AutonomyMode::Off {
        // Not an error and not a silent drop: `off` means no actions, and the
        // findings are still stored for somebody to read.
        return Ok(QueuedActions {
            statuses: Vec::new(),
            held: Vec::new(),
        });
    }

    let store = PublishActionStore::new(pool);
    let mut statuses = Vec::new();
    // Findings this run saw that are already filed, so nothing new is queued.
    let mut recurring = 0_usize;
    let repo_config = serde_json::from_str::<RepoConfig>(&repo.config_json).unwrap_or_default();
    let project = repo_config
        .andare_project
        .clone()
        .filter(|project| !project.trim().is_empty());

    // §12.3's first-use rule is about history, and reading it is the whole point:
    // hardcoding `false` here meant every filing was forever treated as a first
    // filing, so `auto_low_ask_high` asked about every issue it would ever create
    // and the inbox was the only way anything ever reached Andare (RL-1504).
    //
    // Both facts are gathered once per run — neither varies between the findings of
    // one run, and looking them up per finding is a round trip per finding.
    let seasoned = store
        .pair_has_succeeded("andare", Capability::CreateIssue)
        .await
        .map_err(boxed)?;
    let recent = store
        .actions_sent_since(repo.id, at - chrono::Duration::hours(1))
        .await
        .map_err(boxed)?;

    // Which targets this repository actually publishes to (§13.2's `targets`).
    // The field has existed since the first config; nothing read it, so a
    // repository with `targets = []` still had Andare actions queued for it.
    let wants_andare = repo_config.targets_include("andare");
    let wants_report = repo_config.targets_include(revlocal_publish::REPORT_TARGET);
    // A GitHub target needs `owner/name`, and the only place that can come from
    // is the repository's own remote. `github_slug` returns `None` for a remote
    // it does not recognise as GitHub rather than guessing: the path shape is the
    // same on every forge, and a wrong guess files a private repository's
    // findings into a stranger's project.
    let github_repo = repo_config
        .targets_include("github")
        .then(|| {
            repo.remote_url
                .as_deref()
                .and_then(revlocal_publish::github_slug)
        })
        .flatten();
    let wants_github = github_repo.is_some();
    // Resolved once and used twice — by the held note below and by the page
    // itself — so the report and the behaviour cannot disagree about whether a
    // space was configured.
    let trama_space = repo_config
        .trama_space
        .clone()
        .filter(|space| !space.trim().is_empty())
        .filter(|_| repo_config.targets_include("trama"));
    let wants_trama = trama_space.is_some();
    let publishable = stored.iter().filter(|(_, ok)| *ok).count();

    // Reported and skipped, not raised: the review ran and its findings are
    // stored. What cannot happen is filing them into a project nobody named — or
    // into no target at all.
    let mut held = Vec::new();
    if publishable > 0 {
        if !wants_andare && !wants_report && !wants_github {
            held.push(format!(
                "run #{}: {publishable} finding(s) were not published — `{}` has no publish targets enabled\n  try: add `report` to that repository's `targets` to write them to disk",
                run.get(),
                repo.name
            ));
        }
        if wants_andare && project.is_none() {
            held.push(format!(
                "run #{}: {publishable} finding(s) were not filed to Andare — `{}` has no Andare project set\n  try: set the project key under \u{201c}Where findings go\u{201d} on that repository's screen",
                run.get(),
                repo.name
            ));
        }
        if repo_config.targets_include("github") && !wants_github {
            held.push(format!(
                "run #{}: {publishable} finding(s) were not filed to GitHub — `{}` has no recognisable GitHub remote\n  try: set the repository's remote URL, or remove `github` from its `targets`",
                run.get(),
                repo.name
            ));
        }
        // The same distinction the three above draw: asking for a target and
        // lacking its configuration is worth saying, not asking for it at all is
        // not. The page is dropped by a filter chain, and until now silently —
        // §18 broken in the one place RL-1529 was adding behaviour.
        if repo_config.targets_include("trama") && !wants_trama {
            held.push(format!(
                "run #{}: no review page was written — `{}` has no Trama space set\n  try: set `trama_space` in that repository's configuration, or remove `trama` from its `targets`",
                run.get(),
                repo.name
            ));
        }
    }

    // Gated per target rather than once per finding, because where an action goes
    // is part of how risky it is: writing a file on this machine and filing into a
    // shared tracker are the same intent and not the same decision (RL-1519).
    // Hoisted out of the loop because the Trama page below needs it too, and none
    // of it varies between the findings of one run.
    let base_context = gating::GateContext {
        mode,
        destination: revlocal_core::Destination::External,
        run_degraded: outcome.report.degraded.is_some(),
        actions_in_last_hour: recent,
        burst_threshold: config.global.burst_threshold,
    };

    for (finding, filable) in stored {
        if !filable {
            continue;
        }

        let gate_for = |destination| {
            gating::gate(
                ActionIntent::CreateIssue,
                Some(finding.confidence),
                seasoned,
                gating::GateContext {
                    destination,
                    ..base_context
                },
            )
        };

        let context = revlocal_publish::IssueContext::default();

        // One finding can owe work to several targets, and they fail
        // independently: a missing Andare project must not stop the local report
        // that needs no configuration at all.
        let mut payloads: Vec<(&str, revlocal_core::Destination, String)> = Vec::new();

        if wants_andare {
            if let Some(project) = project.clone() {
                let options = revlocal_publish::AndareOptions {
                    project,
                    min_severity: repo_config.andare_min_severity,
                };
                let payload = revlocal_publish::AndarePayload {
                    recurrence_body: revlocal_publish::recurrence_comment(finding, &context),
                    draft: revlocal_publish::compose_issue(finding, &context, &options),
                    context: context.clone(),
                };
                payloads.push((
                    "andare",
                    revlocal_core::Destination::External,
                    encode(&payload, "Andare issue")?,
                ));
            }
        }

        if let Some(slug) = &github_repo {
            let payload = revlocal_publish::GitHubIssue {
                repo: slug.clone(),
                title: finding.title.clone(),
                body: revlocal_publish::compose_body(finding, &context),
                fingerprint: finding.fingerprint.clone(),
            };
            payloads.push((
                "github",
                revlocal_core::Destination::External,
                encode(&payload, "GitHub issue")?,
            ));
        }

        if wants_report {
            let payload = revlocal_publish::ReportPayload {
                repo: repo.name.clone(),
                title: finding.title.clone(),
                body: revlocal_publish::compose_body(finding, &context),
                fingerprint: finding.fingerprint.clone(),
            };
            payloads.push((
                revlocal_publish::REPORT_TARGET,
                // The whole point of this target: it never leaves the machine.
                revlocal_core::Destination::Local,
                encode(&payload, "local report")?,
            ));
        }

        for (target, destination, payload_json) in payloads {
            let gated = gate_for(destination);
            let Some(status) = gated.initial_status() else {
                continue;
            };

            // Keyed by fingerprint **and run**, which is the difference between
            // two dedupes that look alike (RL-1530).
            //
            // §11.4 and M9 are explicit: "a re-run for the same fingerprint
            // produces a comment, not a second issue". The comment is the
            // target's job — it searches for the trailer and comments when it
            // finds one — and it can only do that if an action reaches it.
            //
            // Keyed on the fingerprint alone, RL-1509 stopped the second run
            // queueing anything at all. That fixed a real crash: a duplicate key
            // violated a unique constraint and took the whole executor pass with
            // it. But it also meant `recurrence_comment` was composed into every
            // payload and could never be sent, and a finding still present twenty
            // commits later said nothing.
            //
            // With the run in the key, each review that still sees the problem
            // queues its own action, the constraint still holds, and the remote
            // dedupe decides between filing and commenting — which is where §11.4
            // put that decision.
            let key = format!("{target}-{}-run{}", finding.fingerprint, run.get());
            if store
                .find_by_idempotency_key(target, &key)
                .await
                .map_err(boxed)?
                .is_some()
            {
                recurring += 1;
                continue;
            }

            store
                .insert(&PublishAction {
                    id: PublishActionId::new(0),
                    run_id: run,
                    finding_id: Some(finding.id),
                    target: target.to_owned(),
                    capability: Capability::CreateIssue,
                    risk: gated.assessment.class,
                    idempotency_key: key,
                    payload_json,
                    status,
                    attempts: 0,
                    response_json: None,
                    external_ref: None,
                    error: None,
                    created_at: at,
                    sent_at: None,
                })
                .await
                .map_err(boxed)?;

            statuses.push(status);
        }
    }

    // One page per run, not one per finding — the page *is* the review, with its
    // verdict, summary and findings together. Queued outside the loop above for
    // that reason, with its own idempotency key (RL-1529).
    if let Some(space) = trama_space {
        // §12.3: publishing a page is high risk, leaving it a draft is low, and
        // `UpsertDoc` already carries that distinction. A repository that has not
        // opted in gets a draft — the safe half, and still readable.
        let gated = gating::gate(
            ActionIntent::UpsertDoc {
                published: repo_config.trama_publish,
            },
            None,
            store
                .pair_has_succeeded("trama", Capability::UpsertDoc)
                .await
                .map_err(boxed)?,
            gating::GateContext {
                destination: revlocal_core::Destination::External,
                ..base_context
            },
        );

        if let Some(status) = gated.initial_status() {
            let change = &outcome.report.change;
            let short = change.get(..12).unwrap_or(change);
            let payload = revlocal_publish::PagePayload {
                space,
                title: revlocal_publish::review_page_title(
                    &repo.name, short,
                    // The report carries no change title, and `review_page_title`
                    // falls back to `Review: {repo} {short_id}` rather than
                    // inventing one.
                    "",
                ),
                parent: Some(revlocal_publish::parent_page_title(&repo.name)),
                section: revlocal_publish::review_page_section(
                    &repo.name,
                    &revlocal_publish::compose_review_page(
                        outcome.report.verdict.as_deref(),
                        &outcome.report.summary,
                        &page_findings(stored),
                    ),
                ),
                publish: repo_config.trama_publish,
                // §11.5: the key comes from the Andare target's receipt, never
                // from composing one. A guessed key links this review to somebody
                // else's ticket and the reader cannot tell it is wrong.
                issue_key: None,
            };

            // The run, not a finding: re-reviewing the same change updates the
            // page it already has rather than creating a second one.
            let key = format!("trama-run-{}", run.get());
            if store
                .find_by_idempotency_key("trama", &key)
                .await
                .map_err(boxed)?
                .is_none()
            {
                store
                    .insert(&PublishAction {
                        id: PublishActionId::new(0),
                        run_id: run,
                        finding_id: None,
                        target: "trama".to_owned(),
                        capability: Capability::UpsertDoc,
                        risk: gated.assessment.class,
                        idempotency_key: key,
                        payload_json: encode(&payload, "Trama page")?,
                        status,
                        attempts: 0,
                        response_json: None,
                        external_ref: None,
                        error: None,
                        created_at: at,
                        sent_at: None,
                    })
                    .await
                    .map_err(boxed)?;
                statuses.push(status);
            }
        }
    }

    // Not silence. A finding that was already filed is the system working, but a
    // run that queued nothing and said nothing is indistinguishable from one that
    // found nothing (§18).
    if held.is_empty() && recurring > 0 {
        held.push(format!(
            "run #{}: {recurring} finding(s) are already filed, so nothing new was queued",
            run.get()
        ));
    }

    Ok(QueuedActions { statuses, held })
}

/// The findings a review page lists.
///
/// Publishable ones only, matching what reached a tracker. A page that listed
/// findings no issue exists for would send a reader looking for tickets that were
/// never filed.
fn page_findings(stored: &[(revlocal_core::Finding, bool)]) -> Vec<revlocal_publish::PageFinding> {
    stored
        .iter()
        .filter(|(_, publishable)| *publishable)
        .map(|(finding, _)| revlocal_publish::PageFinding {
            severity: finding.severity.as_str().to_owned(),
            title: finding.title.clone(),
            location: finding.file.as_ref().map(|file| match finding.line_start {
                Some(line) => format!("{file}:{line}"),
                None => file.clone(),
            }),
        })
        .collect()
}

/// Serialise one target's payload, naming what failed to encode.
///
/// A `serde_json` error here means a payload type changed shape, and the message
/// somebody sees should name which target's payload rather than leaving them to
/// guess from a line number.
fn encode<T: serde::Serialize>(payload: &T, what: &str) -> Result<String, ExecutorError> {
    serde_json::to_string(payload).map_err(|error| ExecutorError::PublishConfig {
        detail: format!("could not encode the {what} payload: {error}"),
    })
}
