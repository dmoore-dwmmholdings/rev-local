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

use crate::poll::{HealthReport, PollSchedule};
pub use crate::view::RepoView;
use revlocal_core::{Repo, RepoConfig};
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
        .unwrap_or(crate::poll::DEFAULT_POLL_INTERVAL_SECS);

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

/// Whether this repository's checkout is still on disk (RL-1513, RL-1515).
///
/// A repository added by URL has no local path and is not the subject of this
/// question, so it passes.
///
/// `Path::exists` answers `false` for a path that exists but cannot be read, and
/// for this purpose that is the same answer: rev-local cannot review it either
/// way, and what somebody needs is to be told to go and look.
///
/// Lives here rather than beside either caller because it has two, and the first
/// version had one — the check went into the loop's discovery pass, which stopped
/// *new* runs being queued and did nothing about the fifty-one already waiting.
pub fn checkout_is_present(repo: &revlocal_core::Repo) -> bool {
    repo.local_path
        .as_deref()
        .is_none_or(|path| !path.is_empty() && std::path::Path::new(path).exists())
}

/// The line shown when it is not.
pub fn checkout_missing_detail(repo: &revlocal_core::Repo) -> String {
    format!(
        "{}: the checkout is gone — {}\n  try: put it back, point the repository at its new location, or disable it",
        repo.name,
        repo.local_path.as_deref().unwrap_or("no path recorded")
    )
}

/// Queued runs split into what can run and what cannot (RL-1554).
///
/// # Why the total on its own is misleading
///
/// A run whose repository has no checkout is held every tick and will be held
/// every tick forever: a hold is not an attempt, so `max_attempts` never applies
/// and nothing gives up on it. On the live install 43 of 51 queued runs were
/// like that, and every count — the queue panel, `doctor`, the dashboard's
/// "changes waiting" warning — reported all 51 as work in progress.
///
/// The number then never goes down. Review all eight real changes and it still
/// says 43 are waiting, which turns a warning into something somebody learns to
/// ignore.
///
/// Returns `(runnable, blocked)`. Counted per repository rather than with a
/// join, because the "can this run" test is a filesystem check rather than
/// anything the database knows.
pub async fn queued_split(pool: &Pool) -> Result<(u32, u32), revlocal_store::StoreError> {
    let runs = revlocal_store::RunStore::new(pool);
    let mut runnable = 0_u32;
    let mut blocked = 0_u32;

    for repo in RepoStore::new(pool).list().await? {
        let queued = runs
            .count_matching(Some(repo.id), Some(revlocal_core::RunStatus::Queued))
            .await?;
        if queued == 0 {
            continue;
        }
        // The same predicate the drain uses, rather than a second opinion about
        // what "reachable" means — two of those disagree the first time either
        // changes.
        if repo.enabled && checkout_is_present(&repo) {
            runnable = runnable.saturating_add(queued);
        } else {
            blocked = blocked.saturating_add(queued);
        }
    }

    Ok((runnable, blocked))
}

/// Add a repository (§14).
///
/// `autonomy` of `None` means "whatever this install's default is", which is
/// `dry_run` until somebody says otherwise (see [`default_autonomy`]). A
/// repository added a moment ago has never been reviewed and nobody has seen its
/// findings, so the first thing it does should not be to publish them — but an
/// install that has decided to trust the loop says so once rather than once per
/// repository, which is the whole point of scanning for them.
#[allow(clippy::too_many_arguments)]
pub async fn add(
    pool: &Pool,
    path_or_url: &str,
    kind: &str,
    name: Option<&str>,
    engine: &str,
    autonomy: Option<&str>,
    at: revlocal_core::Timestamp,
) -> Result<RepoWriteReport, RepoCommandError> {
    let kind = parse_enum("kind", kind, &KINDS)?;
    let engine = parse_enum("engine", engine, &ENGINES)?;
    let autonomy = match autonomy {
        Some(given) => parse_enum("autonomy", given, &MODES)?,
        None => default_autonomy(pool).await?,
    };

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
    // Added by URL, the URL *is* the remote. Added by path — which is how the
    // desktop app and most people add one — the remote has to be asked for, and
    // until RL-1514 nobody asked: every repository on the live install had an
    // empty `remote_url` and the GitHub target could never fire.
    //
    // A repository with no `origin`, or one git cannot read, keeps `None`. That
    // is a normal state for a local-only checkout and not a reason to refuse to
    // add it.
    let remote_url = match &local_path {
        Some(path) if kind == revlocal_core::RepoKind::Git => {
            revlocal_vcs::origin_url(&revlocal_vcs::GitRunner::new(), std::path::Path::new(path))
                .await
                .unwrap_or_default()
        }
        _ => looks_like_url(path_or_url).then(|| path_or_url.to_owned()),
    };

    // The branch this checkout is actually on, and the patterns that decide what
    // is ever discovered on it (REVL-200).
    //
    // `RepoConfig::branches` defaults to `["main", "release/*"]`. A repository on
    // `master` matched none of them, discovered nothing, and reported itself
    // healthy — rev-local's own repository, with hundreds of commits, read as one
    // nobody had touched. Asking git once, here, is what stops that; the
    // alternative was somebody running `repo set <name> default_branch=master`
    // for each of thirty repositories a scan had just added, having first worked
    // out that this was the problem.
    let (default_branch, config_json) = match &local_path {
        Some(path) if kind == revlocal_core::RepoKind::Git => {
            let found = revlocal_vcs::head_branch(
                &revlocal_vcs::GitRunner::new(),
                std::path::Path::new(path),
            )
            .await
            .unwrap_or_default();
            (found.clone(), branches_config(found.as_deref()))
        }
        // Detached, remote-only or Subversion: nothing to ask, and the defaults
        // are what those cases had before this existed.
        _ => (None, "{}".to_owned()),
    };

    let repo = store
        .insert(&Repo {
            id: revlocal_core::RepoId::new(0),
            name: derived.clone(),
            kind,
            local_path,
            remote_url,
            default_branch,
            engine,
            autonomy,
            enabled: true,
            config_json,
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
            "added {derived} ({}), engine {}, autonomy {}{}",
            kind.as_str(),
            engine.as_str(),
            autonomy.as_str(),
            publishing_note(autonomy)
        ),
    })
}

/// The stored `config_json` for a repository on `branch`.
///
/// `{}` — the defaults — when the branch is already covered by them, so the
/// common case stores nothing and keeps following the default patterns if those
/// ever change. Otherwise the branch is added *in front of* the defaults rather
/// than replacing them: somebody on `master` still wants `release/*` watched,
/// and a repository that silently narrowed its own patterns at registration
/// would be a second version of the bug this fixes.
fn branches_config(branch: Option<&str>) -> String {
    let defaults = RepoConfig::default().branches;
    let Some(branch) = branch else {
        return "{}".to_owned();
    };
    if defaults.iter().any(|pattern| pattern == branch) {
        return "{}".to_owned();
    }

    let mut branches = vec![branch.to_owned()];
    branches.extend(defaults);
    serde_json::json!({ "branches": branches }).to_string()
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

// --- install-wide defaults (REVL-194) -------------------------------------

/// The key an install-wide autonomy default is stored under.
///
/// Beside `autopilot` and `paused` in `setting` rather than in `config.toml`,
/// for ADR 0015's reason: config is what somebody wrote, and this is something
/// the app's own settings screen has to be able to change at runtime.
pub const SETTING_DEFAULT_AUTONOMY: &str = "default_autonomy";

/// What a repository added without an explicit `--autonomy` gets.
///
/// `dry_run` when nothing is stored, so an install that never touches this
/// behaves exactly as it did before the setting existed. A stored value that is
/// not a mode is treated the same way rather than failing the add: a corrupt
/// setting must not make it impossible to register a repository, and the value
/// is visible from `repo defaults` for anybody wondering why.
pub async fn default_autonomy(pool: &Pool) -> Result<AutonomyMode, RepoCommandError> {
    let stored = revlocal_store::SettingStore::new(pool)
        .get(SETTING_DEFAULT_AUTONOMY)
        .await
        .map_err(boxed)?;

    Ok(stored
        .and_then(|value| {
            MODES
                .iter()
                .find(|mode| mode.as_str() == value.trim())
                .copied()
        })
        .unwrap_or(AutonomyMode::DryRun))
}

/// Set what repositories added from now on get.
///
/// Deliberately does not touch repositories that already exist. Somebody who
/// widens the default is saying what the next repository should do, not
/// re-deciding thirty they have already looked at — `repo set` is how you change
/// one of those, and it is the command that names what it changed.
pub async fn set_default_autonomy(
    pool: &Pool,
    autonomy: &str,
    at: revlocal_core::Timestamp,
) -> Result<AutonomyMode, RepoCommandError> {
    let mode = parse_enum("autonomy", autonomy, &MODES)?;
    revlocal_store::SettingStore::new(pool)
        .set(SETTING_DEFAULT_AUTONOMY, mode.as_str(), at)
        .await
        .map_err(boxed)?;
    Ok(mode)
}

/// What `repo defaults` reports.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DefaultsReport {
    /// The autonomy a repository added without `--autonomy` gets.
    pub autonomy: String,
    /// Whether that came from the database or is the fallback.
    pub is_set: bool,
    /// A sentence for a person.
    pub detail: String,
}

/// Read (or, with `set_to`, write) the install-wide defaults (§14).
pub async fn defaults(
    pool: &Pool,
    set_to: Option<&str>,
    at: revlocal_core::Timestamp,
) -> Result<DefaultsReport, RepoCommandError> {
    let is_set = match set_to {
        Some(value) => {
            set_default_autonomy(pool, value, at).await?;
            true
        }
        None => revlocal_store::SettingStore::new(pool)
            .get(SETTING_DEFAULT_AUTONOMY)
            .await
            .map_err(boxed)?
            .is_some(),
    };

    let autonomy = default_autonomy(pool).await?;
    Ok(DefaultsReport {
        autonomy: autonomy.as_str().to_owned(),
        is_set,
        detail: format!(
            "repositories added without --autonomy get `{}`{}{}",
            autonomy.as_str(),
            if is_set { "" } else { " (the default default)" },
            publishing_note(autonomy)
        ),
    })
}

/// Render a defaults report.
///
/// Its own function rather than an inline `to_string_pretty` at the call site,
/// which swallowed a serialisation failure into an empty line — under `--json`
/// that is a document nobody can parse and no error saying why.
pub fn render_defaults(report: &DefaultsReport, json: bool) -> Result<String, RepoCommandError> {
    if json {
        return serde_json::to_string_pretty(report)
            .map_err(|source| RepoCommandError::Unrenderable { source });
    }
    Ok(report.detail.clone())
}

/// The half-sentence that says whether this mode actually delivers anything.
///
/// `dry_run` records the payload and sends nothing, which is not obvious from
/// the word and is the single thing most likely to leave somebody waiting for
/// issues that were never going to be filed.
fn publishing_note(autonomy: AutonomyMode) -> &'static str {
    match autonomy {
        AutonomyMode::Off => " — reviews are off for it",
        AutonomyMode::DryRun => " — findings are recorded, nothing is published until you widen it",
        AutonomyMode::AutoLowAskHigh => " — low-risk deliveries go out, the rest wait for approval",
        AutonomyMode::Auto => " — findings are published without asking",
    }
}

// --- scan (REVL-193, SPEC §14's `repo scan`) ------------------------------

/// What happened to one discovered checkout.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScanEntry {
    /// Where it is on disk.
    pub path: String,
    /// `git` or `svn`.
    pub kind: String,
    /// What it was (or would have been) registered as.
    pub name: String,
    /// `added`, `would_add`, or `skipped`.
    pub outcome: String,
    /// Why, when it was skipped.
    pub reason: Option<String>,
}

/// What a whole scan did.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScanReport {
    /// The directory that was walked.
    pub root: String,
    /// How deep below it the walk went.
    pub depth: usize,
    /// Whether anything was actually written.
    pub dry_run: bool,
    /// The autonomy every repository in this scan was given.
    pub autonomy: String,
    /// Every checkout found, in the order found.
    pub entries: Vec<ScanEntry>,
    /// How many were registered (or would be).
    pub added: usize,
    /// How many were passed over.
    pub skipped: usize,
    /// Directories the depth bound stopped the walk from looking inside (§18).
    ///
    /// A scan that stopped one level above thirty checkouts and a scan of an
    /// empty tree both find nothing. This is what tells them apart.
    pub not_descended: usize,
    /// Directories that exist but could not be read (§18).
    pub unreadable: usize,
}

/// Register every checkout under a directory (§14's `repo scan`).
///
/// # Why a scan does not stop at the first refusal
///
/// The reason to scan rather than add thirty times is that thirty repositories
/// are more than somebody wants to think about individually. A scan that
/// aborted on the first name collision would leave the caller doing exactly the
/// per-repository work the command was meant to remove, and would leave the
/// database half-written with no record of where it got to. So every checkout
/// gets its own outcome, and the report is the record.
///
/// `dry_run` walks and decides and writes nothing, using the same code path as
/// a real scan — a preview produced by a second implementation is a preview of
/// the second implementation.
#[allow(clippy::too_many_arguments)]
pub async fn scan(
    pool: &Pool,
    root: &std::path::Path,
    depth: usize,
    engine: &str,
    autonomy: Option<&str>,
    dry_run: bool,
    at: revlocal_core::Timestamp,
) -> Result<ScanReport, RepoCommandError> {
    // Parsed before the walk. A mistyped `--engine` should be a message about
    // the flag, not a message about the flag after thirty repositories were
    // registered with the wrong one.
    let engine_kind = parse_enum("engine", engine, &ENGINES)?;
    let autonomy_mode = match autonomy {
        Some(given) => parse_enum("autonomy", given, &MODES)?,
        None => default_autonomy(pool).await?,
    };

    let existing = RepoStore::new(pool).list().await.map_err(boxed)?;
    // Compared as canonical paths so `~/code/acme` and `~/code/./acme` are one
    // repository. A path that no longer resolves keeps its stored spelling: it
    // is a repository whose checkout is gone (RL-1513), and re-registering it
    // under a second name would hide that rather than fix it.
    let known: Vec<std::path::PathBuf> = existing
        .iter()
        .filter_map(|repo| repo.local_path.as_deref())
        .filter(|path| !path.is_empty())
        .map(|path| canonical(std::path::Path::new(path)))
        .collect();
    let mut taken_names: Vec<String> = existing.iter().map(|repo| repo.name.clone()).collect();

    let mut report = ScanReport {
        root: root.display().to_string(),
        depth,
        dry_run,
        autonomy: autonomy_mode.as_str().to_owned(),
        entries: Vec::new(),
        added: 0,
        skipped: 0,
        not_descended: 0,
        unreadable: 0,
    };

    let discovery = crate::scan::discover(root, depth);
    report.not_descended = discovery.not_descended;
    report.unreadable = discovery.unreadable;

    for found in discovery.found {
        let path = found.path.display().to_string();
        let kind = found.kind.as_str().to_owned();

        if known.contains(&canonical(&found.path)) {
            report.skipped += 1;
            report.entries.push(ScanEntry {
                path,
                kind,
                name: String::new(),
                outcome: "skipped".to_owned(),
                reason: Some("already configured".to_owned()),
            });
            continue;
        }

        // Two checkouts called `api` under different organisations is the normal
        // case in the layout this command exists for, and §5 makes the name
        // unique — so a collision is disambiguated with the directory above it
        // rather than reported as a failure somebody has to resolve by hand.
        let Some(name) = free_name(&found.path, &taken_names) else {
            report.skipped += 1;
            report.entries.push(ScanEntry {
                path,
                kind,
                name: String::new(),
                outcome: "skipped".to_owned(),
                reason: Some(
                    "every name derived from this path is already taken — add it with --name"
                        .to_owned(),
                ),
            });
            continue;
        };

        if dry_run {
            // Reserved anyway, so a preview of two same-named checkouts shows
            // the two names a real scan would give them.
            taken_names.push(name.clone());
            report.added += 1;
            report.entries.push(ScanEntry {
                path,
                kind,
                name,
                outcome: "would_add".to_owned(),
                reason: None,
            });
            continue;
        }

        match add(
            pool,
            &path,
            found.kind.as_str(),
            Some(&name),
            engine_kind.as_str(),
            Some(autonomy_mode.as_str()),
            at,
        )
        .await
        {
            Ok(_) => {
                taken_names.push(name.clone());
                report.added += 1;
                report.entries.push(ScanEntry {
                    path,
                    kind,
                    name,
                    outcome: "added".to_owned(),
                    reason: None,
                });
            }
            // One repository that cannot be registered must not cost the other
            // twenty-nine. The reason is carried so the report says which.
            Err(error) => {
                report.skipped += 1;
                report.entries.push(ScanEntry {
                    path,
                    kind,
                    name,
                    outcome: "skipped".to_owned(),
                    reason: Some(error.to_string()),
                });
            }
        }
    }

    Ok(report)
}

/// The path as the filesystem sees it, or as given when it cannot say.
fn canonical(path: &std::path::Path) -> std::path::PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// A name for this checkout that nothing else has: `api`, then `acme-api`,
/// then `work-acme-api`.
///
/// Gives up after three ancestors rather than climbing to the root — a name
/// built from five directories is not a name anybody wants to type into
/// `repo set`, and `--name` exists for the case that gets there.
///
/// §18: giving up is not silent. The caller records the checkout as skipped
/// with the reason, so a repository passed over for want of a free name is in
/// the report rather than merely absent from it.
fn free_name(path: &std::path::Path, taken: &[String]) -> Option<String> {
    let mut segments: Vec<&str> = Vec::new();
    for component in path
        .components()
        .rev()
        .filter_map(|c| c.as_os_str().to_str())
        .filter(|s| !s.is_empty() && *s != "/")
        .take(3)
    {
        segments.insert(0, component);
        let candidate = segments.join("-");
        if !taken.contains(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// Render a scan report.
pub fn render_scan(report: &ScanReport, json: bool) -> Result<String, RepoCommandError> {
    if json {
        return serde_json::to_string_pretty(report)
            .map_err(|source| RepoCommandError::Unrenderable { source });
    }

    if report.entries.is_empty() {
        return Ok(format!(
            "no git or Subversion checkouts under {} (depth {}){}\n  try: --depth, or check the path\n",
            report.root,
            report.depth,
            unexplored(report)
        ));
    }

    let mut out = String::new();
    for entry in &report.entries {
        let verb = match entry.outcome.as_str() {
            "added" => "added   ",
            "would_add" => "would add",
            _ => "skipped ",
        };
        out.push_str(&format!("{verb} {}", entry.path));
        if !entry.name.is_empty() {
            out.push_str(&format!(" as {}", entry.name));
        }
        if let Some(reason) = &entry.reason {
            out.push_str(&format!(" — {reason}"));
        }
        out.push('\n');
    }

    out.push_str(&format!(
        "\n{} {}, {} skipped, autonomy {}{}{}\n",
        report.added,
        if report.dry_run { "to add" } else { "added" },
        report.skipped,
        report.autonomy,
        if report.dry_run {
            " — nothing was written; run it again without --dry-run"
        } else {
            ""
        },
        unexplored(report)
    ));
    Ok(out)
}

/// What the scan did not look at, as a clause to append (§18).
///
/// Empty when it looked everywhere, so the ordinary case reads cleanly and the
/// sentence appears exactly when it changes what the numbers above it mean.
fn unexplored(report: &ScanReport) -> String {
    let mut clauses = Vec::new();
    if report.not_descended > 0 {
        clauses.push(format!(
            "{} director(ies) were not looked inside at depth {} — try --depth",
            report.not_descended, report.depth
        ));
    }
    if report.unreadable > 0 {
        clauses.push(format!("{} could not be read at all", report.unreadable));
    }
    if clauses.is_empty() {
        return String::new();
    }
    format!("\n  {}", clauses.join("; "))
}
