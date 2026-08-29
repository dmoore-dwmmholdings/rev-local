//! `revlocal pause | resume | kill --hard` (RL-1201, SPEC §12.1, §14).
//!
//! §15 requires the kill switch to be reachable from every screen and the tray.
//! §14 requires it from the command line, which is the one that still works when
//! the UI is what has gone wrong — and the one a script can call.
//!
//! # Three commands, because they are three different acts
//!
//! `pause` stops new work and **holds** publish actions. `resume` releases both.
//! `kill --hard` additionally reaps engine processes by pid.
//!
//! Collapsing `pause` and `kill --hard` would be the tempting simplification and
//! it is wrong: a pause is reversible and loses nothing, while a hard kill takes a
//! running engine's output with it. Somebody reaching for the gentler one should
//! not get the other because the CLI decided they meant the same thing.
//!
//! # Paused state is persisted, so a restart does not undo an emergency
//!
//! RL-804 made the state survive a restart. That matters most in the case it was
//! built for: somebody pauses because something is wrong, the daemon is restarted
//! while they investigate, and it must not quietly start reviewing again.

use revlocal_core::Timestamp;
use revlocal_daemon::kill_switch::PauseReport;
use revlocal_store::{Pool, SettingStore};
use serde::{Deserialize, Serialize};

/// Why a control command could not complete.
#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    /// The database could not be read or written.
    #[error("could not reach the local database: {source}\n  try: revlocal db migrate")]
    Store {
        /// Why.
        #[source]
        source: Box<revlocal_store::StoreError>,
    },

    /// The report could not be serialised.
    #[error("could not render the report: {source}")]
    Unrenderable {
        /// Why.
        #[source]
        source: serde_json::Error,
    },
}

/// What a control command did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlReport {
    /// `pause`, `resume` or `kill`.
    pub action: String,
    /// Whether the switch is engaged after this.
    pub paused: bool,
    /// Whether the state changed, as opposed to already being that way.
    ///
    /// Distinct from `paused` so a script can tell "I stopped it" from "it was
    /// already stopped" — which matters when two operators reach for the switch
    /// at once and only one of them should be writing the incident note.
    pub changed: bool,
    /// Runs cancelled, by id.
    pub runs_cancelled: Vec<i64>,
    /// Publish actions held, awaiting a resume.
    pub actions_held: usize,
    /// Engine processes reaped. Only `kill --hard` reaps.
    pub processes_reaped: usize,
    /// A sentence for a person.
    pub detail: String,
}

impl ControlReport {
    /// The line the human path prints.
    pub fn summary_line(&self) -> String {
        self.detail.clone()
    }
}

/// Engage the kill switch (SPEC §12.1).
pub async fn pause(pool: &Pool, at: Timestamp) -> Result<ControlReport, ControlError> {
    let settings = SettingStore::new(pool);
    let already = settings.is_paused().await.map_err(boxed)?;

    settings.set_paused(true, at).await.map_err(boxed)?;

    // TODO(RL-1201): cancelling in-flight runs and holding actions needs the
    // daemon's run registry, which arrives with `watch`. Reported as zero rather
    // than omitted, so the shape does not change when it starts counting.
    let report = PauseReport::default();

    Ok(ControlReport {
        action: "pause".to_owned(),
        paused: true,
        changed: !already,
        runs_cancelled: Vec::new(),
        actions_held: report.actions_held,
        processes_reaped: 0,
        detail: if already {
            "already paused; nothing changed".to_owned()
        } else {
            format!("paused. {}", report.summary())
        },
    })
}

/// Release the kill switch.
pub async fn resume(pool: &Pool, at: Timestamp) -> Result<ControlReport, ControlError> {
    let settings = SettingStore::new(pool);
    let was_paused = settings.is_paused().await.map_err(boxed)?;

    settings.set_paused(false, at).await.map_err(boxed)?;

    Ok(ControlReport {
        action: "resume".to_owned(),
        paused: false,
        changed: was_paused,
        runs_cancelled: Vec::new(),
        actions_held: 0,
        processes_reaped: 0,
        detail: if was_paused {
            "resumed. Held publish actions will be sent.".to_owned()
        } else {
            "was not paused; nothing changed".to_owned()
        },
    })
}

/// Stop everything and reap engine processes by pid (§12.1's `--hard`).
///
/// Reaping takes a running engine's output with it, which is why this is a
/// separate command rather than a flag on `pause`.
pub async fn kill_hard(pool: &Pool, at: Timestamp) -> Result<ControlReport, ControlError> {
    let mut report = pause(pool, at).await?;

    // TODO(RL-1201): the pids come from `run.engine_pid`, which migration 0006
    // added for exactly this. Wiring it needs the run registry `watch` brings.
    let reaped = 0_usize;

    report.action = "kill".to_owned();
    report.processes_reaped = reaped;
    report.detail = format!(
        "{}. {reaped} engine process(es) reaped; any output they had not written \
         is lost",
        report.detail.trim_end_matches('.')
    );
    Ok(report)
}

/// Report whether the switch is engaged, changing nothing.
pub async fn status(pool: &Pool) -> Result<ControlReport, ControlError> {
    let paused = SettingStore::new(pool).is_paused().await.map_err(boxed)?;

    Ok(ControlReport {
        action: "status".to_owned(),
        paused,
        changed: false,
        runs_cancelled: Vec::new(),
        actions_held: 0,
        processes_reaped: 0,
        detail: if paused {
            "paused; `revlocal resume` releases it".to_owned()
        } else {
            "running".to_owned()
        },
    })
}

/// Render a report for whichever output the caller asked for.
pub fn render(report: &ControlReport, json: bool) -> Result<String, ControlError> {
    if json {
        return serde_json::to_string_pretty(report)
            .map_err(|source| ControlError::Unrenderable { source });
    }
    Ok(report.summary_line())
}

fn boxed(source: revlocal_store::StoreError) -> ControlError {
    ControlError::Store {
        source: Box::new(source),
    }
}
