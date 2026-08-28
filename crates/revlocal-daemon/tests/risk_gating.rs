//! Risk gating at enqueue time (RL-802, SPEC §12.3).
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use revlocal_core::{
    ActionIntent, AutonomyMode, CheckConclusion, PublishActionStatus, RiskClass, RiskReason,
    Verdict, DEFAULT_BURST_THRESHOLD,
};
use revlocal_daemon::{gate, Disposition, GateContext};

/// A settled repository: the pair has succeeded before, nothing is degraded, no
/// burst. Escalations have to be asked for one at a time.
fn settled(mode: AutonomyMode) -> GateContext {
    GateContext {
        mode,
        run_degraded: false,
        actions_in_last_hour: 0,
        burst_threshold: DEFAULT_BURST_THRESHOLD,
    }
}

// --- criterion 1: one run, two actions, two answers -----------------------

#[test]
fn risk_gating_a_pr_comment_is_sent_while_an_issue_from_the_same_run_is_queued() {
    let context = settled(AutonomyMode::AutoLowAskHigh);

    let comment = gate(ActionIntent::Comment, Some(0.9), true, context);
    let issue = gate(ActionIntent::CreateIssue, Some(0.9), true, context);

    assert_eq!(comment.assessment.class, RiskClass::Low);
    assert_eq!(comment.disposition, Disposition::Send);
    assert_eq!(comment.initial_status(), Some(PublishActionStatus::Pending));

    assert_eq!(issue.assessment.class, RiskClass::High);
    assert_eq!(issue.disposition, Disposition::AwaitApproval);
    assert_eq!(
        issue.initial_status(),
        Some(PublishActionStatus::AwaitingApproval),
        "§12.3 classifies per action, not per run — the same run's comment goes \
         while its issue waits, because they have different blast radii"
    );
}

// --- criterion 2: first use, both halves ----------------------------------

#[test]
fn risk_gating_first_use_of_a_pair_is_high_risk_under_both_modes() {
    // A comment is the lowest-risk action there is, so if first-use escalates
    // this, it escalates anything.
    let queued = gate(
        ActionIntent::Comment,
        Some(0.99),
        false,
        settled(AutonomyMode::AutoLowAskHigh),
    );

    assert_eq!(queued.assessment.class, RiskClass::High);
    assert!(
        queued
            .assessment
            .reasons
            .contains(&RiskReason::FirstUseOfCapability),
        "the reason must be recorded, or the inbox cannot say why: {:?}",
        queued.assessment.reasons
    );
    assert_eq!(
        queued.disposition,
        Disposition::AwaitApproval,
        "the first time rev-local ever writes to a system, a human sees it"
    );

    // The other half, which the criterion asks for explicitly: `auto` still sends
    // it. First-use is a classification, not a veto.
    let sent = gate(
        ActionIntent::Comment,
        Some(0.99),
        false,
        settled(AutonomyMode::Auto),
    );
    assert_eq!(sent.assessment.class, RiskClass::High);
    assert_eq!(
        sent.disposition,
        Disposition::Send,
        "under `auto` there is nobody to ask, and §12.2's table sends high-risk \
         actions — the classification still stands and is still recorded"
    );
    assert_eq!(sent.initial_status(), Some(PublishActionStatus::Pending));
}

#[test]
fn risk_gating_a_settled_pair_does_not_carry_the_first_use_reason() {
    let action = gate(
        ActionIntent::Comment,
        Some(0.99),
        true,
        settled(AutonomyMode::AutoLowAskHigh),
    );

    assert_eq!(action.assessment.class, RiskClass::Low);
    assert!(action.assessment.reasons.is_empty());
    assert_eq!(action.explain(), "low risk");
}

// --- criterion 3: a degraded run escalates everything --------------------

#[test]
fn risk_gating_a_degraded_run_escalates_all_of_its_actions() {
    let degraded = GateContext {
        run_degraded: true,
        ..settled(AutonomyMode::AutoLowAskHigh)
    };

    // Every low-risk intent §12.3 lists.
    let intents = [
        ActionIntent::Comment,
        ActionIntent::Review {
            verdict: Verdict::Comment,
        },
        ActionIntent::UpsertDoc { published: false },
        ActionIntent::Check {
            conclusion: CheckConclusion::Success,
        },
        ActionIntent::LinkDocToIssue,
    ];

    for intent in intents {
        let action = gate(intent, Some(0.99), true, degraded);
        assert_eq!(
            action.assessment.class,
            RiskClass::High,
            "{intent:?} should escalate on a degraded run"
        );
        assert!(
            action.assessment.reasons.contains(&RiskReason::DegradedRun),
            "{intent:?}: {:?}",
            action.assessment.reasons
        );
        assert_eq!(action.disposition, Disposition::AwaitApproval);
    }
}

// --- criterion 4: the burst threshold ------------------------------------

#[test]
fn risk_gating_burst_escalation_triggers_above_the_threshold_not_at_it() {
    let at_threshold = GateContext {
        actions_in_last_hour: DEFAULT_BURST_THRESHOLD,
        ..settled(AutonomyMode::AutoLowAskHigh)
    };
    let over = GateContext {
        actions_in_last_hour: DEFAULT_BURST_THRESHOLD + 1,
        ..settled(AutonomyMode::AutoLowAskHigh)
    };

    let steady = gate(ActionIntent::Comment, Some(0.99), true, at_threshold);
    assert_eq!(
        steady.assessment.class,
        RiskClass::Low,
        "§12.3 says `> burst_threshold`, so being exactly at it does not escalate"
    );

    let bursting = gate(ActionIntent::Comment, Some(0.99), true, over);
    assert_eq!(bursting.assessment.class, RiskClass::High);
    assert!(bursting
        .assessment
        .reasons
        .contains(&RiskReason::BurstThresholdExceeded));
    assert_eq!(bursting.disposition, Disposition::AwaitApproval);
}

#[test]
fn risk_gating_a_lower_configured_threshold_bites_sooner() {
    let strict = GateContext {
        burst_threshold: 2,
        actions_in_last_hour: 3,
        ..settled(AutonomyMode::AutoLowAskHigh)
    };

    assert_eq!(
        gate(ActionIntent::Comment, Some(0.99), true, strict)
            .assessment
            .class,
        RiskClass::High,
        "the threshold is the repo's, not a constant"
    );
}

// --- escalations compose --------------------------------------------------

#[test]
fn risk_gating_every_reason_that_applied_is_recorded_not_just_the_first() {
    let bad_day = GateContext {
        run_degraded: true,
        actions_in_last_hour: DEFAULT_BURST_THRESHOLD + 5,
        ..settled(AutonomyMode::AutoLowAskHigh)
    };

    let action = gate(ActionIntent::CreateIssue, Some(0.2), false, bad_day);

    for reason in [
        RiskReason::InherentlyHighRisk,
        RiskReason::FirstUseOfCapability,
        RiskReason::DegradedRun,
        RiskReason::LowConfidence,
        RiskReason::BurstThresholdExceeded,
    ] {
        assert!(
            action.assessment.reasons.contains(&reason),
            "{reason:?} missing from {:?}",
            action.assessment.reasons
        );
    }

    let explanation = action.explain();
    assert!(
        explanation.starts_with("high risk:"),
        "§12.4's inbox has to say why somebody is being asked, and `high risk` \
         alone is an answer nobody can act on: {explanation}"
    );
}

// --- the modes still bound everything ------------------------------------

#[test]
fn risk_gating_dry_run_records_both_classes_and_sends_neither() {
    let context = settled(AutonomyMode::DryRun);

    for (intent, expected) in [
        (ActionIntent::Comment, RiskClass::Low),
        (ActionIntent::CreateIssue, RiskClass::High),
    ] {
        let action = gate(intent, Some(0.99), true, context);
        assert_eq!(action.assessment.class, expected);
        assert_eq!(
            action.initial_status(),
            Some(PublishActionStatus::SkippedDryRun),
            "a dry run still classifies — the class is what the UI shows next to \
             the payload it would have sent"
        );
        assert!(!action.disposition.sends());
    }
}

#[test]
fn risk_gating_off_produces_no_action_at_all() {
    let action = gate(
        ActionIntent::Comment,
        Some(0.99),
        true,
        settled(AutonomyMode::Off),
    );
    assert_eq!(action.disposition, Disposition::NoReview);
    assert_eq!(
        action.initial_status(),
        None,
        "no run means no actions, so there is no row to give a status to"
    );
}

fn at(minute: u32) -> Timestamp {
    chrono::Utc
        .with_ymd_and_hms(2026, 8, 28, 12, minute, 0)
        .single()
        .unwrap_or_default()
}

async fn seeded() -> Result<(TempDir, Pool, RunId), Box<dyn std::error::Error>> {
    let dir = TempDir::new()?;
    let pool = open(&dir.path().join("rev-local.db")).await?;

    let repo = RepoStore::new(&pool)
        .insert(&Repo {
            id: RepoId::new(0),
            name: "rev-local".to_owned(),
            kind: RepoKind::Git,
            local_path: None,
            remote_url: None,
            default_branch: Some("main".to_owned()),
            engine: EngineKind::Mock,
            autonomy: AutonomyMode::DryRun,
            enabled: true,
            config_json: "{}".to_owned(),
            created_at: at(0),
            updated_at: at(0),
        })
        .await?;

    let change = ChangeStore::new(&pool)
        .upsert(&Change {
            id: ChangeId::new(0),
            repo_id: repo.id,
            kind: ChangeKind::Commit,
            external_id: "deadbeef".to_owned(),
            title: None,
            author_name: None,
            author_email: None,
            authored_at: None,
            branch: None,
            base_ref: None,
            head_ref: None,
            url: None,
            diff_stat: DiffStat::default(),
            detected_at: at(1),
        })
        .await?;

    let run = RunStore::new(&pool)
        .insert(&Run {
            id: RunId::new(0),
            change_id: change.id,
            attempt: 1,
            status: RunStatus::Publishing,
            engine: EngineKind::Mock,
            depth: Depth::Standard,
            trigger: TriggerSource::Manual,
            skip_reason: None,
            error: None,
            degraded: None,
            usage: Usage::default(),
            started_at: Some(at(2)),
            finished_at: None,
            transcript_path: None,
            truncated: false,
            omitted_files: Vec::new(),
            verdict: None,
            summary: None,
            created_at: at(2),
        })
        .await?;

    Ok((dir, pool, run.id))
}

// --- criterion 1, end to end: the gate actually gates ---------------------

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::TimeZone;
use revlocal_core::{
    AutonomyMode as Mode, Capability, CapabilitySet, Change, ChangeId, ChangeKind, Depth, DiffStat,
    EngineKind, PublishAction, PublishActionId, PublishReceipt, Repo, RepoId, RepoKind, Run, RunId,
    RunStatus, TargetHealth, Timestamp, TriggerSource, Usage,
};
use revlocal_publish::{PublishError, PublishQueue, PublishTarget, QueueConfig};
use revlocal_store::{open, ChangeStore, Pool, PublishActionStore, RepoStore, RunStore};
use tempfile::TempDir;

/// A target that counts what it was actually asked to send.
struct CountingTarget {
    id: String,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl PublishTarget for CountingTarget {
    fn id(&self) -> &str {
        &self.id
    }

    async fn discover(&self) -> Result<CapabilitySet, PublishError> {
        Ok(CapabilitySet::new([Capability::Comment]))
    }

    async fn execute(&self, _action: &PublishAction) -> Result<PublishReceipt, PublishError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(PublishReceipt {
            external_ref: Some(self.id.clone()),
            response_json: None,
            deduplicated: false,
        })
    }

    async fn health(&self) -> Result<TargetHealth, PublishError> {
        Ok(TargetHealth {
            reachable: true,
            capabilities: CapabilitySet::new([Capability::Comment]),
            detail: None,
        })
    }
}

fn action_for(
    run_id: RunId,
    target: &str,
    key: &str,
    capability: Capability,
    gated: &revlocal_daemon::GatedAction,
) -> PublishAction {
    PublishAction {
        id: PublishActionId::new(0),
        run_id,
        finding_id: None,
        target: target.to_owned(),
        capability,
        risk: gated.assessment.class,
        idempotency_key: key.to_owned(),
        payload_json: "{}".to_owned(),
        status: gated
            .initial_status()
            .unwrap_or(revlocal_core::PublishActionStatus::Pending),
        attempts: 0,
        response_json: None,
        external_ref: None,
        error: None,
        created_at: at(3),
        sent_at: None,
    }
}

#[tokio::test]
async fn risk_gating_the_queue_dispatches_the_comment_and_leaves_the_issue_waiting() {
    let (_dir, pool, run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));

    let context = GateContext {
        mode: Mode::AutoLowAskHigh,
        run_degraded: false,
        actions_in_last_hour: 0,
        burst_threshold: DEFAULT_BURST_THRESHOLD,
    };
    let comment = gate(ActionIntent::Comment, Some(0.9), true, context);
    let issue = gate(ActionIntent::CreateIssue, Some(0.9), true, context);

    let github = Arc::new(AtomicUsize::new(0));
    let andare = Arc::new(AtomicUsize::new(0));

    let mut queue = PublishQueue::new(pool.clone(), QueueConfig::default());
    queue.register(Arc::new(CountingTarget {
        id: "github".to_owned(),
        calls: Arc::clone(&github),
    }));
    queue.register(Arc::new(CountingTarget {
        id: "andare".to_owned(),
        calls: Arc::clone(&andare),
    }));

    queue
        .enqueue(&action_for(
            run,
            "github",
            "c1",
            Capability::Comment,
            &comment,
        ))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    queue
        .enqueue(&action_for(
            run,
            "andare",
            "i1",
            Capability::CreateIssue,
            &issue,
        ))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    let report = queue
        .dispatch_pending(at(4))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(report.sent, 1);
    assert_eq!(github.load(Ordering::SeqCst), 1, "the comment went");
    assert_eq!(
        andare.load(Ordering::SeqCst),
        0,
        "the issue did not — and this is the assertion that proves the gate gates, \
         rather than merely labelling the row"
    );

    let rows = PublishActionStore::new(&pool)
        .list_for_run(run)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let waiting = rows
        .iter()
        .find(|a| a.target == "andare")
        .expect("the issue row exists");
    assert_eq!(
        waiting.status,
        revlocal_core::PublishActionStatus::AwaitingApproval,
        "and it is still waiting afterwards, not quietly failed"
    );
}
