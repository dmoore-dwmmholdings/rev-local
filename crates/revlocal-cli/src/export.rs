//! `revlocal db export --format json` (RL-1201, SPEC §14).
//!
//! # What "export" was decided to mean
//!
//! §14 names the command and its format and says nothing about its content, so
//! the shape was a choice. Three readings were plausible — a backup, a portable
//! review record, a debug dump — and this is the second.
//!
//! **A backup is already solved, and better, by copying the file.** The store is
//! one SQLite database: `cp rev-local.db backup.db` is atomic, needs no format,
//! cannot drift from the schema, and restores by copying back. A JSON export
//! aiming at backup would be a worse `cp` that also has to be maintained across
//! every migration.
//!
//! **A debug dump is already served** by `runs show`, the audit log and the
//! transcripts on disk, all of which have `--json`.
//!
//! What neither covers is taking the *review record* somewhere else: reading last
//! month's findings on another machine, or keeping them before `db vacuum` deletes
//! the rows. That is what this produces.
//!
//! # Four properties, each load-bearing
//!
//! **A `schema_version`.** The first thing an unversioned format needs is a
//! version, and by then something is already reading it.
//!
//! **`excluded` is listed, not implied.** §18: an export that silently omits
//! tables is indistinguishable from one where those tables were empty. The field
//! costs nothing and answers "is this everything?" without anybody reading source.
//!
//! **Deterministic order** (ADR 0024). Ordered by id throughout, so two exports of
//! an unchanged database are byte-identical and a diff means something changed.
//!
//! **No secrets, and that is checkable.** The schema carries none: decision D9
//! keeps API keys out of rev-local entirely, and `webhook_secret_ref` is a
//! keychain *reference*. `audit.detail_json` is the one free-form column and it is
//! excluded — which is a second reason to exclude it beyond it being a debug
//! artefact.

use revlocal_core::{RepoId, RunId};
use revlocal_store::{ChangeStore, FindingStore, Pool, RepoStore, RunStore};
use serde::{Deserialize, Serialize};

/// The format version this build writes.
///
/// Bumped when a consumer would have to change. Adding a field is not that;
/// removing or reinterpreting one is.
pub const SCHEMA_VERSION: u32 = 1;

/// How many runs an export reads before stopping.
///
/// Not a silent cap: [`Export::truncated`] says when it was hit, and the human
/// output says so too. A number rather than "all" because an unbounded read of a
/// year-old database would hold the whole thing in memory to serialise it.
pub const EXPORT_RUN_CAP: u32 = 100_000;

/// Tables deliberately absent, and why.
///
/// A `&[(table, reason)]` rather than a bare list, because "why is `audit` not
/// here" is the question the list provokes and the answer belongs with it.
pub const EXCLUDED: &[(&str, &str)] = &[
    (
        "audit",
        "free-form `detail_json`; a debug artefact rather than a review record",
    ),
    (
        "publish_action",
        "where a finding was sent, which is about this install rather than the review",
    ),
    (
        "budget_ledger",
        "spend against this machine's ceilings; meaningless elsewhere",
    ),
    (
        "cursor",
        "discovery position; meaningless outside this install",
    ),
    ("setting", "local daemon state such as the kill switch"),
    ("suppression", "local policy, not a finding"),
];

/// Why an export could not be produced.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    /// The database could not be read.
    #[error("could not read the local database: {source}\n  try: revlocal db migrate")]
    Store {
        /// Why.
        #[source]
        source: Box<revlocal_store::StoreError>,
    },

    /// `--format` named something this build cannot write.
    #[error("unknown export format {given:?}\n  try: --format json")]
    UnknownFormat {
        /// What was asked for.
        given: String,
    },

    /// The document could not be serialised.
    #[error("could not render the export: {source}")]
    Unrenderable {
        /// Why.
        #[source]
        source: serde_json::Error,
    },
}

fn boxed(source: revlocal_store::StoreError) -> ExportError {
    ExportError::Store {
        source: Box::new(source),
    }
}

/// One repository, as exported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportedRepo {
    /// Stable within one export, and the join key for runs.
    pub id: i64,
    /// The repository's name.
    pub name: String,
    /// Which VCS backs it.
    pub kind: String,
    /// Which engine reviews it.
    pub engine: String,
    /// Its remote, when it has one.
    ///
    /// `local_path` is deliberately absent: it is where this machine keeps the
    /// repository, which says nothing to a reader elsewhere and is the closest
    /// thing here to personal information.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
}

/// One run, as exported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportedRun {
    /// The run's id, and the join key for findings.
    pub id: i64,
    /// Which repository it belongs to.
    pub repo_id: i64,
    /// The change it reviewed, as the VCS names it.
    pub change: String,
    /// Which attempt this was.
    pub attempt: u32,
    /// Where it ended up.
    pub status: String,
    /// Which engine ran it.
    pub engine: String,
    /// Its verdict, if it reached one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    /// Tokens spent, as far as anybody knows.
    pub tokens: u64,
    /// Whether that number is the whole story (RL-409).
    ///
    /// Carried into the export for the same reason it exists at all: a total that
    /// might be a lower bound must not read as a total somewhere else either.
    pub tokens_known: bool,
    /// When it finished, if it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
}

/// One finding, as exported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportedFinding {
    /// The finding's id.
    pub id: i64,
    /// Which run produced it.
    pub run_id: i64,
    /// The fingerprint, which is how the same finding is recognised across runs.
    pub fingerprint: String,
    /// How bad.
    pub severity: String,
    /// What kind.
    pub category: String,
    /// Where, when it names a file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// The first line of the range, when it names one.
    ///
    /// `line_start` only. §10.3's fingerprint deliberately does not include line
    /// numbers — the same finding moves as code above it changes — so an export
    /// carrying an exact range would invite a consumer to key on something the
    /// product itself treats as unstable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_start: Option<u32>,
    /// One line.
    pub title: String,
}

/// A table left out, with the reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Excluded {
    /// The table.
    pub table: String,
    /// Why it is not here.
    pub reason: String,
}

/// The whole document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Export {
    /// The format version. See [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// When this was produced.
    pub exported_at: String,
    /// Repositories, by id.
    pub repos: Vec<ExportedRepo>,
    /// Runs, by id.
    pub runs: Vec<ExportedRun>,
    /// Findings, by id.
    pub findings: Vec<ExportedFinding>,
    /// What is not here, and why (§18).
    pub excluded: Vec<Excluded>,
    /// Whether [`EXPORT_RUN_CAP`] was reached.
    ///
    /// §18 again: a truncated export that did not say so would present a partial
    /// history as a whole one.
    pub truncated: bool,
}

impl Export {
    /// The one-line summary the human path prints to stderr.
    pub fn summary_line(&self) -> String {
        let mut line = format!(
            "exported {} repositor{}, {} run(s) and {} finding(s)",
            self.repos.len(),
            if self.repos.len() == 1 { "y" } else { "ies" },
            self.runs.len(),
            self.findings.len()
        );
        if self.truncated {
            line.push_str(&format!(
                "; stopped at {EXPORT_RUN_CAP} runs, so this is not the whole history"
            ));
        }
        line
    }
}

/// Produce the export (SPEC §14).
pub async fn export(
    pool: &Pool,
    format: &str,
    at: revlocal_core::Timestamp,
) -> Result<Export, ExportError> {
    // §14 says `--format json` and this build writes exactly that. Accepting a
    // format it cannot write, or silently writing JSON for `--format yaml`, are
    // both worse than saying so.
    if format != "json" {
        return Err(ExportError::UnknownFormat {
            given: format.to_owned(),
        });
    }

    let repos = RepoStore::new(pool).list().await.map_err(boxed)?;
    let mut exported_repos: Vec<ExportedRepo> = repos
        .iter()
        .map(|repo| ExportedRepo {
            id: repo.id.get(),
            name: repo.name.clone(),
            kind: repo.kind.as_str().to_owned(),
            engine: repo.engine.as_str().to_owned(),
            remote_url: repo.remote_url.clone(),
        })
        .collect();
    exported_repos.sort_by_key(|repo| repo.id);

    let runs = RunStore::new(pool)
        .list_recent(None, None, EXPORT_RUN_CAP)
        .await
        .map_err(boxed)?;
    let truncated = u32::try_from(runs.len()).unwrap_or(u32::MAX) >= EXPORT_RUN_CAP;

    let changes = ChangeStore::new(pool);
    let findings_store = FindingStore::new(pool);

    let mut exported_runs = Vec::with_capacity(runs.len());
    let mut exported_findings = Vec::new();

    for run in &runs {
        // The change is fetched per run rather than joined, because the store
        // exposes rows and not joins and a join here would mean new SQL for one
        // caller. At the cap this is 100k reads of a hot SQLite page cache.
        let change = changes.get(run.change_id).await.map_err(boxed)?;

        exported_runs.push(ExportedRun {
            id: run.id.get(),
            repo_id: change.repo_id.get(),
            change: change.external_id.clone(),
            attempt: run.attempt,
            status: run.status.as_str().to_owned(),
            engine: run.engine.as_str().to_owned(),
            verdict: run.verdict.map(|v| v.as_str().to_owned()),
            tokens: run.usage.total_tokens(),
            tokens_known: run.usage.tokens_are_known(),
            finished_at: run.finished_at.map(|at| at.to_rfc3339()),
        });

        for finding in findings_store.list_for_run(run.id).await.map_err(boxed)? {
            exported_findings.push(ExportedFinding {
                id: finding.id.get(),
                run_id: run.id.get(),
                fingerprint: finding.fingerprint.clone(),
                severity: finding.severity.as_str().to_owned(),
                category: finding.category.as_str().to_owned(),
                file: finding.file.clone(),
                line_start: finding.line_start,
                title: finding.title.clone(),
            });
        }
    }

    // ADR 0024: ordered by id, so two exports of an unchanged database are
    // byte-identical. `list_recent` returns newest first, which is right for a
    // report and wrong for a document meant to be diffed.
    exported_runs.sort_by_key(|run| run.id);
    exported_findings.sort_by_key(|finding| finding.id);

    Ok(Export {
        schema_version: SCHEMA_VERSION,
        exported_at: at.to_rfc3339(),
        repos: exported_repos,
        runs: exported_runs,
        findings: exported_findings,
        excluded: EXCLUDED
            .iter()
            .map(|(table, reason)| Excluded {
                table: (*table).to_owned(),
                reason: (*reason).to_owned(),
            })
            .collect(),
        truncated,
    })
}

/// Render the document.
///
/// Always JSON — the command's whole purpose is the document, so a "human" form
/// would either be the same thing or a lossy summary of it. The summary goes to
/// stderr instead, leaving exactly one document on stdout.
pub fn render(export: &Export) -> Result<String, ExportError> {
    serde_json::to_string_pretty(export).map_err(|source| ExportError::Unrenderable { source })
}

/// Unused ids, kept so the signature reads as it should.
const _: fn(RepoId, RunId) = |_, _| {};
