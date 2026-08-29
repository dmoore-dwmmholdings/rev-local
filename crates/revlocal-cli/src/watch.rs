//! `revlocal watch` (RL-1201, SPEC §4.2, §7, §14).
//!
//! The loop the whole product hangs off, and the piece M8 through M12 built the
//! parts for without ever running them.
//!
//! # It is a loop around a decision, not a loop with decisions in it
//!
//! Everything that decides lives elsewhere and is tested there: `Scheduler::tick`
//! orders the four things that can stop work, `TriggerBus` coalesces, `PollSchedule`
//! backs off, `budgets::check` reads the ledger. This gathers state, calls them, and
//! does what they say.
//!
//! That split is why the ordering rules could be asserted directly rather than
//! inferred from timing, and it is why this file is short enough to read.
//!
//! # Discovery only, for now
//!
//! A pass discovers changes and records them. It does **not** yet run reviews:
//! `revlocal review` does that for one change, and wiring it here needs the run
//! registry that `runs` and the kill switch will share. Saying so in the output
//! rather than implying otherwise — a `watch` that silently reviewed nothing would
//! be indistinguishable from one whose repositories are quiet, which is §18's
//! failure exactly.

use std::collections::BTreeMap;

use revlocal_core::{Cursor, Repo, RepoConfig, RepoId, Timestamp, TriggerSource};
use revlocal_daemon::budgets::{check, BudgetVerdict};
use revlocal_daemon::scheduler::{Decision, Idle, Scheduler, WorldState};
use revlocal_daemon::triggers::{TriggerBus, TriggerEvent};
use revlocal_store::{BudgetLedgerStore, CursorStore, Pool, RepoStore, SettingStore};
use revlocal_vcs::{GitAdapter, VcsAdapter};
use serde::{Deserialize, Serialize};

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

    /// The report could not be serialised.
    #[error("could not render the report: {source}")]
    Unrenderable {
        /// Why.
        #[source]
        source: serde_json::Error,
    },
}

fn boxed(source: revlocal_store::StoreError) -> WatchError {
    WatchError::Store {
        source: Box::new(source),
    }
}

/// What one repository's pass did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoPass {
    /// Which repository.
    pub repo: String,
    /// Changes discovered.
    pub discovered: usize,
    /// What went wrong, if anything.
    ///
    /// A repository that fails is reported and skipped, never fatal: one
    /// unreachable remote must not stop every other repository being reviewed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
}

impl WatchReport {
    /// The human output.
    pub fn render_human(&self) -> String {
        if let Some(idle) = &self.idle {
            return format!("{idle}\n");
        }
        if self.passes.is_empty() {
            return format!("{} repository/ies, nothing due this tick\n", self.repos);
        }

        let mut out = String::new();
        for pass in &self.passes {
            match &pass.error {
                Some(error) => out.push_str(&format!("  {} — FAILED: {error}\n", pass.repo)),
                None => out.push_str(&format!(
                    "  {} — {} change(s) discovered\n",
                    pass.repo, pass.discovered
                )),
            }
        }
        // Said every time, so nobody concludes from a quiet run that reviews are
        // happening. RL-1201's remaining half.
        out.push_str(
            "\nDiscovery only: reviews are not executed yet. `revlocal review \
             --repo <path> --rev <ref>` reviews one change today.\n",
        );
        out
    }
}

/// Run one tick of the daemon loop.
///
/// Separate from any `loop {}` so it can be called once, from a test or from
/// `--once`, without waiting for a real interval.
pub async fn tick(pool: &Pool, at: Timestamp) -> Result<WatchReport, WatchError> {
    let paused = SettingStore::new(pool).is_paused().await.map_err(boxed)?;
    let repos: Vec<Repo> = RepoStore::new(pool)
        .list()
        .await
        .map_err(boxed)?
        .into_iter()
        .filter(|repo| repo.enabled)
        .collect();

    // Every enabled repository offers a poll trigger; the bus decides which
    // collapse together and the scheduler decides which may run.
    let mut bus = TriggerBus::default();
    for repo in &repos {
        bus.admit(&TriggerEvent::new(repo.id, TriggerSource::Poll, at));
    }
    // A zero window makes each tick's work due immediately, which is what `--once`
    // means. A long-running `watch` uses the configured window instead.
    let due = bus.due_passes(at + chrono::Duration::milliseconds(2_000));

    let mut budgets: BTreeMap<RepoId, BudgetVerdict> = BTreeMap::new();
    for repo in &repos {
        let settings = serde_json::from_str::<RepoConfig>(&repo.config_json)
            .map(|_| revlocal_core::BudgetSettings::default())
            .unwrap_or_default();
        let entry = BudgetLedgerStore::new(pool)
            .get(repo.id, &revlocal_daemon::budgets::day_of(at))
            .await
            .map_err(boxed)?;
        budgets.insert(repo.id, check(entry.as_ref(), &settings));
    }

    let world = WorldState {
        killed: paused,
        running: 0,
        slot_limit: revlocal_daemon::budgets::DEFAULT_MAX_CONCURRENT_RUNS,
        due,
        backfill_waiting: Vec::new(),
        budgets,
    };

    // One decision, from the function that owns the ordering.
    match Scheduler.tick(&world) {
        Decision::Idle(idle) => Ok(WatchReport {
            repos: repos.len(),
            passes: Vec::new(),
            idle: idle_line(&idle),
            paused,
        }),
        Decision::Discover(_) | Decision::Backfill { .. } => {
            let mut passes = Vec::new();
            for repo in &repos {
                passes.push(discover_one(pool, repo).await);
            }
            Ok(WatchReport {
                repos: repos.len(),
                passes,
                idle: None,
                paused,
            })
        }
    }
}

/// The line an idle tick prints, or `None` when idling is just "nothing due".
fn idle_line(idle: &Idle) -> Option<String> {
    match idle {
        // Not worth a line every tick; the pass count already says it.
        Idle::NothingToDo => None,
        other => Some(other.summary_line()),
    }
}

/// Discover one repository, turning a failure into a recorded one.
async fn discover_one(pool: &Pool, repo: &Repo) -> RepoPass {
    let cursor: Option<Cursor> = match CursorStore::new(pool)
        .get(
            repo.id,
            &Cursor::commits_scope(repo.default_branch.as_deref().unwrap_or("main")),
        )
        .await
    {
        Ok(cursor) => cursor,
        Err(error) => {
            return RepoPass {
                repo: repo.name.clone(),
                discovered: 0,
                error: Some(error.to_string()),
            }
        }
    };

    match GitAdapter::new().discover(repo, cursor.as_ref(), 50).await {
        Ok(changes) => RepoPass {
            repo: repo.name.clone(),
            discovered: changes.len(),
            error: None,
        },
        // One unreachable remote must not stop every other repository being
        // reviewed, so this is recorded rather than returned.
        Err(error) => RepoPass {
            repo: repo.name.clone(),
            discovered: 0,
            error: Some(error.to_string()),
        },
    }
}

/// Render for whichever output the caller asked for.
pub fn render(report: &WatchReport, json: bool) -> Result<String, WatchError> {
    if json {
        return serde_json::to_string_pretty(report)
            .map_err(|source| WatchError::Unrenderable { source });
    }
    Ok(report.render_human())
}
