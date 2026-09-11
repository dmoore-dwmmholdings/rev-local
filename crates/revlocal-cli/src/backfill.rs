//! `revlocal backfill` (RL-1201, SPEC §7.4).
//!
//! The front end for RL-1007's scheduler. The precedence rules, the resume
//! semantics and the limit arithmetic all live there and are tested there; this
//! enumerates candidates and renders what it would do.
//!
//! # Enumeration is discovery with a different starting point
//!
//! `discover(repo, cursor, limit)` returns changes *after* a cursor, which is
//! exactly what `--since <ref>` asks for. Backfill does not need a second way to
//! walk history — it needs the same walk started somewhere older, which is why
//! there is no new adapter method here.
//!
//! # `--dry-run` cannot reach an engine
//!
//! Not a flag on the execution path: enumeration and execution are separate
//! functions, and this one takes no engine. The reason to dry-run a backfill is to
//! find out what it would cost before spending it, and a dry run that spent tokens
//! to answer would be worse than useless (RL-1007's criterion 3).

use revlocal_core::{Cursor, RepoId, Timestamp};
use revlocal_daemon::backfill::{backfill_scope, plan, BackfillItem, BackfillPlan};
use revlocal_store::{CursorStore, Pool, RepoStore};
use serde::{Deserialize, Serialize};

/// How many candidates are enumerated before `--limit` is applied.
///
/// A bound rather than "all of history", because this reads them into memory. It
/// is deliberately far above any plausible `--limit`, so the count `plan` reports
/// as excluded is the real one in every case somebody will meet — and when it is
/// not, [`BackfillReport::truncated_enumeration`] says so rather than letting a
/// capped count read as a total.
pub const ENUMERATION_CAP: usize = 10_000;

/// Why a backfill could not be planned.
#[derive(Debug, thiserror::Error)]
pub enum BackfillError {
    /// The database could not be read.
    #[error("could not read the local database: {source}\n  try: revlocal db migrate")]
    Store {
        /// Why.
        #[source]
        source: Box<revlocal_store::StoreError>,
    },

    /// No repository by that name.
    #[error("no repository named {name} is configured\n  try: revlocal repo list")]
    NoSuchRepo {
        /// The name asked for.
        name: String,
    },

    /// History could not be walked.
    ///
    /// The remedy is carried rather than written into the message: it used to say
    /// `git rev-parse` unconditionally, and appending git advice to a Subversion
    /// failure is misdirection however correct the sentence above it is
    /// (RL-1559).
    #[error("could not enumerate history from {since}: {detail}\n  try: {hint}")]
    Enumerate {
        /// What was asked for.
        since: String,
        /// What went wrong.
        detail: String,
        /// How to check `since` in this repository's own terms.
        hint: String,
    },

    /// The repository's kind has no adapter, so there is no history to read.
    ///
    /// Separate from [`Enumerate`](Self::Enumerate) because that variant appends
    /// a `git rev-parse` remedy, and telling somebody to run it inside a
    /// Subversion working copy is worse than saying nothing (RL-1558).
    #[error("{detail}")]
    UnsupportedKind {
        /// What is wrong and what to try.
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

fn boxed(source: revlocal_store::StoreError) -> BackfillError {
    BackfillError::Store {
        source: Box::new(source),
    }
}

/// What a backfill would do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackfillReport {
    /// Which repository.
    pub repo: String,
    /// The cursor scope this advances — distinct from discovery's (§7.4).
    pub scope: String,
    /// Where it resumed from, if it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resumed_from: Option<String>,
    /// The changes it would review, oldest first.
    pub items: Vec<String>,
    /// How many `--limit` excluded.
    ///
    /// §18: "showing 50 of 3,000" and "there are 50" are different statements.
    pub excluded_by_limit: usize,
    /// Whether anything was actually enqueued.
    pub executed: bool,
    /// Whether enumeration itself hit [`ENUMERATION_CAP`].
    ///
    /// §18 one level up: if this is true, `excluded_by_limit` is itself a lower
    /// bound, and saying nothing would make a capped count read as a total.
    pub truncated_enumeration: bool,
    /// What the sweep actually did, when it ran (REVL-209).
    ///
    /// Absent under `--dry-run`, which is the whole difference between the two
    /// modes: one says what it *would* review and one says what it reviewed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<revlocal_daemon::backfill::BackfillOutcome>,
}

impl BackfillReport {
    /// Whether the excluded count can be trusted as complete.
    pub const fn counts_are_complete(&self) -> bool {
        !self.truncated_enumeration
    }
}

impl BackfillReport {
    /// The human output.
    pub fn render_human(&self) -> String {
        let mut out = String::new();
        for line in self.plan_lines() {
            out.push_str(&line);
            out.push('\n');
        }
        if !self.executed {
            // §7.4 makes execution the default and `--dry-run` the opt-out, so
            // this line is now what a dry run says rather than an apology for a
            // half-built command. It names the invocation that would spend,
            // because the reason to dry-run is to decide whether to.
            out.push_str(
                "\nNothing was reviewed — this was a dry run. Drop --dry-run to \
                 review these changes.\n",
            );
            return out;
        }
        match &self.outcome {
            Some(outcome) => {
                for line in outcome.summary_lines() {
                    out.push_str(&line);
                    out.push('\n');
                }
                if outcome.finished_the_plan() && outcome.reviewed.is_empty() {
                    // "Nothing to do" is a result, and a command that printed a
                    // header and stopped would read as one that failed quietly.
                    out.push_str("  nothing to review\n");
                }
            }
            // Marked executed with nothing to show is a contradiction, and saying
            // so beats printing the plan as though it had run.
            None => out.push_str("  (no outcome was recorded for this run)\n"),
        }
        out
    }

    fn plan_lines(&self) -> Vec<String> {
        let mut lines = match &self.resumed_from {
            Some(cursor) => vec![format!(
                "resuming {} after {cursor}: {} change(s) to review",
                self.scope,
                self.items.len()
            )],
            None => vec![format!(
                "{}: {} change(s) to review",
                self.scope,
                self.items.len()
            )],
        };
        if self.excluded_by_limit > 0 {
            lines.push(format!(
                "  {} more match --since and were excluded by --limit{}",
                self.excluded_by_limit,
                if self.truncated_enumeration {
                    format!(" (at least — enumeration stopped at {ENUMERATION_CAP})")
                } else {
                    String::new()
                }
            ));
        }
        for item in &self.items {
            lines.push(format!("  {item}"));
        }
        lines
    }
}

/// A planned backfill, with everything execution needs alongside it.
///
/// `changes` is carried rather than re-derived. A [`BackfillItem`] has an id and
/// a summary — enough to *list*, not enough to review: materialisation needs the
/// branch and refs that only the adapter's own `Change` rows carry. Enumerating
/// twice would also mean a repository could gain a commit between the two walks
/// and the plan could name something the execution never saw.
pub struct Planned {
    /// The repository, resolved.
    pub repo: revlocal_core::Repo,
    /// What would be reviewed, after `--since`, resume and `--limit`.
    pub plan: BackfillPlan,
    /// The adapter's rows for those items, and for everything else enumerated.
    ///
    /// Kept in the adapter's own shape rather than the stored one: §9.4's skip
    /// rules need the parents and paths that a `change` row does not carry, and
    /// the execution evaluates them exactly as discovery does.
    pub changes: Vec<revlocal_vcs::DetectedChange>,
    /// Whether enumeration itself hit [`ENUMERATION_CAP`].
    pub truncated_enumeration: bool,
}

impl Planned {
    /// The report for this plan, before anything has run.
    pub fn report(&self) -> BackfillReport {
        BackfillReport {
            repo: self.repo.name.clone(),
            scope: self.plan.scope.clone(),
            resumed_from: self.plan.resumed_from.clone(),
            items: self
                .plan
                .items
                .iter()
                .map(|item| format!("{} {}", item.external_id, item.summary))
                .collect(),
            excluded_by_limit: self.plan.excluded_by_limit,
            executed: false,
            truncated_enumeration: self.truncated_enumeration,
            outcome: None,
        }
    }
}

/// Plan a backfill without running anything (§7.4).
///
/// Takes no engine and cannot reach one.
pub async fn plan_backfill(
    pool: &Pool,
    repo_name: &str,
    since: &str,
    limit: Option<usize>,
    at: Timestamp,
) -> Result<BackfillReport, BackfillError> {
    Ok(enumerate(pool, repo_name, since, limit, at).await?.report())
}

/// Walk history and decide what a backfill would review.
///
/// Shared by the dry run and the execution, which is the point: a `--dry-run`
/// that answered a different question from the run it precedes would be a worse
/// than useless preview, and two copies of this walk would eventually answer
/// differently.
///
/// Takes no engine and cannot reach one, so the dry-run path still cannot spend
/// anything (RL-1007 criterion 3).
pub async fn enumerate(
    pool: &Pool,
    repo_name: &str,
    since: &str,
    limit: Option<usize>,
    _at: Timestamp,
) -> Result<Planned, BackfillError> {
    let repo = RepoStore::new(pool)
        .list()
        .await
        .map_err(boxed)?
        .into_iter()
        .find(|repo| repo.name == repo_name)
        .ok_or_else(|| BackfillError::NoSuchRepo {
            name: repo_name.to_owned(),
        })?;

    let branch = repo
        .default_branch
        .clone()
        .unwrap_or_else(|| "main".to_owned());
    let discovery_scope = Cursor::commits_scope(&branch);
    let scope = backfill_scope(&discovery_scope);

    // `--since` is the starting point, expressed the way discovery already
    // expresses one. A backfill is the same walk begun somewhere older.
    let start = Cursor {
        repo_id: repo.id,
        scope: discovery_scope.clone(),
        value: since.to_owned(),
        updated_at: _at,
    };

    // Enumerate **without** the user's limit, and let `plan` apply it.
    //
    // Passing it to both looks equivalent and is not: `discover` would return only
    // as many as the limit, `plan` would see exactly that many, and
    // `excluded_by_limit` would be zero. `--limit 2` against four candidates then
    // reports "2 change(s) to review" — which is the "showing 50 of 3,000" failure
    // §18 names, produced by the code that exists to report it.
    //
    // Found by running it against a five-commit repository.
    // Chosen by kind rather than hardcoded to git. A Subversion repository used
    // to come back "is not a git repository", with a second remedy suggesting
    // `git rev-parse` inside it (RL-1558, RL-1559).
    let adapter =
        revlocal_vcs::adapter_for(&repo).map_err(|error| BackfillError::UnsupportedKind {
            detail: error.to_string(),
        })?;

    // The remedy in this repository's own terms: `git rev-parse` means nothing in
    // a Subversion working copy.
    let hint = match repo.kind {
        revlocal_core::RepoKind::Svn => format!(
            "check that `{since}` is a revision this repository knows — `svn log` lists them"
        ),
        _ => format!(
            "check that `{since}` is a ref this repository knows — `git rev-parse {since}` in the working copy answers that"
        ),
    };

    let changes = adapter
        .discover(&repo, Some(&start), ENUMERATION_CAP)
        .await
        .map_err(|error| BackfillError::Enumerate {
            since: since.to_owned(),
            detail: error.to_string(),
            hint,
        })?;

    let candidates: Vec<BackfillItem> = changes
        .iter()
        .map(|change| BackfillItem {
            external_id: change.external_id.clone(),
            summary: change.title.clone().unwrap_or_default(),
        })
        .collect();

    // Its own cursor, so an interrupted backfill resumes where it stopped —
    // §7.4's `backfill:` scope, distinct from discovery's because the two walk in
    // opposite directions.
    let resume = CursorStore::new(pool)
        .get(repo.id, &scope)
        .await
        .map_err(boxed)?
        .map(|cursor| cursor.value);

    let planned: BackfillPlan = plan(repo.id, &scope, &candidates, resume.as_deref(), limit);

    Ok(Planned {
        truncated_enumeration: candidates.len() >= ENUMERATION_CAP,
        repo,
        plan: planned,
        changes,
    })
}

/// Which repository a backfill would touch, for callers that only need the id.
pub const fn repo_of(report_id: RepoId) -> RepoId {
    report_id
}

/// Render for whichever output the caller asked for.
pub fn render(report: &BackfillReport, json: bool) -> Result<String, BackfillError> {
    if json {
        return serde_json::to_string_pretty(report)
            .map_err(|source| BackfillError::Unrenderable { source });
    }
    Ok(report.render_human())
}

/// Run a backfill: enumerate, then review, oldest first (§7.4).
///
/// The command §7.4 describes — `--dry-run` is the opt-out, not the only mode.
/// Until REVL-209 this function did not exist and `plan_backfill` hardcoded
/// `executed: false`, so the spec clause "enqueues them at low priority behind
/// live work" was answered by a line of output apologising for not doing it.
///
/// Everything about ordering, fairness and resumption lives in
/// `revlocal_daemon::backfill::execute`. This resolves the config, the engine and
/// the data directory — the three things a review needs that enumeration does
/// not — and hands over.
#[allow(clippy::too_many_arguments)]
pub async fn run_backfill(
    pool: &Pool,
    config: &revlocal_core::GlobalConfig,
    data_dir: &std::path::Path,
    repo_name: &str,
    since: &str,
    limit: Option<usize>,
    at: Timestamp,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<BackfillReport, BackfillError> {
    let planned = enumerate(pool, repo_name, since, limit, at).await?;
    let mut report = planned.report();

    let outcome = revlocal_daemon::backfill::execute(
        pool,
        config,
        &revlocal_daemon::state_machine::NullSink,
        data_dir,
        &planned.repo,
        planned.plan,
        &planned.changes,
        at,
        cancel,
    )
    .await
    .map_err(|error| BackfillError::Enumerate {
        since: since.to_owned(),
        detail: error.to_string(),
        hint: "check `revlocal doctor` — a backfill needs the same engine a live review does"
            .to_owned(),
    })?;

    report.executed = true;
    report.outcome = Some(outcome);
    Ok(report)
}
