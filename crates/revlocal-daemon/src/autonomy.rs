//! What an autonomy mode does to an action (RL-801, SPEC §12.2).
//!
//! §12.2's table is small enough to look obvious, and two of its four rows are the
//! ones worth writing code carefully for.
//!
//! # `dry_run` records what would have been sent, and sends nothing
//!
//! Not "sends nothing" alone. A dry run that recorded no payload would be a mode
//! in which the only way to find out what rev-local intends is to let it do it —
//! which is the opposite of what somebody turning on a dry run is asking for. So
//! the action is written to `publish_action` with its payload intact and a status
//! of `skipped_dry_run`, and the approvals inbox renders that payload verbatim.
//!
//! The status is `skipped_dry_run` rather than `failed` or `rejected` because it
//! is neither: nothing went wrong, and nobody declined it. Collapsing it into
//! either would make a dry run look like a problem in the audit log.
//!
//! # The ceiling is a minimum, and it can only ever restrict
//!
//! `min(global, repo)` under `off < dry_run < auto_low_ask_high < auto`. A global
//! `off` therefore beats a repository set to `auto`, which is the only useful
//! direction for a ceiling to work — one that a repository could raise would not
//! be a ceiling.

use revlocal_core::{AutonomyMode, PublishActionStatus, RiskClass};

/// What happens to one action under one effective mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    /// The run does not happen at all.
    NoReview,
    /// Recorded with its payload, never sent.
    RecordOnly,
    /// Queued for delivery now.
    Send,
    /// Held for a human (§12.4).
    AwaitApproval,
}

impl Disposition {
    /// The `publish_action.status` an action starts life in.
    pub const fn initial_status(self) -> Option<PublishActionStatus> {
        match self {
            // No run means no actions, so there is no row to give a status to.
            Self::NoReview => None,
            Self::RecordOnly => Some(PublishActionStatus::SkippedDryRun),
            Self::Send => Some(PublishActionStatus::Pending),
            Self::AwaitApproval => Some(PublishActionStatus::AwaitingApproval),
        }
    }

    /// Whether anything is sent to a target.
    pub const fn sends(self) -> bool {
        matches!(self, Self::Send)
    }
}

/// Whether reviews run at all under this mode (§12.2's first column).
pub const fn reviews_run(mode: AutonomyMode) -> bool {
    !matches!(mode, AutonomyMode::Off)
}

/// §12.2's table, as a function.
///
/// Takes the **effective** mode — the caller resolves the ceiling with
/// [`revlocal_core::effective_autonomy`] rather than this deciding it, so there is
/// one place that knows how a repository's mode and the global one combine.
pub const fn disposition(mode: AutonomyMode, risk: RiskClass) -> Disposition {
    match (mode, risk) {
        (AutonomyMode::Off, _) => Disposition::NoReview,
        (AutonomyMode::DryRun, _) => Disposition::RecordOnly,
        (AutonomyMode::AutoLowAskHigh, RiskClass::Low) => Disposition::Send,
        (AutonomyMode::AutoLowAskHigh, RiskClass::High) => Disposition::AwaitApproval,
        (AutonomyMode::Auto, _) => Disposition::Send,
    }
}

/// The audit event recorded when a mode changes (§12.2's fourth criterion).
///
/// A mode change is a change to what rev-local is allowed to do without asking,
/// which is exactly the class of thing §5's audit log exists for. The entry names
/// both values because "changed to auto" is unreadable without knowing what it was
/// before.
pub const AUDIT_KIND_MODE_CHANGED: &str = "autonomy_mode_changed";

/// The detail recorded alongside [`AUDIT_KIND_MODE_CHANGED`].
pub fn mode_change_detail(scope: &str, from: AutonomyMode, to: AutonomyMode) -> serde_json::Value {
    serde_json::json!({
        "scope": scope,
        "from": from.as_str(),
        "to": to.as_str(),
        "restricts": ranks(to) < ranks(from),
    })
}

/// The ordering §12.2 gives, exposed so a caller can compare two modes without
/// re-deriving it.
const fn ranks(mode: AutonomyMode) -> u8 {
    match mode {
        AutonomyMode::Off => 0,
        AutonomyMode::DryRun => 1,
        AutonomyMode::AutoLowAskHigh => 2,
        AutonomyMode::Auto => 3,
    }
}

/// Whether moving from `from` to `to` grants rev-local more freedom.
///
/// Used to decide how loudly to report a change: taking authority away is
/// unremarkable, and granting it is the direction worth noticing.
pub const fn widens(from: AutonomyMode, to: AutonomyMode) -> bool {
    ranks(to) > ranks(from)
}
