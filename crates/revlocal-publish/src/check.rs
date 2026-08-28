//! The `rev-local/review` check run, and commit comments (RL-704, SPEC §11.3).
//!
//! # A check that is started must always be resolved
//!
//! §11.3 has the check `in_progress` while the run is active. The obligation that
//! creates is the whole of this module: a check left `in_progress` shows as a
//! spinning yellow dot on the commit forever, and in a repository with required
//! checks it blocks the merge. rev-local crashing must not be able to block
//! somebody's merge indefinitely.
//!
//! So resolution is not something the happy path does on its way out. It is
//! derived from state — [`unresolved_check`] takes a finished run and the actions
//! recorded for it and says whether a check is still owed — which means a run that
//! died between starting the check and finishing the review is resolved by the
//! next startup, from the database, without the process that started it having to
//! survive.
//!
//! # A run that failed is `neutral`, never `failure`
//!
//! `failure` is a statement about the code. A run that crashed, timed out, or was
//! killed has made no statement about the code at all, and reporting one would be
//! a lie in the direction that costs somebody an afternoon. The check says neutral
//! and the title says the review did not finish.

use revlocal_core::{CheckConclusion, PublishAction, PublishActionStatus, Run, RunStatus, Verdict};
use serde::{Deserialize, Serialize};

use crate::github::{GhRequest, ReviewOptions};

/// The check run rev-local owns.
pub const CHECK_NAME: &str = "rev-local/review";

/// Where a check run is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    /// The review is running.
    InProgress,
    /// The review is over, one way or another.
    Completed,
}

impl CheckStatus {
    /// GitHub's own name for it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
        }
    }
}

/// §11.3's mapping from a run's verdict to a check conclusion.
///
/// Returns `revlocal_core`'s `CheckConclusion` rather than a type of this crate's
/// own. The risk model (§12.3) classifies a check by its conclusion, so a second
/// enum meaning the same thing would need a conversion between them that nobody
/// would remember to keep in step — and the one place it mattered would be the
/// one deciding whether an action needs a human.
///
/// `failure` requires the repository to have opted in with `block_on_findings`,
/// which defaults to false. Same reasoning as `allow_approve`: a failing required
/// check stops a merge, and a tool that stops merges by default has decided
/// something about somebody's process that they did not ask it to decide.
pub const fn conclusion_for(verdict: Verdict, options: ReviewOptions) -> CheckConclusion {
    match verdict {
        Verdict::Approve | Verdict::Comment => CheckConclusion::Success,
        Verdict::RequestChanges if options.block_on_findings => CheckConclusion::Failure,
        Verdict::RequestChanges => CheckConclusion::Neutral,
    }
}

/// Everything needed to create or resolve the check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckPayload {
    /// `owner/name`.
    pub repo: String,
    /// The commit the check is attached to.
    pub head_sha: String,
    /// Running, or over.
    pub status: CheckStatus,
    /// How it resolved. `None` while in progress.
    ///
    /// `CheckConclusion::InProgress` exists in the core enum because §12.3
    /// classifies the in-progress check too; here the status field carries that,
    /// so this stays `None` rather than duplicating it.
    pub conclusion: Option<CheckConclusion>,
    /// The one-line title.
    pub title: String,
    /// Markdown shown under the title.
    pub summary: String,
}

impl CheckPayload {
    /// The check as it looks when a run starts.
    pub fn starting(repo: &str, head_sha: &str) -> Self {
        Self {
            repo: repo.to_owned(),
            head_sha: head_sha.to_owned(),
            status: CheckStatus::InProgress,
            conclusion: None,
            title: "Review in progress".to_owned(),
            summary: "rev-local is reviewing this change.".to_owned(),
        }
    }

    /// The check as it looks when a run finishes with a verdict.
    pub fn resolved(
        repo: &str,
        head_sha: &str,
        verdict: Verdict,
        options: ReviewOptions,
        findings: usize,
    ) -> Self {
        let conclusion = conclusion_for(verdict, options);
        let title = match (conclusion, findings) {
            (CheckConclusion::Success, 0) => "No findings".to_owned(),
            (CheckConclusion::Success, n) => format!("{n} finding(s), none blocking"),
            (_, n) => format!("{n} finding(s)"),
        };

        Self {
            repo: repo.to_owned(),
            head_sha: head_sha.to_owned(),
            status: CheckStatus::Completed,
            conclusion: Some(conclusion),
            title,
            summary: "See the rev-local review for detail.".to_owned(),
        }
    }

    /// The check as it looks for a run that did not finish.
    ///
    /// Always `neutral`. `failure` is a statement about the code, and a run that
    /// crashed has made no statement about the code — reporting one would be a lie
    /// in the direction that costs somebody an afternoon.
    pub fn abandoned(repo: &str, head_sha: &str, reason: &str) -> Self {
        Self {
            repo: repo.to_owned(),
            head_sha: head_sha.to_owned(),
            status: CheckStatus::Completed,
            conclusion: Some(CheckConclusion::Neutral),
            title: "Review did not finish".to_owned(),
            summary: format!(
                "rev-local did not complete this review: {reason}\n\nThe check is \
                 resolved as neutral rather than failing, because no conclusion was \
                 reached about the change."
            ),
        }
    }
}

/// Whether this run still owes a resolved check, and what it should say.
///
/// Takes the run and the actions recorded for it rather than talking to GitHub:
/// the question is "did we start a check and never finish it", and the database
/// knows that without a round trip. A run that is still running owes nothing yet.
///
/// This is the startup reconciliation. Nothing about it is special-cased for
/// startup — it is the same function whenever it is asked, which is why a crash
/// between starting the check and finishing the review cannot leave the check
/// spinning: the next pass over finished runs sees the same state and resolves it.
pub fn unresolved_check(run: &Run, actions: &[PublishAction]) -> Option<CheckPayload> {
    if !run_is_finished(run.status) {
        return None;
    }

    let checks: Vec<&PublishAction> = actions
        .iter()
        .filter(|action| action.capability == revlocal_core::Capability::SetCheck)
        .collect();

    // Look at the payloads rather than the action count: two actions where the
    // second failed to send is not a resolved check, and counting would say it was.
    let mut started: Option<CheckPayload> = None;
    let mut resolved = false;

    for action in checks {
        let Ok(payload) = serde_json::from_str::<CheckPayload>(&action.payload_json) else {
            continue;
        };
        match payload.status {
            CheckStatus::InProgress => started = Some(payload),
            CheckStatus::Completed => {
                // Only a *delivered* resolution counts. One that is still pending
                // is exactly the case this function exists to catch.
                if action.status == PublishActionStatus::Sent {
                    resolved = true;
                }
            }
        }
    }

    let started = started?;
    if resolved {
        return None;
    }

    Some(CheckPayload::abandoned(
        &started.repo,
        &started.head_sha,
        run.error.as_deref().unwrap_or("the run did not complete"),
    ))
}

/// Whether a run has reached a state it will not leave.
const fn run_is_finished(status: RunStatus) -> bool {
    matches!(
        status,
        RunStatus::Done | RunStatus::Failed | RunStatus::Cancelled | RunStatus::Skipped
    )
}

/// Create or update the check run.
///
/// One endpoint for both: GitHub's check-run API takes a create, and rev-local
/// re-creates rather than patching because a check run is keyed by name and head
/// SHA on GitHub's side — posting the same name for the same SHA supersedes the
/// previous one, which is exactly the idempotency §11.6 asks for and one fewer
/// round trip than fetching an id to patch.
pub fn gh_set_check(payload: &CheckPayload) -> Result<GhRequest, serde_json::Error> {
    let mut body = serde_json::json!({
        "name": CHECK_NAME,
        "head_sha": payload.head_sha,
        "status": payload.status.as_str(),
        "output": {
            "title": payload.title,
            "summary": payload.summary,
        },
    });

    if let Some(conclusion) = payload.conclusion {
        body["conclusion"] = serde_json::json!(conclusion.as_str());
    }

    Ok(GhRequest {
        args: vec![
            "api".to_owned(),
            "--method".to_owned(),
            "POST".to_owned(),
            format!("repos/{}/check-runs", payload.repo),
            "--input".to_owned(),
            "-".to_owned(),
        ],
        stdin: Some(serde_json::to_string(&body)?),
    })
}

/// A comment on a commit, for changes that are not pull requests (§11.3).
pub fn gh_commit_comment(
    repo: &str,
    sha: &str,
    body: &str,
) -> Result<GhRequest, serde_json::Error> {
    let payload = serde_json::json!({ "body": body });
    Ok(GhRequest {
        args: vec![
            "api".to_owned(),
            "--method".to_owned(),
            "POST".to_owned(),
            format!("repos/{repo}/commits/{sha}/comments"),
            "--input".to_owned(),
            "-".to_owned(),
        ],
        stdin: Some(serde_json::to_string(&payload)?),
    })
}
