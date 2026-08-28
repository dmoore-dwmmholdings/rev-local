//! The approvals inbox (RL-803, SPEC §12.4).
//!
//! # Approve sends exactly what was reviewed
//!
//! §12.4's first criterion — "an edit after approval is impossible" — is not
//! something a UI can promise. Any number of paths can write `payload_json`: a
//! second run, a replay, a future feature, a bug. So approval records a digest of
//! the payload the human was shown, and the queue re-computes it before sending.
//! A payload that has moved since somebody looked at it is refused, terminally,
//! naming what happened.
//!
//! That turns the criterion from an intention into a check. It also means the
//! honest answer to "could a determined edit slip through" is no, rather than
//! "not through the UI".
//!
//! **Edit-then-approve** is therefore not an exception to the rule: editing
//! produces a new payload, and the digest recorded is the digest of the edited
//! one. What is impossible is editing *after* the approval, which is the case the
//! criterion is about.
//!
//! # Expiry is a decision nobody made, and says so
//!
//! An approval that times out becomes `rejected` with reason `expired`. It must
//! stay distinguishable from a person saying no: one means somebody looked and
//! declined, the other means nobody looked. Collapsing them would let a queue
//! quietly drop work that nobody ever saw and call it a decision.

use revlocal_core::{PublishAction, RiskAssessment, Timestamp};

/// SPEC §13.1's default: an approval waits three days.
pub const DEFAULT_APPROVAL_TTL_HOURS: i64 = 72;

/// The reason recorded when an approval times out (§12.4).
pub const REASON_EXPIRED: &str = "expired";

pub use revlocal_core::payload_digest;

/// Whether the payload still matches what was approved.
///
/// `None` for an action nobody approved, which is not a mismatch — most actions
/// are never approved by anyone.
pub fn payload_matches_approval(payload_json: &str, approved_digest: Option<&str>) -> Option<bool> {
    approved_digest.map(|digest| payload_digest(payload_json) == digest)
}

/// One queued action, as the inbox shows it.
///
/// Carries the payload verbatim: §12.4 requires the inbox render "exactly the
/// payload that would be sent", and anything this type summarised would be a
/// second version of the truth for somebody to be surprised by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxItem {
    /// The action.
    pub action: PublishAction,
    /// Why it is here.
    pub assessment: RiskAssessment,
    /// When it stops waiting.
    pub expires_at: Timestamp,
}

impl InboxItem {
    /// Whether this item has outlived its TTL.
    pub fn is_expired(&self, now: Timestamp) -> bool {
        now >= self.expires_at
    }

    /// The one-line reason shown beside the item.
    pub fn why(&self) -> String {
        self.assessment.explain()
    }

    /// A sentence naming the target explicitly (§15's non-negotiable UI rule).
    ///
    /// "Approve" on its own is a button somebody clicks without reading. The
    /// target and capability belong in the label, not in a column beside it.
    pub fn what_it_will_do(&self) -> String {
        format!(
            "{} on {}",
            self.action.capability.as_str(),
            self.action.target
        )
    }
}

/// When an action queued at `queued_at` stops waiting.
pub fn expires_at(queued_at: Timestamp, ttl_hours: i64) -> Timestamp {
    queued_at + chrono::Duration::hours(ttl_hours)
}

/// What a person decided about a queued action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Send it as reviewed.
    Approve,
    /// Send this and everything else queued for the same run.
    ApproveAllForRun,
    /// Do not send it.
    Reject,
    /// Do not send it, and never raise this finding again.
    RejectAndSuppress,
    /// Send this instead of what was queued.
    ///
    /// The payload carried here is what gets sent *and* what gets digested — see
    /// the module docs on why that is not a hole in the rule.
    EditThenApprove {
        /// The replacement payload.
        payload_json: String,
    },
}

impl Decision {
    /// Whether this decision results in a send.
    pub const fn approves(&self) -> bool {
        matches!(
            self,
            Self::Approve | Self::ApproveAllForRun | Self::EditThenApprove { .. }
        )
    }

    /// Whether this decision should create a suppression.
    pub const fn suppresses(&self) -> bool {
        matches!(self, Self::RejectAndSuppress)
    }

    /// The payload to record and send, given what was queued.
    pub fn effective_payload<'a>(&'a self, queued: &'a str) -> &'a str {
        match self {
            Self::EditThenApprove { payload_json } => payload_json,
            _ => queued,
        }
    }

    /// The audit event name for this decision.
    pub const fn audit_kind(&self) -> &'static str {
        match self {
            Self::Approve | Self::ApproveAllForRun => "approval_granted",
            Self::EditThenApprove { .. } => "approval_granted_with_edit",
            Self::Reject => "approval_rejected",
            Self::RejectAndSuppress => "approval_rejected_and_suppressed",
        }
    }
}

/// Why the queue refused to send an approved action.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ApprovalError {
    /// The payload changed after a human approved it.
    #[error("the payload of action {action_id} changed after it was approved; it will not be sent\n  try: review it again in the approvals inbox")]
    PayloadChangedAfterApproval {
        /// Which action.
        action_id: i64,
    },
}

/// Check an approved action before it is sent.
///
/// Called by the queue at dispatch. An action nobody approved passes — the check
/// is about honouring an approval, not about requiring one.
pub fn verify_before_send(
    action_id: i64,
    payload_json: &str,
    approved_digest: Option<&str>,
) -> Result<(), ApprovalError> {
    match payload_matches_approval(payload_json, approved_digest) {
        Some(false) => Err(ApprovalError::PayloadChangedAfterApproval { action_id }),
        Some(true) | None => Ok(()),
    }
}

/// The audit detail recorded for a decision.
pub fn decision_detail(
    action: &PublishAction,
    decision: &Decision,
    actor: &str,
) -> serde_json::Value {
    serde_json::json!({
        "action_id": action.id.get(),
        "run_id": action.run_id.get(),
        "target": action.target,
        "capability": action.capability.as_str(),
        "risk": action.risk.as_str(),
        "actor": actor,
        "edited": matches!(decision, Decision::EditThenApprove { .. }),
        "suppressed": decision.suppresses(),
    })
}

/// The audit detail recorded when an approval expires.
///
/// Separate from [`decision_detail`] because nobody decided this, and an audit log
/// that renders a timeout the same way as a person's rejection is one that cannot
/// answer "did anyone actually look at this?".
pub fn expiry_detail(action: &PublishAction, waited_hours: i64) -> serde_json::Value {
    serde_json::json!({
        "action_id": action.id.get(),
        "run_id": action.run_id.get(),
        "target": action.target,
        "capability": action.capability.as_str(),
        "reason": REASON_EXPIRED,
        "waited_hours": waited_hours,
        "actor": "none",
    })
}

/// The audit event name for an expiry.
pub const AUDIT_KIND_EXPIRED: &str = "approval_expired";
