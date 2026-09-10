//! `revlocal watch` delivers to the destinations the config names (REVL-223).
//!
//! # The defect
//!
//! `watch::tick` passed `&[]` as its targets and said so in its own doc comment:
//! delivery was `revlocal publish`'s job. The reasoning was defensible; the result
//! was that the headless daemon — the thing you put on a timer, the whole
//! unattended half of the product — reviewed commits, recorded findings, queued
//! publish actions, and could route none of them anywhere but a markdown file.
//! `AndareTarget::new` had one caller in the workspace and it was the desktop
//! binary; `GitHubTarget::new` had none outside its own tests.
//!
//! # What is asserted, and why it is asserted this way
//!
//! A test that asserted "an issue was filed" would need a tracker. What
//! distinguishes the fixed behaviour from the broken one without one is *which
//! way the action fails*: an unrouted action is counted `unroutable` and reported
//! as "names a target that is not configured", while a routed action is attempted
//! and fails at the transport, leaving an error on its own row. The second is what
//! a wired daemon does with a server that is not there, and the first is what an
//! unwired one did with a server that was.
//!
//! Inner-loop rules hold: the MCP server named here is a program that does not
//! exist, so nothing is spawned successfully, no network is touched and no
//! credential is read.

use revlocal_core::{
    AutonomyMode, Capability, Change, ChangeId, ChangeKind, Depth, DiffStat, EngineKind,
    GlobalConfig, PublishAction, PublishActionId, PublishActionStatus, Repo, RepoId, RepoKind,
    RiskClass, Run, RunId, RunStatus, Timestamp, TriggerSource, Usage,
};
use revlocal_store::{open, ChangeStore, Pool, PublishActionStore, RepoStore, RunStore};
use tempfile::TempDir;

fn at() -> Timestamp {
    chrono::TimeZone::with_ymd_and_hms(&chrono::Utc, 2026, 9, 1, 9, 0, 0)
        .single()
        .unwrap_or_default()
}

/// A finished run with one publish action pending for `target`.
///
/// The repository has no checkout on purpose: this test is about the delivery
/// half of a tick, and a tick that discovered nothing still dispatches what is
/// already queued — which is the behaviour RL-1547 put there and the one an
/// unattended install depends on most.
async fn install(
    dir: &TempDir,
    target: &str,
) -> Result<(Pool, PublishActionId), Box<dyn std::error::Error>> {
    let pool = open(&dir.path().join("rev-local.db")).await?;
    let repo = RepoStore::new(&pool)
        .insert(&Repo {
            id: RepoId::new(0),
            name: "acme".to_owned(),
            kind: RepoKind::Git,
            local_path: None,
            remote_url: None,
            default_branch: Some("main".to_owned()),
            engine: EngineKind::Mock,
            autonomy: AutonomyMode::Auto,
            enabled: false,
            config_json: r#"{"andare_project": "ENG"}"#.to_owned(),
            created_at: at(),
            updated_at: at(),
        })
        .await?;

    let change = ChangeStore::new(&pool)
        .upsert(&Change {
            id: ChangeId::new(0),
            repo_id: repo.id,
            kind: ChangeKind::Commit,
            external_id: "already-reviewed".to_owned(),
            title: None,
            author_name: None,
            author_email: None,
            authored_at: None,
            branch: Some("main".to_owned()),
            base_ref: None,
            head_ref: None,
            url: None,
            diff_stat: DiffStat::default(),
            detected_at: at(),
        })
        .await?;

    let run = RunStore::new(&pool)
        .insert(&Run {
            id: RunId::new(0),
            change_id: change.id,
            attempt: 1,
            status: RunStatus::Done,
            engine: EngineKind::Mock,
            depth: Depth::Summary,
            trigger: TriggerSource::Poll,
            skip_reason: None,
            error: None,
            error_detail: None,
            degraded: None,
            usage: Usage::default(),
            started_at: Some(at()),
            finished_at: Some(at()),
            transcript_path: None,
            truncated: false,
            omitted_files: Vec::new(),
            verdict: None,
            summary: None,
            created_at: at(),
        })
        .await?
        .id;

    let action = PublishActionStore::new(&pool)
        .insert(&PublishAction {
            id: PublishActionId::new(0),
            run_id: run,
            finding_id: None,
            target: target.to_owned(),
            capability: Capability::CreateIssue,
            risk: RiskClass::Low,
            idempotency_key: format!("{target}-waiting-for-a-daemon"),
            payload_json: serde_json::json!({
                "repo": "acme",
                "project": "ENG",
                "title": "A finding that has been waiting",
                "body_md": "Queued by a review, routed by nobody.",
                "fingerprint": "revl-223",
            })
            .to_string(),
            status: PublishActionStatus::Pending,
            attempts: 0,
            response_json: None,
            external_ref: None,
            error: None,
            created_at: at(),
            sent_at: None,
        })
        .await?
        .id;

    Ok((pool, action))
}

/// A config naming an Andare server over stdio, as a local suite is run.
///
/// The command does not exist, which is what keeps this in the inner loop. What
/// matters is that the config makes a target *buildable*: the tick's job is to
/// route the action to it, and what the transport then says is the transport's
/// business.
fn config_naming_andare() -> Result<GlobalConfig, Box<dyn std::error::Error>> {
    let (config, _warnings) = GlobalConfig::parse(
        r#"
[mcpServers.andare]
type = "stdio"
command = "andare-mcp-that-is-not-installed"
"#,
    )?;
    Ok(config)
}

#[tokio::test]
async fn a_configured_tracker_action_is_routed_rather_than_counted_unroutable(
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = TempDir::new()?;
    let (pool, action) = install(&dir, "andare").await?;

    let report = revlocal_cli::watch::tick(
        &pool,
        &config_naming_andare()?,
        &dir.path().join("data"),
        at(),
    )
    .await?;

    assert!(
        !report
            .held
            .iter()
            .any(|note| note.contains("is not configured")),
        "Andare is configured, so no action may be counted unroutable: {report:?}"
    );

    let row = PublishActionStore::new(&pool).get(action).await?;
    assert_ne!(
        row.attempts, 0,
        "the tick must have attempted delivery to the configured target: {row:?}"
    );

    pool.close().await;
    Ok(())
}

/// The other half of the same fact: a destination the config says nothing about
/// still reports itself, and now reports *which* one and what to add.
#[tokio::test]
async fn an_unconfigured_tracker_names_itself_in_the_tick() -> Result<(), Box<dyn std::error::Error>>
{
    let dir = TempDir::new()?;
    let (pool, _action) = install(&dir, "trama").await?;

    let report = revlocal_cli::watch::tick(
        &pool,
        &config_naming_andare()?,
        &dir.path().join("data"),
        at(),
    )
    .await?;

    let note = report
        .held
        .iter()
        .find(|note| note.contains("is not configured"))
        .unwrap_or_else(|| panic!("a Trama action with no Trama went unreported: {report:?}"));

    assert!(
        note.contains("trama"),
        "the note must name the destination, not just count actions: {note}"
    );
    assert!(
        !note.contains("andare:"),
        "and must not blame a destination that was built: {note}"
    );

    pool.close().await;
    Ok(())
}
