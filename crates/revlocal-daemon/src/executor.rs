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
            )
            .await?));
        }
    };

    let context = match materialize(&repo, &change, scratch.path()).await {
        Ok(context) => context,
        Err(detail) => {
            scratch.mark_failed();
            return Ok(Err(
                fail(pool, sink, run.id, RunStatus::Preparing, &detail).await?
            ));
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

    let repo_config = serde_json::from_str::<RepoConfig>(&repo.config_json).unwrap_or_default();
    let suppressions = SuppressionStore::new(pool)
        .list_for_repo(repo.id)
        .await
        .map_err(boxed)?;

    let change_with_stat = Change {
        diff_stat: context.stat,
        ..change.clone()
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
    )
    .await;

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
    finished.skip_reason = outcome.report.skip_reason.clone();
    finished.usage = outcome.report.usage;
    finished.verdict = outcome
        .report
        .verdict
        .as_deref()
        .and_then(|v| v.parse().ok());
    finished.summary = Some(outcome.report.summary.clone());
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

    let awaiting = actions.contains(&revlocal_core::PublishActionStatus::AwaitingApproval);
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
        actions: actions.len(),
        // The failure first: a run that failed and a run that was salvaged are
        // both worth a line, and only one of them produced a review.
        detail: outcome
            .report
            .failure
            .clone()
            .or_else(|| outcome.report.degraded.clone()),
    }))
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
) -> Result<String, ExecutorError> {
    RunStore::new(pool)
        .mark_interrupted(run, detail)
        .await
        .map_err(boxed)?;
    // The store already moved it; the event is what the UI needs.
    sink.emit(crate::state_machine::RunEvent::StageChanged {
        run,
        from,
        to: RunStatus::Failed,
    });
    Ok(format!("run #{}: {detail}", run.get()))
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
) -> Result<Vec<revlocal_core::PublishActionStatus>, ExecutorError> {
    let mode = AutonomyMode::effective(config.global.mode, repo.autonomy);
    if mode == AutonomyMode::Off {
        // Not an error and not a silent drop: `off` means no actions, and the
        // findings are still stored for somebody to read.
        return Ok(Vec::new());
    }

    let store = PublishActionStore::new(pool);
    let mut statuses = Vec::new();

    for (finding, publishable) in stored {
        if !publishable {
            continue;
        }

        let gated = gating::gate(
            ActionIntent::CreateIssue,
            Some(finding.confidence),
            // §12.3's first-use rule needs history this pass does not yet read;
            // `false` is the cautious end of it — a first filing is treated as a
            // first filing, which raises the risk rather than lowering it.
            false,
            gating::GateContext {
                mode,
                run_degraded: outcome.report.degraded.is_some(),
                actions_in_last_hour: 0,
                burst_threshold: config.global.burst_threshold,
            },
        );

        let Some(status) = gated.initial_status() else {
            continue;
        };

        let payload = serde_json::json!({
            "title": finding.title,
            "body": finding.body,
            "rev-local-fingerprint": finding.fingerprint,
        });

        store
            .insert(&PublishAction {
                id: PublishActionId::new(0),
                run_id: run,
                finding_id: Some(finding.id),
                target: "andare".to_owned(),
                capability: Capability::CreateIssue,
                risk: gated.assessment.class,
                // §11.6: the fingerprint makes redelivery safe, and makes a
                // re-reviewed change reuse the issue rather than file a second.
                idempotency_key: format!("andare-{}", finding.fingerprint),
                payload_json: payload.to_string(),
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

    Ok(statuses)
}
