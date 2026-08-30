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
pub use revlocal_daemon::view::RepoView;
use revlocal_store::{Pool, RepoStore};

/// Why a `repo` command could not complete.
#[derive(Debug, thiserror::Error)]
pub enum RepoCommandError {
    /// The database could not be read.
    #[error("could not read the repository list: {source}\n  try: revlocal db migrate")]
    Store {
        /// Why.
        #[source]
        source: Box<revlocal_store::StoreError>,
    },

    /// A name that is already taken.
    ///
    /// §5 makes `repo.name` unique, and the name is what hooks send and what
    /// findings are fingerprinted against — so silently accepting a second one
    /// would merge two repositories' history.
    #[error(
        "a repository named {name} is already configured\n  try: pick another \
         --name, or `revlocal repo remove {name}` first"
    )]
    NameTaken {
        /// The name asked for.
        name: String,
    },

    /// A value that is not one of the ones that exist.
    #[error("{what} `{given}` is not one of: {valid}\n  try: one of those")]
    NotAValue {
        /// Which field.
        what: String,
        /// What was given.
        given: String,
        /// What is allowed.
        valid: String,
    },

    /// A `key=value` that is neither.
    #[error(
        "`{given}` is not a `key=value` pair\n  try: revlocal repo set <name> \
         engine=claude autonomy=dry_run"
    )]
    NotAPair {
        /// What was given.
        given: String,
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

    let mut all: Vec<RepoView> = repos.iter().map(RepoView::of).collect();
    if let Some(name) = name {
        all.retain(|view| view.repo == name);
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
fn render_human(views: &[RepoView]) -> String {
    if views.is_empty() {
        return "no repositories are configured\n  try: revlocal repo add --help\n".to_owned();
    }

    let mut out = String::new();
    for view in views {
        let report = &view.health;
        out.push_str(&format!("{}  [{}]\n", report.repo, report.health.as_str()));
        // Autonomy first among the settings. It is the one that decides whether
        // this repository writes to anybody else's systems, and it is the one
        // somebody is checking when they run this.
        out.push_str(&format!(
            "  {} · engine {} · autonomy {}{}\n",
            view.kind,
            view.engine,
            view.autonomy,
            if view.enabled { "" } else { " · DISABLED" }
        ));
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

// --- add | list | remove | set (RL-1201, SPEC §14) ------------------------

use revlocal_core::{AutonomyMode, EngineKind, RepoKind};

/// Parse one of a string enum's values, naming all of them when it is none.
///
/// A message that says only "invalid kind" makes somebody go and find the list.
/// The list is three words long; printing it costs nothing and saves a lookup.
fn parse_enum<T>(what: &str, given: &str, all: &[T]) -> Result<T, RepoCommandError>
where
    T: Copy,
    T: AsStr,
{
    all.iter()
        .find(|value| value.as_str() == given)
        .copied()
        .ok_or_else(|| RepoCommandError::NotAValue {
            what: what.to_owned(),
            given: given.to_owned(),
            valid: all
                .iter()
                .map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        })
}

/// The `as_str` every string enum in core already has.
pub trait AsStr {
    /// Its wire spelling.
    fn as_str(&self) -> &'static str;
}

impl AsStr for RepoKind {
    fn as_str(&self) -> &'static str {
        (*self).as_str()
    }
}
impl AsStr for EngineKind {
    fn as_str(&self) -> &'static str {
        (*self).as_str()
    }
}
impl AsStr for AutonomyMode {
    fn as_str(&self) -> &'static str {
        (*self).as_str()
    }
}

/// Every value each field accepts, for parsing and for error messages.
const KINDS: [RepoKind; 3] = [RepoKind::Git, RepoKind::GitHub, RepoKind::Svn];
const ENGINES: [EngineKind; 3] = [EngineKind::Claude, EngineKind::Codex, EngineKind::Mock];
const MODES: [AutonomyMode; 4] = [
    AutonomyMode::Off,
    AutonomyMode::DryRun,
    AutonomyMode::AutoLowAskHigh,
    AutonomyMode::Auto,
];

/// What a `repo` write did.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RepoWriteReport {
    /// `add`, `remove` or `set`.
    pub action: String,
    /// The repository's id.
    pub repo_id: i64,
    /// Its name.
    pub name: String,
    /// A sentence for a person.
    pub detail: String,
}

/// Add a repository (§14).
///
/// `autonomy` defaults to `dry_run` rather than anything that acts. A repository
/// added a moment ago has never been reviewed, nobody has seen its findings, and
/// the first thing it does should not be to publish them.
#[allow(clippy::too_many_arguments)]
pub async fn add(
    pool: &Pool,
    path_or_url: &str,
    kind: &str,
    name: Option<&str>,
    engine: &str,
    autonomy: &str,
    at: revlocal_core::Timestamp,
) -> Result<RepoWriteReport, RepoCommandError> {
    let kind = parse_enum("kind", kind, &KINDS)?;
    let engine = parse_enum("engine", engine, &ENGINES)?;
    let autonomy = parse_enum("autonomy", autonomy, &MODES)?;

    // A name derived from the path is what somebody expects when they did not
    // give one, and it is what appears in every finding's fingerprint — so it is
    // derived once, here, rather than at each use.
    let derived = name
        .map(str::to_owned)
        .unwrap_or_else(|| derive_name(path_or_url));

    let store = RepoStore::new(pool);
    if store
        .list()
        .await
        .map_err(boxed)?
        .iter()
        .any(|existing| existing.name == derived)
    {
        return Err(RepoCommandError::NameTaken { name: derived });
    }

    let local_path = (!looks_like_url(path_or_url)).then(|| path_or_url.to_owned());
    let remote_url = looks_like_url(path_or_url).then(|| path_or_url.to_owned());

    let repo = store
        .insert(&Repo {
            id: revlocal_core::RepoId::new(0),
            name: derived.clone(),
            kind,
            local_path,
            remote_url,
            default_branch: None,
            engine,
            autonomy,
            enabled: true,
            config_json: "{}".to_owned(),
            created_at: at,
            updated_at: at,
        })
        .await
        .map_err(boxed)?;

    Ok(RepoWriteReport {
        action: "add".to_owned(),
        repo_id: repo.id.get(),
        name: derived.clone(),
        detail: format!(
            "added {derived} ({}), engine {}, autonomy {} — nothing is published \
             until you widen it",
            kind.as_str(),
            engine.as_str(),
            autonomy.as_str()
        ),
    })
}

/// Whether this looks like a remote rather than a path on disk.
fn looks_like_url(value: &str) -> bool {
    value.contains("://") || value.starts_with("git@")
}

/// A repository's name, when the user did not give one.
fn derive_name(path_or_url: &str) -> String {
    path_or_url
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .rsplit(['/', '\\'])
        .find(|segment| !segment.is_empty())
        .unwrap_or("repo")
        .to_owned()
}

/// Remove a repository (§14).
pub async fn remove(pool: &Pool, name: &str) -> Result<RepoWriteReport, RepoCommandError> {
    let store = RepoStore::new(pool);
    let repo = store
        .list()
        .await
        .map_err(boxed)?
        .into_iter()
        .find(|repo| repo.name == name)
        .ok_or_else(|| RepoCommandError::NoSuchRepo {
            name: name.to_owned(),
        })?;

    store.delete(repo.id).await.map_err(boxed)?;

    Ok(RepoWriteReport {
        action: "remove".to_owned(),
        repo_id: repo.id.get(),
        name: name.to_owned(),
        detail: format!(
            "removed {name}. Its runs and findings are gone with it; hooks in the \
             working copy are not — `revlocal hooks uninstall` removes those"
        ),
    })
}

/// Change settings on a repository (§14's `set <name> key=value...`).
pub async fn set(
    pool: &Pool,
    name: &str,
    pairs: &[String],
    at: revlocal_core::Timestamp,
) -> Result<RepoWriteReport, RepoCommandError> {
    let store = RepoStore::new(pool);
    let mut repo = store
        .list()
        .await
        .map_err(boxed)?
        .into_iter()
        .find(|repo| repo.name == name)
        .ok_or_else(|| RepoCommandError::NoSuchRepo {
            name: name.to_owned(),
        })?;

    let mut changed = Vec::new();
    for pair in pairs {
        let (key, value) = pair
            .split_once('=')
            .ok_or_else(|| RepoCommandError::NotAPair {
                given: pair.clone(),
            })?;

        match key {
            "engine" => {
                repo.engine = parse_enum("engine", value, &ENGINES)?;
                changed.push(format!("engine={value}"));
            }
            "autonomy" => {
                repo.autonomy = parse_enum("autonomy", value, &MODES)?;
                changed.push(format!("autonomy={value}"));
            }
            "enabled" => {
                repo.enabled = value == "true";
                changed.push(format!("enabled={}", repo.enabled));
            }
            "default_branch" => {
                repo.default_branch = Some(value.to_owned());
                changed.push(format!("default_branch={value}"));
            }
            other => {
                return Err(RepoCommandError::NotAValue {
                    what: "key".to_owned(),
                    given: other.to_owned(),
                    valid: "engine, autonomy, enabled, default_branch".to_owned(),
                })
            }
        }
    }

    repo.updated_at = at;
    store.update(&repo).await.map_err(boxed)?;

    Ok(RepoWriteReport {
        action: "set".to_owned(),
        repo_id: repo.id.get(),
        name: name.to_owned(),
        detail: format!("{name}: {}", changed.join(", ")),
    })
}

/// Render a write report.
pub fn render_write(report: &RepoWriteReport, json: bool) -> Result<String, RepoCommandError> {
    if json {
        return serde_json::to_string_pretty(report)
            .map_err(|source| RepoCommandError::Unrenderable { source });
    }
    Ok(report.detail.clone())
}

fn boxed(source: revlocal_store::StoreError) -> RepoCommandError {
    RepoCommandError::Store {
        source: Box::new(source),
    }
}
