//! The approvals inbox (RL-1109, SPEC §12.4, §15 screen 5).
//!
//! # There is no second renderer, and that is the design
//!
//! §12.4: the inbox shows "exactly the payload that would be sent, rendered as the
//! target would render it". The obvious implementation is a preview renderer in
//! the UI that reproduces what the target does — and it is the wrong one, because
//! two renderers drift and the one somebody *approved* against is the one that
//! does not run.
//!
//! So there is one representation. Dispatch sends `payload_json` verbatim; this
//! carries `payload_json` verbatim; the screen displays fields out of that same
//! object. A preview cannot disagree with what is sent, because there is nothing
//! for it to disagree with.
//!
//! What the screen adds is *arrangement* — which field is the title, which is the
//! body — and that is presentation rather than content. The payload is offered
//! whole alongside it so anybody can check.
//!
//! # Why the digest is not shown
//!
//! §12.4's protection is that an edit after approval is impossible: the digest is
//! recorded at approval and re-checked at dispatch. Showing it would invite
//! somebody to compare two hex strings by eye, which is not a check anybody
//! performs correctly. The screen shows the payload; the machine checks the hash.

use revlocal_core::{PublishActionStatus, RunId};
use revlocal_store::{Pool, PublishActionStore};
use serde::{Deserialize, Serialize};

/// Why the inbox could not be read.
#[derive(Debug, thiserror::Error)]
pub enum ApprovalsError {
    /// The database could not be read.
    #[error("could not read the local database: {source}\n  try: revlocal db migrate")]
    Store {
        /// Why.
        #[source]
        source: Box<revlocal_store::StoreError>,
    },
}

fn boxed(source: revlocal_store::StoreError) -> ApprovalsError {
    ApprovalsError::Store {
        source: Box::new(source),
    }
}

/// One action waiting for a human (§12.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedAction {
    /// The action's id.
    pub id: i64,
    /// The run it belongs to, so "approve all for this run" has a scope.
    pub run_id: i64,
    /// Where it would be sent.
    pub target: String,
    /// What it would do there.
    pub capability: String,
    /// How risky §12.3 judged it.
    pub risk: String,
    /// **The payload that would be sent, verbatim.**
    ///
    /// Not a rendering of it. Dispatch sends this string; the screen reads fields
    /// out of this string. One representation, so a preview cannot disagree with
    /// what is sent.
    pub payload_json: String,
    /// Whether this action carries a finding that could be suppressed.
    ///
    /// §12.4's "reject and suppress this finding" is only meaningful when there is
    /// a finding. The button is disabled rather than hidden when there is not —
    /// one that vanishes leaves somebody wondering whether they misremember.
    pub has_finding: bool,
}

/// The inbox (§15 screen 5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalsView {
    /// Everything waiting, oldest first — the order somebody should work through.
    pub waiting: Vec<QueuedAction>,
}

impl ApprovalsView {
    /// The runs represented, in the order they first appear.
    ///
    /// "Approve all for this run" needs a scope, and the scope is a run rather
    /// than the whole inbox: approving everything across every repository from one
    /// button is a blast radius nobody asked for.
    pub fn runs(&self) -> Vec<i64> {
        let mut seen = Vec::new();
        for action in &self.waiting {
            if !seen.contains(&action.run_id) {
                seen.push(action.run_id);
            }
        }
        seen
    }

    /// How many actions one run's "approve all" would cover.
    ///
    /// The confirmation names this number. "Approve all" without a count is a
    /// button whose blast radius is invisible at the moment of pressing it.
    pub fn count_for_run(&self, run_id: i64) -> usize {
        self.waiting.iter().filter(|a| a.run_id == run_id).count()
    }
}

/// Read the inbox (SPEC §12.4).
pub async fn gather(pool: &Pool) -> Result<ApprovalsView, ApprovalsError> {
    let actions = PublishActionStore::new(pool)
        .list_awaiting_approval()
        .await
        .map_err(boxed)?;

    Ok(ApprovalsView {
        waiting: actions
            .into_iter()
            .filter(|action| action.status == PublishActionStatus::AwaitingApproval)
            .map(|action| QueuedAction {
                id: action.id.get(),
                run_id: action.run_id.get(),
                target: action.target,
                capability: action.capability.as_str().to_owned(),
                risk: action.risk.as_str().to_owned(),
                payload_json: action.payload_json,
                has_finding: action.finding_id.is_some(),
            })
            .collect(),
    })
}

/// Everything waiting for one run, for "approve all for this run".
pub async fn for_run(pool: &Pool, run_id: RunId) -> Result<Vec<i64>, ApprovalsError> {
    Ok(gather(pool)
        .await?
        .waiting
        .into_iter()
        .filter(|action| action.run_id == run_id.get())
        .map(|action| action.id)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queued(id: i64, run_id: i64, target: &str, has_finding: bool) -> QueuedAction {
        QueuedAction {
            id,
            run_id,
            target: target.to_owned(),
            capability: "post_review".to_owned(),
            risk: "high".to_owned(),
            payload_json: r#"{"body":"hello"}"#.to_owned(),
            has_finding,
        }
    }

    #[test]
    fn approvals_approve_all_is_scoped_to_one_run() {
        // Approving everything across every repository from one button is a blast
        // radius nobody asked for. §12.4 says "approve all for this run" and the
        // scope is the substance of it.
        let view = ApprovalsView {
            waiting: vec![
                queued(1, 10, "github", true),
                queued(2, 10, "andare", true),
                queued(3, 11, "github", false),
            ],
        };

        assert_eq!(view.runs(), vec![10, 11]);
        assert_eq!(view.count_for_run(10), 2);
        assert_eq!(view.count_for_run(11), 1);
    }

    #[test]
    fn approvals_a_run_with_nothing_waiting_counts_zero() {
        // The confirmation names the count, so it has to be right when there is
        // nothing — "approve all 0 actions" should never appear.
        let view = ApprovalsView { waiting: vec![] };

        assert!(view.runs().is_empty());
        assert_eq!(view.count_for_run(10), 0);
    }

    #[test]
    fn approvals_suppression_is_only_offered_where_there_is_a_finding() {
        // §12.4's "reject and suppress this finding" needs a finding. Offering it
        // for a run-level summary would create a suppression with nothing to
        // suppress, which is a row that can never match anything.
        let view = ApprovalsView {
            waiting: vec![queued(1, 10, "github", true), queued(2, 10, "trama", false)],
        };

        assert!(view.waiting[0].has_finding);
        assert!(!view.waiting[1].has_finding);
    }
}
