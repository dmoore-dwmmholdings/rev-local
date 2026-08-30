//! The IPC command surface (RL-1101, SPEC §4.2, §15).
//!
//! # A thin delegation layer, and how that is kept true
//!
//! §15's fourth criterion is that this layer has **no business logic**. That is
//! easy to say and easy to erode: the first time a screen needs a number the
//! daemon does not expose, the cheap fix is to compute it here, and six months
//! later the UI and the CLI disagree about what "queue depth" means because one of
//! them is doing its own arithmetic.
//!
//! Two things hold the line. The layer cannot reach a webview, so it cannot grow
//! UI-shaped decisions; and every request is a named variant whose handler is
//! required to be a single delegation. `ipc_commands_are_thin_delegations` reads
//! this file and fails on a handler that branches, computes or filters.
//!
//! # Errors cross the boundary as data
//!
//! A Tauri command that returns `Err(String)` gives the front end a sentence to
//! display and nothing to branch on. §18 wants a user-visible error to say what to
//! do; a UI can only do that if it can tell "no such repository" from "the daemon
//! is not running". So [`IpcError`] is a tagged enum and its remediation travels
//! with it.

use serde::{Deserialize, Serialize};

/// Every command the front end may invoke.
///
/// One enum rather than a scattering of free functions: the surface is a contract
/// with the front end, and a contract that has to be discovered by grepping for an
/// attribute is one that drifts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum IpcRequest {
    /// The dashboard's whole snapshot (§15 screen 1).
    ///
    /// One command rather than four, because the dashboard's four regions are one
    /// view of one moment: fetched separately they can disagree — a budget bar
    /// from before a run finished beside a card from after it — and an operations
    /// console that shows two moments at once is worse than one that is a second
    /// stale.
    Dashboard,
    /// Every configured repository and its polling health.
    ListRepos,
    /// One repository's detail (§15 screen 2).
    GetRepo {
        /// Which repository.
        repo_id: i64,
    },
    /// Recent runs for the dashboard's activity feed.
    ListRuns {
        /// Optional repository filter.
        repo_id: Option<i64>,
        /// How many, most recent first.
        limit: u32,
    },
    /// One run's detail (§15 screen 3).
    GetRun {
        /// Which run.
        run_id: i64,
    },
    /// Findings across repositories (§15 screen 4).
    ListFindings {
        /// Optional repository filter.
        repo_id: Option<i64>,
        /// How many.
        limit: u32,
    },
    /// The approvals inbox (§12.4, §15 screen 5).
    ListApprovals,
    /// Stop everything, now (§12.1).
    ///
    /// §15 requires the kill switch be reachable from every screen and the tray,
    /// which is a UI concern; that it is one command with no arguments is what
    /// makes that possible.
    KillSwitch,
    /// Whether the kill switch is engaged.
    KillSwitchState,
    /// Set the global autonomy ceiling (§12.2, §15's mode selector).
    ///
    /// Mutating, and named as such: §15 requires every destructive or outbound
    /// action to name its target, and widening autonomy is the setting that
    /// decides whether anything is written to somebody else's systems at all.
    SetMode {
        /// `off` | `dry_run` | `auto_low_ask_high` | `auto`.
        mode: String,
    },
}

impl IpcRequest {
    /// The command's name on the wire.
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Dashboard => "dashboard",
            Self::ListRepos => "list_repos",
            Self::GetRepo { .. } => "get_repo",
            Self::ListRuns { .. } => "list_runs",
            Self::GetRun { .. } => "get_run",
            Self::ListFindings { .. } => "list_findings",
            Self::ListApprovals => "list_approvals",
            Self::KillSwitch => "kill_switch",
            Self::KillSwitchState => "kill_switch_state",
            Self::SetMode { .. } => "set_mode",
        }
    }

    /// Whether this command changes anything.
    ///
    /// §15 requires every destructive or outbound action to name its target
    /// explicitly. The front end cannot enforce that for a command it cannot tell
    /// apart from a read, so the classification lives with the command rather than
    /// in the screen that happens to call it.
    pub const fn mutates(&self) -> bool {
        matches!(self, Self::KillSwitch | Self::SetMode { .. })
    }
}

/// What a command answers with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IpcResponse {
    /// The command this answers.
    pub command: String,
    /// The payload, shaped by the command.
    pub data: serde_json::Value,
}

impl IpcResponse {
    /// A response for `command` carrying `data`.
    pub fn new(command: &str, data: serde_json::Value) -> Self {
        Self {
            command: command.to_owned(),
            data,
        }
    }
}

/// Why a command could not be answered.
///
/// A tagged enum rather than a string, so the front end can branch. A UI that can
/// only display a sentence cannot offer the remedy §18 asks for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum IpcError {
    /// The daemon is not running, or has stopped.
    #[error("the daemon is not running")]
    DaemonUnavailable {
        /// What the UI should suggest.
        remediation: String,
    },
    /// No repository with that id.
    #[error("no repository with id {repo_id}")]
    NoSuchRepo {
        /// The id asked for.
        repo_id: i64,
    },
    /// No run with that id.
    #[error("no run with id {run_id}")]
    NoSuchRun {
        /// The id asked for.
        run_id: i64,
    },
    /// The store could not be read.
    #[error("could not read the database: {detail}")]
    Store {
        /// What went wrong.
        detail: String,
        /// What the UI should suggest.
        remediation: String,
    },
}

impl IpcError {
    /// What to offer the user, where there is something to offer.
    pub fn remediation(&self) -> Option<&str> {
        match self {
            Self::DaemonUnavailable { remediation } | Self::Store { remediation, .. } => {
                Some(remediation)
            }
            // Naming an id that is not there is a front-end bug or a stale window,
            // and there is no action for the user to take.
            Self::NoSuchRepo { .. } | Self::NoSuchRun { .. } => None,
        }
    }

    /// Whether retrying the same command could succeed.
    ///
    /// The UI shows a retry button for these and not for the others, which is the
    /// whole reason this is an enum rather than a string.
    pub const fn is_retryable(&self) -> bool {
        matches!(self, Self::DaemonUnavailable { .. } | Self::Store { .. })
    }
}
