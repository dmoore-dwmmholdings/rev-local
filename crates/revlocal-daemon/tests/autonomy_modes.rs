//! Autonomy modes and the global ceiling (RL-801, SPEC §12.2).
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use std::path::PathBuf;
use std::process::Stdio;

use revlocal_core::{effective_autonomy, AutonomyMode, PublishActionStatus, RepoConfig, RiskClass};
use revlocal_daemon::{disposition, mode_change_detail, reviews_run, widens, Disposition};
use revlocal_mcp::{ServerCommand, StdioClient};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn node_is_installed() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

// --- criterion 3: the ceiling only ever restricts -------------------------

#[test]
fn autonomy_modes_global_off_overrides_a_repo_set_to_auto() {
    let repo = RepoConfig {
        autonomy: AutonomyMode::Auto,
        ..RepoConfig::default()
    };

    assert_eq!(
        effective_autonomy(AutonomyMode::Off, &repo),
        AutonomyMode::Off,
        "§12.2: the effective mode is min(global, repo). A ceiling a repository \
         could raise would not be a ceiling"
    );
    assert!(!reviews_run(effective_autonomy(AutonomyMode::Off, &repo)));
}

#[test]
fn autonomy_modes_the_ceiling_is_a_minimum_in_both_directions() {
    let cases = [
        (
            AutonomyMode::Auto,
            AutonomyMode::DryRun,
            AutonomyMode::DryRun,
        ),
        (
            AutonomyMode::DryRun,
            AutonomyMode::Auto,
            AutonomyMode::DryRun,
        ),
        (
            AutonomyMode::AutoLowAskHigh,
            AutonomyMode::Auto,
            AutonomyMode::AutoLowAskHigh,
        ),
        (AutonomyMode::Auto, AutonomyMode::Auto, AutonomyMode::Auto),
        (AutonomyMode::Off, AutonomyMode::Off, AutonomyMode::Off),
    ];

    for (global, repo_mode, expected) in cases {
        let repo = RepoConfig {
            autonomy: repo_mode,
            ..RepoConfig::default()
        };
        assert_eq!(
            effective_autonomy(global, &repo),
            expected,
            "global={global:?} repo={repo_mode:?}"
        );
    }
}

// --- §12.2's table ---------------------------------------------------------

#[test]
fn autonomy_modes_follow_the_table_in_the_spec() {
    // off: no reviews at all.
    assert!(!reviews_run(AutonomyMode::Off));
    for risk in [RiskClass::Low, RiskClass::High] {
        assert_eq!(disposition(AutonomyMode::Off, risk), Disposition::NoReview);
    }

    // dry_run: reviews run, nothing is sent, whatever the risk.
    assert!(reviews_run(AutonomyMode::DryRun));
    for risk in [RiskClass::Low, RiskClass::High] {
        assert_eq!(
            disposition(AutonomyMode::DryRun, risk),
            Disposition::RecordOnly
        );
        assert!(!disposition(AutonomyMode::DryRun, risk).sends());
    }

    // auto_low_ask_high: the split the mode is named for.
    assert_eq!(
        disposition(AutonomyMode::AutoLowAskHigh, RiskClass::Low),
        Disposition::Send
    );
    assert_eq!(
        disposition(AutonomyMode::AutoLowAskHigh, RiskClass::High),
        Disposition::AwaitApproval
    );

    // auto: everything goes.
    for risk in [RiskClass::Low, RiskClass::High] {
        assert!(disposition(AutonomyMode::Auto, risk).sends());
    }
}

// --- criterion 2: a dry run records what it would have sent ---------------

#[test]
fn autonomy_modes_dry_run_records_the_action_rather_than_dropping_it() {
    for risk in [RiskClass::Low, RiskClass::High] {
        assert_eq!(
            disposition(AutonomyMode::DryRun, risk).initial_status(),
            Some(PublishActionStatus::SkippedDryRun),
            "a dry run that recorded nothing would be a mode where the only way to \
             learn what rev-local intends is to let it do it"
        );
    }
}

#[test]
fn autonomy_modes_skipped_dry_run_is_not_failed_and_not_rejected() {
    let status = disposition(AutonomyMode::DryRun, RiskClass::High)
        .initial_status()
        .unwrap_or(PublishActionStatus::Pending);

    assert_ne!(
        status,
        PublishActionStatus::Failed,
        "nothing went wrong in a dry run, and calling it failed makes one look like \
         a problem in the audit log"
    );
    assert_ne!(
        status,
        PublishActionStatus::Rejected,
        "and nobody declined it either"
    );
}

#[test]
fn autonomy_modes_a_queued_action_is_awaiting_approval_not_pending() {
    assert_eq!(
        disposition(AutonomyMode::AutoLowAskHigh, RiskClass::High).initial_status(),
        Some(PublishActionStatus::AwaitingApproval),
        "RL-701's queue only dispatches `pending`, so a high-risk action recorded \
         as pending would route around the approval gate entirely"
    );
}

// --- criterion 1: a dry run performs zero MCP writes ---------------------

/// The mock MCP server, journalling every request.
fn mock_server(journal: &std::path::Path) -> ServerCommand {
    let script = workspace_root().join("fixtures/mock-mcp/server.js");
    let mut server = ServerCommand::new("trama", "node", &[&script.display().to_string()]);
    server
        .env
        .insert("MOCK_MCP_JOURNAL".to_owned(), journal.display().to_string());
    server
}

fn journal_entries(path: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The write tools §11.4 and §11.5 expose. A dry run must call none of them.
const WRITE_TOOLS: [&str; 6] = [
    "create_issue",
    "set_issue_status",
    "comment_on_issue",
    "create_page",
    "update_page",
    "publish_page",
];

#[tokio::test]
async fn autonomy_modes_dry_run_performs_zero_mcp_writes() {
    if !node_is_installed() {
        println!("SKIPPED (node not installed, nothing verified): autonomy_modes_dry_run...");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let journal = dir.path().join("journal.jsonl");

    // A live server, connected exactly as a real run would connect it.
    let mut client = StdioClient::new(mock_server(&journal));
    client.list_tools().await.unwrap_or_else(|e| {
        panic!(
            "the server must be reachable, or this proves \
                                   nothing about restraint: {e}"
        )
    });

    // Everything a run would want to publish, under a dry run.
    let intents = [
        RiskClass::Low,
        RiskClass::High,
        RiskClass::High,
        RiskClass::Low,
    ];
    for risk in intents {
        let plan = disposition(AutonomyMode::DryRun, risk);
        assert_eq!(
            plan.initial_status(),
            Some(PublishActionStatus::SkippedDryRun)
        );
        if plan.sends() {
            // Unreachable under dry_run; if it ever becomes reachable this test
            // must fail loudly rather than quietly stop covering anything.
            client
                .call_tool("create_issue", serde_json::json!({}))
                .await
                .unwrap_or_else(|e| panic!("{e}"));
        }
    }

    client.shutdown().await;

    let entries = journal_entries(&journal);
    assert!(
        !entries.is_empty(),
        "the journal must show the connection happened — an empty journal would \
         pass for a server that was never started"
    );
    for tool in WRITE_TOOLS {
        assert!(
            !entries
                .iter()
                .any(|line| line.contains("tools/call") && line.contains(tool)),
            "§12.2: dry_run performs zero MCP writes, but `{tool}` was called:\n{entries:#?}"
        );
    }
}

// --- criterion 4: a mode change is audited -------------------------------

#[test]
fn autonomy_modes_a_mode_change_records_both_values() {
    let detail = mode_change_detail("global", AutonomyMode::Auto, AutonomyMode::DryRun);

    assert_eq!(detail["scope"], "global");
    assert_eq!(
        detail["from"], "auto",
        "`changed to dry_run` is unreadable without knowing what it was before"
    );
    assert_eq!(detail["to"], "dry_run");
    assert_eq!(detail["restricts"], true);
}

#[test]
fn autonomy_modes_widening_authority_is_distinguishable_from_narrowing_it() {
    assert!(
        widens(AutonomyMode::DryRun, AutonomyMode::Auto),
        "granting rev-local more freedom is the direction worth noticing"
    );
    assert!(!widens(AutonomyMode::Auto, AutonomyMode::DryRun));
    assert!(!widens(AutonomyMode::Auto, AutonomyMode::Auto));

    let narrowing = mode_change_detail("repo:rev-local", AutonomyMode::Auto, AutonomyMode::Off);
    assert_eq!(narrowing["restricts"], true);
    let widening = mode_change_detail("repo:rev-local", AutonomyMode::Off, AutonomyMode::Auto);
    assert_eq!(widening["restricts"], false);
}

/// Mode is read per action rather than captured once, which is what "takes effect
/// without restart" means in a process that is already running.
#[test]
fn autonomy_modes_are_resolved_per_action_not_cached() {
    let repo = RepoConfig {
        autonomy: AutonomyMode::Auto,
        ..RepoConfig::default()
    };

    let before = disposition(
        effective_autonomy(AutonomyMode::Auto, &repo),
        RiskClass::High,
    );
    let after = disposition(
        effective_autonomy(AutonomyMode::Off, &repo),
        RiskClass::High,
    );

    assert_eq!(before, Disposition::Send);
    assert_eq!(
        after,
        Disposition::NoReview,
        "the same repo config gives a different answer the moment the global ceiling \
         moves, because nothing here holds state"
    );
}
