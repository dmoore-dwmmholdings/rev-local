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
use revlocal_vcs::{GitAdapter, VcsAdapter};
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
    #[error(
        "could not enumerate history from {since}: {detail}\n  try: check that \
         `{since}` is a ref this repository knows — `git rev-parse {since}` in the \
         working copy answers that"
    )]
    Enumerate {
        /// What was asked for.
        since: String,
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
            out.push_str(
                "\nNothing was enqueued. Reviews are not executed from here yet; \
                 `revlocal review --repo <path> --rev <ref>` reviews one change today.\n",
            );
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

/// Plan a backfill without running anything (§7.4).
///
/// Takes no engine and cannot reach one.
pub async fn plan_backfill(
    pool: &Pool,
    repo_name: &str,
    since: &str,
    limit: Option<usize>,
    _at: Timestamp,
) -> Result<BackfillReport, BackfillError> {
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
    let changes = GitAdapter::new()
        .discover(&repo, Some(&start), ENUMERATION_CAP)
        .await
        .map_err(|error| BackfillError::Enumerate {
            since: since.to_owned(),
            detail: error.to_string(),
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

    Ok(BackfillReport {
        repo: repo.name.clone(),
        scope: planned.scope.clone(),
        resumed_from: planned.resumed_from.clone(),
        items: planned
            .items
            .iter()
            .map(|item| format!("{} {}", item.external_id, item.summary))
            .collect(),
        excluded_by_limit: planned.excluded_by_limit,
        executed: false,
        truncated_enumeration: candidates.len() >= ENUMERATION_CAP,
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
