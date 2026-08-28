//! `revlocal publish status` and `revlocal publish replay` (RL-710, SPEC §11.6).
//!
//! §11.6 makes partial failure normal: a run can be done with GitHub posted and
//! Andare failed. These two commands are what make that liveable from a terminal —
//! seeing which target is in which state, and asking one of them to try again
//! without touching the others.

use std::path::Path;

use revlocal_core::RunId;
use revlocal_publish::{PublishQueue, QueueConfig, RunPublishReport};

/// Why a publish command could not run.
#[derive(Debug, thiserror::Error)]
pub enum PublishCommandError {
    /// The database could not be opened.
    #[error(transparent)]
    Store(#[from] revlocal_store::StoreError),

    /// The queue could not requeue or dispatch.
    #[error(transparent)]
    Queue(#[from] revlocal_publish::queue::QueueError),

    /// The run has no publish actions at all.
    #[error("run {run} has no publish actions\n  try: check the run id with `revlocal runs list`")]
    NoActions {
        /// Which run.
        run: i64,
    },

    /// The report could not be serialized.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// Run `revlocal publish status`.
pub async fn status(database: &Path, run: i64, json: bool) -> Result<(), PublishCommandError> {
    let pool = revlocal_store::open(database).await?;
    let report = RunPublishReport::load(&pool, RunId::new(run)).await?;
    pool.close().await;

    if report.targets.is_empty() {
        return Err(PublishCommandError::NoActions { run });
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&as_json(&report))?);
    } else {
        for line in report.summary_lines() {
            println!("{line}");
        }
        if report.any_failed() {
            println!(
                "revlocal: retry one with `revlocal publish replay --run {run} --target <TARGET>`"
            );
        }
    }

    Ok(())
}

/// Run `revlocal publish replay`.
pub async fn replay(database: &Path, run: i64, target: &str) -> Result<(), PublishCommandError> {
    let pool = revlocal_store::open(database).await?;

    // No targets are registered here yet — the GitHub, Andare and Trama targets
    // are RL-703 through RL-707. Requeuing still does the useful half: the failed
    // actions become pending again, and the daemon's next dispatch pass sends
    // them. Reporting that plainly beats implying something was delivered.
    let queue = PublishQueue::new(pool.clone(), QueueConfig::default());
    let (requeued, dispatch) = queue
        .replay(RunId::new(run), target, chrono::Utc::now())
        .await?;

    let report = RunPublishReport::load(&pool, RunId::new(run)).await?;
    pool.close().await;

    println!("revlocal: requeued {requeued} action(s) for `{target}`");
    if dispatch.attempted() > 0 {
        println!(
            "revlocal: {} sent, {} to retry, {} failed",
            dispatch.sent, dispatch.retryable, dispatch.failed
        );
    } else if requeued > 0 {
        println!(
            "revlocal: no target named `{target}` is registered in this process, so \
             nothing was sent; the daemon will pick them up"
        );
    }

    for line in report.summary_lines() {
        println!("{line}");
    }

    Ok(())
}

fn as_json(report: &RunPublishReport) -> serde_json::Value {
    serde_json::json!({
        "run": report.run_id.get(),
        "blocks_completion": report.blocks_completion(),
        "targets": report.targets.iter().map(|t| serde_json::json!({
            "target": t.target,
            "state": t.state().as_str(),
            "sent": t.sent,
            "pending": t.pending,
            "awaiting_approval": t.awaiting_approval,
            "failed": t.failed,
            "skipped": t.skipped,
            "rejected": t.rejected,
            "last_error": t.last_error,
            "external_refs": t.external_refs,
        })).collect::<Vec<_>>(),
    })
}
