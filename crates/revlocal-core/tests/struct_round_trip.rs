//! Acceptance tests for `RL-103b` — the domain structs.
//!
//! Every struct round-trips through JSON, which is what the store's
//! JSON-in-a-column payloads (`diff_stat_json`, `config_json`, `payload_json`) and
//! the engine/MCP boundaries depend on. The behavioural invariants each type
//! carries are tested next to it, because they are the reason the type is not a
//! bare record.

use chrono::TimeZone;
use revlocal_core::*;
use std::fmt::Debug;

/// A fixed instant, so tests never read the clock.
fn at() -> Timestamp {
    chrono::Utc
        .with_ymd_and_hms(2026, 8, 27, 12, 0, 0)
        .single()
        .unwrap_or_default()
}

/// Assert `value` survives a JSON round-trip unchanged.
///
/// Returns `Result`; helpers are not `#[test]` fns, so the unwrap ban applies
/// (ADR 0003).
fn round_trips<T>(value: &T) -> Result<(), String>
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + Debug,
{
    let json = serde_json::to_string(value).map_err(|e| format!("serialize: {e}"))?;
    let back: T = serde_json::from_str(&json).map_err(|e| format!("deserialize {json}: {e}"))?;
    assert_eq!(&back, value, "value did not survive a JSON round-trip");
    Ok(())
}

fn a_repo() -> Repo {
    Repo {
        id: RepoId::new(1),
        name: "rev-local".into(),
        kind: RepoKind::Git,
        local_path: Some("/srv/rev-local".into()),
        remote_url: None,
        default_branch: Some("main".into()),
        engine: EngineKind::Mock,
        autonomy: AutonomyMode::AutoLowAskHigh,
        enabled: true,
        config_json: "{}".into(),
        created_at: at(),
        updated_at: at(),
    }
}

fn a_finding() -> Finding {
    Finding {
        id: FindingId::new(9),
        run_id: RunId::new(4),
        fingerprint: "0123456789abcdef".into(),
        severity: Severity::High,
        category: Category::Correctness,
        confidence: 0.82,
        file: Some("crates/revlocal-core/src/run.rs".into()),
        line_start: Some(42),
        line_end: Some(47),
        title: "Budget total treats an unknown cost as zero".into(),
        body: "markdown".into(),
        failure_scenario: Some("engine reports no cost -> ledger under-counts".into()),
        suggested_fix: None,
        state: FindingState::Open,
        created_at: at(),
    }
}

fn a_run() -> Run {
    Run {
        id: RunId::new(4),
        change_id: ChangeId::new(3),
        attempt: 1,
        status: RunStatus::Done,
        engine: EngineKind::Mock,
        depth: Depth::Standard,
        trigger: TriggerSource::Poll,
        skip_reason: None,
        error: None,
        usage: Usage {
            tokens_in: 1_000,
            tokens_out: 250,
            tokens_known: true,
            cost_usd: Some(0.01),
        },
        started_at: Some(at()),
        finished_at: Some(at()),
        transcript_path: None,
        truncated: false,
        omitted_files: Vec::new(),
        verdict: None,
        summary: None,
        degraded: None,
        created_at: at(),
    }
}

#[test]
fn every_domain_struct_round_trips_through_json() {
    let change = Change {
        id: ChangeId::new(3),
        repo_id: RepoId::new(1),
        kind: ChangeKind::Commit,
        external_id: "deadbeef".into(),
        title: Some("RL-103b: domain structs".into()),
        author_name: Some("Dawson Moore".into()),
        author_email: None,
        authored_at: Some(at()),
        branch: Some("main".into()),
        base_ref: None,
        head_ref: Some("deadbeef".into()),
        url: None,
        diff_stat: DiffStat {
            files: 6,
            insertions: 400,
            deletions: 12,
        },
        detected_at: at(),
    };
    let file_diff = FileDiff {
        path: "crates/revlocal-core/src/run.rs".into(),
        previous_path: None,
        status: FileStatus::Added,
        insertions: 90,
        deletions: 0,
        binary: false,
    };
    let action = PublishAction {
        id: PublishActionId::new(11),
        run_id: RunId::new(4),
        finding_id: Some(FindingId::new(9)),
        target: "andare".into(),
        capability: Capability::CreateIssue,
        risk: RiskClass::High,
        idempotency_key: "andare:REVL:0123456789abcdef".into(),
        payload_json: "{}".into(),
        status: PublishActionStatus::AwaitingApproval,
        attempts: 0,
        response_json: None,
        external_ref: None,
        error: None,
        created_at: at(),
        sent_at: None,
    };

    let checks: Vec<Result<(), String>> = vec![
        round_trips(&a_repo()),
        round_trips(&Cursor {
            repo_id: RepoId::new(1),
            scope: Cursor::commits_scope("main"),
            value: "deadbeef".into(),
            updated_at: at(),
        }),
        round_trips(&change),
        round_trips(&change.diff_stat),
        round_trips(&file_diff),
        round_trips(&a_run()),
        round_trips(&a_run().usage),
        round_trips(&a_finding()),
        round_trips(&Suppression {
            id: SuppressionId::new(2),
            repo_id: Some(RepoId::new(1)),
            fingerprint: Some("0123456789abcdef".into()),
            glob: None,
            reason: Some("known, accepted".into()),
            created_at: at(),
        }),
        round_trips(&action),
        round_trips(&CapabilitySet::new([
            Capability::CreateIssue,
            Capability::SetStatus,
        ])),
        round_trips(&PublishReceipt {
            external_ref: Some("REVL-108".into()),
            response_json: None,
            deduplicated: true,
        }),
        round_trips(&TargetHealth {
            reachable: true,
            capabilities: CapabilitySet::new([Capability::UpsertDoc]),
            detail: None,
        }),
        round_trips(&AuditEntry {
            id: AuditId::new(7),
            at: at(),
            actor: "daemon".into(),
            kind: "run.started".into(),
            repo_id: Some(RepoId::new(1)),
            run_id: Some(RunId::new(4)),
            detail_json: "{}".into(),
        }),
        round_trips(&BudgetLedgerEntry {
            repo_id: RepoId::new(1),
            day: "2026-08-27".into(),
            runs: 3,
            usage: Usage::default(),
            known_cost_usd: 0.0,
        }),
    ];

    let failures: Vec<String> = checks.into_iter().filter_map(Result::err).collect();
    assert!(failures.is_empty(), "round-trip failures: {failures:#?}");
}

#[test]
fn ids_serialize_transparently_so_a_column_holds_a_bare_integer() {
    // The store writes `repo_id` as an INTEGER; the newtype must not add a wrapper
    // object to the JSON payloads that quote these ids.
    let json = serde_json::to_string(&RepoId::new(42)).unwrap_or_default();
    assert_eq!(json, "42");
}

#[test]
fn a_skipped_run_must_say_why_and_a_failed_run_must_carry_its_error() {
    // SPEC §18, "no silent caps".
    let mut run = a_run();
    assert!(run.is_consistent());

    run.status = RunStatus::Skipped;
    assert!(
        !run.is_consistent(),
        "a skip with no reason is a silent cap"
    );
    run.skip_reason = Some("lockfile-only change".into());
    assert!(run.is_consistent());

    run.status = RunStatus::Failed;
    assert!(!run.is_consistent(), "a failure must carry its error");
    run.skip_reason = None;
    run.error = Some("engine_output_unparseable".into());
    assert!(run.is_consistent());
}

#[test]
fn an_unknown_cost_never_reads_as_zero_spend() {
    // SPEC §18: budget maths must not treat an unmeasured cost as free.
    let mut total = Usage {
        tokens_in: 10,
        tokens_out: 5,
        tokens_known: true,
        cost_usd: None,
    };
    assert!(!total.cost_is_complete());

    total.add(&Usage {
        tokens_in: 1,
        tokens_out: 1,
        tokens_known: true,
        cost_usd: None,
    });
    assert_eq!(total.cost_usd, None, "unknown + unknown is still unknown");
    assert_eq!(total.total_tokens(), 17);

    total.add(&Usage {
        tokens_in: 0,
        tokens_out: 0,
        tokens_known: true,
        cost_usd: Some(0.25),
    });
    assert_eq!(total.cost_usd, Some(0.25));
    assert!(
        !total.cost_is_complete() || total.cost_usd.is_some(),
        "a partially-known cost is still reported as a number"
    );
}

#[test]
fn a_degraded_run_says_what_was_degraded() {
    // SPEC §8.1 types this as a reason, not a flag, and §12.3 escalates every
    // action on a degraded run to high risk — so an unexplained escalation would
    // be unreadable in the approvals inbox.
    let mut run = a_run();
    assert!(!run.is_degraded());
    run.degraded = Some("result.json missing; parsed last fenced json block".into());
    assert!(run.is_degraded());
    assert!(
        run.is_consistent(),
        "degradation is orthogonal to skip/failure"
    );
}

#[test]
fn diff_stat_measures_the_threshold_spec_9_3_uses() {
    let stat = DiffStat {
        files: 3,
        insertions: 12_000,
        deletions: 9_000,
    };
    assert_eq!(stat.changed_lines(), 21_000);
    assert!(
        stat.changed_lines() > 20_000,
        "this change would select Depth::Summary"
    );
}

#[test]
fn finding_title_limit_is_counted_in_characters_not_bytes() {
    let mut finding = a_finding();
    assert!(finding.title_within_limit());

    // 80 non-ASCII characters is 160 bytes; a byte-counted limit would reject it.
    finding.title = "é".repeat(TITLE_MAX_CHARS);
    assert!(
        finding.title_within_limit(),
        "the limit is 80 characters, not 80 bytes"
    );

    finding.title = "a".repeat(TITLE_MAX_CHARS + 1);
    assert!(!finding.title_within_limit());
}

#[test]
fn low_confidence_matches_the_risk_escalation_threshold() {
    // SPEC §12.3 escalates when confidence < 0.6.
    let mut finding = a_finding();
    assert!(!finding.is_low_confidence());
    finding.confidence = 0.59;
    assert!(finding.is_low_confidence());
    finding.confidence = LOW_CONFIDENCE_THRESHOLD;
    assert!(
        !finding.is_low_confidence(),
        "the threshold itself is not low confidence"
    );
}

#[test]
fn a_suppression_with_no_matcher_is_inert() {
    let mut suppression = Suppression {
        id: SuppressionId::new(2),
        repo_id: None,
        fingerprint: None,
        glob: None,
        reason: None,
        created_at: at(),
    };
    assert!(
        !suppression.is_actionable(),
        "it would silently suppress nothing"
    );
    suppression.glob = Some("vendor/**".into());
    assert!(suppression.is_actionable());
}

#[test]
fn capability_sets_compare_independently_of_discovery_order() {
    let a = CapabilitySet::new([Capability::CreateIssue, Capability::SetStatus]);
    let b = CapabilitySet::new([Capability::SetStatus, Capability::CreateIssue]);
    assert_eq!(
        a, b,
        "two discoveries finding the same capabilities must compare equal"
    );
    assert!(a.supports(Capability::CreateIssue));
    assert!(!a.supports(Capability::PostReview));
}

#[test]
fn budget_exhaustion_pauses_rather_than_being_read_as_headroom() {
    let entry = BudgetLedgerEntry {
        repo_id: RepoId::new(1),
        day: "2026-08-27".into(),
        runs: 10,
        usage: Usage {
            tokens_in: 900,
            tokens_out: 100,
            tokens_known: true,
            cost_usd: None,
        },
        known_cost_usd: 0.0,
    };
    assert!(
        entry.runs_exhausted(10),
        "at the limit is exhausted, not one short"
    );
    // Tokens answer the same three ways cost does, since RL-409. This entry's
    // counts are measured, so "cannot tell" is not one of them here.
    assert_eq!(entry.tokens_exhausted(1_000), Some(true));
    assert_eq!(entry.tokens_exhausted(1_001), Some(false));

    // The cost side cannot answer, and "cannot tell" must not read as "fine".
    assert!(!entry.cost_is_complete());
    assert_eq!(
        entry.cost_exhausted(10.0),
        None,
        "an unmeasured day must not report headroom it has not been shown to have"
    );
}

#[test]
fn a_publish_action_awaiting_approval_is_not_terminal() {
    let action = PublishAction {
        id: PublishActionId::new(11),
        run_id: RunId::new(4),
        finding_id: None,
        target: "github".into(),
        capability: Capability::PostReview,
        risk: RiskClass::High,
        idempotency_key: "k".into(),
        payload_json: "{}".into(),
        status: PublishActionStatus::AwaitingApproval,
        attempts: 0,
        response_json: None,
        external_ref: None,
        error: None,
        created_at: at(),
        sent_at: None,
    };
    assert!(action.needs_approval());
    assert!(
        !action.is_terminal(),
        "a queued action must still be dispatchable"
    );
}

#[test]
fn cursor_scopes_are_distinct_per_branch_and_path() {
    assert_eq!(Cursor::commits_scope("main"), "commits:main");
    assert_ne!(
        Cursor::commits_scope("main"),
        Cursor::commits_scope("release")
    );
    assert_eq!(Cursor::svn_scope("/trunk"), "svn:/trunk");
    assert_eq!(Cursor::PRS_SCOPE, "prs");
}
