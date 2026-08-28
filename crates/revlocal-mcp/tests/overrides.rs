//! Manual capability overrides (RL-605, SPEC §11.2).
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use revlocal_mcp::{builtin_target, parse_arg, resolve, Override, OverrideError, Overrides, Tool};
use serde_json::json;

/// A tool as a server would report it.
fn tool(name: &str, schema: serde_json::Value) -> Tool {
    Tool {
        name: name.to_owned(),
        description: String::new(),
        input_schema: schema,
    }
}

/// A server whose issue-filing tool is named something nothing looks for, so
/// nothing binds without an override.
fn stubborn_server() -> Vec<Tool> {
    vec![tool(
        "file_a_thing",
        json!({
            "type": "object",
            "required": ["project", "headline"],
            "properties": {
                "project": {"type": "string"},
                "headline": {"type": "string", "maxLength": 80},
                "detail": {"type": "string"},
                "points": {"type": "integer"}
            }
        }),
    )]
}

fn an_override() -> Override {
    Override {
        target: "andare".to_owned(),
        capability: "create_issue".to_owned(),
        tool: "file_a_thing".to_owned(),
        args: json!({ "project": "REVL", "headline": "{finding.title}" }),
    }
}

// --- criterion 1: the override survives a restart -------------------------

#[test]
fn override_survives_a_restart() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state").join("target-overrides.json");

    let mut saved = Overrides::new();
    saved.set(an_override());
    saved.save(&path).expect("save");

    // A different process would do exactly this: read the file and nothing else.
    let loaded = Overrides::load(&path).expect("load");

    assert_eq!(loaded, saved);
    let entry = loaded
        .get("andare", "create_issue")
        .expect("the override is still there");
    assert_eq!(entry.tool, "file_a_thing");
    assert_eq!(entry.args["headline"], json!("{finding.title}"));
}

#[test]
fn override_a_missing_file_is_no_overrides_rather_than_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let loaded = Overrides::load(&dir.path().join("nothing-here.json")).expect("load");

    assert!(
        loaded.is_empty(),
        "no file is the normal state of a system nobody has had to override anything on"
    );
}

#[test]
fn override_saving_the_same_capability_twice_replaces_rather_than_accumulates() {
    let mut overrides = Overrides::new();
    overrides.set(an_override());

    let mut second = an_override();
    second.tool = "file_a_different_thing".to_owned();
    overrides.set(second);

    assert_eq!(
        overrides.len(),
        1,
        "two rules would need a precedence order"
    );
    assert_eq!(
        overrides
            .get("andare", "create_issue")
            .map(|o| o.tool.as_str()),
        Some("file_a_different_thing")
    );
}

// --- criterion 2: validated at save time ----------------------------------

#[test]
fn override_naming_a_tool_the_server_does_not_have_is_refused_at_save_time() {
    let entry = Override {
        tool: "no_such_tool".to_owned(),
        ..an_override()
    };

    let error = entry
        .check_against(&stubborn_server())
        .expect_err("a typo'd tool name must not be saved");

    let OverrideError::NoSuchTool { available, .. } = &error else {
        panic!("expected NoSuchTool, got {error:?}");
    };
    assert_eq!(available, &vec!["file_a_thing".to_owned()]);
    assert!(error.to_string().contains("try:"), "§18: {error}");
}

#[test]
fn override_missing_a_required_field_is_refused_at_save_time() {
    let entry = Override {
        args: json!({ "project": "REVL" }),
        ..an_override()
    };

    let error = entry
        .check_against(&stubborn_server())
        .expect_err("a template missing a required field must not be saved");

    let OverrideError::MissingRequired { fields, .. } = &error else {
        panic!("expected MissingRequired, got {error:?}");
    };
    assert_eq!(
        fields,
        &vec!["headline".to_owned()],
        "the missing field is named, because that is what the user has to fix"
    );
}

#[test]
fn override_that_supplies_every_required_field_saves() {
    an_override()
        .check_against(&stubborn_server())
        .expect("a complete override is accepted");
}

/// What save-time validation deliberately cannot catch, recorded so the limit is
/// explicit rather than assumed.
#[test]
fn override_save_time_validation_does_not_check_values_because_there_are_none_yet() {
    let entry = Override {
        // `headline` is capped at 80 characters. Whether this template violates
        // that depends on the finding, which does not exist at save time.
        args: json!({ "project": "REVL", "headline": "{finding.title}" }),
        ..an_override()
    };

    entry
        .check_against(&stubborn_server())
        .expect("save-time validation checks shape, not values");
}

// --- applying an override to a resolved mapping ---------------------------

#[test]
fn override_binds_a_capability_that_resolution_could_not() {
    let spec = builtin_target("andare").expect("built-in");
    let tools = stubborn_server();

    let mut mapping = resolve(&spec, &tools);
    assert!(
        mapping.binding("create_issue").is_none(),
        "nothing should resolve against this server without help"
    );

    let mut overrides = Overrides::new();
    overrides.set(an_override());
    overrides.apply(&mut mapping, &tools);

    let binding = mapping
        .binding("create_issue")
        .expect("the override binds it");
    assert_eq!(binding.tool, "file_a_thing");
    assert!(
        binding.from_override,
        "§11.2 needs \"you told us to\" and \"we worked it out\" to be \
         distinguishable in the UI"
    );
    assert!(
        !mapping
            .unmapped
            .iter()
            .any(|u| u.capability == "create_issue"),
        "a capability must not be both bound and unmapped"
    );
}

#[test]
fn override_wins_over_a_capability_that_resolution_already_bound() {
    let spec = builtin_target("andare").expect("built-in");
    let tools = vec![
        tool(
            "create_issue",
            json!({"type": "object", "properties": {"project": {"type": "string"}}}),
        ),
        tool(
            "file_a_thing",
            json!({"type": "object", "properties": {"project": {"type": "string"}}}),
        ),
    ];

    let mut mapping = resolve(&spec, &tools);
    assert_eq!(
        mapping.binding("create_issue").map(|b| b.tool.as_str()),
        Some("create_issue")
    );

    let mut overrides = Overrides::new();
    overrides.set(an_override());
    overrides.apply(&mut mapping, &tools);

    assert_eq!(
        mapping.binding("create_issue").map(|b| b.tool.as_str()),
        Some("file_a_thing"),
        "an override is an instruction, not a suggestion"
    );
    assert_eq!(
        mapping
            .bound
            .iter()
            .filter(|b| b.capability == "create_issue")
            .count(),
        1,
        "the resolved binding is replaced, not duplicated"
    );
}

#[test]
fn override_naming_a_tool_that_has_since_gone_away_leaves_the_capability_unmapped() {
    let spec = builtin_target("andare").expect("built-in");
    // The server no longer has `file_a_thing`.
    let tools = vec![tool("something_else", json!({"type": "object"}))];

    let mut mapping = resolve(&spec, &tools);
    let mut overrides = Overrides::new();
    overrides.set(an_override());
    overrides.apply(&mut mapping, &tools);

    assert!(
        mapping.binding("create_issue").is_none(),
        "save-time validation cannot stop a tool disappearing later; binding to \
         nothing would be worse than reporting it"
    );
    let unmapped = mapping
        .unmapped
        .iter()
        .find(|u| u.capability == "create_issue")
        .expect("reported as unmapped");
    assert_eq!(unmapped.candidates, vec!["file_a_thing".to_owned()]);
}

#[test]
fn override_can_be_cleared() {
    let mut overrides = Overrides::new();
    overrides.set(an_override());

    assert!(overrides.clear("andare", "create_issue"));
    assert!(overrides.is_empty());
    assert!(
        !overrides.clear("andare", "create_issue"),
        "clearing what is not there reports that, rather than pretending"
    );
}

// --- `--arg k=v` parsing ---------------------------------------------------

#[test]
fn override_arg_values_keep_the_type_they_were_written_as() {
    assert_eq!(
        parse_arg("headline={finding.title}"),
        Some(("headline".to_owned(), json!("{finding.title}"))),
        "a template is a string until it is rendered"
    );
    assert_eq!(
        parse_arg("points=3"),
        Some(("points".to_owned(), json!(3))),
        "a schema saying `integer` would reject \"3\""
    );
    assert_eq!(
        parse_arg(r#"labels=["rev-local","bug"]"#),
        Some(("labels".to_owned(), json!(["rev-local", "bug"])))
    );
    assert_eq!(
        parse_arg("draft=true"),
        Some(("draft".to_owned(), json!(true)))
    );
}

#[test]
fn override_a_value_containing_an_equals_sign_survives() {
    assert_eq!(
        parse_arg("aql=text ~ \"rev-local\""),
        Some(("aql".to_owned(), json!("text ~ \"rev-local\""))),
        "splitting on the LAST `=` would corrupt query syntax"
    );
}

#[test]
fn override_a_malformed_arg_is_rejected_rather_than_guessed_at() {
    assert!(parse_arg("no-equals-sign").is_none());
    assert!(
        parse_arg("=value").is_none(),
        "an empty field name is not a field"
    );
}
