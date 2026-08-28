//! `revlocal targets list` (RL-604 criterion 4, SPEC §11.2, §14).
//!
//! Runs the real binary against the real mock MCP server, because the criterion is
//! that mapping state is *visible* — which is a statement about what reaches a
//! terminal, not about what a function returns.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn node_is_installed() -> bool {
    Command::new("node")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A config naming the mock server and one target with a capability that binds
/// and one that cannot.
fn config_text() -> String {
    let script = workspace_root().join("fixtures/mock-mcp/server.js");
    format!(
        r#"
[mcpServers.andare]
type = "stdio"
command = "node"
args = ["{script}"]

[targets.andare]
mcp_server = "andare"

[targets.andare.map.create_issue]
tool_candidates = ["create_issue", "create_work_item"]
args = {{ title = "{{finding.title}}", body = "{{finding.body_md}}" }}

[targets.andare.map.upload_attachment]
tool_candidates = ["upload_attachment", "add_attachment"]
args = {{ }}
"#,
        script = script.display()
    )
}

/// Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.
fn run(config: &Path, json: bool) -> Result<Output, String> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_revlocal"));
    command
        .arg("targets")
        .arg("list")
        .arg("--config")
        .arg(config)
        .current_dir(workspace_root());
    if json {
        command.arg("--json");
    }
    command
        .output()
        .map_err(|e| format!("running revlocal targets list: {e}"))
}

#[test]
fn targets_list_shows_what_bound_and_what_did_not() {
    if !node_is_installed() {
        println!("SKIPPED (node not installed, nothing verified): targets_list_shows...");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("config.toml");
    std::fs::write(&config, config_text()).expect("write config");

    let output = run(&config, false).unwrap_or_else(|e| panic!("{e}"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "targets list failed: {}\n{stdout}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        stdout.contains("andare: 5 tools, 0 capabilities mapped, 0 unmapped"),
        "the server health line must be shown: {stdout}"
    );
    assert!(
        stdout.contains("andare → andare: 1 mapped, 1 unmapped"),
        "the target summary must be shown: {stdout}"
    );
    assert!(
        stdout.contains("create_issue → create_issue"),
        "a bound capability must name the tool it bound to: {stdout}"
    );
    assert!(
        stdout.contains("`upload_attachment` is unmapped"),
        "§11.2: the UI shows exactly which capability failed to bind: {stdout}"
    );
}

#[test]
fn targets_list_json_prints_exactly_one_document_and_nothing_else() {
    if !node_is_installed() {
        println!("SKIPPED (node not installed, nothing verified): targets_list_json...");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("config.toml");
    std::fs::write(&config, config_text()).expect("write config");

    let output = run(&config, true).unwrap_or_else(|e| panic!("{e}"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let document: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout must be exactly one JSON document: {e}\n{stdout}"));

    let target = &document["targets"][0];
    assert_eq!(target["target"], "andare");
    assert_eq!(target["resolved"], true);
    assert_eq!(target["mapped"][0]["capability"], "create_issue");
    assert_eq!(target["mapped"][0]["tool"], "create_issue");
    assert_eq!(target["unmapped"][0]["capability"], "upload_attachment");
    assert!(
        target["unmapped"][0]["available"]
            .as_array()
            .is_some_and(|a| a.iter().any(|v| v == "get_page")),
        "the unmapped entry carries what the server does have, so an override can \
         be offered without asking again: {target}"
    );
}

#[test]
fn targets_list_reports_an_unreachable_server_as_unresolved_not_unmapped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("config.toml");
    std::fs::write(
        &config,
        r#"
[mcpServers.ghost]
type = "stdio"
command = "revlocal-no-such-mcp-server"

[targets.ghost]
mcp_server = "ghost"

[targets.ghost.map.create_issue]
tool_candidates = ["create_issue"]
args = { }
"#,
    )
    .expect("write config");

    let output = run(&config, false).unwrap_or_else(|e| panic!("{e}"));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "a dead server is a report, not a crash"
    );

    assert!(
        stdout.contains("not resolved"),
        "\"we could not ask\" and \"we asked and it does not have it\" need different \
         remedies, so they must not print the same line: {stdout}"
    );
    assert!(
        !stdout.contains("is unmapped"),
        "an unreachable server must not be reported as an unmapped capability: {stdout}"
    );
}

#[test]
fn targets_list_says_what_to_do_when_the_config_is_missing() {
    let output = run(Path::new("/no/such/config.toml"), false).unwrap_or_else(|e| panic!("{e}"));
    assert!(!output.status.success());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("try:"), "§18: {stderr}");
}

// --- RL-605: map and test --------------------------------------------------

/// Run a `targets` subcommand with arbitrary arguments.
fn run_args(args: &[&str]) -> Result<Output, String> {
    Command::new(env!("CARGO_BIN_EXE_revlocal"))
        .arg("targets")
        .args(args)
        .current_dir(workspace_root())
        .output()
        .map_err(|e| format!("running revlocal targets: {e}"))
}

#[test]
fn targets_map_saves_an_override_and_test_renders_it() {
    if !node_is_installed() {
        println!("SKIPPED (node not installed, nothing verified): targets_map_saves...");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("config.toml");
    let overrides = dir.path().join("overrides.json");
    std::fs::write(&config, config_text()).expect("write config");

    // `upload_attachment` binds to nothing on the mock server. Bind it by hand to
    // a tool that does exist.
    let mapped = run_args(&[
        "map",
        "andare",
        "upload_attachment",
        "--tool",
        "create_page",
        "--arg",
        "space=ENG",
        "--arg",
        "title={finding.title}",
        "--arg",
        "markdown={finding.body_md}",
        "--config",
        &config.display().to_string(),
        "--overrides",
        &overrides.display().to_string(),
    ])
    .unwrap_or_else(|e| panic!("{e}"));

    assert!(
        mapped.status.success(),
        "map failed: {}",
        String::from_utf8_lossy(&mapped.stderr)
    );
    assert!(
        overrides.is_file(),
        "criterion 1: the override must be persisted to survive a restart"
    );

    // A separate process reads it back — which is what "survives restart" means.
    let tested = run_args(&[
        "test",
        "andare",
        "--config",
        &config.display().to_string(),
        "--overrides",
        &overrides.display().to_string(),
    ])
    .unwrap_or_else(|e| panic!("{e}"));

    let stdout = String::from_utf8_lossy(&tested.stdout);
    assert!(
        tested.status.success(),
        "test failed: {stdout}{}",
        String::from_utf8_lossy(&tested.stderr)
    );
    assert!(
        stdout.contains("upload_attachment → create_page (override)"),
        "the dry run must show the override and say it is one: {stdout}"
    );
    assert!(
        stdout.contains("create_issue → create_issue"),
        "and still render the capabilities that resolved normally: {stdout}"
    );
    assert!(
        stdout.contains("no tool was called"),
        "a dry run must say it was one: {stdout}"
    );
}

#[test]
fn targets_map_refuses_a_tool_the_server_does_not_have() {
    if !node_is_installed() {
        println!("SKIPPED (node not installed, nothing verified): targets_map_refuses...");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("config.toml");
    let overrides = dir.path().join("overrides.json");
    std::fs::write(&config, config_text()).expect("write config");

    let output = run_args(&[
        "map",
        "andare",
        "upload_attachment",
        "--tool",
        "definitely_not_a_tool",
        "--config",
        &config.display().to_string(),
        "--overrides",
        &overrides.display().to_string(),
    ])
    .unwrap_or_else(|e| panic!("{e}"));

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("definitely_not_a_tool"), "{stderr}");
    assert!(stderr.contains("try:"), "§18: {stderr}");
    assert!(
        !overrides.exists(),
        "criterion 2: validation happens at save time, so nothing invalid is written"
    );
}

#[test]
fn targets_test_fails_when_a_template_would_not_render() {
    if !node_is_installed() {
        println!("SKIPPED (node not installed, nothing verified): targets_test_fails...");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("config.toml");
    std::fs::write(
        &config,
        config_text().replace(
            r#"args = { title = "{finding.title}", body = "{finding.body_md}" }"#,
            r#"args = { title = "{finding.title}" }"#,
        ),
    )
    .expect("write config");

    let output = run_args(&[
        "test",
        "andare",
        "--config",
        &config.display().to_string(),
        "--overrides",
        &dir.path().join("overrides.json").display().to_string(),
    ])
    .unwrap_or_else(|e| panic!("{e}"));

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !output.status.success(),
        "a template that cannot render is a failure, not a note: {stdout}"
    );
    assert!(
        stdout.contains("FAIL create_issue"),
        "the failing capability is named: {stdout}"
    );
    assert!(
        stdout.contains("\"body\" is a required property"),
        "and so is the field, from the server's own schema: {stdout}"
    );
}
