//! `StdioClient` against the real mock MCP server (RL-601, SPEC §11.2).
//!
//! The mock is a real process speaking real newline-delimited JSON-RPC, not a
//! hand-rolled in-memory stub. That matters for two of the three criteria: a stub
//! cannot crash, and a stub leaves no process to leak.

use std::path::PathBuf;
use std::process::Stdio;

use revlocal_mcp::{McpError, ServerCommand, StdioClient};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// The mock MCP server, optionally with a profile and failure mode.
fn mock_server(env: &[(&str, &str)]) -> ServerCommand {
    let script = workspace_root().join("fixtures/mock-mcp/server.js");
    let mut server = ServerCommand::new("mock", "node", &[&script.display().to_string()]);
    for (key, value) in env {
        server.env.insert((*key).to_owned(), (*value).to_owned());
    }
    server
}

/// The process's state letter, or `None` once the pid is gone entirely.
///
/// `kill(pid, 0)` succeeds for a zombie too, so it cannot answer the question this
/// file asks. A reaped child is what criterion 3 is about, and "the pid no longer
/// answers" would pass for a zombie — which is precisely the leak.
///
/// Read via `ps` rather than `/proc/<pid>/stat`: macOS has no procfs, so the procfs
/// version returned `None` unconditionally there and the two tests that establish
/// "the server was running *before* we killed it" failed on their precondition. The
/// engine was fine; the probe was Linux-only. `ps -o state=` reports `Z` for a
/// zombie and exits non-zero for a pid that does not exist, on both platforms.
fn process_state(pid: u32) -> Option<char> {
    let output = std::process::Command::new("ps")
        .args(["-o", "state=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    // macOS decorates the state with flags (`S+`, `Ss`); the first letter is the
    // state proper, which is all either platform is being asked for here.
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .chars()
        .next()
}

fn node_is_installed() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

// --- criterion 1: connects and lists tools --------------------------------

#[tokio::test]
async fn stdio_connects_to_the_mock_and_lists_tools() {
    if !node_is_installed() {
        println!("SKIPPED (node not installed, nothing verified): stdio_connects...");
        return;
    }

    let mut client = StdioClient::new(mock_server(&[]));
    let tools = client.list_tools().await.unwrap_or_else(|e| panic!("{e}"));

    assert!(!tools.is_empty(), "the mock reported no tools");
    assert!(tools.iter().any(|t| t.name == "create_issue"), "{tools:?}");

    // §11.2 caches tools *with their input schemas*, because the mapper validates
    // rendered args against them. A tool list without schemas would look complete
    // and make every validation vacuous.
    let create = tools
        .iter()
        .find(|t| t.name == "create_issue")
        .expect("create_issue");
    assert!(
        create.input_schema.get("properties").is_some(),
        "the schema was dropped: {:?}",
        create.input_schema
    );

    assert_eq!(client.connect_count(), 1);
    client.shutdown().await;
}

#[tokio::test]
async fn stdio_the_handshake_is_recorded() {
    if !node_is_installed() {
        println!("SKIPPED (node not installed, nothing verified): stdio_the_handshake...");
        return;
    }

    let mut client = StdioClient::new(mock_server(&[]));
    client.list_tools().await.unwrap_or_else(|e| panic!("{e}"));

    let handshake = client.handshake().expect("connected");
    assert_eq!(
        handshake.protocol_version.as_deref(),
        Some(revlocal_mcp::PROTOCOL_VERSION)
    );
    assert!(handshake
        .server_info
        .as_ref()
        .is_some_and(|i| i.name.starts_with("mock-mcp/")));

    client.shutdown().await;
}

/// Nothing is spawned until something is asked for. A daemon configures every
/// server at startup and may never use most of them.
#[tokio::test]
async fn stdio_nothing_is_spawned_until_the_first_call() {
    let client = StdioClient::new(mock_server(&[]));

    assert!(!client.is_connected());
    assert_eq!(client.connect_count(), 0);
    assert!(client.pid().is_none());
}

#[tokio::test]
async fn stdio_calls_a_tool_and_reads_its_text() {
    if !node_is_installed() {
        println!("SKIPPED (node not installed, nothing verified): stdio_calls_a_tool...");
        return;
    }

    let mut client = StdioClient::new(mock_server(&[]));
    let result = client
        .call_tool(
            "create_issue",
            serde_json::json!({ "title": "t", "body": "b", "project": "REVL" }),
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert!(!result.is_error);
    assert!(!result.text().is_empty());

    client.shutdown().await;
}

/// A tool that ran and refused is an **answer**, not a failure to get one. §11.5's
/// read-before-write refusal is the case that matters: collapsing it into an error
/// would make "Trama protected your page" indistinguishable from "Trama has no such
/// tool", which need opposite responses.
#[tokio::test]
async fn stdio_a_tool_that_refuses_is_ok_with_is_error_not_an_err() {
    if !node_is_installed() {
        println!("SKIPPED (node not installed, nothing verified): stdio_a_tool_that_refuses...");
        return;
    }

    let mut client = StdioClient::new(mock_server(&[]));
    let result = client
        .call_tool(
            "update_page",
            serde_json::json!({ "space": "ENG", "title": "Never Read", "markdown": "x" }),
        )
        .await
        .unwrap_or_else(|e| panic!("a refusal must not be an Err: {e}"));

    assert!(
        result.is_error,
        "the mock should have refused a blind write"
    );
    assert!(result.text().contains("refusing to update"));

    client.shutdown().await;
}

/// A call that did **not** happen is an `Err`, and carries the server's own error
/// object rather than a flattened message.
#[tokio::test]
async fn stdio_an_unknown_tool_is_a_protocol_error() {
    if !node_is_installed() {
        println!("SKIPPED (node not installed, nothing verified): stdio_an_unknown_tool...");
        return;
    }

    let mut client = StdioClient::new(mock_server(&[]));
    let error = client
        .call_tool("no_such_tool", serde_json::json!({}))
        .await
        .expect_err("an unknown tool must fail");

    assert!(matches!(error, McpError::Protocol { .. }), "{error:?}");
    assert!(
        !error.is_transport(),
        "an unknown tool did not kill the connection"
    );
    assert_eq!(
        error.retryable(),
        Some(false),
        "retrying would loop forever"
    );
    assert!(
        client.is_connected(),
        "a tool error must not drop the connection"
    );

    client.shutdown().await;
}

/// §11.6: retryability comes from what the server *said*, never from the code.
/// Guessing wrong on a non-retryable error turns a caller bug into a slow failure.
#[tokio::test]
async fn stdio_retryability_comes_from_the_server_not_from_the_code() {
    if !node_is_installed() {
        println!("SKIPPED (node not installed, nothing verified): stdio_retryability...");
        return;
    }

    let mut rate_limited = StdioClient::new(mock_server(&[("MOCK_MCP_FAIL_MODE", "rate_limit")]));
    let error = rate_limited
        .call_tool(
            "create_issue",
            serde_json::json!({ "title": "t", "body": "b", "project": "P" }),
        )
        .await
        .expect_err("the mock was told to fail");
    assert_eq!(error.retryable(), Some(true));
    rate_limited.shutdown().await;

    let mut bad_params = StdioClient::new(mock_server(&[("MOCK_MCP_FAIL_MODE", "invalid_params")]));
    let error = bad_params
        .call_tool(
            "create_issue",
            serde_json::json!({ "title": "t", "body": "b", "project": "P" }),
        )
        .await
        .expect_err("the mock was told to fail");
    assert_eq!(error.retryable(), Some(false));
    bad_params.shutdown().await;
}

// --- criterion 2: a crash is typed, and the next use reconnects -----------

#[tokio::test]
async fn stdio_a_server_crash_is_a_typed_error_and_the_next_call_reconnects() {
    if !node_is_installed() {
        println!("SKIPPED (node not installed, nothing verified): stdio_a_server_crash...");
        return;
    }

    let mut client = StdioClient::new(mock_server(&[]));
    client.list_tools().await.unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(client.connect_count(), 1);

    // Kill the server out from under the client, the way a real one crashes.
    let pid = client.pid().expect("connected");
    let raw = i32::try_from(pid).unwrap_or_else(|e| panic!("{e}"));
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(raw),
        nix::sys::signal::Signal::SIGKILL,
    )
    .unwrap_or_else(|e| panic!("{e}"));

    let error = client
        .list_tools()
        .await
        .expect_err("a killed server must fail the call");

    assert!(
        matches!(error, McpError::Disconnected { .. }),
        "a crash must be typed as a disconnect, not as something a caller has to \
         pattern-match on a message to recognise: {error:?}"
    );
    assert!(error.is_transport());
    assert!(!client.is_connected(), "the dead connection was kept");

    // The next use reconnects. Asserted on `connect_count`, not on the call merely
    // succeeding — it would have succeeded anyway if the first connection had never
    // died, so success alone proves nothing.
    let tools = client.list_tools().await.unwrap_or_else(|e| panic!("{e}"));
    assert!(!tools.is_empty());
    assert_eq!(client.connect_count(), 2, "no reconnect happened");
    assert_ne!(client.pid(), Some(pid), "the same dead process was reused");

    client.shutdown().await;
}

/// Reconnection is on **next use**, never eager. Against a server that crashes on
/// startup, an eager reconnect spins — spawning processes as fast as the OS allows,
/// forever, reporting nothing.
#[tokio::test]
async fn stdio_a_dead_connection_is_not_reconnected_until_something_asks() {
    if !node_is_installed() {
        println!("SKIPPED (node not installed, nothing verified): stdio_a_dead_connection...");
        return;
    }

    let mut client = StdioClient::new(mock_server(&[]));
    client.list_tools().await.unwrap_or_else(|e| panic!("{e}"));

    let pid = client.pid().expect("connected");
    let raw = i32::try_from(pid).unwrap_or_else(|e| panic!("{e}"));
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(raw),
        nix::sys::signal::Signal::SIGKILL,
    )
    .unwrap_or_else(|e| panic!("{e}"));

    let _ = client.list_tools().await;

    // Nothing has asked for anything since. No new process.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(!client.is_connected());
    assert_eq!(
        client.connect_count(),
        1,
        "the client reconnected on its own"
    );
}

#[tokio::test]
async fn stdio_a_missing_binary_is_a_spawn_error_that_says_what_to_do() {
    let mut client = StdioClient::new(ServerCommand::new("ghost", "revlocal-no-such-binary", &[]));

    let error = client
        .list_tools()
        .await
        .expect_err("a missing binary must fail");

    assert!(matches!(error, McpError::Spawn { .. }), "{error:?}");
    assert_eq!(
        error.retryable(),
        Some(false),
        "respawning would fail again"
    );

    let message = error.to_string();
    assert!(message.contains("ghost"), "{message}");
    // §18: a user-visible error says what to do about it.
    assert!(message.contains("try:"), "{message}");
}

// --- criterion 3: the child is reaped, not leaked -------------------------

#[tokio::test]
async fn stdio_the_server_process_is_reaped_on_shutdown() {
    if !node_is_installed() {
        println!("SKIPPED (node not installed, nothing verified): stdio_the_server_process...");
        return;
    }

    let mut client = StdioClient::new(mock_server(&[]));
    client.list_tools().await.unwrap_or_else(|e| panic!("{e}"));
    let pid = client.pid().expect("connected");

    assert!(process_state(pid).is_some(), "the server was never running");

    client.shutdown().await;

    // Gone entirely, not a zombie. `kill(pid, 0)` would succeed for a zombie, which
    // is exactly the leak this criterion is about — so the check reads procfs.
    let state = process_state(pid);
    assert!(
        !matches!(state, Some('Z')),
        "the server was left as a zombie: it was killed but never reaped"
    );
    assert!(state.is_none(), "the server is still running: {state:?}");
}

/// **The test that actually tests criterion 3.**
///
/// A well-behaved server exits by itself when its stdin closes, so a client that
/// reaps *nothing* still passes "the process is gone afterwards" — the server left
/// on its own and the client took the credit. A negative probe proved it: removing
/// `kill_on_drop` and the `Drop` kill entirely changed no test.
///
/// `MOCK_MCP_IGNORE_EOF=1` is a server that will not leave. Same reason the mock
/// engine has a `hang` mode that ignores SIGTERM.
#[tokio::test]
async fn stdio_a_server_that_ignores_eof_is_still_killed_on_shutdown() {
    if !node_is_installed() {
        println!(
            "SKIPPED (node not installed, nothing verified): stdio_a_server_that_ignores_eof..."
        );
        return;
    }

    let mut client = StdioClient::new(mock_server(&[("MOCK_MCP_IGNORE_EOF", "1")]));
    client.list_tools().await.unwrap_or_else(|e| panic!("{e}"));
    let pid = client.pid().expect("connected");

    client.shutdown().await;

    let state = process_state(pid);
    assert!(
        !matches!(state, Some('Z')),
        "the wedged server was killed but never reaped: a zombie is still a leak"
    );
    assert!(
        state.is_none(),
        "a server that ignores stdin EOF outlived shutdown(): {state:?}"
    );
}

/// The same, on the *error* path: dropped without `shutdown()`.
#[tokio::test]
async fn stdio_a_server_that_ignores_eof_is_still_killed_on_drop() {
    if !node_is_installed() {
        println!("SKIPPED (node not installed, nothing verified): stdio_a_server_that_ignores_eof_on_drop...");
        return;
    }

    let pid = {
        let mut client = StdioClient::new(mock_server(&[("MOCK_MCP_IGNORE_EOF", "1")]));
        client.list_tools().await.unwrap_or_else(|e| panic!("{e}"));
        client.pid().expect("connected")
    };

    for _ in 0..50 {
        if process_state(pid).is_none() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    panic!(
        "a server that ignores stdin EOF outlived the dropped client: pid {pid} is {:?}",
        process_state(pid)
    );
}

/// Kept alongside the two above, but note what it does and does not prove: with a
/// well-behaved server this passes even if the client reaps nothing.
#[tokio::test]
async fn stdio_dropping_the_client_does_not_leak_the_server() {
    if !node_is_installed() {
        println!("SKIPPED (node not installed, nothing verified): stdio_dropping_the_client...");
        return;
    }

    let pid = {
        let mut client = StdioClient::new(mock_server(&[]));
        client.list_tools().await.unwrap_or_else(|e| panic!("{e}"));
        let pid = client.pid().expect("connected");
        assert!(process_state(pid).is_some());
        pid
        // dropped here, without shutdown() — the error path
    };

    // `kill_on_drop` reaps in the background, so this is a bounded wait rather than
    // an instant assertion. A leak fails it; a slow reap does not.
    for _ in 0..50 {
        if process_state(pid).is_none() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    panic!(
        "the server process outlived the client: pid {pid} is {:?}",
        process_state(pid)
    );
}

#[tokio::test]
async fn stdio_shutdown_is_safe_to_call_twice_and_before_connecting() {
    let mut never_connected = StdioClient::new(mock_server(&[]));
    never_connected.shutdown().await;
    never_connected.shutdown().await;

    assert!(!never_connected.is_connected());
}
