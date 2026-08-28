//! Capability mapping (RL-604, SPEC §11.2).
//!
//! The load-bearing claim under test is §11.2's: integrating Andare does not
//! require knowing Andare's tool names at build time. A test that resolves
//! `create_issue` against a server exposing `create_issue` does not test that
//! claim at all — it passes for an implementation that ignores the candidate list
//! entirely. So the resolution tests run against `profiles/andare-renamed.json`,
//! which deliberately exposes `create_work_item` and **not** `create_issue`.

use std::path::PathBuf;
use std::process::Stdio;

use revlocal_mcp::{
    builtin_target, resolve, Discovery, MappingError, NoSecrets, RenderContext, ServerCommand,
    SpecError, StdioClient, TargetSpec, Tool,
};
use serde_json::json;

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

/// The mock server, running one of the RL-204 profiles.
fn mock_server(id: &str, profile: &str) -> ServerCommand {
    let root = workspace_root();
    let script = root.join("fixtures/mock-mcp/server.js");
    let mut server = ServerCommand::new(id, "node", &[&script.display().to_string()]);
    server.env.insert(
        "MOCK_MCP_PROFILE".to_owned(),
        root.join("fixtures/mock-mcp/profiles")
            .join(profile)
            .display()
            .to_string(),
    );
    server
}

/// SPEC §11.2's own example, verbatim apart from the target name.
///
/// Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.
fn andare_spec() -> Result<TargetSpec, String> {
    let table: toml::Value = toml::from_str(
        r#"
mcp_server = "andare"

[map.create_issue]
tool_candidates = ["create_issue", "create_work_item", "issue_create", "create_ticket"]
args = { summary = "{finding.title}", description = "{finding.body_md}", projectKey = "{repo.config.andare_project}" }

[map.set_status]
tool_candidates = ["update_issue", "set_issue_status", "transition_issue"]
args = { id = "{issue_ref}", status = "{status}" }
"#,
    )
    .map_err(|e| format!("the spec's example must parse: {e}"))?;

    TargetSpec::from_toml("andare", &table).map_err(|e| e.to_string())
}

fn context() -> RenderContext {
    RenderContext::new(json!({
        "finding": { "title": "SQL injection in the login path", "body_md": "…", "line": 42 },
        "repo": { "config": { "andare_project": "REVL" } },
        "issue_ref": "REVL-1",
        "status": "In Review",
    }))
}

/// Discover a server's real tools, the way the daemon does.
async fn discovered_tools(id: &str, profile: &str) -> Result<Vec<Tool>, String> {
    let mut discovery = Discovery::new();
    discovery.insert(StdioClient::new(mock_server(id, profile)));
    let tools = discovery
        .tools(id, &NoSecrets)
        .await
        .ok_or_else(|| format!("{id} was not registered"))?
        .map_err(|e| e.to_string())?
        .to_vec();
    discovery.shutdown(id).await;
    Ok(tools)
}

// --- criterion 1: resolution against the server's real names ---------------

#[tokio::test]
async fn mapping_resolves_create_issue_to_the_name_the_server_actually_has() {
    if !node_is_installed() {
        println!(
            "SKIPPED (node not installed, nothing verified): mapping_resolves_create_issue..."
        );
        return;
    }

    let tools = discovered_tools("andare", "andare-renamed.json")
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(
        !tools.iter().any(|t| t.name == "create_issue"),
        "the renamed profile must not expose the obvious name, or nothing is proven"
    );

    let mapping = resolve(&andare_spec().unwrap_or_else(|e| panic!("{e}")), &tools);

    let binding = mapping
        .binding("create_issue")
        .expect("create_issue must bind to the renamed tool");
    assert_eq!(binding.tool, "create_work_item");
    assert_eq!(
        binding.candidate_index, 1,
        "it resolved to the second candidate, not the first"
    );

    // The other capability resolves through a different candidate position, which
    // is what shows the list is being walked rather than a fixed index used.
    let status = mapping
        .binding("set_status")
        .expect("set_status must bind to transition_issue");
    assert_eq!(status.tool, "transition_issue");
    assert_eq!(status.candidate_index, 2);

    assert!(mapping.is_complete());
    assert_eq!(
        mapping.summary_line(),
        "andare → andare: 2 mapped, 0 unmapped"
    );
}

#[test]
fn mapping_takes_the_first_candidate_the_server_has_not_the_best_looking_one() {
    let tools = vec![
        tool("create_ticket", json!({"type": "object"})),
        tool("create_work_item", json!({"type": "object"})),
    ];

    let mapping = resolve(&andare_spec().unwrap_or_else(|e| panic!("{e}")), &tools);
    let binding = mapping.binding("create_issue").expect("bound");

    assert_eq!(
        binding.tool, "create_work_item",
        "candidate order is the priority order; `create_ticket` is listed later and \
         must not win just because the server listed it first"
    );
}

// --- criterion 2: unmapped is reported, never guessed ----------------------

#[tokio::test]
async fn mapping_reports_unmapped_rather_than_calling_a_plausible_wrong_tool() {
    if !node_is_installed() {
        println!("SKIPPED (node not installed, nothing verified): mapping_reports_unmapped...");
        return;
    }

    let tools = discovered_tools("bare", "unmappable.json")
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let mapping = resolve(&andare_spec().unwrap_or_else(|e| panic!("{e}")), &tools);

    assert!(mapping.bound.is_empty(), "nothing should have bound");
    assert!(!mapping.is_complete());
    assert_eq!(mapping.unmapped.len(), 2);

    let unmapped = &mapping.unmapped[0];
    assert_eq!(unmapped.capability, "create_issue");
    // The available list travels with the report so RL-605's manual override can
    // offer a choice without asking the server again.
    assert_eq!(unmapped.available, vec!["ping".to_owned()]);

    let explained = unmapped.explain();
    assert!(explained.contains("unmapped"), "{explained}");
    assert!(explained.contains("ping"), "{explained}");
}

/// The failure this criterion is really about: a name that a fuzzy matcher would
/// happily accept.
#[test]
fn mapping_does_not_match_a_near_miss() {
    for name in ["create_issues", "createIssue", "create", "issue"] {
        let tools = vec![tool(name, json!({"type": "object"}))];
        let mapping = resolve(&andare_spec().unwrap_or_else(|e| panic!("{e}")), &tools);
        assert!(
            mapping.binding("create_issue").is_none(),
            "`{name}` is not one of the candidates and must not bind — filing a \
             finding into whatever happened to look close is the failure mode this \
             rule exists for"
        );
    }
}

// --- criterion 3: the schema is checked before anything is sent ------------

#[tokio::test]
async fn mapping_refuses_a_payload_the_tool_would_reject_and_names_the_field() {
    if !node_is_installed() {
        println!("SKIPPED (node not installed, nothing verified): mapping_refuses_a_payload...");
        return;
    }

    let tools = discovered_tools("andare", "andare-renamed.json")
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let mapping = resolve(&andare_spec().unwrap_or_else(|e| panic!("{e}")), &tools);
    let binding = mapping.binding("create_issue").expect("bound");

    // `summary` is capped at 80 characters by the server's own schema.
    let long = "x".repeat(200);
    let context = RenderContext::new(json!({
        "finding": { "title": long, "body_md": "…" },
        "repo": { "config": { "andare_project": "REVL" } },
    }));

    let error = binding
        .render(&context)
        .expect_err("a summary past maxLength must not be sent");

    let MappingError::SchemaRejected {
        tool, violations, ..
    } = &error
    else {
        panic!("expected SchemaRejected, got {error:?}");
    };

    assert_eq!(tool, "create_work_item");
    assert!(
        violations.iter().any(|v| v.contains("summary")),
        "the message must name the offending field: {violations:?}"
    );
    assert!(
        error.to_string().contains("try:"),
        "§18: a user-visible error says what to do about it"
    );
}

#[tokio::test]
async fn mapping_accepts_a_payload_the_schema_allows() {
    if !node_is_installed() {
        println!("SKIPPED (node not installed, nothing verified): mapping_accepts_a_payload...");
        return;
    }

    let tools = discovered_tools("andare", "andare-renamed.json")
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let mapping = resolve(&andare_spec().unwrap_or_else(|e| panic!("{e}")), &tools);
    let binding = mapping.binding("create_issue").expect("bound");

    let payload = binding
        .render(&context())
        .unwrap_or_else(|e| panic!("a well-formed finding must render: {e}"));

    assert_eq!(payload["summary"], json!("SQL injection in the login path"));
    assert_eq!(payload["projectKey"], json!("REVL"));
}

#[test]
fn mapping_rejects_a_payload_missing_a_required_field() {
    let schema = json!({
        "type": "object",
        "required": ["summary", "description"],
        "properties": { "summary": {"type": "string"}, "description": {"type": "string"} }
    });
    let tools = vec![tool("create_issue", schema)];

    let table: toml::Value = toml::from_str(
        r#"
mcp_server = "andare"
[map.create_issue]
tool_candidates = ["create_issue"]
args = { summary = "{finding.title}" }
"#,
    )
    .expect("parses");
    let spec = TargetSpec::from_toml("andare", &table).expect("valid");
    let mapping = resolve(&spec, &tools);
    let binding = mapping.binding("create_issue").expect("bound");

    let error = binding
        .render(&context())
        .expect_err("a missing required field must be caught before the call");
    assert!(
        error.to_string().contains("description"),
        "the missing field must be named: {error}"
    );
}

// --- rendering rules -------------------------------------------------------

#[test]
fn mapping_a_whole_placeholder_keeps_the_value_s_type() {
    let schema = json!({
        "type": "object",
        "properties": { "line": {"type": "integer"}, "where": {"type": "string"} }
    });
    let tools = vec![tool("annotate", schema)];

    let table: toml::Value = toml::from_str(
        r#"
mcp_server = "andare"
[map.annotate]
tool_candidates = ["annotate"]
args = { line = "{finding.line}", where = "line {finding.line} of the login path" }
"#,
    )
    .expect("parses");
    let spec = TargetSpec::from_toml("andare", &table).expect("valid");
    let binding = resolve(&spec, &tools)
        .binding("annotate")
        .cloned()
        .expect("bound");

    let payload = binding.render(&context()).unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(
        payload["line"],
        json!(42),
        "a string that is exactly one placeholder keeps the referenced type — a \
         schema saying `integer` would reject \"42\""
    );
    assert_eq!(payload["where"], json!("line 42 of the login path"));
}

#[test]
fn mapping_an_unresolvable_placeholder_is_an_error_not_an_empty_string() {
    let tools = vec![tool(
        "create_issue",
        json!({"type": "object", "properties": {"summary": {"type": "string"}}}),
    )];
    let table: toml::Value = toml::from_str(
        r#"
mcp_server = "andare"
[map.create_issue]
tool_candidates = ["create_issue"]
args = { summary = "{finding.nope}" }
"#,
    )
    .expect("parses");
    let spec = TargetSpec::from_toml("andare", &table).expect("valid");
    let binding = resolve(&spec, &tools)
        .binding("create_issue")
        .cloned()
        .expect("bound");

    let error = binding
        .render(&context())
        .expect_err("an unknown placeholder must not render as empty");

    assert!(
        matches!(error, MappingError::UnknownPlaceholder { .. }),
        "{error:?}"
    );
    assert!(error.to_string().contains("finding.nope"), "{error}");
    // §18 again: an issue filed with an empty title looks like it worked.
}

#[test]
fn mapping_renders_inside_arrays() {
    let tools = vec![tool(
        "create_issue",
        json!({
            "type": "object",
            "properties": { "labels": {"type": "array", "items": {"type": "string"}} }
        }),
    )];
    let table: toml::Value = toml::from_str(
        r#"
mcp_server = "andare"
[map.create_issue]
tool_candidates = ["create_issue"]
args = { labels = ["rev-local", "{status}"] }
"#,
    )
    .expect("parses");
    let spec = TargetSpec::from_toml("andare", &table).expect("valid");
    let binding = resolve(&spec, &tools)
        .binding("create_issue")
        .cloned()
        .expect("bound");

    let payload = binding.render(&context()).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(payload["labels"], json!(["rev-local", "In Review"]));
}

// --- reading the config table ---------------------------------------------

#[test]
fn mapping_a_target_without_a_server_is_a_named_error() {
    let table: toml::Value =
        toml::from_str("[map.create_issue]\ntool_candidates = [\"x\"]").expect("parses");
    let error = TargetSpec::from_toml("andare", &table).expect_err("mcp_server is required");

    assert_eq!(
        error,
        SpecError::MissingField {
            target: "andare".to_owned(),
            field: "mcp_server".to_owned()
        }
    );
    assert!(error.to_string().contains("try:"));
}

#[test]
fn mapping_a_capability_with_no_candidates_is_refused_at_load() {
    let table: toml::Value =
        toml::from_str("mcp_server = \"andare\"\n[map.create_issue]\ntool_candidates = []")
            .expect("parses");
    let error = TargetSpec::from_toml("andare", &table).expect_err("an empty list binds nothing");

    assert!(
        matches!(error, SpecError::NoCandidates { .. }),
        "a capability that can never bind is a config mistake, and saying so at load \
         beats reporting it as unmapped on every run: {error:?}"
    );
}

/// A tool as a server would report it.
fn tool(name: &str, schema: serde_json::Value) -> Tool {
    Tool {
        name: name.to_owned(),
        description: String::new(),
        input_schema: schema,
    }
}

// --- built-in profiles (RL-606, ADR 0028) ---------------------------------

#[test]
fn mapping_the_builtin_andare_profile_parses_and_uses_andare_s_own_argument_names() {
    let spec = builtin_target("andare").expect("andare has a built-in profile");

    assert_eq!(spec.mcp_server, "andare");
    let names: Vec<&str> = spec.capabilities.iter().map(|c| c.name.as_str()).collect();
    for expected in ["comment", "create_issue", "search", "set_status"] {
        assert!(
            names.contains(&expected),
            "{expected} missing from {names:?}"
        );
    }

    let create = spec
        .capabilities
        .iter()
        .find(|c| c.name == "create_issue")
        .expect("create_issue");

    // ADR 0028: Andare takes `summary`/`description`, not the `title`/`body` of
    // SPEC §11.2's illustrative example. Copying the example verbatim would fail
    // schema validation, which is the check earning itself.
    assert!(create.args.get("summary").is_some(), "{:?}", create.args);
    assert!(
        create.args.get("description").is_some(),
        "{:?}",
        create.args
    );
    assert!(create.args.get("title").is_none(), "{:?}", create.args);
    assert!(create.args.get("body").is_none(), "{:?}", create.args);

    assert_eq!(
        create.tool_candidates.first().map(String::as_str),
        Some("create_issue"),
        "the real name is listed first; candidate order is priority order"
    );

    let status = spec
        .capabilities
        .iter()
        .find(|c| c.name == "set_status")
        .expect("set_status");
    assert_eq!(
        status.tool_candidates.first().map(String::as_str),
        Some("set_issue_status"),
        "Andare has both `set_issue_status` and `update_issue`, and `update_issue` \
         does not accept a status — so the order here is load-bearing"
    );
}

#[test]
fn mapping_a_target_with_no_builtin_profile_is_none() {
    assert!(
        builtin_target("jira").is_none(),
        "a built-in profile is a convenience, not a requirement; an unknown target \
         is configured by the user rather than guessed at"
    );
}

#[test]
fn mapping_the_builtin_profile_binds_against_the_mock_default_server() {
    // The mock's default profile exposes `create_issue` and `set_issue_status`,
    // which are two of Andare's four. The other two are absent there, so this also
    // shows a partial bind reported as such rather than as a failure.
    let spec = builtin_target("andare").expect("built-in");
    let tools = vec![
        tool("create_issue", json!({"type": "object"})),
        tool("set_issue_status", json!({"type": "object"})),
    ];

    let mapping = resolve(&spec, &tools);
    assert_eq!(mapping.bound.len(), 2);
    assert_eq!(mapping.unmapped.len(), 2);
    assert!(!mapping.is_complete());
}
