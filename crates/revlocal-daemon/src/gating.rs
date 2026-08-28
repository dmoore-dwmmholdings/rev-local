//! Risk gating at enqueue time (RL-802, SPEC §12.3).
//!
//! §12.3 classifies **per action, not per run**, and this module is where that
//! becomes visible: one run's PR comment can be sent while the Andare issue from
//! the same run waits for a human, because they are different actions with
//! different blast radii.
//!
//! # The classification happens when the action is created
//!
//! Not when it is dispatched. The status a row is written with is what decides
//! whether RL-701's queue will ever pick it up, so classifying later would mean
//! writing every action as `pending` and hoping something intercepts it — a design
//! where the approval gate is a second chance rather than the only path.
//!
//! # The reasons travel with the class
//!
//! `RiskAssessment` carries every reason that applied, and this module keeps them
//! rather than reducing to a boolean. §12.4's inbox has to tell somebody *why*
//! they are being asked, and "high risk" on its own is an answer nobody can act
//! on — a first use of a capability and a burst threshold breach want completely
//! different responses.

use revlocal_core::{
    classify, ActionIntent, AutonomyMode, PublishActionStatus, RiskAssessment, RiskInputs,
};

use crate::autonomy::{disposition, Disposition};

/// What a repository and run contribute to every action's classification.
///
/// Gathered once per run: none of it varies between the actions of one run, and
/// looking it up per action would mean a database round trip for each finding.
#[derive(Debug, Clone, Copy)]
pub struct GateContext {
    /// The effective autonomy mode (§12.2's ceiling already applied).
    pub mode: AutonomyMode,
    /// Whether the run's engine output had to be salvaged (§8.2).
    pub run_degraded: bool,
    /// How many actions this repo has already sent in the last hour.
    pub actions_in_last_hour: u32,
    /// The repo's burst threshold.
    pub burst_threshold: u32,
}

/// One action, classified and dispositioned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatedAction {
    /// The class and every reason for it.
    pub assessment: RiskAssessment,
    /// What §12.2 does with an action of that class under this mode.
    pub disposition: Disposition,
}

impl GatedAction {
    /// The `publish_action.status` this action is written with.
    ///
    /// `None` only when the mode is `off`, where there is no run and therefore no
    /// action to write.
    pub const fn initial_status(&self) -> Option<PublishActionStatus> {
        self.disposition.initial_status()
    }

    /// Whether a human has to see this before it goes.
    pub const fn needs_approval(&self) -> bool {
        matches!(self.disposition, Disposition::AwaitApproval)
    }

    /// The line the approvals inbox and the audit log show.
    pub fn explain(&self) -> String {
        self.assessment.explain()
    }
}

/// Classify one action and decide what happens to it.
///
/// `pair_previously_succeeded` comes from the store — §12.3's first-use rule is
/// about history, and there is no safe default for it. It is a parameter rather
/// than something this function looks up so the whole gate stays a pure function
/// of values, testable without a database.
pub fn gate(
    intent: ActionIntent,
    finding_confidence: Option<f64>,
    pair_previously_succeeded: bool,
    context: GateContext,
) -> GatedAction {
    let assessment = classify(&RiskInputs {
        intent,
        pair_previously_succeeded,
        run_degraded: context.run_degraded,
        finding_confidence,
        actions_in_last_hour: context.actions_in_last_hour,
        burst_threshold: context.burst_threshold,
    });

    GatedAction {
        disposition: disposition(context.mode, assessment.class),
        assessment,
    }
}
