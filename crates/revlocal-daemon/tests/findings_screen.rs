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
            // Andare needs a project to file into, and a repository that files
            // without naming one is a configuration error rather than a review
            // failure — `a_repository_with_no_andare_project_still_completes_its_review`
            // covers that case. These tests are about the gate, so they configure
            // the repository the way a real one filing issues has to be.
            config_json: r#"{"andare_project": "ENG"}"#.to_owned(),
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
            error_detail: None,
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

#[tokio::test]
async fn the_run_screen_carries_what_the_finding_says() -> Result<(), Box<dyn std::error::Error>> {
    // RL-1552. `AnchoredFinding` had a title, a severity and a location and none
    // of the three content fields, so the run screen could show where a finding
    // was and never what it meant. The only way to read it was to open the
    // markdown report by hand, and the app names no path to one.
    //
    // Database-backed rather than a mapping test: the fields are copied inside
    // `gather`, and the regression this guards against is somebody dropping one
    // from that builder.
    let (_dir, pool, _first) = seeded(
        AutonomyMode::DryRun,
        &[(Severity::High, Category::Security, "SQL injection")],
    )
    .await?;

    let view = revlocal_daemon::run_view::gather(&pool, revlocal_core::RunId::new(1)).await?;
    let finding = view.findings.first().ok_or("no findings on the run")?;

    assert_eq!(finding.title, "SQL injection");
    assert_eq!(finding.body, "why it matters");
    Ok(())
}

#[tokio::test]
async fn queued_work_behind_a_missing_checkout_is_counted_apart(
) -> Result<(), Box<dyn std::error::Error>> {
    // RL-1554. On the live install 43 of 51 queued runs belonged to a repository
    // whose checkout was gone. A run like that is held every tick forever — a
    // hold is not an attempt, so `max_attempts` never applies and nothing gives
    // up on it — and every count folded it into "waiting". The number then never
    // goes down, which is how a warning becomes wallpaper.
    let (dir, pool, _first) = seeded(
        AutonomyMode::DryRun,
        &[(Severity::High, Category::Security, "SQL injection")],
    )
    .await?;

    // The fixture's repository has no `local_path`, which is "not known to be
    // unreachable" rather than gone — so its queued work counts as runnable.
    let (runnable, blocked) = revlocal_daemon::repos::queued_split(&pool).await?;
    assert_eq!(
        (runnable, blocked),
        (0, 0),
        "the fixture leaves nothing queued"
    );

    // A second repository pointing at a path that does not exist, with a run
    // queued against it.
    let gone = RepoStore::new(&pool)
        .insert(&Repo {
            id: RepoId::new(0),
            name: "gone".to_owned(),
            kind: RepoKind::Git,
            local_path: Some(dir.path().join("not-here").display().to_string()),
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
            repo_id: gone.id,
            kind: ChangeKind::Commit,
            external_id: "blocked-1".to_owned(),
            title: None,
            author_name: None,
            author_email: None,
            authored_at: None,
            branch: Some("main".to_owned()),
            base_ref: None,
            head_ref: None,
            url: None,
            diff_stat: DiffStat::default(),
            detected_at: at(1),
        })
        .await?;

    RunStore::new(&pool)
        .insert(&Run {
            id: RunId::new(0),
            change_id: change.id,
            attempt: 1,
            status: RunStatus::Queued,
            engine: EngineKind::Mock,
            depth: Depth::Summary,
            trigger: TriggerSource::Poll,
            skip_reason: None,
            error: None,
            error_detail: None,
            degraded: None,
            usage: Usage::default(),
            started_at: None,
            finished_at: None,
            transcript_path: None,
            truncated: false,
            omitted_files: Vec::new(),
            verdict: None,
            summary: None,
            created_at: at(1),
        })
        .await?;

    let (runnable, blocked) = revlocal_daemon::repos::queued_split(&pool).await?;
    assert_eq!(
        runnable, 0,
        "a run behind a missing checkout is not runnable"
    );
    assert_eq!(blocked, 1, "and it is still counted, not dropped");
    Ok(())
}

#[tokio::test]
async fn one_problem_across_two_runs_is_one_row() -> Result<(), Box<dyn std::error::Error>> {
    // RL-1563. `gather` returned one row per `finding` row, so a defect that
    // survived twenty commits was twenty rows on the screen whose job is "what is
    // wrong with my code". Scanning it meant reading the same finding over and
    // over.
    //
    // Database-backed rather than a unit test on the collapse: the key is
    // (repository, fingerprint) and the repository comes from a join through the
    // run's change, which a unit test would not exercise.
    let (_dir, pool, _first) = seeded(
        AutonomyMode::DryRun,
        &[(Severity::High, Category::Security, "SQL injection")],
    )
    .await?;

    let before = findings_view::gather(&pool, &FindingFilter::default()).await?;
    assert_eq!(before.rows.len(), 1, "{before:?}");
    assert_eq!(before.rows[0].occurrences, 1);

    // A second run of the same repository finding the same thing — same
    // fingerprint, new row, as a re-review produces.
    let runs = RunStore::new(&pool).list_recent(None, None, 10).await?;
    let first_run = runs.first().ok_or("no run")?;
    let again = RunStore::new(&pool)
        .insert(&Run {
            id: RunId::new(0),
            change_id: first_run.change_id,
            attempt: 2,
            status: RunStatus::Done,
            engine: EngineKind::Mock,
            depth: Depth::Summary,
            trigger: TriggerSource::Poll,
            skip_reason: None,
            error: None,
            error_detail: None,
            degraded: None,
            usage: Usage::default(),
            started_at: Some(at(4)),
            finished_at: Some(at(4)),
            transcript_path: None,
            truncated: false,
            omitted_files: Vec::new(),
            verdict: None,
            summary: None,
            created_at: at(4),
        })
        .await?;

    FindingStore::new(&pool)
        .insert(&Finding {
            id: FindingId::new(0),
            run_id: again.id,
            // The same fingerprint is what makes it the same problem (§10.3).
            fingerprint: "fp-0".to_owned(),
            severity: Severity::High,
            category: Category::Security,
            confidence: 0.9,
            file: Some("src/lib.rs".to_owned()),
            line_start: None,
            line_end: None,
            title: "SQL injection".to_owned(),
            body: "why it matters".to_owned(),
            failure_scenario: None,
            suggested_fix: None,
            state: FindingState::Open,
            created_at: at(4),
        })
        .await?;

    let after = findings_view::gather(&pool, &FindingFilter::default()).await?;
    assert_eq!(after.rows.len(), 1, "two runs, one problem: {after:?}");
    assert_eq!(after.rows[0].occurrences, 2, "the count is the news");
    assert_eq!(
        after.total_before_filter, 1,
        "the total counts problems too, or \"1 of 2\" is nonsense"
    );
    Ok(())
}
