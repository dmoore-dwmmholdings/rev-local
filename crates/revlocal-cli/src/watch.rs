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
//! # Discovery is persistent; reviewing is not wired yet
//!
//! A pass records every change it finds, applies §9.4's skip rules, and advances
//! the cursor — so a second pass over a quiet repository finds nothing rather than
//! rediscovering the same commits forever.
//!
//! It does **not** yet run reviews. `revlocal review` does that for one change,
//! and wiring it here needs the run registry the kill switch's cancellation path
//! also wants. Every tick says so, because a `watch` that silently reviewed
//! nothing would be indistinguishable from one whose repositories are quiet —
//! §18's failure exactly.
//!
//! # The cursor advances last, and past skipped changes too
//!
//! Last, because a crash between recording and advancing costs a re-read and a
//! crash the other way loses a change permanently. Past skipped ones, because a
//! change that was looked at and deliberately not reviewed is *finished* — leaving
//! the cursor behind it means re-deciding it on every poll forever.

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
    /// Changes recorded for review.
    pub recorded: usize,
    /// Changes recorded and deliberately skipped (§9.4), with the reason.
    ///
    /// Counted separately because "nothing to review" and "everything was a
    /// lockfile" are different facts, and only one of them is a quiet repository.
    pub skipped: Vec<String>,
    /// Where the cursor now stands, when it moved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
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
                None => {
                    out.push_str(&format!(
                        "  {} — {} discovered, {} recorded",
                        pass.repo, pass.discovered, pass.recorded
                    ));
                    if !pass.skipped.is_empty() {
                        out.push_str(&format!(", {} skipped", pass.skipped.len()));
                    }
                    out.push('\n');
                    // §9.4: a skipped change is recorded with its reason, and the
                    // reason is the point — "why did rev-local ignore my commit?"
                    // has an answer only if it is shown.
                    for reason in &pass.skipped {
                        out.push_str(&format!("      skipped: {reason}\n"));
                    }
                }
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
                passes.push(discover_one(pool, repo, at).await);
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

/// Discover one repository, record what it found, and advance its cursor.
///
/// A failure becomes a recorded one: see the caller.
async fn discover_one(pool: &Pool, repo: &Repo, at: Timestamp) -> RepoPass {
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
                recorded: 0,
                skipped: Vec::new(),
                cursor: None,
                error: Some(error.to_string()),
            }
        }
    };

    let scope = Cursor::commits_scope(repo.default_branch.as_deref().unwrap_or("main"));

    match GitAdapter::new().discover(repo, cursor.as_ref(), 50).await {
        Ok(changes) => {
            let config = serde_json::from_str::<RepoConfig>(&repo.config_json).unwrap_or_default();

            let mut recorded = 0_usize;
            let mut skipped = Vec::new();
            let mut furthest: Option<String> = None;

            for change in &changes {
                let skip = revlocal_vcs::skip_rules::evaluate(change, &config);

                // Recorded either way. §9.4's rule is that a skipped change is
                // still written down with its reason — a change that vanishes is
                // indistinguishable from one that was never seen.
                if let Err(error) = record(pool, repo, change, at).await {
                    return RepoPass {
                        repo: repo.name.clone(),
                        discovered: changes.len(),
                        recorded,
                        skipped,
                        cursor: furthest,
                        error: Some(error.to_string()),
                    };
                }

                match skip {
                    Some(skip) => skipped.push(format!("{} — {}", change.external_id, skip.detail)),
                    None => recorded += 1,
                }
                furthest = Some(change.cursor_value.clone());
            }

            // Last, and past skipped changes too. See the module docs.
            if let Some(value) = &furthest {
                if let Err(error) = CursorStore::new(pool)
                    .advance(repo.id, &scope, value, at)
                    .await
                {
                    return RepoPass {
                        repo: repo.name.clone(),
                        discovered: changes.len(),
                        recorded,
                        skipped,
                        cursor: None,
                        error: Some(error.to_string()),
                    };
                }
            }

            RepoPass {
                repo: repo.name.clone(),
                discovered: changes.len(),
                recorded,
                skipped,
                cursor: furthest,
                error: None,
            }
        }
        // One unreachable remote must not stop every other repository being
        // reviewed, so this is recorded rather than returned.
        Err(error) => RepoPass {
            repo: repo.name.clone(),
            discovered: 0,
            recorded: 0,
            skipped: Vec::new(),
            cursor: None,
            error: Some(error.to_string()),
        },
    }
}

/// Write one discovered change down.
///
/// `upsert`, not `insert`: discovery can legitimately see the same change twice —
/// a force-push, an overlapping poll — and a unique-constraint failure there would
/// stop a pass over something harmless.
async fn record(
    pool: &Pool,
    repo: &Repo,
    change: &revlocal_vcs::DetectedChange,
    at: Timestamp,
) -> Result<(), WatchError> {
    revlocal_store::ChangeStore::new(pool)
        .upsert(&revlocal_core::Change {
            id: revlocal_core::ChangeId::new(0),
            repo_id: repo.id,
            kind: change.kind,
            external_id: change.external_id.clone(),
            title: change.title.clone(),
            author_name: change.author_name.clone(),
            author_email: change.author_email.clone(),
            authored_at: change.authored_at,
            branch: change.branch.clone(),
            base_ref: change.base_ref.clone(),
            head_ref: change.head_ref.clone(),
            url: change.url.clone(),
            diff_stat: change.diff_stat,
            detected_at: at,
        })
        .await
        .map_err(boxed)?;
    Ok(())
}

/// Render for whichever output the caller asked for.
pub fn render(report: &WatchReport, json: bool) -> Result<String, WatchError> {
    if json {
        return serde_json::to_string_pretty(report)
            .map_err(|source| WatchError::Unrenderable { source });
    }
    Ok(report.render_human())
}
