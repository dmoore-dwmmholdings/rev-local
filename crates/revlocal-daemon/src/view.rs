//! The repository view both front ends render (RL-1105, SPEC §15, §14).
//!
//! Lives in the daemon rather than in either front end. `revlocal repo show` and
//! the desktop dashboard must agree about what a repository *is*, and the way to
//! guarantee that is for neither to own the answer — a front end that depended on
//! another front end for it would be the wrong shape, and a second copy would
//! drift.

use revlocal_core::{Repo, RepoConfig};
use serde::{Deserialize, Serialize};

use crate::poll::{HealthReport, PollSchedule};

/// The health report for one stored repository.
///
/// The interval comes from the repo's own `config_json` (§13.2). A row whose JSON
/// cannot be parsed falls back to the default interval rather than failing the
/// listing: a repository with a corrupt config blob is exactly the one an operator
/// needs to be able to see.
pub fn report_for(repo: &Repo) -> HealthReport {
    let configured = serde_json::from_str::<RepoConfig>(&repo.config_json)
        .map(|config| config.poll_interval_secs)
        .unwrap_or(crate::poll::DEFAULT_POLL_INTERVAL_SECS);

    // The real id, so jitter is stable for a repository across restarts — the
    // property §7.1 wants is that twenty repos on one interval do not all poll on
    // the same second, and an index would renumber them when one is deleted.
    let schedule = PollSchedule::new(repo.id, configured);
    schedule.health_report(&repo.name)
}

/// A repository's settings, beside its polling health.
///
/// `repo show` reported health and nothing else, so there was no way to ask what
/// autonomy a repository was on — the single setting that decides whether it
/// writes to somebody else's systems. "Is this repo going to publish?" had no
/// answer from the command line, which is a poor property for the command named
/// `show`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoView {
    /// The repository's id, so a card can address it (§15 screen 1 → screen 2).
    pub id: i64,
    /// The repository's name.
    pub repo: String,
    /// Which VCS backs it.
    pub kind: String,
    /// Which engine reviews it (decision D3 — per repo, not global).
    pub engine: String,
    /// What it is allowed to do without asking (§12.2).
    pub autonomy: String,
    /// Whether triggers fire for it at all.
    pub enabled: bool,
    /// Where it is on disk.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_path: Option<String>,
    /// Its polling health.
    pub health: HealthReport,
}

impl RepoView {
    /// Build the view for one stored repository.
    pub fn of(repo: &Repo) -> Self {
        Self {
            id: repo.id.get(),
            repo: repo.name.clone(),
            kind: repo.kind.as_str().to_owned(),
            engine: repo.engine.as_str().to_owned(),
            autonomy: repo.autonomy.as_str().to_owned(),
            enabled: repo.enabled,
            local_path: repo.local_path.clone(),
            health: report_for(repo),
        }
    }
}
