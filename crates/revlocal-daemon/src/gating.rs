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
    /// Where this action's effect lands (RL-1519).
    ///
    /// Per action rather than per run, unlike everything else here: one run's
    /// findings can go to a tracker and to a local file, and those are not the
    /// same decision. `GateContext` is `Copy`, so a caller varies it with
    /// `GateContext { destination, ..context }`.
    pub destination: revlocal_core::Destination,
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
        destination: context.destination,
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

/// Why one action is waiting, and which setting would release it.
///
/// §12.2's effective mode is `min(global, repo)`, so an action can be held by a
/// setting the operator has already widened on the repository. Naming the wrong
/// half sends somebody to change a setting that was right, watch nothing happen,
/// and conclude the product is broken — which is what this exists to prevent
/// (REVL-198, REVL-199).
///
/// Lives here rather than in either front end because both inboxes ask it, and
/// the project's rule is that a headless operator and somebody looking at the
/// app see the same thing. A copy in each is how the two come to disagree.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Held {
    /// What is holding it, in one sentence.
    pub reason: String,
    /// What would release it, when anything would.
    ///
    /// `None` when there is nothing useful to say — advice that cannot work is
    /// worse than none, because somebody follows it, nothing happens, and they
    /// stop believing the screen.
    pub remedy: Option<String>,
}

/// Why this action is waiting (see [`Held`]).
///
/// Phrased in terms of *settings* rather than either front end's spelling of
/// them, so the CLI can print it after "try:" and the app can put it under a
/// button without either one lying about where the setting lives.
pub fn held_by(risk: revlocal_core::RiskClass, mode: revlocal_core::AutonomyMode) -> Held {
    use revlocal_core::{AutonomyMode, RiskClass};

    match (risk, mode) {
        (_, AutonomyMode::Auto) => Held {
            // Not reachable from the mode: under `auto` the gate sends. It is
            // reachable in the database — an item queued before somebody widened
            // the mode is still sitting in the inbox, and telling them to widen a
            // mode they have already widened would be nonsense.
            reason: "queued while a narrower mode was in force".to_owned(),
            remedy: Some("approving it sends it; nothing needs changing".to_owned()),
        },
        (RiskClass::High, AutonomyMode::AutoLowAskHigh) => Held {
            reason: "high risk, and the global autonomy is `auto_low_ask_high`".to_owned(),
            remedy: Some(
                "set the global mode to `auto` (§13.1's `[global] mode`); a \
                 repository's own `autonomy` widens the other half of \
                 `min(global, repo)` only"
                    .to_owned(),
            ),
        },
        (_, AutonomyMode::DryRun | AutonomyMode::Off) => Held {
            reason: format!(
                "the global autonomy is `{}`, which publishes nothing on its own",
                mode.as_str()
            ),
            remedy: Some("set the global mode to `auto` or `auto_low_ask_high`".to_owned()),
        },
        (RiskClass::Low, _) => Held {
            reason: "low risk, so the class is not what held it — first use of this \
                     target and capability pair (§12.3) is the usual reason"
                .to_owned(),
            // Deliberately none. First use is meant to be answered once, by a
            // person looking at the payload, and "turn off the first-use rule"
            // is not a remedy anybody should be handed in an inbox.
            remedy: None,
        },
    }
}
