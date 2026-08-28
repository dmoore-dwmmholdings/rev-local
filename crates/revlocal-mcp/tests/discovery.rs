//! Tool discovery cache and health reporting (RL-603, SPEC §11.2).
//!
//! The cache criteria are about calls that did **not** happen, which no return
//! value can show: a client that re-lists every time returns exactly the same
//! tools as one that caches. So these tests read the mock server's request journal
//! and count `tools/list` entries. That is the same reason RL-204 built the
//! journal in the first place.

use std::path::PathBuf;
use std::process::Stdio;

use revlocal_mcp::{
    Discovery, HttpClient, HttpEndpoint, NoSecrets, ServerCommand, ServerState, StdioClient,
};

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

/// A mock server that journals every request to `journal`.
fn mock_server(id: &str, journal: &std::path::Path) -> ServerCommand {
    let script = workspace_root().join("fixtures/mock-mcp/server.js");
    let mut server = ServerCommand::new(id, "node", &[&script.display().to_string()]);
    server
        .env
        .insert("MOCK_MCP_JOURNAL".to_owned(), journal.display().to_string());
    server
}

/// How many `tools/list` requests the journal recorded.
///
/// Reads the file rather than holding a handle: the server appends, and a test
/// that cached a file handle would read its own stale view.
fn tools_list_count(journal: &std::path::Path) -> usize {
    std::fs::read_to_string(journal)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains("\"tools/list\""))
        .count()
}

/// A server that cannot be started at all.
fn missing_server(id: &str) -> ServerCommand {
    ServerCommand::new(id, "revlocal-no-such-mcp-server", &[])
}

// --- criterion 1: the cache, and what invalidates it ----------------------

#[tokio::test]
async fn discovery_asking_twice_lists_once() {
    if !node_is_installed() {
        println!("SKIPPED (node not installed, nothing verified): discovery_asking_twice...");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let journal = dir.path().join("journal.jsonl");

    let mut discovery = Discovery::new();
    discovery.insert(StdioClient::new(mock_server("mock", &journal)));

    let first = discovery
        .tools("mock", &NoSecrets)
        .await
        .expect("registered")
        .unwrap_or_else(|e| panic!("{e}"))
        .len();
    let second = discovery
        .tools("mock", &NoSecrets)
        .await
        .expect("registered")
        .unwrap_or_else(|e| panic!("{e}"))
        .len();

    assert_eq!(first, 5, "the default profile has five tools");
    assert_eq!(second, first);
    assert_eq!(
        tools_list_count(&journal),
        1,
        "the second ask must be served from the cache"
    );
}

/// The criterion. A tool list describes one server process, so a reconnect must
/// not serve names read from the previous one.
#[tokio::test]
async fn discovery_the_cache_invalidates_on_reconnect() {
    if !node_is_installed() {
        println!("SKIPPED (node not installed, nothing verified): discovery_the_cache...");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let journal = dir.path().join("journal.jsonl");

    let mut discovery = Discovery::new();
    discovery.insert(StdioClient::new(mock_server("mock", &journal)));

    discovery
        .tools("mock", &NoSecrets)
        .await
        .expect("registered")
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(discovery.is_cached("mock"));
    assert_eq!(tools_list_count(&journal), 1);

    // Drop the connection the way the daemon would after a transport error.
    discovery.shutdown("mock").await;

    assert!(
        !discovery.is_cached("mock"),
        "a list read from a dead connection is not a valid cache entry"
    );

    discovery
        .tools("mock", &NoSecrets)
        .await
        .expect("registered")
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(
        tools_list_count(&journal),
        2,
        "the reconnected server must be asked again"
    );
    assert!(discovery.is_cached("mock"));
}

// --- criterion 2: the line doctor prints ----------------------------------

#[tokio::test]
async fn discovery_the_health_line_is_the_one_doctor_prints() {
    if !node_is_installed() {
        println!("SKIPPED (node not installed, nothing verified): discovery_the_health_line...");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let journal = dir.path().join("journal.jsonl");

    let mut discovery = Discovery::new();
    discovery.insert(StdioClient::new(mock_server("andare", &journal)));
    discovery.refresh_all(&NoSecrets).await;

    // Discovery does not know about capabilities; the mapper (RL-604) reports them.
    discovery.set_capability_counts("andare", 3, 2);

    let report = discovery.health();
    assert_eq!(
        report.lines(),
        vec!["andare: 5 tools, 3 capabilities mapped, 2 unmapped".to_owned()],
        "RL-603 fixes this wording; doctor prints it verbatim"
    );
    assert!(report.all_reachable());
}

#[test]
fn discovery_a_server_never_contacted_does_not_claim_zero_tools() {
    let mut discovery = Discovery::new();
    discovery.insert(StdioClient::new(missing_server("never-asked")));

    let report = discovery.health();
    assert_eq!(
        report.lines(),
        vec!["never-asked: not contacted".to_owned()]
    );
    assert!(
        !report.all_reachable(),
        "not contacted is not the same as reachable"
    );
}

// --- criterion 3: one dead server degrades one target ---------------------

#[tokio::test]
async fn discovery_an_unreachable_server_degrades_only_itself() {
    if !node_is_installed() {
        println!("SKIPPED (node not installed, nothing verified): discovery_an_unreachable...");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let journal = dir.path().join("journal.jsonl");

    let mut discovery = Discovery::new();
    discovery.insert(StdioClient::new(mock_server("good", &journal)));
    discovery.insert(StdioClient::new(missing_server("bad")));

    let report = discovery.refresh_all(&NoSecrets).await;

    assert_eq!(report.servers.len(), 2);
    assert!(!report.all_reachable());

    let good = report
        .servers
        .iter()
        .find(|s| s.id == "good")
        .expect("good is reported");
    assert_eq!(good.state, ServerState::Reachable { tools: 5 });

    let bad = report
        .servers
        .iter()
        .find(|s| s.id == "bad")
        .expect("bad is reported");
    assert!(
        matches!(bad.state, ServerState::Unreachable { .. }),
        "{:?}",
        bad.state
    );

    // The point of the criterion: the working target still works afterwards.
    let tools = discovery
        .tools("good", &NoSecrets)
        .await
        .expect("registered")
        .unwrap_or_else(|e| panic!("the good server must be unaffected: {e}"));
    assert_eq!(tools.len(), 5);

    assert_eq!(
        discovery.health().unreachable().count(),
        1,
        "exactly one target is degraded"
    );
}

#[tokio::test]
async fn discovery_an_unreachable_server_says_why_and_whether_to_retry() {
    let mut discovery = Discovery::new();
    discovery.insert(StdioClient::new(missing_server("ghost")));

    discovery.refresh_all(&NoSecrets).await;
    let report = discovery.health();
    let ghost = &report.servers[0];

    let ServerState::Unreachable { reason, retryable } = &ghost.state else {
        panic!("expected unreachable, got {:?}", ghost.state);
    };

    // §18: a user-visible failure says what to do about it. The health line is
    // user-visible, so the remediation has to survive into it.
    assert!(reason.contains("ghost"), "{reason}");
    assert!(reason.contains("try:"), "{reason}");
    assert_eq!(
        *retryable,
        Some(false),
        "a binary that is not installed will not be installed by retrying"
    );

    assert!(ghost.summary_line().starts_with("ghost: unreachable — "));
}

/// The other transport reaches the same report, through the same enum.
#[tokio::test]
async fn discovery_an_http_server_that_cannot_be_reached_is_reported_the_same_way() {
    // Port 1 on loopback: reserved, never listening, and no packets leave the host.
    let endpoint = HttpEndpoint::new("trama", "http://127.0.0.1:1/mcp");
    let client = HttpClient::new(endpoint).expect("client");

    let mut discovery = Discovery::new();
    discovery.insert(client);

    let report = discovery.refresh_all(&NoSecrets).await;
    assert_eq!(report.servers.len(), 1);
    assert!(
        matches!(report.servers[0].state, ServerState::Unreachable { .. }),
        "{:?}",
        report.servers[0].state
    );
    assert!(report.servers[0].summary_line().contains("unreachable"));
}

// --- the registry itself ---------------------------------------------------

#[test]
fn discovery_reports_servers_in_a_stable_order() {
    let mut discovery = Discovery::new();
    for id in ["zulu", "alpha", "mike"] {
        discovery.insert(StdioClient::new(missing_server(id)));
    }

    assert_eq!(discovery.len(), 3);
    assert!(!discovery.is_empty());
    assert_eq!(
        discovery.ids().collect::<Vec<_>>(),
        vec!["alpha", "mike", "zulu"],
        "doctor output and the UI's server list must not shuffle between runs"
    );
}

#[tokio::test]
async fn discovery_an_unknown_server_is_none_rather_than_an_error() {
    let mut discovery = Discovery::new();
    assert!(
        discovery.tools("nope", &NoSecrets).await.is_none(),
        "asking for a server that was never configured is a caller bug, not a \
         runtime failure, and must not be reportable as an unreachable server"
    );
}
