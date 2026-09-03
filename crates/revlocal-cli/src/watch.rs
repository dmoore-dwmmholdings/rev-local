//! `revlocal watch` (RL-1201, SPEC §4.2, §7, §14).
//!
//! # The loop moved; this is the terminal's view of it
//!
//! The pass this file used to own — recover, discover, enqueue, review — now lives
//! in `revlocal_daemon::autopilot`, because the desktop app cannot depend on the
//! CLI and so grew a second, worse half of it: a button that drained the queue and
//! a discovery call that never advanced a cursor. Two implementations of one loop
//! meant "it works from the terminal" and "it works in the app" could be true
//! separately, and were.
//!
//! What is left here is the shape of the report a terminal wants, which is not the
//! shape a status line wants. `watch` prints per-repository detail — how many
//! changes each one discovered, which were skipped and why — because somebody
//! running it is debugging a specific repository. The app shows one sentence.

use std::path::Path;

use revlocal_core::{GlobalConfig, Timestamp};
use revlocal_daemon::state_machine::NullSink;
use revlocal_store::Pool;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

/// One repository's discovery pass, as the loop records it.
///
/// Re-exported rather than redefined: the terminal renders the same facts the app
/// stores, and two structs would drift.
pub use revlocal_daemon::autopilot::RepoPass;

/// Why a watch pass could not run.
#[derive(Debug, thiserror::Error)]
pub enum WatchError {
    /// The database could not be read.
    #[error("could not read the local database: {source}\n  try: revlocal db migrate")]
    Store {
        /// Why.
        #[source]
        source: Box<revlocal_store::StoreError>,
    },

    /// A repository could not be discovered.
    ///
    /// Named rather than collapsed, because one broken repository must not stop
    /// the others — the caller records this and carries on.
    #[error("{repo}: {detail}")]
    Discovery {
        /// Which repository.
        repo: String,
        /// What went wrong.
        detail: String,
    },

    /// A review could not be executed (RL-1207).
    ///
    /// Distinct from `Discovery`: finding a change and reviewing it fail for
    /// unrelated reasons, and a message that blamed the remote for an engine
    /// failure would send somebody to check their network.
    #[error("{detail}")]
    Execute {
        /// What went wrong.
        detail: String,
    },

    /// The report could not be serialised.
    #[error("could not render the report: {source}")]
    Unrenderable {
        /// Why.
        #[source]
        source: serde_json::Error,
    },
}

/// What one tick of the loop did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchReport {
    /// Repositories considered.
    pub repos: usize,
    /// Passes that ran.
    pub passes: Vec<RepoPass>,
    /// Why nothing ran, when nothing did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idle: Option<String>,
    /// Whether the kill switch is engaged.
    pub paused: bool,
    /// Runs queued this tick, per repository.
    pub queued: usize,
    /// Reviews that finished this tick (RL-1207).
    pub reviewed: Vec<revlocal_daemon::executor::RunOutcome>,
    /// Runs that did not go ahead, and why — never dropped in silence (§18).
    pub held: Vec<String>,
}

impl WatchReport {
    /// The human output.
    pub fn render_human(&self) -> String {
        if let Some(idle) = &self.idle {
            return format!("{idle}\n");
        }
        let mut out = String::new();
        // A line rather than the whole report. Returning here skipped the held
        // and reviewed sections below, so a tick where no repository was due for
        // discovery said "nothing due" over the top of runs that were held and
        // the reason they were held — which `held` exists to prevent (§18,
        // RL-1545). The live case is a checkout that has been deleted: the
        // diagnosis and its remedy went to `--json` and nowhere else.
        if self.passes.is_empty() {
            out.push_str(&format!(
                "{} repository/ies, nothing due this tick\n",
                self.repos
            ));
        }
        for pass in &self.passes {
            match &pass.error {
                Some(error) => out.push_str(&format!("  {} — FAILED: {error}\n", pass.repo)),
                None => {
                    out.push_str(&format!(
                        "  {} — {} discovered, {} recorded",
                        pass.repo, pass.discovered, pass.recorded
                    ));
                    if !pass.skipped.is_empty() {
                        out.push_str(&format!(", {} skipped", pass.skipped.len()));
                    }
                    out.push('\n');
                    // §9.4: a skipped change is recorded with its reason, and the
                    // reason is the point — "why did rev-local ignore my commit?"
                    // has an answer only if it is shown.
                    for reason in &pass.skipped {
                        out.push_str(&format!("      skipped: {reason}\n"));
                    }
                }
            }
        }
        if self.queued > 0 {
            out.push_str(&format!("\n  queued {} run(s)\n", self.queued));
        }
        for outcome in &self.reviewed {
            out.push_str(&format!(
                "  reviewed {} {} — {} ({} finding(s), {} action(s), {})\n",
                outcome.repo,
                short(&outcome.change),
                outcome.verdict.as_deref().unwrap_or("no verdict"),
                outcome.findings,
                outcome.actions,
                outcome.status
            ));
        }
        // §18: a run that did not happen is reported with its reason. "Nothing
        // ran" and "everything was over budget" are different facts.
        for held in &self.held {
            out.push_str(&format!("  held: {held}\n"));
        }
        out
    }
}

/// The first 12 characters of an identifier, for a status line.
fn short(id: &str) -> &str {
    id.get(..12).unwrap_or(id)
}

/// Run one tick of the daemon loop.
///
/// Separate from any `loop {}` so it can be called once, from a test or from
/// `--once`, without waiting for a real interval.
///
/// The only target wired here is the local report, which needs no configuration.
/// `revlocal publish` is what delivers to a tracker; a pass that has actions for
/// one says so rather than leaving findings that look filed.
pub async fn tick(
    pool: &Pool,
    config: &GlobalConfig,
    data_dir: &Path,
    at: Timestamp,
) -> Result<WatchReport, WatchError> {
    let report = revlocal_daemon::autopilot::tick(
        pool,
        config,
        &NullSink,
        data_dir,
        &[],
        at,
        &CancellationToken::new(),
    )
    .await
    .map_err(|error| match error {
        revlocal_daemon::autopilot::AutopilotError::Store { source } => {
            WatchError::Store { source }
        }
        revlocal_daemon::autopilot::AutopilotError::Execute { detail } => {
            WatchError::Execute { detail }
        }
    })?;

    Ok(WatchReport {
        repos: report.repos,
        passes: report.passes,
        // Only a reason, never a summary. "Nothing due this tick" is the pass
        // count's job, and an empty install is not an error state.
        idle: report.stopped,
        paused: report.paused,
        queued: report.queued,
        reviewed: report.reviewed,
        held: report.notes,
    })
}

/// Render for whichever output the caller asked for.
pub fn render(report: &WatchReport, json: bool) -> Result<String, WatchError> {
    if json {
        return serde_json::to_string_pretty(report)
            .map_err(|source| WatchError::Unrenderable { source });
    }
    Ok(report.render_human())
}
