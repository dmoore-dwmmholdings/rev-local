//! The run detail screen's data (RL-1107, SPEC §15 screen 3).
//!
//! Composed in the daemon for the same reason the dashboard is: `revlocal runs
//! show` and the desktop screen must agree about what a run *was*, and the way to
//! guarantee that is for neither front end to own the answer.
//!
//! # The transcript is not in here
//!
//! §15 wants the raw transcript, collapsible. It is deliberately **not** a field
//! on this snapshot — only its size is.
//!
//! An engine transcript is routinely megabytes. Inlining it would mean the screen
//! cannot render the stage summary until the whole log has crossed the IPC
//! boundary and been parsed as JSON, so the cost of the biggest thing on the
//! screen is paid by the smallest. Collapsed-by-default in the DOM does not help:
//! by then it has already been fetched.
//!
//! So the snapshot carries `transcript_bytes` and the screen asks for the text
//! only when somebody expands it. That is what makes "does not block rendering on
//! a large log" a property of the design rather than a hope about React.
//!
//! # What is missing, and is not pretended otherwise
//!
//! §15 also asks for a **stage timeline with durations**. Nothing records stage
//! transitions today: `run` carries `status`, `started_at` and `finished_at`, and
//! that is all. A timeline drawn from those would be one bar labelled with the
//! current status, which is not a timeline — so [`RunView::stages`] reports the
//! two moments that *are* recorded and says the rest is unavailable, rather than
//! inventing boundaries nobody measured.

use revlocal_core::{RunId, Timestamp};
use revlocal_store::{FindingStore, Pool, PublishActionStore, RunStore};
use serde::{Deserialize, Serialize};

/// Why the run view could not be assembled.
#[derive(Debug, thiserror::Error)]
pub enum RunViewError {
    /// No such run.
    #[error("no run with id {id}\n  try: revlocal runs list")]
    NoSuchRun {
        /// What was asked for.
        id: i64,
    },

    /// The database could not be read.
    #[error("could not read the local database: {source}\n  try: revlocal db migrate")]
    Store {
        /// Why.
        #[source]
        source: Box<revlocal_store::StoreError>,
    },
}

fn boxed(source: revlocal_store::StoreError) -> RunViewError {
    RunViewError::Store {
        source: Box::new(source),
    }
}

/// One finding, anchored where the screen can put it against a diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnchoredFinding {
    /// The finding's id.
    pub id: i64,
    /// How bad.
    pub severity: String,
    /// What kind.
    pub category: String,
    /// One line.
    pub title: String,
    /// The file it names, if it names one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// The first line of the range it names.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_start: Option<u32>,
    /// The last line, when the finding spans more than one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line_end: Option<u32>,
    /// Whether this finding can be placed against the diff at all.
    ///
    /// A finding with no file, or one naming a file the diff does not contain,
    /// cannot be anchored — and §18 says so rather than dropping it. A review that
    /// found something outside the changed lines has still found something, and a
    /// screen that silently omitted it would be hiding a result.
    pub anchorable: bool,
}

/// What is known about when a run passed through its stages.
///
/// Deliberately not called a timeline. §15 asks for one and the data for it does
/// not exist; this is the two moments that are recorded, named honestly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stages {
    /// When the run started, if it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// When it finished, if it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
    /// Wall-clock seconds between them, when both are known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_secs: Option<i64>,
    /// Why there is no per-stage breakdown.
    ///
    /// Present rather than implied: a screen showing start and end with no
    /// explanation looks like a timeline that failed to load.
    pub per_stage_unavailable: String,
}

/// The run detail snapshot (§15 screen 3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunView {
    /// The run's id.
    pub run_id: i64,
    /// Which change it reviewed, as the VCS names it.
    pub change: String,
    /// Where it ended up.
    pub status: String,
    /// Which engine ran it.
    pub engine: String,
    /// How deep it looked.
    pub depth: String,
    /// Its verdict, if it reached one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    /// Why its output was salvaged rather than parsed cleanly (§8.2).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded: Option<String>,
    /// Tokens spent, as far as anybody knows.
    pub tokens: u64,
    /// Whether that is the whole story (RL-409).
    pub tokens_known: bool,
    /// What is known about its stages.
    pub stages: Stages,
    /// Whether the diff was reduced (§9.4).
    pub truncated: bool,
    /// Every file left out, by name.
    ///
    /// Names, not a count: §18's point about truncation is that "58 files
    /// omitted" cannot be checked and a list can.
    pub omitted_files: Vec<String>,
    /// What it found.
    pub findings: Vec<AnchoredFinding>,
    /// How large the transcript is, so the screen can warn before fetching it.
    pub transcript_bytes: u64,
    /// Per-target publish status (§11.6, §15's retry buttons).
    pub targets: Vec<TargetLine>,
}

/// One target's publish state, and whether retrying it is meaningful.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetLine {
    /// Which target.
    pub target: String,
    /// Actions delivered.
    pub sent: usize,
    /// Actions still to be attempted.
    pub pending: usize,
    /// Actions waiting on a person (§12.4).
    pub awaiting_approval: usize,
    /// Actions that will not be attempted again without a retry.
    pub failed: usize,
    /// Whether a retry would do anything.
    ///
    /// The screen disables the button when this is false rather than hiding it.
    /// A button that vanishes leaves somebody wondering whether they misremember;
    /// one that is visibly disabled says "nothing to retry here".
    pub retryable: bool,
}

impl RunView {
    /// Whether this run's output can be taken as the whole of what the engine said.
    ///
    /// Three separate things can make it not: the diff was truncated, the tokens
    /// were not fully counted, or the output was salvaged. They are reported
    /// separately because they have different remedies, and this is only for a
    /// screen deciding whether to show a banner at all.
    pub fn fully_reported(&self) -> bool {
        !self.truncated && self.tokens_known && self.degraded.is_none()
    }
}

/// Assemble the run detail (SPEC §15 screen 3).
pub async fn gather(pool: &Pool, run_id: RunId) -> Result<RunView, RunViewError> {
    let runs = RunStore::new(pool);
    let run = runs.get(run_id).await.map_err(|source| {
        // A missing row is a typo; a store failure is an unreachable database.
        // Collapsing them offers `db migrate` to somebody whose database is fine.
        if matches!(source, revlocal_store::StoreError::NotFound { .. }) {
            RunViewError::NoSuchRun { id: run_id.get() }
        } else {
            boxed(source)
        }
    })?;

    let change = revlocal_store::ChangeStore::new(pool)
        .get(run.change_id)
        .await
        .map_err(boxed)?;

    let findings = FindingStore::new(pool)
        .list_for_run(run_id)
        .await
        .map_err(boxed)?
        .into_iter()
        .map(|finding| AnchoredFinding {
            id: finding.id.get(),
            severity: finding.severity.as_str().to_owned(),
            category: finding.category.as_str().to_owned(),
            title: finding.title.clone(),
            // A finding that names no file cannot be placed against a diff, and
            // that is a property of the finding rather than of the screen.
            anchorable: finding.file.is_some() && finding.line_start.is_some(),
            file: finding.file,
            line_start: finding.line_start,
            line_end: finding.line_end,
        })
        .collect();

    // Tallied from the action rows rather than through `revlocal-publish`, which
    // the daemon deliberately depends on only for tests. A per-target count is
    // arithmetic over rows the store already returns; reaching for the publish
    // crate to get it would add an edge to the dependency graph for no more
    // information.
    let targets = tally_targets(
        &PublishActionStore::new(pool)
            .list_for_run(run_id)
            .await
            .map_err(boxed)?,
    );

    let transcript_bytes = run
        .transcript_path
        .as_deref()
        .and_then(|path| std::fs::metadata(path).ok())
        .map_or(0, |meta| meta.len());

    Ok(RunView {
        run_id: run.id.get(),
        change: change.external_id,
        status: run.status.as_str().to_owned(),
        engine: run.engine.as_str().to_owned(),
        depth: run.depth.as_str().to_owned(),
        verdict: run.verdict.map(|v| v.as_str().to_owned()),
        degraded: run.degraded.clone(),
        tokens: run.usage.total_tokens(),
        tokens_known: run.usage.tokens_are_known(),
        stages: stages_of(run.started_at, run.finished_at),
        truncated: run.truncated,
        omitted_files: run.omitted_files.clone(),
        findings,
        transcript_bytes,
        targets,
    })
}

/// What is recorded about a run's passage through its stages.
fn stages_of(started: Option<Timestamp>, finished: Option<Timestamp>) -> Stages {
    Stages {
        started_at: started.map(|at| at.to_rfc3339()),
        finished_at: finished.map(|at| at.to_rfc3339()),
        elapsed_secs: match (started, finished) {
            (Some(a), Some(b)) => Some((b - a).num_seconds()),
            _ => None,
        },
        per_stage_unavailable:
            "stage transitions are not recorded, so only start and end are known".to_owned(),
    }
}

/// Per-target counts, in target order.
///
/// `BTreeMap` rather than `HashMap`: two runs of the same data must produce the
/// same order, or a screen reshuffles its rows between refreshes and a capture
/// diff is noise (ADR 0024).
fn tally_targets(actions: &[revlocal_core::PublishAction]) -> Vec<TargetLine> {
    use revlocal_core::PublishActionStatus as Status;
    use std::collections::BTreeMap;

    let mut by_target: BTreeMap<&str, TargetLine> = BTreeMap::new();

    for action in actions {
        let line = by_target
            .entry(action.target.as_str())
            .or_insert_with(|| TargetLine {
                target: action.target.clone(),
                sent: 0,
                pending: 0,
                awaiting_approval: 0,
                failed: 0,
                retryable: false,
            });

        match action.status {
            Status::Sent => line.sent += 1,
            Status::Pending | Status::Approved => line.pending += 1,
            Status::AwaitingApproval => line.awaiting_approval += 1,
            Status::Failed => line.failed += 1,
            // A rejected action and one skipped by a dry run are both settled and
            // neither is a failure. Counting them as failed would put a retry
            // button on a decision somebody already made.
            Status::Rejected | Status::SkippedDryRun => {}
        }
    }

    by_target
        .into_values()
        .map(|mut line| {
            // Only a failed action can be retried. A pending one is already going
            // to be tried, and one awaiting approval needs a person.
            line.retryable = line.failed > 0;
            line
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use revlocal_core::{
        Capability, PublishAction, PublishActionId, PublishActionStatus, RiskClass,
    };

    fn action(target: &str, status: PublishActionStatus) -> PublishAction {
        PublishAction {
            id: PublishActionId::new(0),
            run_id: RunId::new(1),
            finding_id: None,
            target: target.to_owned(),
            capability: Capability::PostReview,
            risk: RiskClass::Low,
            idempotency_key: format!("{target}-{status:?}"),
            payload_json: "{}".to_owned(),
            status,
            attempts: 0,
            response_json: None,
            external_ref: None,
            error: None,
            created_at: chrono::Utc::now(),
            sent_at: None,
        }
    }

    #[test]
    fn run_view_only_a_failed_target_offers_a_retry() {
        // A pending action is already going to be tried and one awaiting approval
        // needs a person. Offering "retry" for either would be a button that
        // either does nothing or does the wrong thing.
        let lines = tally_targets(&[
            action("github", PublishActionStatus::Sent),
            action("andare", PublishActionStatus::Failed),
            action("trama", PublishActionStatus::AwaitingApproval),
        ]);

        let retryable: Vec<&str> = lines
            .iter()
            .filter(|line| line.retryable)
            .map(|line| line.target.as_str())
            .collect();

        assert_eq!(retryable, vec!["andare"]);
    }

    #[test]
    fn run_view_a_rejected_action_is_not_a_failure() {
        // Somebody decided. A retry button on a decision is an invitation to undo
        // it by accident, and §12.4 keeps a rejection distinct from a failure for
        // exactly this reason.
        let lines = tally_targets(&[
            action("andare", PublishActionStatus::Rejected),
            action("andare", PublishActionStatus::SkippedDryRun),
        ]);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].failed, 0);
        assert!(!lines[0].retryable);
    }

    #[test]
    fn run_view_targets_come_back_in_a_stable_order() {
        // ADR 0024. A screen that reshuffles its rows between refreshes makes a
        // capture diff meaningless and makes a reader doubt what they just read.
        let first = tally_targets(&[
            action("trama", PublishActionStatus::Sent),
            action("andare", PublishActionStatus::Sent),
            action("github", PublishActionStatus::Sent),
        ]);
        let second = tally_targets(&[
            action("github", PublishActionStatus::Sent),
            action("trama", PublishActionStatus::Sent),
            action("andare", PublishActionStatus::Sent),
        ]);

        let names: Vec<&str> = first.iter().map(|l| l.target.as_str()).collect();
        assert_eq!(names, vec!["andare", "github", "trama"]);
        assert_eq!(first, second, "the same data must tally the same way");
    }

    #[test]
    fn run_view_says_why_there_is_no_per_stage_breakdown() {
        // §15 asks for a timeline and nothing records stage transitions. Start and
        // end with no explanation looks like a timeline that failed to load, which
        // is worse than saying the data is not kept.
        let stages = stages_of(None, None);

        assert!(
            stages.per_stage_unavailable.contains("not recorded"),
            "{stages:?}"
        );
        assert!(stages.elapsed_secs.is_none());
    }

    #[test]
    fn run_view_elapsed_needs_both_ends() {
        let start = chrono::Utc::now();
        let end = start + chrono::Duration::seconds(94);

        assert_eq!(stages_of(Some(start), Some(end)).elapsed_secs, Some(94));
        // A run still going has no duration. Reporting "0s" or "now minus start"
        // would both be numbers nobody measured.
        assert_eq!(stages_of(Some(start), None).elapsed_secs, None);
    }
}
