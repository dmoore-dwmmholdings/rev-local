//! The findings screen against a real store (RL-1108, SPEC §15 screen 4).
//!
//! The unit tests beside `findings_view` pin the filter algebra. These pin the
//! three things only a database can answer: that the filter reaches the rows,
//! that suppressing changes what the *next* read returns, and that a manual file
//! to Andare goes through the same gate an automatic one does.
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use chrono::TimeZone;
use revlocal_core::{
    AutonomyMode, Category, Change, ChangeId, ChangeKind, Depth, DiffStat, EngineKind, Finding,
    FindingId, FindingState, PublishActionStatus, Repo, RepoId, RepoKind, Run, RunId, RunStatus,
    Severity, Timestamp, TriggerSource, Usage,
};
use revlocal_daemon::findings_view::{self, FindingFilter};
use revlocal_store::{open, ChangeStore, FindingStore, Pool, RepoStore, RunStore};
use tempfile::TempDir;

fn at(minute: u32) -> Timestamp {
    chrono::Utc
        .with_ymd_and_hms(2026, 8, 30, 9, minute, 0)
        .single()
        .unwrap_or_default()
}

/// One repository, one change, one run, and the findings asked for.
async fn seeded(
    autonomy: AutonomyMode,
    findings: &[(Severity, Category, &str)],
) -> Result<(TempDir, Pool, i64), Box<dyn std::error::Error>> {
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
            autonomy,
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

    let store = FindingStore::new(&pool);
    let mut first = 0_i64;
    for (index, (severity, category, title)) in findings.iter().enumerate() {
        let row = store
            .insert(&Finding {
                id: FindingId::new(0),
                run_id: run.id,
                fingerprint: format!("fp-{index}"),
                severity: *severity,
                category: *category,
                confidence: 0.9,
                file: Some("src/lib.rs".to_owned()),
                line_start: None,
                line_end: None,
                title: (*title).to_owned(),
                body: "why it matters".to_owned(),
                failure_scenario: None,
                suggested_fix: None,
                state: FindingState::Open,
                created_at: at(3),
            })
            .await?;
        if index == 0 {
            first = row.id.get();
        }
    }

    Ok((dir, pool, first))
}

#[tokio::test]
async fn findings_filters_compose_against_the_store() {
    let (_dir, pool, _first) = seeded(
        AutonomyMode::AutoLowAskHigh,
        &[
            (Severity::Critical, Category::Security, "a leak"),
            (Severity::Low, Category::Security, "a nit"),
            (Severity::Critical, Category::Perf, "a stall"),
        ],
    )
    .await
    .expect("seed");

    let all = findings_view::gather(&pool, &FindingFilter::default())
        .await
        .expect("gather");
    assert_eq!(all.rows.len(), 3);
    assert_eq!(all.total_before_filter, 3);
    // Offered from the data, so the dropdown cannot list a category nothing has.
    assert_eq!(all.categories, vec!["perf", "security"]);

    // Both filters, and only the row satisfying both survives. This is the
    // acceptance criterion: composing narrows.
    let both = findings_view::gather(
        &pool,
        &FindingFilter {
            min_severity: Some(Severity::High),
            category: Some("security".to_owned()),
            ..FindingFilter::default()
        },
    )
    .await
    .expect("gather");

    assert_eq!(both.rows.len(), 1);
    assert_eq!(both.rows[0].title, "a leak");
    // And the screen can still say "1 of 3" rather than presenting a filtered
    // table as if it were everything.
    assert_eq!(both.total_before_filter, 3);
}

#[tokio::test]
async fn suppressing_a_finding_shows_up_on_the_next_read() {
    let (_dir, pool, first) = seeded(
        AutonomyMode::AutoLowAskHigh,
        &[(Severity::High, Category::Convention, "trailing whitespace")],
    )
    .await
    .expect("seed");

    let state = findings_view::suppress(&pool, first, at(4))
        .await
        .expect("suppress");
    assert_eq!(state, FindingState::Suppressed);

    // The row itself changed, not only a suppression row somewhere else. A screen
    // that showed the finding still open would leave somebody clicking twice.
    let view = findings_view::gather(&pool, &FindingFilter::default())
        .await
        .expect("gather");
    assert_eq!(view.rows[0].state, FindingState::Suppressed);

    // And the suppression exists, which is what stops §10.3 raising it again.
    let suppressions = revlocal_store::SuppressionStore::new(&pool)
        .list_for_repo(RepoId::new(1))
        .await
        .expect("list");
    assert_eq!(suppressions.len(), 1);
    assert_eq!(suppressions[0].fingerprint.as_deref(), Some("fp-0"));
    // Scoped to the repository the row named, not everywhere — which is why
    // asking the store for *that* repository is what finds it.
    assert_eq!(suppressions[0].repo_id, Some(RepoId::new(1)));
}

#[tokio::test]
async fn a_manual_file_to_andare_waits_for_approval_under_the_default_mode() {
    let (_dir, pool, first) = seeded(
        AutonomyMode::AutoLowAskHigh,
        &[(Severity::Critical, Category::Security, "a leak")],
    )
    .await
    .expect("seed");

    let status = findings_view::file_to_andare(&pool, first, AutonomyMode::AutoLowAskHigh, at(5))
        .await
        .expect("file");

    // The point of the screen's gate. A person asked for it; the repository owner
    // still set the mode, and §12.3 makes creating an issue high risk.
    assert_eq!(status, PublishActionStatus::AwaitingApproval);

    // It really is in the inbox — the same list the approvals screen reads, so
    // there is one queue rather than a manual path beside it.
    let waiting = revlocal_store::PublishActionStore::new(&pool)
        .list_awaiting_approval()
        .await
        .expect("waiting");
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0].target, "andare");
    // §11.6: the fingerprint is in the key, so a manual file and an automatic one
    // for the same finding cannot become two issues.
    assert!(waiting[0].idempotency_key.contains("fp-0"));
}

#[tokio::test]
async fn a_manual_file_is_recorded_and_not_sent_in_dry_run() {
    // The other half of the gate. `dry_run` is a mode somebody chose for the
    // repository, and a manual click is not a reason to leave it.
    let (_dir, pool, first) = seeded(
        AutonomyMode::DryRun,
        &[(Severity::Critical, Category::Security, "a leak")],
    )
    .await
    .expect("seed");

    let status = findings_view::file_to_andare(&pool, first, AutonomyMode::AutoLowAskHigh, at(5))
        .await
        .expect("file");

    assert_eq!(status, PublishActionStatus::SkippedDryRun);

    // And nothing is waiting, because nothing is going to be sent.
    let waiting = revlocal_store::PublishActionStore::new(&pool)
        .list_awaiting_approval()
        .await
        .expect("waiting");
    assert!(waiting.is_empty());
}
