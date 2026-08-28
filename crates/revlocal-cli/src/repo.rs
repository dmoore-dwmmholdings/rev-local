//! `revlocal repo show` (RL-1002, SPEC §7.1, §14).
//!
//! §7.1 says a repository that keeps failing to poll "reports repo health as
//! `degraded` in the UI". The UI is RL-1101 and later; this is the headless half,
//! and it exists for the reason every other `--json` surface does: a state nobody
//! can observe is a state nobody can act on.
//!
//! A degraded repository is the one worth being able to see from a script. It is
//! still configured, still polling, and quietly seeing nothing — which looks
//! exactly like a repository where nobody has committed lately.
//!
//! Nothing here polls. Showing state must not be able to change it.

use revlocal_core::{Repo, RepoConfig};
use revlocal_daemon::poll::{HealthReport, PollSchedule};
use revlocal_store::{Pool, RepoStore};

/// Why `repo show` could not report.
#[derive(Debug, thiserror::Error)]
pub enum RepoCommandError {
    /// The database could not be read.
    #[error("could not read the repository list: {source}\n  try: revlocal db migrate")]
    Store {
        /// Why.
        #[source]
        source: Box<revlocal_store::StoreError>,
    },

    /// No repository by that name is configured.
    #[error("no repository named {name} is configured\n  try: revlocal repo show")]
    NoSuchRepo {
        /// The name asked for.
        name: String,
    },

    /// The report could not be serialised.
    #[error("could not render the report: {source}")]
    Unrenderable {
        /// Why.
        #[source]
        source: serde_json::Error,
    },
}

/// The health report for one stored repository.
///
/// The interval comes from the repo's own `config_json` (§13.2). A row whose JSON
/// cannot be parsed falls back to the default interval rather than failing the
/// listing: a repository with a corrupt config blob is exactly the one an operator
/// needs to be able to see.
pub fn report_for(repo: &Repo) -> HealthReport {
    let configured = serde_json::from_str::<RepoConfig>(&repo.config_json)
        .map(|config| config.poll_interval_secs)
        .unwrap_or(revlocal_daemon::poll::DEFAULT_POLL_INTERVAL_SECS);

    // The real id, so jitter is stable for a repository across restarts — the
    // property §7.1 wants is that twenty repos on one interval do not all poll on
    // the same second, and an index would renumber them when one is deleted.
    let schedule = PollSchedule::new(repo.id, configured);
    schedule.health_report(&repo.name)
}

/// Run `revlocal repo show`.
pub async fn run(pool: &Pool, name: Option<&str>, json: bool) -> Result<String, RepoCommandError> {
    let repos = RepoStore::new(pool)
        .list()
        .await
        .map_err(|source| RepoCommandError::Store {
            source: Box::new(source),
        })?;

    let mut all: Vec<HealthReport> = repos.iter().map(report_for).collect();
    if let Some(name) = name {
        all.retain(|report| report.repo == name);
        if all.is_empty() {
            return Err(RepoCommandError::NoSuchRepo {
                name: name.to_owned(),
            });
        }
    }

    if json {
        // Exactly one JSON document reaches stdout and nothing else.
        return serde_json::to_string_pretty(&all)
            .map_err(|source| RepoCommandError::Unrenderable { source });
    }

    Ok(render_human(&all))
}

/// The human form: one repository per block, notes last.
fn render_human(reports: &[HealthReport]) -> String {
    if reports.is_empty() {
        return "no repositories are configured\n  try: revlocal repo add --help\n".to_owned();
    }

    let mut out = String::new();
    for report in reports {
        out.push_str(&format!("{}  [{}]\n", report.repo, report.health.as_str()));
        out.push_str(&format!(
            "  poll every {}s, next in about {}s\n",
            report.poll_interval_secs, report.next_poll_in_secs
        ));
        if report.consecutive_failures > 0 {
            out.push_str(&format!(
                "  {} consecutive failure(s)\n",
                report.consecutive_failures
            ));
        }
        if let Some(error) = &report.last_error {
            out.push_str(&format!("  last error: {error}\n"));
        }
        for note in &report.notes {
            out.push_str(&format!("  note: {note}\n"));
        }
    }
    out
}
