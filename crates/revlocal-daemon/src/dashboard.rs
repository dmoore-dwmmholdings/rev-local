//! The dashboard's data, composed once (RL-1105, SPEC §15).
//!
//! §15's first screen is repo cards — health, last run, queue depth, today's
//! budget — plus a global mode selector, the kill switch and a live activity feed.
//!
//! # Why this lives here and not in the Tauri layer
//!
//! A card is a *composition*: it joins a repository to its most recent run, to
//! however many runs are queued, and to today's ledger row. That is a decision
//! about what an operator needs to see together, and RL-1101 asserts the IPC
//! surface holds no decisions — a number computed there is a number the CLI will
//! eventually disagree with.
//!
//! So the composition is here in the daemon, which both front ends already depend
//! on — `revlocal dashboard` and the desktop screen render the same snapshot, and
//! neither front end owns the answer. It is also testable without a webview.
//!
//! # The feed is not here
//!
//! §15 requires live updates to come from Tauri events rather than polling, so the
//! activity feed is the event bridge's job and deliberately absent from this
//! snapshot. A `recent_activity` field would invite a screen to poll for it, which
//! is the rule this design exists to keep.

use revlocal_core::{AutonomyMode, BudgetSettings, RepoId, Timestamp};
use revlocal_store::{BudgetLedgerStore, Pool, RunStore, SettingStore};
use serde::{Deserialize, Serialize};

use crate::view::RepoView;

/// Setting key: the global autonomy ceiling, when the UI has overridden it.
///
/// Stored rather than written back into the user's `config.toml`. §13.1's document
/// is hand-edited and commented; rewriting it from a dropdown would lose both. The
/// kill switch already works this way, and a mode selector is the same kind of
/// thing — a live operational control, not a configuration change.
pub const SETTING_MODE: &str = "global.mode";

/// How many runs to look at when finding a repository's most recent one.
///
/// Not a silent cap: it bounds a *lookup*, not a report. A repository with more
/// than this many runs still has its latest found, because the store returns them
/// newest first — this only stops the query reading a year of history to answer
/// "what happened last".
pub const RECENT_RUN_SCAN: u32 = 200;

/// Why the dashboard could not be assembled.
#[derive(Debug, thiserror::Error)]
pub enum DashboardError {
    /// The database could not be read.
    #[error("could not read the local database: {source}\n  try: revlocal db migrate")]
    Store {
        /// Why.
        #[source]
        source: Box<revlocal_store::StoreError>,
    },

    /// The report could not be serialised.
    #[error("could not render the dashboard: {source}")]
    Unrenderable {
        /// Why.
        #[source]
        source: serde_json::Error,
    },
}

fn boxed(source: revlocal_store::StoreError) -> DashboardError {
    DashboardError::Store {
        source: Box::new(source),
    }
}

/// What a repository's most recent run was.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastRun {
    /// The run's id, so a card can link to the run detail (§15 screen 3).
    pub run_id: i64,
    /// Where it ended up.
    pub status: String,
    /// Its verdict, if it reached one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    /// When it finished, if it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
}

/// Today's spend against today's ceilings.
///
/// Every number is paired with its limit rather than pre-divided into a
/// percentage: a bar that knows only "62%" cannot say 62% *of what*, and an
/// operator deciding whether to widen a budget needs both.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetBar {
    /// Runs today.
    pub runs: u32,
    /// The daily run ceiling. `0` means unlimited.
    pub runs_limit: u32,
    /// Tokens today, as far as anybody knows.
    pub tokens: u64,
    /// The daily token ceiling. `0` means unlimited.
    pub tokens_limit: u64,
    /// Whether that token figure accounts for every run today (RL-409).
    ///
    /// When false the bar is a **lower bound**, and §18 forbids drawing it as a
    /// total. The screen renders it differently rather than hiding the
    /// distinction.
    pub tokens_known: bool,
}

/// One repository's card (§15 screen 1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoCard {
    /// Name, kind, engine, autonomy, enabled, and polling health.
    pub repo: RepoView,
    /// The most recent run, if there has ever been one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run: Option<LastRun>,
    /// How many runs are waiting to execute.
    pub queue_depth: u32,
    /// Today's spend against today's ceilings.
    pub budget: BudgetBar,
}

/// The dashboard's snapshot (§15 screen 1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dashboard {
    /// Every repository, in name order.
    pub repos: Vec<RepoCard>,
    /// The global autonomy ceiling (§12.2).
    pub mode: String,
    /// Whether the kill switch is engaged (§12.1).
    ///
    /// On the snapshot rather than fetched separately, because §15 requires the
    /// switch to be reachable from every screen: a screen that has to make a
    /// second call to know its state has a window where it renders the wrong one.
    pub paused: bool,
}

impl Dashboard {
    /// The block the human path prints.
    pub fn render_human(&self) -> String {
        let mut out = format!("mode: {}", self.mode);
        if self.paused {
            out.push_str("  [PAUSED — revlocal resume]");
        }
        out.push_str("\n\n");

        if self.repos.is_empty() {
            out.push_str("no repositories are configured\n  try: revlocal repo add --help\n");
            return out;
        }

        for card in &self.repos {
            out.push_str(&format!(
                "{}  [{}]\n",
                card.repo.repo,
                card.repo.health.health.as_str()
            ));
            match &card.last_run {
                Some(run) => out.push_str(&format!(
                    "  last run #{} {}{}\n",
                    run.run_id,
                    run.status,
                    run.verdict
                        .as_deref()
                        .map_or_else(String::new, |v| format!(" ({v})"))
                )),
                // Said, not omitted. A card with no line about runs reads as a
                // card whose runs failed to load.
                None => out.push_str("  no runs yet\n"),
            }
            out.push_str(&format!("  queued: {}\n", card.queue_depth));
            out.push_str(&format!("  {}\n", render_budget(&card.budget)));
        }
        out
    }
}

/// One line of budget, hedged when the token count is a lower bound.
fn render_budget(budget: &BudgetBar) -> String {
    let runs = if budget.runs_limit == 0 {
        format!("{} runs", budget.runs)
    } else {
        format!("{} of {} runs", budget.runs, budget.runs_limit)
    };
    let tokens = if budget.tokens_limit == 0 {
        format!("{} tokens", budget.tokens)
    } else {
        format!("{} of {} tokens", budget.tokens, budget.tokens_limit)
    };
    // §18: a lower bound must not read as a total.
    let hedge = if budget.tokens_known {
        ""
    } else {
        " (at least — a run today reported no count)"
    };
    format!("today: {runs}, {tokens}{hedge}")
}

/// Assemble the dashboard (SPEC §15).
pub async fn gather(
    pool: &Pool,
    budgets: &BudgetSettings,
    at: Timestamp,
) -> Result<Dashboard, DashboardError> {
    let settings = SettingStore::new(pool);
    let paused = settings.is_paused().await.map_err(boxed)?;

    // Config's value is the default and the store's is an override, so a fresh
    // install shows what §13.1 says without anything having been written.
    let mode = settings
        .get(SETTING_MODE)
        .await
        .map_err(boxed)?
        .and_then(|value| value.parse::<AutonomyMode>().ok())
        .unwrap_or(AutonomyMode::AutoLowAskHigh);

    let views: Vec<RepoView> = revlocal_store::RepoStore::new(pool)
        .list()
        .await
        .map_err(boxed)?
        .iter()
        .map(RepoView::of)
        .collect();

    let runs = RunStore::new(pool);
    let ledger = BudgetLedgerStore::new(pool);
    // Local calendar day: a budget is a human-facing daily allowance and rolls
    // over on the user's midnight, not UTC's.
    let day = at
        .with_timezone(&chrono::Local)
        .format("%Y-%m-%d")
        .to_string();

    let mut cards = Vec::with_capacity(views.len());
    for view in views {
        let repo_id = RepoId::new(view.id);

        let recent = runs
            .list_recent(Some(repo_id), None, RECENT_RUN_SCAN)
            .await
            .map_err(boxed)?;

        let last_run = recent.first().map(|run| LastRun {
            run_id: run.id.get(),
            status: run.status.as_str().to_owned(),
            verdict: run.verdict.map(|v| v.as_str().to_owned()),
            finished_at: run.finished_at.map(|at| at.to_rfc3339()),
        });

        let queue_depth = runs
            .count_matching(Some(repo_id), Some(revlocal_core::RunStatus::Queued))
            .await
            .map_err(boxed)?;

        let today = ledger.get(repo_id, &day).await.map_err(boxed)?;
        let budget = today.map_or(
            BudgetBar {
                runs: 0,
                runs_limit: budgets.daily_runs_per_repo,
                tokens: 0,
                tokens_limit: budgets.daily_tokens_per_repo,
                // Nothing spent is exactly known. A fresh day is not an
                // unmeasured one, and hedging it would cry wolf.
                tokens_known: true,
            },
            |entry| BudgetBar {
                runs: entry.runs,
                runs_limit: budgets.daily_runs_per_repo,
                tokens: entry.usage.total_tokens(),
                tokens_limit: budgets.daily_tokens_per_repo,
                tokens_known: entry.usage.tokens_are_known(),
            },
        );

        cards.push(RepoCard {
            repo: view,
            last_run,
            queue_depth,
            budget,
        });
    }

    Ok(Dashboard {
        repos: cards,
        mode: mode.as_str().to_owned(),
        paused,
    })
}

/// Render for a person or a script.
pub fn render(dashboard: &Dashboard, json: bool) -> Result<String, DashboardError> {
    if json {
        serde_json::to_string_pretty(dashboard)
            .map_err(|source| DashboardError::Unrenderable { source })
    } else {
        Ok(dashboard.render_human())
    }
}
