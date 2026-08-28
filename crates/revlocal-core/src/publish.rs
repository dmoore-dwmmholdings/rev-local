//! The `publish_action` table and the publish-target support types
//! (SPEC §5, §11.1, §11.6).

use crate::{
    Capability, FindingId, PublishActionId, PublishActionStatus, RiskClass, RunId, Timestamp,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// One outbound action against one target (`publish_action`, SPEC §5).
///
/// `(target, idempotency_key)` is unique, which is what makes redelivery safe:
/// SPEC §11.6 requires at-least-once delivery with exactly-once *effect*.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishAction {
    /// Primary key.
    pub id: PublishActionId,
    /// The run this action reports on.
    pub run_id: RunId,
    /// The finding it carries, when the action is finding-scoped.
    pub finding_id: Option<FindingId>,
    /// `github` | `andare` | `trama` | a custom target id.
    pub target: String,
    /// The abstract operation being asked for.
    pub capability: Capability,
    /// Computed per action, never per run (SPEC §12.3).
    pub risk: RiskClass,
    /// Unique per target; makes redelivery idempotent.
    pub idempotency_key: String,
    /// Exactly what would be sent. The approvals inbox renders this verbatim.
    pub payload_json: String,
    /// Where the action is in its lifecycle.
    pub status: PublishActionStatus,
    /// How many delivery attempts have been made.
    pub attempts: u32,
    /// The target's response, once there is one.
    pub response_json: Option<String>,
    /// Issue key, PR review id, or page id/URL.
    pub external_ref: Option<String>,
    /// Why delivery failed.
    pub error: Option<String>,
    /// When the action was created.
    pub created_at: Timestamp,
    /// When it was delivered.
    pub sent_at: Option<Timestamp>,
}

impl PublishAction {
    /// Whether this action still needs a human before it can be sent.
    pub const fn needs_approval(&self) -> bool {
        matches!(self.status, PublishActionStatus::AwaitingApproval)
    }

    /// Whether the action has reached a state it will not leave.
    pub const fn is_terminal(&self) -> bool {
        matches!(
            self.status,
            PublishActionStatus::Sent
                | PublishActionStatus::Failed
                | PublishActionStatus::Rejected
                | PublishActionStatus::SkippedDryRun
        )
    }
}

/// What a target can actually do (SPEC §11.1, `PublishTarget::discover`).
///
/// A `BTreeSet` rather than a `Vec` so the set is order-independent and
/// deduplicated — two discoveries that find the same capabilities compare equal.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitySet {
    /// The capabilities the target reports.
    pub capabilities: BTreeSet<Capability>,
}

impl CapabilitySet {
    /// Build a set from anything iterable.
    pub fn new(capabilities: impl IntoIterator<Item = Capability>) -> Self {
        Self {
            capabilities: capabilities.into_iter().collect(),
        }
    }

    /// Whether the target advertises `capability`.
    pub fn supports(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }
}

/// The outcome of one delivered action (SPEC §11.1, `PublishTarget::execute`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishReceipt {
    /// What the target created or changed — issue key, review id, page URL.
    pub external_ref: Option<String>,
    /// The target's raw response, kept for the audit log.
    pub response_json: Option<String>,
    /// True when the target reported the effect already existed.
    ///
    /// A redelivery that lands on an existing effect is a success, not a failure;
    /// this is what distinguishes the two in the audit log (SPEC §11.6).
    pub deduplicated: bool,
}

/// Whether a target is reachable and usable (SPEC §11.1, `PublishTarget::health`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetHealth {
    /// Whether the target answered at all.
    pub reachable: bool,
    /// What it can do, when it answered.
    pub capabilities: CapabilitySet,
    /// Why it is unhealthy, when it is.
    pub detail: Option<String>,
}
