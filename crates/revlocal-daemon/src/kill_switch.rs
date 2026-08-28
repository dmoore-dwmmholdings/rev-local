//! The kill switch (RL-804, SPEC §12.1).
//!
//! # Hold the publish queue; do not drain it
//!
//! §12.1 is precise about this and it is the part that is easy to get wrong. The
//! run queue drains to `cancelled` — those reviews were interrupted and are not
//! worth resuming half-done. The **publish** queue is held: every pending action
//! stays exactly where it is, and goes out when somebody resumes.
//!
//! Draining it would be the destructive reading. Those actions were already
//! decided — some of them by a human in the approvals inbox — and throwing them
//! away because somebody hit pause turns a safety control into a way to lose work.
//! Pause means stop, not undo.
//!
//! # Paused is persisted, because a crash is not consent
//!
//! The state lives in the database, not in the process. If it lived in memory then
//! restarting a paused daemon would resume it, and the most likely reason a paused
//! daemon restarts is that something went wrong — which is the worst possible
//! moment to start writing to other people's systems again.
//!
//! # `--hard` is a separate verb
//!
//! Engaging the switch cancels what rev-local is supervising. `--hard` also goes
//! looking for processes it *was* supervising and lost track of, which is a
//! different and more invasive act: it signals pids recorded on runs that have
//! since finished. Those are orphan candidates rather than known orphans, so the
//! caller checks each is still alive before signalling it.

use revlocal_core::{RunId, RunStatus, Timestamp};
use tokio_util::sync::CancellationToken;

/// The audit event for engaging the switch.
pub const AUDIT_KIND_PAUSED: &str = "kill_switch_engaged";

/// The audit event for releasing it.
pub const AUDIT_KIND_RESUMED: &str = "kill_switch_released";

/// The run statuses a pause cancels.
///
/// Anything not yet finished. A run that is already `done`, `failed`, `skipped` or
/// `cancelled` is not in flight and rewriting it would falsify history.
pub const CANCELLABLE: [RunStatus; 6] = [
    RunStatus::Queued,
    RunStatus::Preparing,
    RunStatus::Reviewing,
    RunStatus::Synthesizing,
    RunStatus::Publishing,
    RunStatus::AwaitingApproval,
];

/// Whether a pause cancels a run in this state.
pub fn cancels(status: RunStatus) -> bool {
    CANCELLABLE.contains(&status)
}

/// The live half of the kill switch.
///
/// Holds the token every supervised engine is watching. Cloning it is cheap and
/// every clone observes the same cancellation, which is what lets one toggle reach
/// every run in flight.
#[derive(Debug, Clone, Default)]
pub struct KillSwitch {
    cancel: CancellationToken,
}

impl KillSwitch {
    /// A released switch.
    pub fn new() -> Self {
        Self::default()
    }

    /// The token to hand to [`revlocal_engine::supervise`].
    pub const fn token(&self) -> &CancellationToken {
        &self.cancel
    }

    /// Whether the switch is currently engaged in this process.
    ///
    /// The database is the source of truth across restarts; this is the in-process
    /// view that supervised work is actually watching.
    pub fn is_engaged(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Engage it: every in-flight engine is cancelled.
    pub fn engage(&self) {
        self.cancel.cancel();
    }

    /// Release it, returning a switch with a fresh token.
    ///
    /// A `CancellationToken` cannot be un-cancelled, and that is the right shape:
    /// work already told to stop must not silently un-stop. Resuming produces a new
    /// token for new work, and anything still holding the old one stays cancelled.
    pub fn released(&self) -> Self {
        Self::new()
    }
}

/// Whether the publish queue may dispatch right now.
///
/// The one question the queue asks. Held rather than drained — see the module
/// docs — so this is a gate on dispatch and not a filter on rows.
pub const fn may_dispatch(paused: bool) -> bool {
    !paused
}

/// What a pause did, for the audit log and for the CLI's output.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PauseReport {
    /// Runs moved to `cancelled`.
    pub runs_cancelled: Vec<RunId>,
    /// Publish actions left pending, deliberately.
    pub actions_held: usize,
}

impl PauseReport {
    /// The line the CLI prints.
    ///
    /// Names the held actions explicitly. Somebody who has just hit a kill switch
    /// needs to know what is *waiting*, not only what stopped — otherwise resuming
    /// later comes as a surprise.
    pub fn summary(&self) -> String {
        format!(
            "cancelled {} run(s); {} publish action(s) held and will be sent on resume",
            self.runs_cancelled.len(),
            self.actions_held
        )
    }
}

/// The audit detail for engaging or releasing the switch.
pub fn switch_detail(engaged: bool, report: &PauseReport, at: Timestamp) -> serde_json::Value {
    serde_json::json!({
        "engaged": engaged,
        "runs_cancelled": report.runs_cancelled.iter().map(|id| id.get()).collect::<Vec<_>>(),
        "actions_held": report.actions_held,
        "at": at.to_rfc3339(),
    })
}

/// Pids that must never be signalled, whatever a row says.
///
/// **0 is not a harmless sentinel on POSIX.** `kill(0, sig)` means "every process
/// in the caller's process group", and `killpg(0, …)` the same — so reaping a pid
/// of 0 would have rev-local killing itself and everything it had spawned. 1 is
/// init. A stored pid of either is corrupt data, not an orphan.
///
/// This is guarded here rather than at the call site because there is more than
/// one call site and only one of them has to forget.
const fn is_signallable(pid: u32) -> bool {
    pid > 1
}

/// Whether a recorded pid is still a live process.
///
/// `kill(pid, 0)` on Unix and `tasklist` on Windows — the same probe RL-601's
/// tests use, for the same reason: a pid that no longer exists is not an orphan,
/// and signalling a pid that has been reused would kill a stranger's process.
pub fn process_is_alive(pid: u32) -> bool {
    if !is_signallable(pid) {
        return false;
    }

    #[cfg(unix)]
    {
        let Ok(raw) = i32::try_from(pid) else {
            return false;
        };
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(raw), None).is_ok()
    }

    #[cfg(not(unix))]
    {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output()
            .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\"")))
    }
}

/// Signal an orphaned engine process (`kill --hard`).
///
/// Returns whether anything was signalled. A pid that is already gone is not a
/// failure — it is the normal case, and reporting it as one would make `--hard`
/// look broken every time it had nothing to do.
pub fn reap(pid: u32) -> bool {
    if !is_signallable(pid) || !process_is_alive(pid) {
        return false;
    }

    #[cfg(unix)]
    {
        let Ok(raw) = i32::try_from(pid) else {
            return false;
        };
        // The group first: RL-405 spawns engines in their own process group
        // precisely so a kill reaches whatever they spawned, and signalling only
        // the parent leaves the grandchild holding the scratch worktree open.
        let target = nix::unistd::Pid::from_raw(raw);
        if nix::sys::signal::killpg(target, nix::sys::signal::Signal::SIGKILL).is_ok() {
            return true;
        }

        // No group with that id — the process is not a group leader, which is what
        // an engine recorded by an older build looks like. Signal it alone rather
        // than leaving it running. Safe because `is_signallable` has already ruled
        // out 0, the value that would mean "my own group".
        nix::sys::signal::kill(target, nix::sys::signal::Signal::SIGKILL).is_ok()
    }

    #[cfg(not(unix))]
    {
        std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .status()
            .is_ok_and(|status| status.success())
    }
}
