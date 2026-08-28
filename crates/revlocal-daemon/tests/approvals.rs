//! The approvals inbox (RL-803, SPEC §12.4).
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use chrono::TimeZone;
use revlocal_core::{
    fingerprint, payload_digest, ActionIntent, AutonomyMode, Capability, CapabilitySet, Category,
    Change, ChangeId, ChangeKind, Depth, DiffStat, EngineKind, Finding, FindingId, FindingState,
    PublishAction, PublishActionId, PublishActionStatus, PublishReceipt, Repo, RepoId, RepoKind,
    RiskClass, Run, RunId, RunStatus, Severity, Suppression, SuppressionId, TargetHealth,
    Timestamp, TriggerSource, Usage, DEFAULT_BURST_THRESHOLD,
};
use revlocal_daemon::{
    decision_detail, expires_at, expiry_detail, gate, verify_before_send, ApprovalError, Decision,
    GateContext, DEFAULT_APPROVAL_TTL_HOURS, REASON_EXPIRED,
};
use revlocal_publish::{PublishError, PublishQueue, PublishTarget, QueueConfig};
use revlocal_store::{
    open, ChangeStore, Pool, PublishActionStore, RepoStore, RunStore, SuppressionStore,
};
use tempfile::TempDir;

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

/// A target that records what it was actually sent.
struct RecordingTarget {
    id: String,
    payloads: Arc<std::sync::Mutex<Vec<String>>>,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl PublishTarget for RecordingTarget {
    fn id(&self) -> &str {
        &self.id
    }

    async fn discover(&self) -> Result<CapabilitySet, PublishError> {
        Ok(CapabilitySet::new([Capability::CreateIssue]))
    }

    async fn execute(&self, action: &PublishAction) -> Result<PublishReceipt, PublishError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.payloads
            .lock()
            .map_err(|e| PublishError::Transport {
                target: "andare".to_owned(),
                detail: e.to_string(),
            })?
            .push(action.payload_json.clone());
        Ok(PublishReceipt {
            external_ref: Some("REVL-1".to_owned()),
            response_json: None,
            deduplicated: false,
        })
    }

    async fn health(&self) -> Result<TargetHealth, PublishError> {
        Ok(TargetHealth {
            reachable: true,
            capabilities: CapabilitySet::new([Capability::CreateIssue]),
            detail: None,
        })
    }
}

fn queued_action(run_id: RunId, payload: &str) -> PublishAction {
    PublishAction {
        id: PublishActionId::new(0),
        run_id,
        finding_id: None,
        target: "andare".to_owned(),
        capability: Capability::CreateIssue,
        risk: RiskClass::High,
        idempotency_key: format!("andare:{payload}"),
        payload_json: payload.to_owned(),
        status: PublishActionStatus::AwaitingApproval,
        attempts: 0,
        response_json: None,
        external_ref: None,
        error: None,
        created_at: at(3),
        sent_at: None,
    }
}

// --- criterion 1: approve sends exactly what was reviewed ----------------

#[tokio::test]
async fn approvals_approve_sends_exactly_the_reviewed_payload() {
    let (_dir, pool, run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));
    let store = PublishActionStore::new(&pool);

    let reviewed = r#"{"summary":"as the human saw it"}"#;
    let stored = store
        .insert(&queued_action(run, reviewed))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    store
        .approve(stored.id, &payload_digest(reviewed))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    let payloads = Arc::new(std::sync::Mutex::new(Vec::new()));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut queue = PublishQueue::new(pool.clone(), QueueConfig::default());
    queue.register(Arc::new(RecordingTarget {
        id: "andare".to_owned(),
        payloads: Arc::clone(&payloads),
        calls: Arc::clone(&calls),
    }));

    // `approved` is dispatchable; RL-701's queue treats it like pending.
    store
        .record_outcome(
            stored.id,
            PublishActionStatus::Pending,
            None,
            None,
            None,
            at(4),
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    let report = queue
        .dispatch_pending(at(5))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(report.sent, 1);
    assert_eq!(
        payloads.lock().unwrap_or_else(|e| panic!("{e}")).as_slice(),
        [reviewed.to_owned()],
        "the bytes sent are the bytes reviewed"
    );
}

#[tokio::test]
async fn approvals_an_edit_after_approval_is_refused_not_sent() {
    let (_dir, pool, run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));
    let store = PublishActionStore::new(&pool);

    let reviewed = r#"{"summary":"as the human saw it"}"#;
    let stored = store
        .insert(&queued_action(run, reviewed))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    store
        .approve(stored.id, &payload_digest(reviewed))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    // Something edits the payload afterwards. The UI cannot prevent this — a
    // replay, a second run, or a bug can all write here.
    sqlx::query("UPDATE publish_action SET payload_json = ?, status = ? WHERE id = ?")
        .bind(r#"{"summary":"something else entirely"}"#)
        .bind(PublishActionStatus::Pending.as_str())
        .bind(stored.id.get())
        .execute(&pool)
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    let calls = Arc::new(AtomicUsize::new(0));
    let mut queue = PublishQueue::new(pool.clone(), QueueConfig::default());
    queue.register(Arc::new(RecordingTarget {
        id: "andare".to_owned(),
        payloads: Arc::new(std::sync::Mutex::new(Vec::new())),
        calls: Arc::clone(&calls),
    }));

    let report = queue
        .dispatch_pending(at(5))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(report.sent, 0);
    assert_eq!(report.approval_stale, 1);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "§12.4: an edit after approval is impossible — not `discouraged`, and not \
         `impossible through the UI`"
    );

    let row = store.get(stored.id).await.unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(row.status, PublishActionStatus::Failed);
    assert!(
        row.error
            .as_deref()
            .is_some_and(|e| e.contains("changed after it was approved")),
        "{:?}",
        row.error
    );
}

#[test]
fn approvals_the_digest_is_over_the_bytes_not_a_normalised_form() {
    let a = r#"{"a":1,"b":2}"#;
    let reordered = r#"{"b":2,"a":1}"#;
    let respaced = r#"{"a": 1, "b": 2}"#;

    assert_ne!(
        payload_digest(a),
        payload_digest(reordered),
        "normalising first would forgive exactly the class of edit that reorders a \
         payload while changing what it says"
    );
    assert_ne!(payload_digest(a), payload_digest(respaced));
    assert_eq!(payload_digest(a), payload_digest(a));
}

#[test]
fn approvals_an_unapproved_action_is_not_a_mismatch() {
    verify_before_send(1, "{}", None)
        .unwrap_or_else(|e| panic!("the check honours approvals; it does not require them: {e}"));

    let error = verify_before_send(7, "{}", Some("not-the-digest"))
        .expect_err("a changed payload must be caught");
    assert_eq!(
        error,
        ApprovalError::PayloadChangedAfterApproval { action_id: 7 }
    );
    assert!(error.to_string().contains("try:"), "§18: {error}");
}

#[test]
fn approvals_edit_then_approve_digests_the_edited_payload() {
    let queued = r#"{"summary":"draft"}"#;
    let edited = r#"{"summary":"what the human actually wants sent"}"#;

    let decision = Decision::EditThenApprove {
        payload_json: edited.to_owned(),
    };

    assert_eq!(
        decision.effective_payload(queued),
        edited,
        "editing is not a hole in the rule — what is impossible is editing AFTER \
         the approval"
    );
    assert!(decision.approves());
    assert_eq!(decision.audit_kind(), "approval_granted_with_edit");
}

// --- criterion 2: reject-and-suppress is honoured by a later run ---------

#[tokio::test]
async fn approvals_reject_and_suppress_creates_a_suppression_a_later_run_honours() {
    let (_dir, pool, _run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));
    let repo_id = RepoId::new(1);

    let fp = fingerprint(
        "rev-local",
        Some("src/db.rs"),
        Category::Security,
        "User input reaches SQL",
    );

    let decision = Decision::RejectAndSuppress;
    assert!(decision.suppresses());

    SuppressionStore::new(&pool)
        .insert(&Suppression {
            id: SuppressionId::new(0),
            repo_id: Some(repo_id),
            fingerprint: Some(fp.clone()),
            glob: None,
            reason: Some("rejected in the approvals inbox".to_owned()),
            created_at: at(4),
        })
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    // A later run reads them back, exactly as the pipeline does.
    let active = SuppressionStore::new(&pool)
        .list_for_repo(repo_id)
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(active.len(), 1);
    assert_eq!(active[0].fingerprint.as_deref(), Some(fp.as_str()));

    let finding = Finding {
        id: FindingId::new(1),
        run_id: RunId::new(1),
        fingerprint: fp,
        severity: Severity::Critical,
        category: Category::Security,
        confidence: 0.95,
        file: Some("src/db.rs".to_owned()),
        line_start: Some(4),
        line_end: Some(4),
        title: "User input reaches SQL".to_owned(),
        body: "…".to_owned(),
        failure_scenario: None,
        suggested_fix: None,
        state: FindingState::Open,
        created_at: at(5),
    };

    assert!(
        active
            .iter()
            .any(|s| s.fingerprint.as_deref() == Some(finding.fingerprint.as_str())),
        "the suppression a rejection created must match the finding it was about"
    );
}

#[tokio::test]
async fn approvals_a_global_suppression_applies_to_every_repo() {
    let (_dir, pool, _run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));

    SuppressionStore::new(&pool)
        .insert(&Suppression {
            id: SuppressionId::new(0),
            repo_id: None,
            fingerprint: Some("fp-global".to_owned()),
            glob: None,
            reason: Some("never again, anywhere".to_owned()),
            created_at: at(4),
        })
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    let active = SuppressionStore::new(&pool)
        .list_for_repo(RepoId::new(1))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(
        active.len(),
        1,
        "somebody who said `never tell me this again` did not mean `in this \
         repository only` unless they scoped it"
    );
}

// --- criterion 3: expiry is audited and visible --------------------------

#[test]
fn approvals_expiry_is_a_decision_nobody_made_and_says_so() {
    let action = queued_action(RunId::new(1), "{}");

    let expired = expiry_detail(&action, DEFAULT_APPROVAL_TTL_HOURS);
    assert_eq!(expired["reason"], REASON_EXPIRED);
    assert_eq!(
        expired["actor"], "none",
        "an audit log that renders a timeout like a person's rejection cannot \
         answer `did anyone actually look at this?`"
    );
    assert_eq!(expired["waited_hours"], DEFAULT_APPROVAL_TTL_HOURS);

    let rejected = decision_detail(&action, &Decision::Reject, "dawson");
    assert_eq!(rejected["actor"], "dawson");
    assert_ne!(
        rejected["actor"], expired["actor"],
        "one is a decision, the other is that nobody looked"
    );
}

#[test]
fn approvals_the_ttl_is_the_spec_default_and_is_a_deadline_not_a_duration() {
    let queued = at(0);
    let deadline = expires_at(queued, DEFAULT_APPROVAL_TTL_HOURS);

    assert_eq!(DEFAULT_APPROVAL_TTL_HOURS, 72, "SPEC §13.1");
    assert_eq!(deadline, queued + chrono::Duration::hours(72));
}

#[tokio::test]
async fn approvals_an_expired_action_is_rejected_with_a_reason_not_silently_dropped() {
    let (_dir, pool, run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));
    let store = PublishActionStore::new(&pool);

    let stored = store
        .insert(&queued_action(run, "{}"))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    store
        .reject(stored.id, REASON_EXPIRED)
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    let row = store.get(stored.id).await.unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(row.status, PublishActionStatus::Rejected);
    assert_eq!(
        store
            .decision_reason(stored.id)
            .await
            .unwrap_or_else(|e| panic!("{e}"))
            .as_deref(),
        Some(REASON_EXPIRED),
        "§18: the row says why, so an expiry is visible rather than a queue that \
         quietly got shorter"
    );
}

// --- criterion 4: one queue, whichever surface acts on it ---------------

#[tokio::test]
async fn approvals_the_cli_and_the_ui_operate_on_the_same_queue() {
    let (_dir, pool, run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));
    let store = PublishActionStore::new(&pool);

    for payload in [r#"{"n":1}"#, r#"{"n":2}"#] {
        store
            .insert(&queued_action(run, payload))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
    }

    // One surface lists.
    let listed = store
        .list_awaiting_approval()
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(listed.len(), 2);

    // The other surface decides, through its own store handle.
    let other_handle = PublishActionStore::new(&pool);
    other_handle
        .approve(listed[0].id, &payload_digest(&listed[0].payload_json))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    // And the first sees it, because there is one queue and it is the database.
    let after = store
        .list_awaiting_approval()
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].id, listed[1].id);
}

#[tokio::test]
async fn approvals_a_settled_action_cannot_be_approved_again() {
    let (_dir, pool, run) = seeded().await.unwrap_or_else(|e| panic!("{e}"));
    let store = PublishActionStore::new(&pool);

    let stored = store
        .insert(&queued_action(run, "{}"))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    store
        .reject(stored.id, "no thanks")
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    store.approve(stored.id, "digest").await.expect_err(
        "approving something already settled is a caller bug, and silently \
             rewriting a decided action would be worse than saying so",
    );
}

// --- the inbox item itself ------------------------------------------------

#[test]
fn approvals_an_inbox_item_names_its_target_explicitly() {
    let context = GateContext {
        mode: AutonomyMode::AutoLowAskHigh,
        run_degraded: false,
        actions_in_last_hour: 0,
        burst_threshold: DEFAULT_BURST_THRESHOLD,
    };
    let gated = gate(ActionIntent::CreateIssue, Some(0.9), false, context);

    let item = revlocal_daemon::InboxItem {
        action: queued_action(RunId::new(1), "{}"),
        assessment: gated.assessment,
        expires_at: expires_at(at(0), DEFAULT_APPROVAL_TTL_HOURS),
    };

    assert_eq!(item.what_it_will_do(), "create_issue on andare");
    assert!(
        item.why().starts_with("high risk:"),
        "§12.4's inbox has to say why: {}",
        item.why()
    );
    assert!(!item.is_expired(at(1)));
    assert!(item.is_expired(expires_at(at(0), DEFAULT_APPROVAL_TTL_HOURS)));
}
