//! Findings written to disk (RL-1507, SPEC §11.1).
//!
//! The report target is the one output that works with nothing configured, so
//! these tests configure nothing beyond a directory.
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use chrono::TimeZone;
use revlocal_core::{
    Capability, PublishAction, PublishActionId, PublishActionStatus, RiskClass, RunId, Timestamp,
};
use revlocal_publish::{PublishError, PublishTarget, ReportPayload, ReportTarget};
use tempfile::TempDir;

fn at(minute: u32) -> Timestamp {
    chrono::Utc
        .with_ymd_and_hms(2026, 9, 1, 10, minute, 0)
        .single()
        .unwrap_or_default()
}

fn payload(repo: &str, fingerprint: &str) -> ReportPayload {
    ReportPayload {
        repo: repo.to_owned(),
        title: "Nested patch content is written without redaction".to_owned(),
        body: "`walk` only checks the key after descending.\n\n---\nrev-local-fingerprint: abc\n"
            .to_owned(),
        fingerprint: fingerprint.to_owned(),
    }
}

fn action(payload: &ReportPayload, capability: Capability) -> PublishAction {
    PublishAction {
        id: PublishActionId::new(1),
        run_id: RunId::new(1),
        finding_id: None,
        target: "report".to_owned(),
        capability,
        risk: RiskClass::High,
        idempotency_key: format!("report-{}", payload.fingerprint),
        payload_json: serde_json::to_string(payload).unwrap_or_default(),
        status: PublishActionStatus::Pending,
        attempts: 0,
        response_json: None,
        external_ref: None,
        error: None,
        created_at: at(1),
        sent_at: None,
    }
}

#[tokio::test]
async fn a_finding_becomes_a_markdown_file_named_by_its_fingerprint(
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new()?;
    let target = ReportTarget::new(dir.path().join("reports"));
    let payload = payload("acme", "a5ecf9b30dadf8b9");

    let receipt = target
        .execute(&action(&payload, Capability::CreateIssue))
        .await?;

    let path = dir.path().join("reports/acme/a5ecf9b30dadf8b9.md");
    assert!(path.exists(), "no file at {}", path.display());
    // The receipt is what the run detail screen shows as "where it went", so it
    // has to be the actual path rather than a target name.
    assert_eq!(
        receipt.external_ref.as_deref(),
        Some(path.display().to_string().as_str())
    );
    assert!(!receipt.deduplicated, "the first write is not a redelivery");

    let body = std::fs::read_to_string(&path)?;
    assert!(body.starts_with("# Nested patch content"), "{body}");
    assert!(body.contains("rev-local-fingerprint"), "{body}");
    Ok(())
}

#[tokio::test]
async fn writing_the_same_finding_again_rewrites_one_file() -> Result<(), Box<dyn std::error::Error>>
{
    // §11.6: a redelivery lands on the existing effect and says so, rather than
    // accumulating a directory of the same finding.
    let dir = TempDir::new()?;
    let target = ReportTarget::new(dir.path().join("reports"));
    let payload = payload("acme", "fp1");

    target
        .execute(&action(&payload, Capability::CreateIssue))
        .await?;
    let second = target
        .execute(&action(&payload, Capability::CreateIssue))
        .await?;

    assert!(second.deduplicated, "the second write is a redelivery");
    let count = std::fs::read_dir(dir.path().join("reports/acme"))?.count();
    assert_eq!(count, 1);
    Ok(())
}

#[tokio::test]
async fn a_reader_never_sees_a_half_written_report() -> Result<(), Box<dyn std::error::Error>> {
    // Write-then-rename, so the temporary must not survive the call. An agent
    // reading this directory on a timer would otherwise parse a truncated file.
    let dir = TempDir::new()?;
    let target = ReportTarget::new(dir.path().join("reports"));

    target
        .execute(&action(&payload("acme", "fp1"), Capability::CreateIssue))
        .await?;

    let leftovers: Vec<_> = std::fs::read_dir(dir.path().join("reports/acme"))?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".part"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "temporary files left behind: {leftovers:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_repository_name_cannot_escape_the_reports_directory(
) -> Result<(), Box<dyn std::error::Error>> {
    // A repository is named by whoever added it, and `..` in a path segment is
    // the oldest way to turn "write a file" into "write any file".
    let dir = TempDir::new()?;
    let root = dir.path().join("reports");
    let target = ReportTarget::new(root.clone());

    target
        .execute(&action(
            &payload("../../etc/evil", "fp1"),
            Capability::CreateIssue,
        ))
        .await?;

    assert!(
        !dir.path().join("etc").exists(),
        "the report escaped its root"
    );
    let written = std::fs::read_dir(&root)?.count();
    assert_eq!(written, 1, "exactly one directory under the root");
    Ok(())
}

#[tokio::test]
async fn a_capability_a_file_cannot_have_is_refused_rather_than_dropped(
) -> Result<(), Box<dyn std::error::Error>> {
    // §11.2: an unsupported capability is reported, not silently ignored. A file
    // cannot move a ticket's state, and pretending otherwise would let the
    // executor queue work that vanishes.
    let dir = TempDir::new()?;
    let target = ReportTarget::new(dir.path().join("reports"));

    let error = target
        .execute(&action(&payload("acme", "fp1"), Capability::SetStatus))
        .await
        .err()
        .ok_or("set_status must be refused")?;

    assert!(matches!(error, PublishError::Unsupported { .. }), "{error}");
    assert!(
        !error.is_retryable(),
        "a wrong capability will not fix itself"
    );
    Ok(())
}

#[tokio::test]
async fn health_creates_the_directory_rather_than_reporting_it_missing(
) -> Result<(), Box<dyn std::error::Error>> {
    // "The reports directory does not exist yet" is not a fault on a fresh
    // install; it is the fresh install.
    let dir = TempDir::new()?;
    let target = ReportTarget::new(dir.path().join("reports"));

    let health = target.health().await?;

    assert!(health.reachable, "{:?}", health.detail);
    assert!(health.capabilities.supports(Capability::CreateIssue));
    Ok(())
}
