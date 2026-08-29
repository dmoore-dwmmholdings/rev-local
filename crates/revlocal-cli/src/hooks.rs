//! `revlocal hooks install | uninstall` (RL-1201, SPEC §7.2, §14).
//!
//! The front end for RL-1004's installer. Everything load-bearing lives there —
//! the block markers, the `exit 0` guarantee, the LF line endings — and this
//! decides nothing except which repository and which mode.
//!
//! # `uninstall` is the reason `install` is safe to try
//!
//! A user asked to add hooks to a repository they did not write is being asked to
//! trust that it comes out cleanly. It does: the block is delimited, removed by
//! exactly its markers, and a hook rev-local wrote entirely is deleted rather than
//! left inert. That property is RL-1004's and is tested there; this exposes it so
//! the promise is reachable rather than theoretical.
//!
//! # What it prints is what it did, per file
//!
//! Not "installed" — which of created, appended to, replaced or left alone, for
//! each hook. Somebody putting a script into a repository with existing hooks
//! wants to know that theirs was appended to and not overwritten, and wants to
//! know it without opening the file.

use std::path::Path;

use revlocal_daemon::hooks::{self, HookMode, HookOutcome};
use serde::{Deserialize, Serialize};

/// Why a hooks command could not complete.
#[derive(Debug, thiserror::Error)]
pub enum HooksCommandError {
    /// The installer refused.
    #[error(transparent)]
    Hook(#[from] hooks::HookError),

    /// The report could not be serialised.
    #[error("could not render the report: {source}")]
    Unrenderable {
        /// Why.
        #[source]
        source: serde_json::Error,
    },
}

/// What happened to one hook file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookChange {
    /// The hook's path.
    pub path: String,
    /// `created`, `appended`, `replaced`, `removed`, `deleted` or `untouched`.
    pub action: String,
    /// Whether anything on disk changed.
    pub changed: bool,
}

/// What a hooks command did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HooksReport {
    /// `install` or `uninstall`.
    pub command: String,
    /// `reference` or `bare-mirror`.
    pub mode: String,
    /// The repository acted on.
    pub repo: String,
    /// One entry per hook file.
    pub hooks: Vec<HookChange>,
    /// A sentence for a person.
    pub detail: String,
}

impl HooksReport {
    /// The human output.
    pub fn render_human(&self) -> String {
        let mut out = format!("{} ({} mode)\n", self.detail, self.mode);
        for change in &self.hooks {
            out.push_str(&format!("  {:<10} {}\n", change.action, change.path));
        }
        out
    }
}

fn describe(outcome: &HookOutcome) -> HookChange {
    let (action, changed) = match outcome {
        HookOutcome::Created(_) => ("created", true),
        HookOutcome::Appended(_) => ("appended", true),
        HookOutcome::Replaced(_) => ("replaced", true),
        HookOutcome::Removed(_) => ("removed", true),
        HookOutcome::Deleted(_) => ("deleted", true),
        HookOutcome::Untouched(_) => ("untouched", false),
    };
    HookChange {
        path: outcome.path().display().to_string(),
        action: action.to_owned(),
        changed,
    }
}

/// Install rev-local's hooks into a repository (§7.2).
pub fn install(
    repo_path: &Path,
    repo_name: &str,
    mode: HookMode,
    port: u16,
    secret_env: &str,
) -> Result<HooksReport, HooksCommandError> {
    let outcomes = hooks::install(repo_path, repo_name, mode, port, secret_env)?;
    let changes: Vec<HookChange> = outcomes.iter().map(describe).collect();
    let appended = changes.iter().filter(|c| c.action == "appended").count();

    let mut detail = format!("installed {} hook(s) into {repo_name}", changes.len());
    if appended > 0 {
        // The reassurance somebody needs before running this on a repository that
        // already has hooks, said without their having to open the file.
        detail.push_str(&format!(
            "; {appended} existing hook(s) were appended to, not overwritten"
        ));
    }

    Ok(HooksReport {
        command: "install".to_owned(),
        mode: mode.as_str().to_owned(),
        repo: repo_name.to_owned(),
        hooks: changes,
        detail,
    })
}

/// Remove rev-local's hooks, leaving anything else exactly as it was.
pub fn uninstall(
    repo_path: &Path,
    repo_name: &str,
    mode: HookMode,
) -> Result<HooksReport, HooksCommandError> {
    let outcomes = hooks::uninstall(repo_path, mode)?;
    let changes: Vec<HookChange> = outcomes.iter().map(describe).collect();
    let touched = changes.iter().filter(|c| c.changed).count();

    Ok(HooksReport {
        command: "uninstall".to_owned(),
        mode: mode.as_str().to_owned(),
        repo: repo_name.to_owned(),
        hooks: changes,
        detail: if touched == 0 {
            // Not an error. Running uninstall on a repository that never had them
            // is how somebody checks, and it should answer rather than complain.
            "no rev-local hooks were installed; nothing changed".to_owned()
        } else {
            format!("removed rev-local's block from {touched} hook(s)")
        },
    })
}

/// Render for whichever output the caller asked for.
pub fn render(report: &HooksReport, json: bool) -> Result<String, HooksCommandError> {
    if json {
        return serde_json::to_string_pretty(report)
            .map_err(|source| HooksCommandError::Unrenderable { source });
    }
    Ok(report.render_human())
}
