//! Building publish targets from a config (REVL-223, SPEC §11.2, §13.1).
//!
//! The defect these guard is not "the builder is wrong" — the desktop app's two
//! builders were right, and had been for milestones. It is that they lived in a
//! binary, so exactly one of rev-local's two surfaces could deliver to a tracker
//! and the headless one silently could not. So what is asserted here is mostly
//! *coverage*: which destinations a config makes buildable, and that a
//! destination it does not make buildable says why in a sentence somebody can act
//! on rather than vanishing.
//!
//! Nothing here connects to anything. `from_config` is required not to: an
//! `HttpEndpoint` holds deferred keychain references and a `StdioClient` does not
//! spawn its child until a tool is called, which is what lets this run in the
//! inner loop with no network and no keychain read.

use revlocal_core::GlobalConfig;
use revlocal_publish::{targets_from_config, TargetSet};

/// Parse a fixture config and build its targets.
///
/// Helpers return `Result` and only `#[test]` functions panic (ADR 0003), so a
/// fixture that stops parsing is reported as a failure with the parser's own
/// message rather than as a panic in a helper.
fn targets(toml_text: &str) -> Result<TargetSet, Box<dyn std::error::Error>> {
    let (config, _warnings) = GlobalConfig::parse(toml_text)?;
    Ok(targets_from_config(&config))
}

/// The bare case, and the one that matters most: a config with no MCP servers at
/// all still delivers to GitHub, because GitHub needs nothing from the config —
/// `gh` holds its own credentials and the repository's remote names the project.
///
/// Nothing in the workspace constructed a `GitHubTarget` outside its own tests
/// before this, so a repository with `targets = ["github"]` queued actions that
/// no process anywhere could route.
#[test]
fn github_is_built_from_a_config_that_mentions_nothing() -> Result<(), Box<dyn std::error::Error>> {
    let set = targets("")?;

    assert!(
        set.has("github"),
        "GitHub needs no [mcpServers] entry and must be built regardless: {set:?}"
    );
    assert_eq!(
        set.reason("github"),
        None,
        "a built target has no unavailability reason: {set:?}"
    );

    Ok(())
}

/// Andare and Trama need a server, and not having one is ordinary rather than
/// broken — so what is asserted is that the *reason* survives, in a form that
/// names the config key to add.
#[test]
fn an_unconfigured_tracker_is_reported_rather_than_dropped(
) -> Result<(), Box<dyn std::error::Error>> {
    let set = targets("")?;

    assert!(!set.has("andare"), "nothing was configured: {set:?}");
    assert!(!set.has("trama"), "nothing was configured: {set:?}");

    for target in ["andare", "trama"] {
        let reason = set
            .reason(target)
            .unwrap_or_else(|| panic!("{target} is not built and gave no reason: {set:?}"));
        assert!(
            reason.contains(&format!("mcpServers.{target}")),
            "the reason must name the config key to add, and said: {reason}"
        );
    }

    assert_eq!(
        set.notes().len(),
        2,
        "one line per destination that cannot be delivered to: {set:?}"
    );

    Ok(())
}

#[test]
fn an_http_server_builds_its_target() -> Result<(), Box<dyn std::error::Error>> {
    let set = targets(
        r#"
[mcpServers.andare]
type = "http"
url = "https://andare.invalid/mcp"

[mcpServers.trama]
type = "http"
url = "https://trama.invalid/mcp"
"#,
    )?;

    assert!(set.has("andare"), "{set:?}");
    assert!(set.has("trama"), "{set:?}");
    assert!(
        set.unavailable.is_empty(),
        "everything asked for was built: {set:?}"
    );

    Ok(())
}

/// The desktop app's own wiring took `type = "http"` and refused everything else,
/// which meant a suite run locally over stdio was unreachable from the surface
/// that could publish at all. Both transports are the point of moving this out of
/// that binary.
#[test]
fn a_stdio_server_builds_its_target_too() -> Result<(), Box<dyn std::error::Error>> {
    let set = targets(
        r#"
[mcpServers.andare]
type = "stdio"
command = "andare-mcp"
args = ["--stdio"]
"#,
    )?;

    assert!(set.has("andare"), "a stdio server is a server: {set:?}");

    Ok(())
}

/// §18: a cap is never silent. A server entry that cannot be used says which
/// field is missing, not "andare is unavailable".
#[test]
fn an_incomplete_server_entry_names_the_field_it_is_missing(
) -> Result<(), Box<dyn std::error::Error>> {
    let set = targets(
        r#"
[mcpServers.andare]
type = "http"

[mcpServers.trama]
type = "stdio"
"#,
    )?;

    let andare = set.reason("andare").unwrap_or_default();
    assert!(
        andare.contains("url"),
        "an http server with no url must say so: {andare}"
    );

    let trama = set.reason("trama").unwrap_or_default();
    assert!(
        trama.contains("command"),
        "a stdio server with no command must say so: {trama}"
    );

    Ok(())
}

#[test]
fn an_unknown_transport_is_quoted_back() -> Result<(), Box<dyn std::error::Error>> {
    let set = targets(
        r#"
[mcpServers.andare]
type = "carrier-pigeon"
"#,
    )?;

    let reason = set.reason("andare").unwrap_or_default();
    assert!(
        reason.contains("carrier-pigeon"),
        "the reason must quote what the config actually said: {reason}"
    );
    assert!(
        reason.contains("stdio") && reason.contains("http"),
        "and name the transports that would work: {reason}"
    );

    Ok(())
}

/// One destination failing to build must not take the others with it — a
/// repository can want a wiki page and no issue, or the reverse.
#[test]
fn one_broken_server_does_not_stop_the_others() -> Result<(), Box<dyn std::error::Error>> {
    let set = targets(
        r#"
[mcpServers.andare]
type = "http"
url = "https://andare.invalid/mcp"

[mcpServers.trama]
type = "nonsense"
"#,
    )?;

    assert!(set.has("andare"), "{set:?}");
    assert!(set.has("github"), "{set:?}");
    assert!(!set.has("trama"), "{set:?}");
    assert_eq!(set.unavailable.len(), 1, "{set:?}");

    Ok(())
}

/// The local report is registered by whoever owns the queue, once. Building it
/// here as well would register it twice and make "what did the config ask for?"
/// unanswerable.
#[test]
fn the_local_report_is_not_one_of_these() -> Result<(), Box<dyn std::error::Error>> {
    let set = targets("")?;

    assert!(
        !set.has(revlocal_publish::REPORT_TARGET),
        "the report target needs no configuration and is not built from one: {set:?}"
    );

    Ok(())
}
