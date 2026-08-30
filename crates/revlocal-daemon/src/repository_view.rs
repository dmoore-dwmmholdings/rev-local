//! The repository screen (RL-1106, SPEC §15 screen 2).
//!
//! # Four indicators, four independent answers
//!
//! §15 wants a live indicator per trigger. The temptation is one "trigger health"
//! light, because that is what fits in a card — and it is exactly wrong. The four
//! ways a change reaches rev-local fail for unrelated reasons: the poller fails on
//! the network, hooks fail because a file on disk was replaced, webhooks fail
//! because a secret is missing, and manual never fails at all. Rolling them into
//! one light means a repository with dead hooks and healthy polling shows amber
//! and somebody goes looking in the wrong place.
//!
//! So each is computed from its own inputs and [`TriggerStatus`] carries its own
//! reason. Nothing here reads another trigger's state.
//!
//! # An SVN repository is not a git repository with different words
//!
//! §6.4: SVN has no branches, only paths that everybody agrees to treat as
//! branches. Rendering `branches: ["main"]` over an SVN repository would be a
//! sentence that parses and means nothing — worse, it would suggest a filter that
//! is not being applied. [`Watching`] is therefore a tagged enum rather than a
//! list of strings with a label, so the screen cannot show the wrong vocabulary by
//! accident, and hooks report *not applicable* rather than *not installed*: SVN
//! hooks live on the server, and telling somebody to install one locally sends
//! them somewhere that does not exist.

use revlocal_core::{RepoId, RepoKind, Timestamp};
use revlocal_store::{Pool, RepoStore, RunStore};
use serde::{Deserialize, Serialize};

use crate::dashboard::{BudgetBar, LastRun};
use crate::view::RepoView;

/// How many recent runs the screen lists.
///
/// Not a silent cap: [`RepositoryView::more_runs`] says whether there are older
/// ones. "The last ten" and "all ten there have ever been" must not look alike.
pub const RECENT_RUNS: u32 = 10;

/// Why the screen could not be read.
#[derive(Debug, thiserror::Error)]
pub enum RepositoryError {
    /// The database could not be read.
    #[error("could not read the local database: {source}\n  try: revlocal db migrate")]
    Store {
        /// Why.
        #[source]
        source: Box<revlocal_store::StoreError>,
    },

    /// No such repository.
    #[error("no repository with id {id}\n  try: revlocal repo list")]
    NoSuchRepo {
        /// Which id was asked for.
        id: i64,
    },

    /// The submitted config was not valid (§13.2).
    #[error("{detail}")]
    InvalidConfig {
        /// What is wrong with it, in the terms the editor can show inline.
        detail: String,
    },
}

fn boxed(source: revlocal_store::StoreError) -> RepositoryError {
    RepositoryError::Store {
        source: Box::new(source),
    }
}

/// One trigger's live state.
///
/// Four cases rather than a bool, because "off" and "broken" want opposite
/// responses from whoever is reading. A repository whose webhook is deliberately
/// disabled is fine; one whose webhook is enabled with no secret is silently
/// dropping every delivery, and a single "inactive" would hide that.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TriggerState {
    /// Working, and here is what it is doing.
    Active,
    /// Deliberately off.
    Off,
    /// On, and unable to work. This is the one worth a colour.
    Broken,
    /// Cannot apply to this kind of repository at all (§6.4).
    NotApplicable,
}

/// A trigger, its state, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerStatus {
    /// `poll`, `hooks`, `webhook` or `manual`.
    pub trigger: String,
    /// Where it stands.
    #[serde(flatten)]
    pub state: TriggerState,
    /// One line saying why it stands there.
    ///
    /// Always present. An indicator with no explanation is a light somebody has
    /// to guess at, and the guess is usually "it is fine".
    pub detail: String,
}

/// What a repository is watching (§6.4).
///
/// Tagged, so the screen cannot render SVN paths under a "branches" heading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Watching {
    /// Git: branch globs.
    Branches {
        /// The globs, as configured.
        globs: Vec<String>,
    },
    /// SVN: repository paths (decision D6).
    Paths {
        /// The paths under watch, `trunk` first.
        paths: Vec<String>,
    },
}

/// One line in the recent-runs list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunLine {
    /// The run.
    pub run_id: i64,
    /// Where it got to.
    pub status: String,
    /// What it concluded, when it concluded anything.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    /// What started it — the same four the indicators describe.
    pub trigger: String,
    /// When it started.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
}

/// The screen's data (§15 screen 2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryView {
    /// The repository itself, as `repo show` reports it.
    pub repo: RepoView,
    /// Branches or paths, never both.
    pub watching: Watching,
    /// Exactly four, each independent.
    pub triggers: Vec<TriggerStatus>,
    /// The most recent runs, newest first.
    pub recent_runs: Vec<RunLine>,
    /// Whether there are older runs than the ones listed (§18).
    pub more_runs: bool,
    /// Today's spend, the same figure the dashboard shows.
    pub budget: BudgetBar,
    /// The repository's config as §13.2's JSON, pretty-printed for editing.
    ///
    /// The document itself, not a form over it. A typed form would be a second
    /// spelling of `RepoConfig` that has to be updated by hand every time a field
    /// is added — and the field somebody could not reach would be invisible
    /// rather than obviously missing.
    pub config_json: String,
    /// The last run, for the header.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run: Option<LastRun>,
}

/// Whether §13.2's JSON is valid, and what is wrong with it if not.
///
/// Validation is `RepoConfig`'s own deserialiser rather than a schema written
/// beside it. A second description of a valid config is a second thing to keep
/// in step, and the one that drifts is always the one doing the checking.
///
/// Returns the config re-serialised, so what is stored is what was validated —
/// not the bytes somebody typed, which could differ in ways the parse ignored.
pub fn validate_config(json: &str) -> Result<String, RepositoryError> {
    let parsed: revlocal_core::RepoConfig =
        serde_json::from_str(json).map_err(|error| RepositoryError::InvalidConfig {
            // serde's message carries the line and column, which is the half of
            // an error that makes it fixable. Passed through rather than
            // summarised to "invalid config".
            detail: format!("line {}, column {}: {error}", error.line(), error.column()),
        })?;

    serde_json::to_string_pretty(&parsed).map_err(|error| RepositoryError::InvalidConfig {
        detail: error.to_string(),
    })
}

/// Validate, then store. Never one without the other.
///
/// The order is the point. A config written first and validated afterwards is a
/// repository that polls on a corrupt blob until somebody notices, and §13.2's
/// fallback to defaults would make that quiet.
pub async fn save_config(
    pool: &Pool,
    repo_id: i64,
    json: &str,
    at: Timestamp,
) -> Result<String, RepositoryError> {
    let normalised = validate_config(json)?;

    let repos = RepoStore::new(pool);
    let mut repo = repos.get(RepoId::new(repo_id)).await.map_err(boxed)?;

    repo.config_json = normalised.clone();
    repo.updated_at = at;
    repos.update(&repo).await.map_err(boxed)?;

    Ok(normalised)
}

/// What a git repository's hooks are doing (§7.2).
fn hook_status(local_path: Option<&str>) -> TriggerStatus {
    let Some(path) = local_path else {
        return TriggerStatus {
            trigger: "hooks".to_owned(),
            state: TriggerState::Off,
            detail: "this repository has no local path, so there is nowhere to install a hook"
                .to_owned(),
        };
    };

    let installed = crate::hooks::installed(std::path::Path::new(path));
    match installed {
        Err(error) => TriggerStatus {
            trigger: "hooks".to_owned(),
            state: TriggerState::Broken,
            detail: format!("could not read the hooks directory: {error}"),
        },
        Ok(names) if names.is_empty() => TriggerStatus {
            trigger: "hooks".to_owned(),
            state: TriggerState::Off,
            detail: "no rev-local hook is installed; try: revlocal hooks install".to_owned(),
        },
        Ok(names) => TriggerStatus {
            trigger: "hooks".to_owned(),
            state: TriggerState::Active,
            // Named, not counted. "3 hooks installed" cannot be checked against
            // the repository and a list can.
            detail: format!("installed: {}", names.join(", ")),
        },
    }
}

/// What a repository's webhook is doing (§7.3).
fn webhook_status(config: &revlocal_core::RepoConfig, listener_port: u16) -> TriggerStatus {
    let trigger = "webhook".to_owned();

    if !config.webhook_enabled {
        return TriggerStatus {
            trigger,
            state: TriggerState::Off,
            detail: "not enabled for this repository (§13.2 webhook_enabled)".to_owned(),
        };
    }
    if listener_port == 0 {
        // Enabled here and impossible globally. Broken rather than off, because
        // somebody turned this on and is entitled to think it is working.
        return TriggerStatus {
            trigger,
            state: TriggerState::Broken,
            detail: "enabled here, but no listener is running (§13.1 webhook_port = 0)".to_owned(),
        };
    }
    if config.webhook_secret_ref.is_none() {
        return TriggerStatus {
            trigger,
            state: TriggerState::Broken,
            detail: "enabled with no secret; every delivery will be rejected unsigned".to_owned(),
        };
    }

    TriggerStatus {
        trigger,
        state: TriggerState::Active,
        detail: format!("listening on port {listener_port}"),
    }
}

/// The four indicators (§15 screen 2).
///
/// Each from its own inputs. Nothing here consults another trigger's state, which
/// is the property that makes four lights worth more than one.
fn triggers(
    repo: &revlocal_core::Repo,
    config: &revlocal_core::RepoConfig,
    health: &crate::poll::HealthReport,
    listener_port: u16,
) -> Vec<TriggerStatus> {
    let poll = if !repo.enabled {
        TriggerStatus {
            trigger: "poll".to_owned(),
            state: TriggerState::Off,
            detail: "the repository is disabled, so nothing polls it".to_owned(),
        }
    } else if health.consecutive_failures > 0 {
        TriggerStatus {
            trigger: "poll".to_owned(),
            state: TriggerState::Broken,
            detail: health.last_error.clone().map_or_else(
                || format!("{} polls have failed in a row", health.consecutive_failures),
                |error| {
                    format!(
                        "{} failed in a row; last: {error}",
                        health.consecutive_failures
                    )
                },
            ),
        }
    } else {
        TriggerStatus {
            trigger: "poll".to_owned(),
            state: TriggerState::Active,
            detail: format!(
                "every {}s; next in about {}s",
                health.poll_interval_secs, health.next_poll_in_secs
            ),
        }
    };

    let hooks = match repo.kind {
        // §6.4: SVN hooks run on the server. "Not installed" would send somebody
        // to a directory on their own machine that has nothing to do with it.
        RepoKind::Svn => TriggerStatus {
            trigger: "hooks".to_owned(),
            state: TriggerState::NotApplicable,
            detail: "SVN hooks run on the server; rev-local polls instead (§6.4)".to_owned(),
        },
        // A GitHub repository is watched through the API and its webhook. There
        // may be no clone on this machine at all, and offering to install a hook
        // into one would be advice that cannot be followed.
        RepoKind::GitHub => TriggerStatus {
            trigger: "hooks".to_owned(),
            state: TriggerState::NotApplicable,
            detail: "a GitHub repository is watched through the API; local hooks do not apply"
                .to_owned(),
        },
        RepoKind::Git => hook_status(repo.local_path.as_deref()),
    };

    let webhook = match repo.kind {
        RepoKind::Svn => TriggerStatus {
            trigger: "webhook".to_owned(),
            state: TriggerState::NotApplicable,
            detail: "webhooks are a GitHub delivery; an SVN repository has none".to_owned(),
        },
        RepoKind::Git | RepoKind::GitHub => webhook_status(config, listener_port),
    };

    // Manual is the one that always works, and saying so is not filler: when the
    // other three are off, this is the answer to "can I review anything at all".
    let manual = TriggerStatus {
        trigger: "manual".to_owned(),
        state: TriggerState::Active,
        detail: format!("always available: revlocal review --repo {}", repo.name),
    };

    vec![poll, hooks, webhook, manual]
}

/// What a repository is watching, in its own vocabulary (§6.4).
fn watching(repo: &revlocal_core::Repo, config: &revlocal_core::RepoConfig) -> Watching {
    match repo.kind {
        RepoKind::Git | RepoKind::GitHub => Watching::Branches {
            globs: config.branches.clone(),
        },
        RepoKind::Svn => {
            // Decision D6: trunk always, `branches/*` when asked for. Spelled as
            // paths because that is what they are — an SVN "branch" is a
            // directory, and calling it a branch is where the confusion starts.
            let mut paths = vec!["trunk".to_owned()];
            if config.watch_branches {
                paths.push("branches/*".to_owned());
            }
            Watching::Paths { paths }
        }
    }
}

/// Read one repository's screen (SPEC §15 screen 2).
pub async fn gather(
    pool: &Pool,
    repo_id: i64,
    budgets: &revlocal_core::BudgetSettings,
    listener_port: u16,
    at: Timestamp,
) -> Result<RepositoryView, RepositoryError> {
    let repo = RepoStore::new(pool)
        .get(RepoId::new(repo_id))
        .await
        .map_err(|error| match error {
            revlocal_store::StoreError::NotFound { .. } => {
                RepositoryError::NoSuchRepo { id: repo_id }
            }
            other => boxed(other),
        })?;

    // §13.2: a config that will not parse falls back to defaults rather than
    // failing the screen. A repository with a corrupt blob is precisely the one
    // somebody needs to open in order to fix it — and the editor below shows the
    // stored bytes, so what is wrong stays visible.
    let config =
        serde_json::from_str::<revlocal_core::RepoConfig>(&repo.config_json).unwrap_or_default();

    let health = crate::view::report_for(&repo);
    let view = RepoView::of(&repo);

    let runs = RunStore::new(pool)
        .list_recent(Some(repo.id), None, RECENT_RUNS + 1)
        .await
        .map_err(boxed)?;
    let more_runs = u32::try_from(runs.len()).unwrap_or(u32::MAX) > RECENT_RUNS;

    let last_run = runs.first().map(|run| LastRun {
        run_id: run.id.get(),
        status: run.status.as_str().to_owned(),
        verdict: run.verdict.map(|v| v.as_str().to_owned()),
        finished_at: run.finished_at.map(|at| at.to_rfc3339()),
    });

    let recent_runs = runs
        .iter()
        .take(RECENT_RUNS as usize)
        .map(|run| RunLine {
            run_id: run.id.get(),
            status: run.status.as_str().to_owned(),
            verdict: run.verdict.map(|v| v.as_str().to_owned()),
            trigger: run.trigger.as_str().to_owned(),
            started_at: run.started_at.map(|at| at.to_rfc3339()),
        })
        .collect();

    let budget = crate::dashboard::budget_bar(pool, repo.id, budgets, at)
        .await
        .map_err(|error| RepositoryError::InvalidConfig {
            detail: error.to_string(),
        })?;

    Ok(RepositoryView {
        watching: watching(&repo, &config),
        triggers: triggers(&repo, &config, &health, listener_port),
        recent_runs,
        more_runs,
        budget,
        // The stored bytes, pretty-printed only when they parse. Somebody editing
        // a broken config needs to see what is actually there.
        config_json: serde_json::to_string_pretty(&config)
            .unwrap_or_else(|_| repo.config_json.clone()),
        last_run,
        repo: view,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use revlocal_core::{AutonomyMode, EngineKind, Repo};

    fn repo(kind: RepoKind) -> Repo {
        Repo {
            id: RepoId::new(1),
            name: "acme".to_owned(),
            kind,
            local_path: Some("/nowhere/acme".to_owned()),
            remote_url: None,
            default_branch: Some("main".to_owned()),
            engine: EngineKind::Mock,
            autonomy: AutonomyMode::DryRun,
            enabled: true,
            config_json: "{}".to_owned(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn healthy() -> crate::poll::HealthReport {
        crate::poll::HealthReport {
            repo: "acme".to_owned(),
            health: crate::poll::RepoHealth::Healthy,
            poll_interval_secs: 300,
            configured_interval_clamped: false,
            next_poll_in_secs: 120,
            consecutive_failures: 0,
            last_error: None,
            notes: Vec::new(),
        }
    }

    fn find<'a>(statuses: &'a [TriggerStatus], name: &str) -> &'a TriggerStatus {
        statuses
            .iter()
            .find(|s| s.trigger == name)
            .unwrap_or_else(|| panic!("no {name} indicator"))
    }

    #[test]
    fn repository_has_one_indicator_per_trigger() {
        let statuses = triggers(
            &repo(RepoKind::Git),
            &revlocal_core::RepoConfig::default(),
            &healthy(),
            0,
        );

        let names: Vec<&str> = statuses.iter().map(|s| s.trigger.as_str()).collect();
        assert_eq!(names, vec!["poll", "hooks", "webhook", "manual"]);
    }

    #[test]
    fn repository_a_broken_webhook_does_not_dim_the_poller() {
        // The reason there are four lights rather than one. These fail for
        // unrelated reasons, and a rolled-up indicator sends somebody looking in
        // the wrong place — the poller is fine, the secret is missing.
        let config = revlocal_core::RepoConfig {
            webhook_enabled: true,
            webhook_secret_ref: None,
            ..revlocal_core::RepoConfig::default()
        };

        let statuses = triggers(&repo(RepoKind::Git), &config, &healthy(), 8080);

        assert_eq!(find(&statuses, "webhook").state, TriggerState::Broken);
        assert_eq!(find(&statuses, "poll").state, TriggerState::Active);
        assert_eq!(find(&statuses, "manual").state, TriggerState::Active);
    }

    #[test]
    fn repository_a_failing_poller_does_not_dim_the_webhook() {
        // And the same in the other direction, because "independent" is a claim
        // about both directions and testing one is testing half of it.
        let config = revlocal_core::RepoConfig {
            webhook_enabled: true,
            webhook_secret_ref: Some("keychain:acme".to_owned()),
            ..revlocal_core::RepoConfig::default()
        };
        let failing = crate::poll::HealthReport {
            consecutive_failures: 3,
            last_error: Some("connection refused".to_owned()),
            ..healthy()
        };

        let statuses = triggers(&repo(RepoKind::Git), &config, &failing, 8080);

        assert_eq!(find(&statuses, "poll").state, TriggerState::Broken);
        // The reason travels with the light. An indicator with no explanation is
        // one somebody guesses at, and the guess is usually "it is fine".
        assert!(find(&statuses, "poll")
            .detail
            .contains("connection refused"));
        assert_eq!(find(&statuses, "webhook").state, TriggerState::Active);
    }

    #[test]
    fn repository_enabled_webhook_with_no_listener_is_broken_not_off() {
        // Somebody turned this on and is entitled to think it works. "Off" would
        // read as their own choice rather than as a global setting overriding it.
        let config = revlocal_core::RepoConfig {
            webhook_enabled: true,
            webhook_secret_ref: Some("keychain:acme".to_owned()),
            ..revlocal_core::RepoConfig::default()
        };

        let statuses = triggers(&repo(RepoKind::Git), &config, &healthy(), 0);

        assert_eq!(find(&statuses, "webhook").state, TriggerState::Broken);
        assert!(find(&statuses, "webhook").detail.contains("webhook_port"));
    }

    #[test]
    fn repository_a_disabled_repo_stops_polling_and_nothing_else() {
        let mut disabled = repo(RepoKind::Git);
        disabled.enabled = false;

        let statuses = triggers(
            &disabled,
            &revlocal_core::RepoConfig::default(),
            &healthy(),
            0,
        );

        assert_eq!(find(&statuses, "poll").state, TriggerState::Off);
        // Manual still works on a disabled repository — that is what `revlocal
        // review` is for, and saying otherwise would send somebody re-enabling a
        // repository they had deliberately turned off.
        assert_eq!(find(&statuses, "manual").state, TriggerState::Active);
    }

    #[test]
    fn repository_svn_hooks_are_not_applicable_rather_than_missing() {
        // §6.4. "Not installed" points at a directory on this machine that has
        // nothing to do with an SVN server, which is worse than saying nothing.
        let statuses = triggers(
            &repo(RepoKind::Svn),
            &revlocal_core::RepoConfig::default(),
            &healthy(),
            8080,
        );

        assert_eq!(find(&statuses, "hooks").state, TriggerState::NotApplicable);
        assert_eq!(
            find(&statuses, "webhook").state,
            TriggerState::NotApplicable
        );
        // Polling is how SVN is watched at all, so it had better still be live.
        assert_eq!(find(&statuses, "poll").state, TriggerState::Active);
    }

    #[test]
    fn repository_svn_watches_paths_and_git_watches_branches() {
        // Criterion 3, at the level where the vocabulary is chosen. A tagged enum
        // rather than a labelled list, so the screen cannot render SVN paths
        // under a "branches" heading by accident.
        let git = watching(
            &repo(RepoKind::Git),
            &revlocal_core::RepoConfig {
                branches: vec!["main".to_owned(), "release/*".to_owned()],
                ..revlocal_core::RepoConfig::default()
            },
        );
        assert_eq!(
            git,
            Watching::Branches {
                globs: vec!["main".to_owned(), "release/*".to_owned()]
            }
        );

        let trunk_only = watching(
            &repo(RepoKind::Svn),
            &revlocal_core::RepoConfig {
                watch_branches: false,
                ..revlocal_core::RepoConfig::default()
            },
        );
        assert_eq!(
            trunk_only,
            Watching::Paths {
                paths: vec!["trunk".to_owned()]
            }
        );

        // Decision D6: `branches/*` only when asked for, and it is a path.
        let with_branches = watching(
            &repo(RepoKind::Svn),
            &revlocal_core::RepoConfig {
                watch_branches: true,
                ..revlocal_core::RepoConfig::default()
            },
        );
        assert_eq!(
            with_branches,
            Watching::Paths {
                paths: vec!["trunk".to_owned(), "branches/*".to_owned()]
            }
        );
    }

    #[test]
    fn repository_an_svn_config_never_reports_git_branch_globs() {
        // The failure this guards is subtle: `branches` is a real field on every
        // config, so an SVN repository with a stale `["main"]` in it would render
        // a branch filter that is not being applied to anything.
        let svn = watching(
            &repo(RepoKind::Svn),
            &revlocal_core::RepoConfig {
                branches: vec!["main".to_owned()],
                watch_branches: false,
                ..revlocal_core::RepoConfig::default()
            },
        );

        match svn {
            Watching::Paths { paths } => assert_eq!(paths, vec!["trunk".to_owned()]),
            Watching::Branches { .. } => panic!("an SVN repository must not report branches"),
        }
    }

    #[test]
    fn repository_invalid_config_says_where() {
        // Criterion 2's half that matters: the message has to be usable inline.
        // "Invalid config" tells somebody to re-read 30 fields.
        let error = validate_config(r#"{"poll_interval_secs": "soon"}"#)
            .expect_err("a string is not an interval");

        let text = error.to_string();
        assert!(text.contains("line"), "no position in {text:?}");
        assert!(text.contains("column"), "no position in {text:?}");
    }

    #[test]
    fn repository_valid_config_is_stored_as_it_was_validated() {
        // Round-tripped through `RepoConfig`, so what is saved is what was
        // checked. Storing the typed bytes would let a field the parser ignored
        // survive into the database and mean something later.
        let normalised = validate_config(r#"{"poll_interval_secs": 900}"#).expect("valid");

        let parsed: revlocal_core::RepoConfig = serde_json::from_str(&normalised).expect("parses");
        assert_eq!(parsed.poll_interval_secs, 900);
        // A field left out came back with its default, spelled out rather than
        // absent — which is what makes the editor show the whole document.
        assert!(normalised.contains("review_prs"));
    }
}
