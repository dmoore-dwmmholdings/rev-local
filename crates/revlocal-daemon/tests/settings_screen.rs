//! The settings screen against a real MCP server (RL-1110, SPEC §15 screen 6).
//!
//! The unit tests beside `settings_view` pin the shapes. These run the mapping
//! against `fixtures/mock-mcp` with the `andare-renamed` profile, which exposes
//! `create_work_item` and `transition_issue` and nothing else — so two of
//! Andare's four capabilities bind and two genuinely do not. That is the state
//! §15's criterion is about, and asserting it against a server that really
//! answered is the difference between testing the mapper and testing a mock of it.
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use revlocal_core::GlobalConfig;
use revlocal_daemon::doctor::DoctorReport;
use revlocal_daemon::settings_view;
use tempfile::TempDir;

fn workspace_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// Whether `node` is available to run the mock server.
fn have_node() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A config pointing at the mock server on its default profile.
///
/// The default profile exposes `create_issue`, `set_issue_status`, `get_page`,
/// `update_page` and `create_page` — so of Andare's four capabilities, two bind
/// and two genuinely do not. That partial mapping is what these tests are about.
///
/// Two things this deliberately does *not* do, both learned from a red Windows
/// leg:
///
/// The command is plain `node`, not `env VAR=... node`. `env` does not exist on
/// Windows, so the first version could not start the server there at all — and
/// picking a profile is not worth a fixture that only runs on two platforms when
/// the default profile answers the same question.
///
/// The path goes in a TOML **literal** string. A Windows path is full of
/// backslashes and TOML treats those as escapes inside a basic string, so
/// `C:\Users\...` is a parse error and the config silently became empty —
/// which surfaced three layers away as "no MCP server `andare` is configured".
fn config_for() -> String {
    let script = workspace_root().join("fixtures/mock-mcp/server.js");

    format!(
        r#"
[mcpServers.andare]
type = "stdio"
command = "node"
args = ['{}']
"#,
        script.display()
    )
}

/// Parse, and refuse to continue on a warning.
///
/// The first version of this fell back to `GlobalConfig::default()` on any
/// problem, and a mistyped table name (`mcp_servers` for `[mcpServers]`) turned
/// into "no MCP server is configured" three layers away. A fixture that silently
/// becomes an empty config tests nothing.
fn parsed(text: &str) -> Result<GlobalConfig, String> {
    let (config, warnings) = GlobalConfig::parse(text).map_err(|error| error.to_string())?;
    if !warnings.is_empty() {
        return Err(format!(
            "the fixture config is not clean: {:?}",
            warnings
                .iter()
                .map(revlocal_core::ConfigWarning::message)
                .collect::<Vec<_>>()
        ));
    }
    Ok(config)
}

#[tokio::test]
async fn a_partially_mapped_target_reports_exactly_what_did_not_bind() {
    if !have_node() {
        eprintln!("settings_screen: node is not installed; skipping");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    let overrides = dir.path().join("target-overrides.json");
    let config = parsed(&config_for()).expect("fixture config");

    let view = settings_view::gather(
        &config,
        "/tmp/config.toml",
        &overrides.display().to_string(),
        DoctorReport::default(),
    )
    .await;

    let andare = view
        .targets
        .iter()
        .find(|t| t.target == "andare")
        .expect("the built-in andare target");

    assert!(andare.server_contacted, "the mock server did not answer");

    // `create_issue` and `set_status` both bind — the latter to
    // `set_issue_status`, a non-primary candidate, which is §11.2's whole claim:
    // tool names are discovered rather than assumed.
    let bound: Vec<&str> = andare.bound.iter().map(|b| b.capability.as_str()).collect();
    assert!(bound.contains(&"create_issue"), "bound: {bound:?}");
    assert!(bound.contains(&"set_status"), "bound: {bound:?}");

    // `comment` and `search` have no tool on this server, and are reported
    // rather than guessed at.
    let unmapped: Vec<&str> = andare
        .unmapped
        .iter()
        .map(|u| u.capability.as_str())
        .collect();
    assert!(unmapped.contains(&"comment"), "unmapped: {unmapped:?}");
    assert!(unmapped.contains(&"search"), "unmapped: {unmapped:?}");

    // Each carries what a fix needs: what was wanted, and what there is.
    let comment = andare
        .unmapped
        .iter()
        .find(|u| u.capability == "comment")
        .expect("comment is unmapped");
    assert!(comment.candidates.contains(&"comment_on_issue".to_owned()));
    assert!(comment.available.contains(&"create_issue".to_owned()));

    // And the screen can lead with a number rather than making somebody count.
    assert_eq!(view.unmapped_count(), 2);
}

#[tokio::test]
async fn a_manual_override_binds_a_capability_and_says_a_person_did_it() {
    if !have_node() {
        eprintln!("settings_screen: node is not installed; skipping");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    let overrides = dir.path().join("target-overrides.json");
    let overrides_path = overrides.display().to_string();
    let config = parsed(&config_for()).expect("fixture config");

    // The fix affordance, exercised: bind `comment` to a tool the server does
    // have. `transition_issue` is not what a comment means, and that is the
    // point — rev-local does not know better than the person mapping it, it only
    // refuses names the server does not expose.
    settings_view::set_override(
        &config,
        &overrides_path,
        "andare",
        "comment",
        "set_issue_status",
        // The server's own required fields, which `check_against` insists on:
        // this tool wants `key` and `status`. Getting them wrong here is what
        // the check is for, and it caught me doing exactly that.
        serde_json::json!({ "key": "{issue_ref}", "status": "commented" }),
    )
    .await
    .expect("the override is accepted");

    let view = settings_view::gather(
        &config,
        "/tmp/config.toml",
        &overrides_path,
        DoctorReport::default(),
    )
    .await;

    let andare = view
        .targets
        .iter()
        .find(|t| t.target == "andare")
        .expect("andare");

    let comment = andare
        .bound
        .iter()
        .find(|b| b.capability == "comment")
        .expect("comment is now bound");
    assert_eq!(comment.tool, "set_issue_status");
    // ADR 0015: a table that showed "you told us to" and "we worked it out"
    // alike would make an override impossible to find again.
    assert!(comment.from_override);

    // One fewer unmapped than before, and the count the screen leads with moves.
    assert_eq!(view.unmapped_count(), 1);

    // Clearing it hands the capability back to resolution, which cannot bind it.
    let removed =
        settings_view::clear_override(&overrides_path, "andare", "comment").expect("clear");
    assert!(removed);

    let after = settings_view::gather(
        &config,
        "/tmp/config.toml",
        &overrides_path,
        DoctorReport::default(),
    )
    .await;
    assert_eq!(after.unmapped_count(), 2);
}

#[tokio::test]
async fn an_override_naming_a_tool_the_server_lacks_is_refused_at_the_point_of_writing() {
    if !have_node() {
        eprintln!("settings_screen: node is not installed; skipping");
        return;
    }

    let dir = TempDir::new().expect("tempdir");
    let overrides = dir.path().join("target-overrides.json");
    let overrides_path = overrides.display().to_string();
    let config = parsed(&config_for()).expect("fixture config");

    // RL-605's rule. A typo discovered at dispatch time is a review that
    // silently did not publish, hours after the person who typed it left.
    let error = settings_view::set_override(
        &config,
        &overrides_path,
        "andare",
        "comment",
        "comment_on_isue",
        serde_json::json!({}),
    )
    .await
    .expect_err("a tool the server does not expose");

    assert!(
        error.to_string().contains("comment_on_isue"),
        "the message must name the tool: {error}"
    );

    // And nothing was written: a refused override that left a file behind would
    // be worse than one that was accepted.
    assert!(
        !overrides.exists()
            || std::fs::read_to_string(&overrides).is_ok_and(|t| !t.contains("comment_on_isue")),
        "the refused override was written anyway"
    );
}

#[tokio::test]
async fn a_server_that_cannot_start_is_reported_and_its_target_is_not_called_unmapped() {
    let dir = TempDir::new().expect("tempdir");
    let overrides = dir.path().join("target-overrides.json");
    let config = parsed(
        r#"
[mcpServers.andare]
type = "stdio"
command = "definitely-not-a-real-binary-9f3a"
args = []
"#,
    )
    .expect("fixture config");

    let view = settings_view::gather(
        &config,
        "/tmp/config.toml",
        &overrides.display().to_string(),
        DoctorReport::default(),
    )
    .await;

    let server = view.servers.first().expect("one configured server");
    assert!(server.error.is_some(), "an unstartable server says why");
    assert!(!server.summary.contains("0 tools"));

    // The distinction that matters: nobody has asked this server what it has, so
    // its capabilities are unknown rather than unmapped. Reporting four unmapped
    // would send somebody writing overrides for tools that are probably there.
    let andare = view
        .targets
        .iter()
        .find(|t| t.target == "andare")
        .expect("andare");
    assert!(!andare.server_contacted);
    assert_eq!(view.unmapped_count(), 0);
}
