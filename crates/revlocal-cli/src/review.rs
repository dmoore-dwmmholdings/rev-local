//! `revlocal review --repo R --rev X [--json]` (SPEC §9, §14).
//!
//! # `--json` means exactly one document on stdout
//!
//! A `--json` flag whose output is interleaved with a progress line is not
//! machine-readable — a caller piping it to `jq` gets a parse error, and the failure
//! looks like a bug in their pipeline rather than in ours. So under `--json` this
//! module writes the report to stdout and **nothing else ever does**; everything
//! informational goes to stderr, where a pipe does not see it.
//!
//! # Two renderers, not one renderer with a flag
//!
//! [`render_human`] formats for a person; the JSON path serializes
//! [`ReviewReport`] and does not touch it. Sharing one path would eventually mean
//! rounding a number or trimming a string "for readability" and silently breaking the
//! byte-stability that `report_is_byte_stable_across_runs` depends on.

use std::path::Path;

use revlocal_core::{
    Change, ChangeId, ChangeKind, DiffStat, Repo, RepoConfig, RepoId, RepoKind, Timestamp,
};
use revlocal_daemon::pipeline::{self, ReviewReport, ReviewStatus};
use revlocal_engine::MockEngine;
use revlocal_vcs::{GitAdapter, VcsAdapter};
use tokio_util::sync::CancellationToken;

/// What can stop `revlocal review`.
#[derive(Debug, thiserror::Error)]
pub enum ReviewCommandError {
    /// The repository or revision could not be read.
    #[error(transparent)]
    Vcs(#[from] revlocal_vcs::VcsError),

    /// The pipeline could not run.
    #[error(transparent)]
    Pipeline(#[from] pipeline::PipelineError),

    /// A scratch directory could not be made.
    #[error(
        "could not create a scratch directory: {0}\n  try: check that TMPDIR (or \
         TEMP on Windows) points somewhere writable with space free"
    )]
    Scratch(#[source] std::io::Error),

    /// The report could not be serialized.
    #[error("could not render the report as JSON: {0}")]
    Json(#[from] serde_json::Error),
}

/// Build the in-memory `Repo` for a path given on the command line.
///
/// `revlocal review` takes a path rather than a configured repository so it can be
/// run against anything without registering it first — which is what makes it usable
/// as a debugging tool and as this crate's test surface.
fn repo_for(path: &Path) -> Repo {
    Repo {
        id: RepoId::new(0),
        // The *name*, not the path: it reaches the fingerprint (§10.3), and a
        // fingerprint that moved when a user moved their checkout would make every
        // finding look new.
        name: path
            .file_name()
            .map_or_else(|| "repo".to_owned(), |n| n.to_string_lossy().into_owned()),
        kind: RepoKind::Git,
        local_path: Some(path.display().to_string()),
        remote_url: None,
        default_branch: None,
        engine: revlocal_core::EngineKind::Claude,
        autonomy: revlocal_core::AutonomyMode::Off,
        enabled: true,
        config_json: "{}".to_owned(),
        created_at: Timestamp::default(),
        updated_at: Timestamp::default(),
    }
}

/// Run one review and print it.
pub async fn run(repo_path: &Path, rev: &str, json: bool) -> Result<(), ReviewCommandError> {
    let repo = repo_for(repo_path);
    let adapter = GitAdapter::new();
    let config = RepoConfig::default();

    let change = Change {
        id: ChangeId::new(0),
        repo_id: repo.id,
        kind: ChangeKind::Commit,
        external_id: rev.to_owned(),
        title: None,
        author_name: None,
        author_email: None,
        authored_at: None,
        branch: None,
        base_ref: None,
        head_ref: Some(rev.to_owned()),
        url: None,
        diff_stat: DiffStat::default(),
        detected_at: Timestamp::default(),
    };

    let scratch = tempfile::tempdir().map_err(ReviewCommandError::Scratch)?;

    let context = adapter.materialize(&repo, &change, scratch.path()).await?;

    let change = Change {
        diff_stat: context.stat,
        ..change
    };

    // §1.1: no credential is ever stored, and the inner loop spends nothing. Until
    // RL-1201 wires engine selection, this command reviews with the mock engine so it
    // is safe to run anywhere — and says so, on stderr, where `--json` cannot see it.
    let engine = MockEngine::new();
    if !json {
        eprintln!(
            "revlocal: reviewing with the mock engine (live engine selection is not wired yet)"
        );
    }

    let report = pipeline::review(
        &pipeline::ReviewInputs {
            repo_name: &repo.name,
            repo_kind: "git",
            change: &change,
            config: &config,
            worktree: &context.worktree,
            diff_unified: &context.diff_unified,
            diff_files: &context.diff_files,
            labels: &[],
            suppressions: &[],
            published_fingerprints: &[],
            prior_findings: &[],
            skip: None,
            now: Timestamp::default(),
        },
        &engine,
        scratch.path(),
        &CancellationToken::new(),
    )
    .await?;

    if json {
        // The only thing this branch may write to stdout.
        println!("{}", report.to_json()?);
    } else {
        print!("{}", render_human(&report));
    }

    Ok(())
}

/// Format a report for a person.
///
/// Separate from the JSON path on purpose: sharing one would eventually mean
/// rounding or trimming a value "for readability" and breaking byte-stability.
pub fn render_human(report: &ReviewReport) -> String {
    let mut out = String::new();

    let status = match report.status {
        ReviewStatus::Done => "reviewed",
        ReviewStatus::Skipped => "skipped",
        ReviewStatus::Failed => "failed",
    };
    out.push_str(&format!("{} {} — {status}\n", report.repo, report.change));

    if let Some(reason) = &report.skip_reason {
        out.push_str(&format!("  skipped: {reason}\n"));
        return out;
    }
    if let Some(failure) = &report.failure {
        out.push_str(&format!("  failed: {failure}\n"));
        return out;
    }

    out.push_str(&format!("  depth: {}", report.depth));
    if report.escalated {
        out.push_str(" (escalated)");
    }
    out.push('\n');
    for reason in &report.depth_reasons {
        out.push_str(&format!("    - {reason}\n"));
    }

    // §18: truncation is stated, with what was lost, never merely flagged.
    if report.truncated {
        out.push_str(&format!(
            "  diff reduced: {} file(s) omitted, {} shown as a stat line\n",
            report.omitted_files.len(),
            report.reduced_files.len()
        ));
        for omitted in &report.omitted_files {
            out.push_str(&format!("    - omitted: {omitted}\n"));
        }
    }

    if report.findings.is_empty() {
        out.push_str("  no findings\n");
    } else {
        out.push_str(&format!("  {} finding(s):\n", report.findings.len()));
        for finding in &report.findings {
            let where_ = finding.file.as_deref().unwrap_or("(repo-wide)");
            let line = finding
                .line_start
                .map_or_else(String::new, |l| format!(":{l}"));
            let out_of_diff = if finding.out_of_diff {
                " [not in this change]"
            } else {
                ""
            };
            out.push_str(&format!(
                "    {} {where_}{line}{out_of_diff} — {}\n",
                finding.severity, finding.title
            ));
        }
    }

    // §18 again: a finding held back is reported, not silently absent.
    for withheld in &report.withheld {
        out.push_str(&format!(
            "  {}: {} ({})\n",
            withheld.state, withheld.title, withheld.reason
        ));
    }

    out
}
