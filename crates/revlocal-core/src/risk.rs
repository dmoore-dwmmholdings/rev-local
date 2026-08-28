//! Risk classification for publish actions (SPEC §12.3).
//!
//! [`classify`] is a pure function. It takes the shape of one action plus the
//! state around it and returns a [`RiskAssessment`] — a class *and the reasons
//! for it*. The reasons are not decoration: under `auto_low_ask_high` a high-risk
//! action lands in a human's approvals inbox, and an inbox that cannot say why
//! something is waiting there is unusable (SPEC §12.4, §18).
//!
//! Risk is computed **per action, never per run** (§12.3). Two actions from the
//! same run routinely differ: the PR comment goes out, the Andare issue waits.
//!
//! # Decision of record
//!
//! First-ever use of a `(target, capability)` pair is **always** high risk, even
//! when the action is otherwise trivially safe. This is deliberate — the first
//! time rev-local ever writes to a system, a human sees it. It is a fixed
//! constraint, not a threshold to be tuned.

use crate::{Capability, RiskClass, Verdict, LOW_CONFIDENCE_THRESHOLD};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

/// Default for `global.burst_threshold` (SPEC §13.1).
pub const DEFAULT_BURST_THRESHOLD: u32 = 10;

string_enum! {
    /// The conclusion of a `rev-local/review` check run (SPEC §11.3).
    pub enum CheckConclusion {
        /// Reported while the run is active.
        InProgress => "in_progress",
        /// Approve or comment verdict.
        Success => "success",
        /// `request_changes` while `block_on_findings` is false (the default).
        Neutral => "neutral",
        /// `request_changes` while `block_on_findings` is true.
        Failure => "failure",
    }
}

/// What an action actually does, in enough detail to classify it.
///
/// A [`Capability`] alone is not enough: §12.3 splits `post_review` by verdict,
/// `set_check` by conclusion, and `upsert_doc` by whether the page is published.
/// Those distinctions are the difference between low and high risk, so they are
/// carried in the type rather than rediscovered from a JSON payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActionIntent {
    /// A single comment on a PR or commit.
    Comment,
    /// A threaded review carrying `verdict`.
    Review {
        /// The stance the review takes.
        verdict: Verdict,
    },
    /// A check run reporting `conclusion`.
    Check {
        /// What the check concluded.
        conclusion: CheckConclusion,
    },
    /// Filing a work item.
    CreateIssue,
    /// Transitioning a work item's state.
    SetStatus,
    /// Creating or updating a document.
    UpsertDoc {
        /// `false` is a draft; `true` is Trama's `publish_page`.
        published: bool,
    },
    /// Cross-linking a document and an issue.
    LinkDocToIssue,
}

impl ActionIntent {
    /// The capability this intent requires of a target.
    ///
    /// Keeps the intent and the `publish_action.capability` column in step: the
    /// column is derived from the intent rather than set alongside it.
    pub const fn capability(self) -> Capability {
        match self {
            Self::Comment => Capability::Comment,
            Self::Review { .. } => Capability::PostReview,
            Self::Check { .. } => Capability::SetCheck,
            Self::CreateIssue => Capability::CreateIssue,
            Self::SetStatus => Capability::SetStatus,
            Self::UpsertDoc { .. } => Capability::UpsertDoc,
            Self::LinkDocToIssue => Capability::LinkDocToIssue,
        }
    }

    /// The risk this action carries on its own, before any escalation (§12.3).
    ///
    /// Low risk is "additive, easily reversible, low blast radius"; high risk is
    /// "blocks people, notifies broadly, or creates work". Two cases §12.3 does not
    /// enumerate are classified by that principle and noted here:
    ///
    /// - `Check { InProgress }` — a progress report that blocks nothing: **low**.
    /// - `LinkDocToIssue` — an additive, reversible cross-reference: **low**.
    pub const fn baseline_risk(self) -> RiskClass {
        match self {
            Self::Comment | Self::LinkDocToIssue => RiskClass::Low,

            // §10.2: the app posts a COMMENT review saying "no blocking findings"
            // rather than approving. An actual APPROVE is a stronger claim than the
            // product makes unattended, so it is high risk alongside REQUEST_CHANGES.
            Self::Review {
                verdict: Verdict::Comment,
            } => RiskClass::Low,
            Self::Review {
                verdict: Verdict::Approve | Verdict::RequestChanges,
            } => RiskClass::High,

            Self::Check {
                conclusion: CheckConclusion::Success | CheckConclusion::Neutral,
            } => RiskClass::Low,
            Self::Check {
                conclusion: CheckConclusion::InProgress,
            } => RiskClass::Low,
            Self::Check {
                conclusion: CheckConclusion::Failure,
            } => RiskClass::High,

            // An unpublished draft is low; publish_page is high (§12.3, §11.5).
            Self::UpsertDoc { published: false } => RiskClass::Low,
            Self::UpsertDoc { published: true } => RiskClass::High,

            Self::CreateIssue | Self::SetStatus => RiskClass::High,
        }
    }
}

string_enum! {
    /// Why an action was classified as it was.
    ///
    /// Rendered in the approvals inbox and written to the audit log, so a human
    /// can see what made an action wait rather than inferring it.
    pub enum RiskReason {
        /// The action is high risk by its own nature (§12.3's high-risk list).
        InherentlyHighRisk => "inherently_high_risk",
        /// This `(target, capability)` pair has never succeeded before.
        FirstUseOfCapability => "first_use_of_capability",
        /// The run's engine output had to be salvaged (§8.2).
        DegradedRun => "degraded_run",
        /// The finding's confidence is below the §12.3 threshold.
        LowConfidence => "low_confidence",
        /// The repo has exceeded its burst threshold in the last hour.
        BurstThresholdExceeded => "burst_threshold_exceeded",
    }
}

/// Everything [`classify`] needs. Values only — no lookups, no I/O.
#[derive(Debug, Clone, Copy)]
pub struct RiskInputs {
    /// What the action does.
    pub intent: ActionIntent,
    /// Whether this `(target, capability)` pair has ever succeeded before.
    ///
    /// `false` makes the action high risk unconditionally — the decision of record
    /// described on this module.
    pub pair_previously_succeeded: bool,
    /// Whether the run's engine output had to be salvaged (§8.2).
    pub run_degraded: bool,
    /// The confidence of the finding this action carries, if it carries one.
    ///
    /// `None` for actions with no finding behind them, such as a summary review.
    pub finding_confidence: Option<f64>,
    /// How many actions this repo has posted in the last hour.
    pub actions_in_last_hour: u32,
    /// The repo's burst threshold; see [`DEFAULT_BURST_THRESHOLD`].
    pub burst_threshold: u32,
}

impl RiskInputs {
    /// Inputs for `intent` with every escalation switched off.
    ///
    /// A constructor rather than `Default` because `pair_previously_succeeded`
    /// has no safe default: defaulting it to `true` would silently skip the
    /// first-use rule, which is the one rule that must never be skipped.
    pub const fn new(intent: ActionIntent, pair_previously_succeeded: bool) -> Self {
        Self {
            intent,
            pair_previously_succeeded,
            run_degraded: false,
            finding_confidence: None,
            actions_in_last_hour: 0,
            burst_threshold: DEFAULT_BURST_THRESHOLD,
        }
    }
}

/// A risk class together with every reason that produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskAssessment {
    /// The resulting class.
    pub class: RiskClass,
    /// Every reason that applied, in a stable order. Empty when the action is low
    /// risk — there is nothing to explain about an action that simply proceeds.
    pub reasons: Vec<RiskReason>,
}

impl RiskAssessment {
    /// Whether this action needs a human under `auto_low_ask_high` (§12.2).
    pub const fn requires_approval(&self) -> bool {
        matches!(self.class, RiskClass::High)
    }

    /// A one-line explanation for the approvals inbox and the audit log.
    pub fn explain(&self) -> String {
        if self.reasons.is_empty() {
            return "low risk".to_owned();
        }
        let reasons: Vec<&str> = self.reasons.iter().map(|r| r.as_str()).collect();
        format!("{} risk: {}", self.class.as_str(), reasons.join(", "))
    }
}

/// Classify one publish action (SPEC §12.3).
///
/// Total by construction: every match is exhaustive, there are no panicking
/// operations, and every input is a value. Escalations **compose** — each reason
/// that applies is recorded, so a low-risk comment on a degraded run is high risk
/// and says so.
pub fn classify(inputs: &RiskInputs) -> RiskAssessment {
    let mut reasons = Vec::new();

    if inputs.intent.baseline_risk() == RiskClass::High {
        reasons.push(RiskReason::InherentlyHighRisk);
    }

    // Decision of record: first use of a (target, capability) pair is always high
    // risk. Checked regardless of baseline, so the reason is recorded even when the
    // action would have been high risk anyway.
    if !inputs.pair_previously_succeeded {
        reasons.push(RiskReason::FirstUseOfCapability);
    }

    if inputs.run_degraded {
        reasons.push(RiskReason::DegradedRun);
    }

    // Spelled out with `partial_cmp` so the NaN case is a decision rather than an
    // accident: `NAN < 0.6` is false, so a plain comparison would let an
    // unmeasurable confidence through as if it were a high one.
    let low_confidence = inputs.finding_confidence.is_some_and(|c| {
        match c.partial_cmp(&LOW_CONFIDENCE_THRESHOLD) {
            Some(Ordering::Less) => true,
            Some(Ordering::Equal | Ordering::Greater) => false,
            // Not comparable — an unknown confidence is not a confident one, and
            // the safe direction for an unknown is to ask a human.
            None => true,
        }
    });
    if low_confidence {
        reasons.push(RiskReason::LowConfidence);
    }

    // §12.3: "> burst_threshold", so being exactly at the threshold does not
    // escalate.
    if inputs.actions_in_last_hour > inputs.burst_threshold {
        reasons.push(RiskReason::BurstThresholdExceeded);
    }

    let class = if reasons.is_empty() {
        RiskClass::Low
    } else {
        RiskClass::High
    };
    RiskAssessment { class, reasons }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Inputs for a *seasoned* pair, so the first-use rule does not mask the row
    /// under test. First use has its own tests below.
    fn seasoned(intent: ActionIntent) -> RiskInputs {
        RiskInputs::new(intent, true)
    }

    fn review(verdict: Verdict) -> ActionIntent {
        ActionIntent::Review { verdict }
    }

    fn check(conclusion: CheckConclusion) -> ActionIntent {
        ActionIntent::Check { conclusion }
    }

    #[test]
    fn every_row_of_the_spec_12_3_matrix() {
        // (intent, expected class, which line of §12.3 this is)
        let rows: [(ActionIntent, RiskClass, &str); 11] = [
            // Low risk — "additive, easily reversible, low blast radius".
            (
                ActionIntent::Comment,
                RiskClass::Low,
                "a comment on a PR/commit",
            ),
            (
                review(Verdict::Comment),
                RiskClass::Low,
                "PR review with verdict `comment`",
            ),
            (
                ActionIntent::UpsertDoc { published: false },
                RiskClass::Low,
                "an *unpublished* Trama draft page",
            ),
            (
                check(CheckConclusion::Neutral),
                RiskClass::Low,
                "a `neutral` check",
            ),
            (
                check(CheckConclusion::Success),
                RiskClass::Low,
                "a `success` check",
            ),
            // High risk — "blocks people, notifies broadly, or creates work".
            (
                review(Verdict::RequestChanges),
                RiskClass::High,
                "PR review with REQUEST_CHANGES",
            ),
            (
                review(Verdict::Approve),
                RiskClass::High,
                "PR review with APPROVE",
            ),
            (
                check(CheckConclusion::Failure),
                RiskClass::High,
                "a `failure` check run",
            ),
            (
                ActionIntent::CreateIssue,
                RiskClass::High,
                "creating an Andare issue",
            ),
            (
                ActionIntent::SetStatus,
                RiskClass::High,
                "transitioning an Andare issue's status",
            ),
            (
                ActionIntent::UpsertDoc { published: true },
                RiskClass::High,
                "`publish_page` on Trama",
            ),
        ];

        for (intent, expected, spec_line) in rows {
            let assessment = classify(&seasoned(intent));
            assert_eq!(
                assessment.class, expected,
                "§12.3 says {spec_line:?} is {expected:?}, got {assessment:?}"
            );
        }
    }

    #[test]
    fn the_two_cases_the_spec_does_not_enumerate_are_low_by_its_own_principle() {
        // Neither appears in §12.3's lists. Both are additive and reversible and
        // block nobody, so they take the low-risk side of the stated principle.
        assert_eq!(
            classify(&seasoned(check(CheckConclusion::InProgress))).class,
            RiskClass::Low,
            "an in-progress check is a progress report; it blocks nothing"
        );
        assert_eq!(
            classify(&seasoned(ActionIntent::LinkDocToIssue)).class,
            RiskClass::Low,
            "a cross-link is additive and reversible"
        );
    }

    #[test]
    fn first_use_of_a_pair_is_always_high_risk() {
        // Decision of record. Every otherwise-low action becomes high the first
        // time its (target, capability) pair is used, with no exceptions.
        let otherwise_low = [
            ActionIntent::Comment,
            review(Verdict::Comment),
            check(CheckConclusion::Success),
            check(CheckConclusion::Neutral),
            check(CheckConclusion::InProgress),
            ActionIntent::UpsertDoc { published: false },
            ActionIntent::LinkDocToIssue,
        ];

        for intent in otherwise_low {
            assert_eq!(
                classify(&seasoned(intent)).class,
                RiskClass::Low,
                "precondition: {intent:?} is low risk once the pair is established"
            );

            let first_use = classify(&RiskInputs::new(intent, false));
            assert_eq!(
                first_use.class,
                RiskClass::High,
                "first use of the pair behind {intent:?} must be high risk"
            );
            assert!(
                first_use
                    .reasons
                    .contains(&RiskReason::FirstUseOfCapability),
                "and must say so: {first_use:?}"
            );
        }
    }

    #[test]
    fn first_use_is_recorded_even_when_the_action_was_high_risk_anyway() {
        // Otherwise the audit log would show only "inherently high risk" and lose
        // the fact that this was the first time rev-local wrote to that system.
        let assessment = classify(&RiskInputs::new(ActionIntent::CreateIssue, false));
        assert_eq!(assessment.class, RiskClass::High);
        assert!(assessment.reasons.contains(&RiskReason::InherentlyHighRisk));
        assert!(assessment
            .reasons
            .contains(&RiskReason::FirstUseOfCapability));
    }

    #[test]
    fn a_low_risk_comment_on_a_degraded_run_is_high() {
        let mut inputs = seasoned(ActionIntent::Comment);
        assert_eq!(classify(&inputs).class, RiskClass::Low);

        inputs.run_degraded = true;
        let assessment = classify(&inputs);
        assert_eq!(assessment.class, RiskClass::High);
        assert_eq!(assessment.reasons, vec![RiskReason::DegradedRun]);
    }

    #[test]
    fn low_confidence_escalates_at_the_stated_threshold() {
        let mut inputs = seasoned(ActionIntent::Comment);

        inputs.finding_confidence = Some(LOW_CONFIDENCE_THRESHOLD);
        assert_eq!(
            classify(&inputs).class,
            RiskClass::Low,
            "§12.3 escalates below 0.6; 0.6 itself does not"
        );

        inputs.finding_confidence = Some(0.59);
        assert!(classify(&inputs)
            .reasons
            .contains(&RiskReason::LowConfidence));

        inputs.finding_confidence = None;
        assert_eq!(
            classify(&inputs).class,
            RiskClass::Low,
            "an action with no finding behind it has no confidence to judge"
        );
    }

    #[test]
    fn an_unmeasurable_confidence_escalates_rather_than_passing_silently() {
        // f64::NAN < 0.6 is false, so a naive comparison would let a NaN through as
        // confident. An unknown confidence is not a high one.
        let mut inputs = seasoned(ActionIntent::Comment);
        inputs.finding_confidence = Some(f64::NAN);
        let assessment = classify(&inputs);
        assert_eq!(assessment.class, RiskClass::High);
        assert!(assessment.reasons.contains(&RiskReason::LowConfidence));
    }

    #[test]
    fn the_burst_threshold_escalates_strictly_above_the_limit() {
        let mut inputs = seasoned(ActionIntent::Comment);
        inputs.burst_threshold = DEFAULT_BURST_THRESHOLD;

        inputs.actions_in_last_hour = DEFAULT_BURST_THRESHOLD;
        assert_eq!(
            classify(&inputs).class,
            RiskClass::Low,
            "§12.3 says `> burst_threshold`, so being at it is not over it"
        );

        inputs.actions_in_last_hour = DEFAULT_BURST_THRESHOLD + 1;
        let assessment = classify(&inputs);
        assert_eq!(assessment.class, RiskClass::High);
        assert!(assessment
            .reasons
            .contains(&RiskReason::BurstThresholdExceeded));
    }

    #[test]
    fn escalations_compose_and_every_reason_is_kept() {
        // All five reasons at once. Losing any of them would leave the approvals
        // inbox unable to explain what is actually wrong.
        let inputs = RiskInputs {
            intent: ActionIntent::CreateIssue,
            pair_previously_succeeded: false,
            run_degraded: true,
            finding_confidence: Some(0.1),
            actions_in_last_hour: 99,
            burst_threshold: DEFAULT_BURST_THRESHOLD,
        };
        let assessment = classify(&inputs);

        assert_eq!(assessment.class, RiskClass::High);
        assert_eq!(
            assessment.reasons,
            vec![
                RiskReason::InherentlyHighRisk,
                RiskReason::FirstUseOfCapability,
                RiskReason::DegradedRun,
                RiskReason::LowConfidence,
                RiskReason::BurstThresholdExceeded,
            ],
            "reasons must be complete and in a stable order"
        );
        assert!(assessment.requires_approval());
    }

    #[test]
    fn a_low_risk_action_carries_no_reasons_to_explain() {
        let assessment = classify(&seasoned(ActionIntent::Comment));
        assert!(assessment.reasons.is_empty());
        assert!(!assessment.requires_approval());
        assert_eq!(assessment.explain(), "low risk");
    }

    #[test]
    fn the_explanation_names_every_reason() {
        let mut inputs = seasoned(ActionIntent::CreateIssue);
        inputs.run_degraded = true;
        let explanation = classify(&inputs).explain();
        assert!(explanation.starts_with("high risk: "), "{explanation}");
        assert!(
            explanation.contains("inherently_high_risk"),
            "{explanation}"
        );
        assert!(explanation.contains("degraded_run"), "{explanation}");
    }

    #[test]
    fn every_intent_maps_to_the_capability_its_target_must_advertise() {
        assert_eq!(ActionIntent::Comment.capability(), Capability::Comment);
        assert_eq!(
            review(Verdict::Comment).capability(),
            Capability::PostReview
        );
        assert_eq!(
            check(CheckConclusion::Success).capability(),
            Capability::SetCheck
        );
        assert_eq!(
            ActionIntent::CreateIssue.capability(),
            Capability::CreateIssue
        );
        assert_eq!(ActionIntent::SetStatus.capability(), Capability::SetStatus);
        assert_eq!(
            ActionIntent::UpsertDoc { published: true }.capability(),
            Capability::UpsertDoc
        );
        assert_eq!(
            ActionIntent::LinkDocToIssue.capability(),
            Capability::LinkDocToIssue
        );
    }

    #[test]
    fn classification_is_total_over_every_intent_shape() {
        // Exhaustive over the intent space, including both booleans and every
        // enum variant, under both first-use states. Nothing panics, and every
        // combination yields a class.
        let mut intents = vec![
            ActionIntent::Comment,
            ActionIntent::CreateIssue,
            ActionIntent::SetStatus,
            ActionIntent::LinkDocToIssue,
            ActionIntent::UpsertDoc { published: true },
            ActionIntent::UpsertDoc { published: false },
        ];
        intents.extend(Verdict::ALL.iter().copied().map(review));
        intents.extend(CheckConclusion::ALL.iter().copied().map(check));

        for intent in intents {
            for seen_before in [true, false] {
                for degraded in [true, false] {
                    for confidence in [None, Some(0.0), Some(1.0), Some(f64::NAN)] {
                        let inputs = RiskInputs {
                            intent,
                            pair_previously_succeeded: seen_before,
                            run_degraded: degraded,
                            finding_confidence: confidence,
                            actions_in_last_hour: 0,
                            burst_threshold: DEFAULT_BURST_THRESHOLD,
                        };
                        let assessment = classify(&inputs);
                        assert_eq!(
                            assessment.class == RiskClass::High,
                            !assessment.reasons.is_empty(),
                            "class and reasons must agree for {inputs:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn an_assessment_round_trips_through_json_for_the_audit_log() {
        let assessment = classify(&RiskInputs::new(ActionIntent::CreateIssue, false));
        let json = serde_json::to_string(&assessment).unwrap_or_default();
        let back: RiskAssessment = serde_json::from_str(&json)
            .unwrap_or_else(|e| panic!("assessment must survive the audit log: {e}"));
        assert_eq!(back, assessment);
    }
}
