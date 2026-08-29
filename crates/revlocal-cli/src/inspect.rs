//! `revlocal approvals list` and `revlocal budget show` (RL-1201, SPEC §12.4, §13.1, §14).
//!
//! Both are reads, and both answer a question somebody asks when the system has
//! gone quiet: *what is waiting on me*, and *have I run out*. Those are the two
//! reasons rev-local stops doing anything without being broken, and neither is
//! visible from the outside.
//!
//! Neither command writes. `approvals approve` and `budget reset` are separate,
//! because a command that shows you a queue should not be able to empty it.

use revlocal_core::{BudgetLedgerEntry, BudgetSettings, RepoId, Timestamp};
use revlocal_daemon::budgets::{check, BudgetVerdict};
use revlocal_store::{BudgetLedgerStore, Pool, PublishActionStore};
use serde::{Deserialize, Serialize};

/// Why an inspection could not complete.
#[derive(Debug, thiserror::Error)]
pub enum InspectError {
    /// The database could not be read.
    #[error("could not read the local database: {source}\n  try: revlocal db migrate")]
    Store {
        /// Why.
        #[source]
        source: Box<revlocal_store::StoreError>,
    },

    /// No run with that id.
    ///
    /// Told apart from a store failure because the remedy is opposite: the
    /// database is fine and the id is not. Suggesting `db migrate` here — which is
    /// what the generic variant does — sends somebody to fix something that is not
    /// broken.
    #[error("no run with id {run_id}\n  try: revlocal runs list, to see which exist")]
    NoSuchRun {
        /// The id asked for.
        run_id: i64,
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

    /// The report could not be serialised.
    #[error("could not render the report: {source}")]
    Unrenderable {
        /// Why.
        #[source]
        source: serde_json::Error,
    },
}

fn boxed(source: revlocal_store::StoreError) -> InspectError {
    InspectError::Store {
        source: Box::new(source),
    }
}

/// One action waiting for a human (§12.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitingAction {
    /// The action's id, for `revlocal approvals approve <id>`.
    pub id: i64,
    /// The run it belongs to.
    pub run_id: i64,
    /// Where it would be sent.
    pub target: String,
    /// What it would do.
    pub capability: String,
}

/// The approvals inbox (§12.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalsReport {
    /// Everything waiting, oldest first.
    pub waiting: Vec<WaitingAction>,
}

impl ApprovalsReport {
    /// The human output.
    pub fn render_human(&self) -> String {
        if self.waiting.is_empty() {
            // Said explicitly. An empty list rendered as nothing is
            // indistinguishable from a command that failed to read anything.
            return "Nothing is waiting for approval.\n".to_owned();
        }

        let mut out = format!("{} action(s) waiting for approval\n", self.waiting.len());
        for item in &self.waiting {
            // §15's rule, applied to the CLI: a pending outbound action names its
            // target, so approving it is not a leap of faith.
            out.push_str(&format!(
                "  #{:<5} run {:<5} {} → {}\n",
                item.id, item.run_id, item.capability, item.target
            ));
        }
        out.push_str("\nApprove with: revlocal approvals approve <id>\n");
        out
    }
}

/// Read the approvals inbox.
pub async fn approvals(pool: &Pool) -> Result<ApprovalsReport, InspectError> {
    let actions = PublishActionStore::new(pool)
        .list_awaiting_approval()
        .await
        .map_err(boxed)?;

    Ok(ApprovalsReport {
        waiting: actions
            .into_iter()
            .map(|action| WaitingAction {
                id: action.id.get(),
                run_id: action.run_id.get(),
                target: action.target.clone(),
                capability: action.capability.to_string(),
            })
            .collect(),
    })
}

/// One repository's spend for one day (§13.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BudgetReport {
    /// Which repository.
    pub repo_id: i64,
    /// The day, `YYYY-MM-DD`.
    pub day: String,
    /// Runs executed.
    pub runs: u32,
    /// The run ceiling; `0` means unlimited.
    pub daily_runs: u32,
    /// Tokens known to have been spent.
    pub tokens: u64,
    /// Whether that token count is the whole story (RL-409).
    pub tokens_known: bool,
    /// The token ceiling; `0` means unlimited.
    pub daily_tokens: u64,
    /// Cost known to have been spent.
    pub known_cost_usd: f64,
    /// Whether that cost is the whole story (D10).
    pub cost_known: bool,
    /// Whether a run may start right now, and why not if not.
    pub may_run: bool,
    /// The reason, when something is stopping it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl BudgetReport {
    /// The human output.
    pub fn render_human(&self) -> String {
        let ceiling = |limit: u64| {
            if limit == 0 {
                "unlimited".to_owned()
            } else {
                limit.to_string()
            }
        };

        let mut out = format!("repo {} on {}\n", self.repo_id, self.day);
        out.push_str(&format!(
            "  runs    {} of {}\n",
            self.runs,
            ceiling(u64::from(self.daily_runs))
        ));
        // §18, and RL-409's whole point: a number that might be a lower bound must
        // not be printed as though it were a total.
        out.push_str(&format!(
            "  tokens  {}{} of {}\n",
            self.tokens,
            if self.tokens_known {
                ""
            } else {
                " (at least — one run reported no count)"
            },
            ceiling(self.daily_tokens)
        ));
        out.push_str(&format!(
            "  cost    ${:.2}{}\n",
            self.known_cost_usd,
            if self.cost_known {
                ""
            } else {
                " (at least — one run reported no price)"
            }
        ));
        out.push_str(&format!(
            "\n{}\n",
            if self.may_run {
                "A run may start.".to_owned()
            } else {
                format!(
                    "Holding: {}",
                    self.reason.as_deref().unwrap_or("the budget is spent")
                )
            }
        ));
        out
    }
}

/// Read one repository's budget for a day (§13.1).
pub async fn budget(
    pool: &Pool,
    repo_id: RepoId,
    at: Timestamp,
    settings: &BudgetSettings,
) -> Result<BudgetReport, InspectError> {
    let day = revlocal_daemon::budgets::day_of(at);
    let entry: Option<BudgetLedgerEntry> = BudgetLedgerStore::new(pool)
        .get(repo_id, &day)
        .await
        .map_err(boxed)?;

    let verdict = check(entry.as_ref(), settings);

    // A day with no ledger row is a day nothing ran — which is *not* the same as a
    // day whose spend nobody measured. `Usage::default()` means "unmeasured"
    // (RL-409, deliberately), and using it here made a fresh install report
    // "0 tokens (at least — one run reported no count)" when no run had happened
    // at all. Found by running the command, not by reading it.
    let (usage, measured) = match entry.as_ref() {
        Some(entry) => (entry.usage, None),
        None => (revlocal_core::Usage::default(), Some(true)),
    };
    let tokens_known = measured.unwrap_or_else(|| usage.tokens_are_known());
    let cost_known = measured.unwrap_or_else(|| usage.cost_is_complete());

    Ok(BudgetReport {
        repo_id: repo_id.get(),
        day,
        runs: entry.as_ref().map_or(0, |e| e.runs),
        daily_runs: settings.daily_runs_per_repo,
        tokens: usage.total_tokens(),
        tokens_known,
        daily_tokens: settings.daily_tokens_per_repo,
        known_cost_usd: entry.as_ref().map_or(0.0, |e| e.known_cost_usd),
        cost_known,
        may_run: verdict.allows_run(),
        reason: verdict.reason().map(str::to_owned),
    })
}

/// Render whichever the caller asked for.
pub fn render<T: Serialize>(report: &T, human: String, json: bool) -> Result<String, InspectError> {
    if json {
        return serde_json::to_string_pretty(report)
            .map_err(|source| InspectError::Unrenderable { source });
    }
    Ok(human)
}

/// Whether the verdict is one an operator should look at.
pub const fn needs_attention(verdict: &BudgetVerdict) -> bool {
    !verdict.allows_run()
}

// --- runs and findings (RL-1201, §14) --------------------------------------

use revlocal_core::{RunId, RunStatus, Severity};
use revlocal_store::{FindingStore, RunStore};

/// One run, as `runs list` shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRow {
    /// Its id, for `runs show <id>`.
    pub id: i64,
    /// Which change.
    pub change_id: i64,
    /// Which attempt this is.
    pub attempt: u32,
    /// Where it got to.
    pub status: String,
    /// Which engine ran it.
    pub engine: String,
    /// What it concluded, when it concluded anything.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    /// Why it was skipped, if it was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    /// Why its output had to be salvaged, if it did (§8.2).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded: Option<String>,
    /// What went wrong, if anything.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A page of runs (§14's `runs list`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunsReport {
    /// The runs shown, newest first.
    pub runs: Vec<RunRow>,
    /// How many matched in total, ignoring the limit.
    ///
    /// §18: a list showing the first twenty of nine hundred, without saying so,
    /// reads as nine hundred being twenty.
    pub matched: u32,
    /// The limit that was applied.
    pub limit: u32,
}

impl RunsReport {
    /// The human output.
    pub fn render_human(&self) -> String {
        if self.runs.is_empty() {
            return "No runs match.\n".to_owned();
        }

        let mut out = if self.matched > self.runs.len() as u32 {
            format!(
                "showing {} of {} run(s) — raise --limit to see more\n",
                self.runs.len(),
                self.matched
            )
        } else {
            format!("{} run(s)\n", self.runs.len())
        };

        for run in &self.runs {
            out.push_str(&format!(
                "  #{:<5} change {:<5} attempt {}  {:<10} {}",
                run.id, run.change_id, run.attempt, run.status, run.engine
            ));
            if let Some(verdict) = &run.verdict {
                out.push_str(&format!("  {verdict}"));
            }
            out.push('\n');

            // The three reasons a run did not do what it looks like it did. Each
            // is the answer to "why is this not what I expected", and burying them
            // in `runs show` means nobody sees them while scanning.
            for (label, value) in [
                ("skipped", &run.skip_reason),
                ("degraded", &run.degraded),
                ("error", &run.error),
            ] {
                if let Some(value) = value {
                    out.push_str(&format!("         {label}: {value}\n"));
                }
            }
        }
        out
    }
}

fn row(run: &revlocal_core::Run) -> RunRow {
    RunRow {
        id: run.id.get(),
        change_id: run.change_id.get(),
        attempt: run.attempt,
        status: run.status.as_str().to_owned(),
        engine: run.engine.as_str().to_owned(),
        verdict: run.verdict.map(|v| v.as_str().to_owned()),
        skip_reason: run.skip_reason.clone(),
        degraded: run.degraded.clone(),
        error: run.error.clone(),
    }
}

/// List recent runs (§14).
pub async fn runs(
    pool: &Pool,
    repo_id: Option<RepoId>,
    status: Option<RunStatus>,
    limit: u32,
) -> Result<RunsReport, InspectError> {
    let store = RunStore::new(pool);
    let found = store
        .list_recent(repo_id, status, limit)
        .await
        .map_err(boxed)?;
    let matched = store.count_matching(repo_id, status).await.map_err(boxed)?;

    Ok(RunsReport {
        runs: found.iter().map(row).collect(),
        matched,
        limit,
    })
}

/// One run in full, with its findings (§14's `runs show`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunDetail {
    /// The run.
    pub run: RunRow,
    /// Tokens known to have been spent.
    pub tokens: u64,
    /// Whether that is the whole story (RL-409).
    pub tokens_known: bool,
    /// Whether the diff was reduced, and what was left out (§9.4).
    pub truncated: bool,
    /// Every file omitted, by name.
    ///
    /// Names, not a count: §18's whole point about truncation is that "58 files
    /// omitted" cannot be checked and a list can.
    pub omitted_files: Vec<String>,
    /// What it found.
    pub findings: Vec<FindingRow>,
}

/// One finding, as the CLI shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingRow {
    /// Its id.
    pub id: i64,
    /// §10.3's fingerprint, which is what `findings suppress` takes.
    pub fingerprint: String,
    /// How bad.
    pub severity: String,
    /// What kind.
    pub category: String,
    /// Where.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// The claim.
    pub title: String,
    /// Where it is in its life.
    pub state: String,
}

fn finding_row(finding: &revlocal_core::Finding) -> FindingRow {
    FindingRow {
        id: finding.id.get(),
        fingerprint: finding.fingerprint.clone(),
        severity: finding.severity.as_str().to_owned(),
        category: finding.category.as_str().to_owned(),
        file: finding.file.clone(),
        title: finding.title.clone(),
        state: finding.state.as_str().to_owned(),
    }
}

impl RunDetail {
    /// The human output.
    pub fn render_human(&self) -> String {
        let mut out = format!(
            "run #{} — change {}, attempt {}, {}\n",
            self.run.id, self.run.change_id, self.run.attempt, self.run.status
        );
        if let Some(verdict) = &self.run.verdict {
            out.push_str(&format!("  verdict {verdict}\n"));
        }
        out.push_str(&format!(
            "  tokens  {}{}\n",
            self.tokens,
            if self.tokens_known {
                ""
            } else {
                " (at least — one run reported no count)"
            }
        ));

        if self.truncated {
            out.push_str(&format!(
                "  diff reduced: {} file(s) omitted\n",
                self.omitted_files.len()
            ));
            for name in &self.omitted_files {
                out.push_str(&format!("    - {name}\n"));
            }
        }

        for (label, value) in [
            ("skipped", &self.run.skip_reason),
            ("degraded", &self.run.degraded),
            ("error", &self.run.error),
        ] {
            if let Some(value) = value {
                out.push_str(&format!("  {label}: {value}\n"));
            }
        }

        if self.findings.is_empty() {
            out.push_str("\nNo findings.\n");
            return out;
        }
        out.push_str(&format!("\n{} finding(s)\n", self.findings.len()));
        for finding in &self.findings {
            out.push_str(&format!(
                "  {:<8} {:<12} {}\n",
                finding.severity,
                finding.category,
                finding.file.as_deref().unwrap_or("(no file)")
            ));
            out.push_str(&format!("           {}\n", finding.title));
            // The fingerprint is what `findings suppress` takes, so it is printed
            // rather than looked up separately.
            out.push_str(&format!("           {}\n", finding.fingerprint));
        }
        out
    }
}

/// Show one run in full (§14).
pub async fn run_detail(pool: &Pool, run_id: RunId) -> Result<RunDetail, InspectError> {
    let run = RunStore::new(pool).get(run_id).await.map_err(|source| {
        // A missing row is the caller naming something that is not there; a store
        // failure is the database being unreachable. Collapsing them would offer
        // `db migrate` to somebody whose database is perfectly healthy.
        if matches!(source, revlocal_store::StoreError::NotFound { .. }) {
            InspectError::NoSuchRun {
                run_id: run_id.get(),
            }
        } else {
            boxed(source)
        }
    })?;
    let findings = FindingStore::new(pool)
        .list_for_run(run_id)
        .await
        .map_err(boxed)?;

    Ok(RunDetail {
        run: row(&run),
        tokens: run.usage.total_tokens(),
        tokens_known: run.usage.tokens_are_known(),
        truncated: run.truncated,
        omitted_files: run.omitted_files.clone(),
        findings: findings.iter().map(finding_row).collect(),
    })
}

/// Findings across runs (§14's `findings list`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingsReport {
    /// What was found.
    pub findings: Vec<FindingRow>,
    /// How many runs were read to build this.
    pub from_runs: usize,
}

impl FindingsReport {
    /// The human output.
    pub fn render_human(&self) -> String {
        if self.findings.is_empty() {
            return format!("No findings across {} run(s).\n", self.from_runs);
        }
        let mut out = format!(
            "{} finding(s) across {} run(s)\n",
            self.findings.len(),
            self.from_runs
        );
        for finding in &self.findings {
            out.push_str(&format!(
                "  {:<8} {:<12} {}\n           {}\n           {}\n",
                finding.severity,
                finding.category,
                finding.file.as_deref().unwrap_or("(no file)"),
                finding.title,
                finding.fingerprint
            ));
        }
        out.push_str("\nSuppress one with: revlocal findings suppress <fingerprint>\n");
        out
    }
}

/// List findings, newest run first (§14).
pub async fn findings(
    pool: &Pool,
    repo_id: Option<RepoId>,
    severity: Option<Severity>,
    limit: u32,
) -> Result<FindingsReport, InspectError> {
    let runs = RunStore::new(pool)
        .list_recent(repo_id, None, limit)
        .await
        .map_err(boxed)?;
    let store = FindingStore::new(pool);

    let mut found = Vec::new();
    for run in &runs {
        for finding in store.list_for_run(run.id).await.map_err(boxed)? {
            // Filtering here rather than in SQL: severity is an ordered enum in
            // Rust and a string in SQLite, and comparing it as a string would put
            // `critical` below `low` alphabetically.
            if severity.is_none_or(|floor| finding.severity >= floor) {
                found.push(finding_row(&finding));
            }
        }
    }

    Ok(FindingsReport {
        findings: found,
        from_runs: runs.len(),
    })
}

/// Parse a run status, naming all of them when it is none.
pub fn parse_status(given: &str) -> Result<RunStatus, InspectError> {
    RunStatus::ALL
        .iter()
        .find(|status| status.as_str() == given)
        .copied()
        .ok_or_else(|| InspectError::NotAValue {
            what: "status".to_owned(),
            given: given.to_owned(),
            valid: RunStatus::ALL
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        })
}

/// Parse a severity floor, naming all of them when it is none.
pub fn parse_severity(given: &str) -> Result<Severity, InspectError> {
    [
        Severity::Info,
        Severity::Low,
        Severity::Medium,
        Severity::High,
        Severity::Critical,
    ]
    .into_iter()
    .find(|severity| severity.as_str() == given)
    .ok_or_else(|| InspectError::NotAValue {
        what: "severity".to_owned(),
        given: given.to_owned(),
        valid: "info, low, medium, high, critical".to_owned(),
    })
}
