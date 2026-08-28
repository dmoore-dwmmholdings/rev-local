//! `revlocal targets list` (RL-604, SPEC §11.2, §14).
//!
//! Capability mapping is the part of rev-local most likely to be *silently* half
//! working: a target binds four of its five capabilities, publishing looks fine,
//! and the fifth turns out to be missing only when a run needed it. §11.2 requires
//! the UI show exactly which capability failed to bind and to which server. This
//! is the headless equivalent, and it exists for the same reason: unmapped is only
//! useful if somebody can see it.
//!
//! Nothing here calls a tool. Listing is discovery plus resolution, and a command
//! that reports on your publish configuration should not be able to publish.

use std::path::Path;

use revlocal_core::{GlobalConfig, McpServerSettings};
use revlocal_mcp::{
    resolve, Discovery, HttpClient, HttpEndpoint, NoSecrets, ServerCommand, ServerState,
    StdioClient, TargetSpec,
};

/// Why `targets list` could not report.
#[derive(Debug, thiserror::Error)]
pub enum TargetsCommandError {
    /// The config file could not be read.
    #[error(
        "could not read {path}: {source}\n  try: pass --config with the path to your config.toml"
    )]
    Unreadable {
        /// Which file.
        path: String,
        /// Why.
        #[source]
        source: std::io::Error,
    },

    /// The config file is not valid TOML, or not a valid document.
    #[error("could not parse {path}: {source}")]
    Malformed {
        /// Which file.
        path: String,
        /// Why.
        #[source]
        source: toml::de::Error,
    },

    /// A `[targets.*]` table could not be read.
    #[error(transparent)]
    Spec(#[from] revlocal_mcp::SpecError),

    /// An MCP server entry names a transport this build does not speak.
    #[error("mcpServers.{server} has type `{transport}`\n  try: use `stdio` or `http`")]
    UnknownTransport {
        /// Which server.
        server: String,
        /// What it said.
        transport: String,
    },

    /// A server entry is missing the field its transport needs.
    #[error("mcpServers.{server} is missing `{field}` for a {transport} server\n  try: add it to your config")]
    IncompleteServer {
        /// Which server.
        server: String,
        /// Which field.
        field: String,
        /// Which transport.
        transport: String,
    },

    /// The HTTP client could not be built for that endpoint.
    #[error(transparent)]
    Http(#[from] revlocal_mcp::HttpError),

    /// The report could not be serialized.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// Build one client from a `[mcpServers.<id>]` entry.
fn client(id: &str, settings: &McpServerSettings) -> Result<Client, TargetsCommandError> {
    match settings.transport.as_str() {
        "stdio" => {
            let command = settings.command.as_deref().ok_or_else(|| {
                TargetsCommandError::IncompleteServer {
                    server: id.to_owned(),
                    field: "command".to_owned(),
                    transport: "stdio".to_owned(),
                }
            })?;
            let args: Vec<&str> = settings.args.iter().map(String::as_str).collect();
            Ok(Client::Stdio(ServerCommand::new(id, command, &args)))
        }
        "http" => {
            let url =
                settings
                    .url
                    .as_deref()
                    .ok_or_else(|| TargetsCommandError::IncompleteServer {
                        server: id.to_owned(),
                        field: "url".to_owned(),
                        transport: "http".to_owned(),
                    })?;
            let mut endpoint = HttpEndpoint::new(id, url);
            for (name, value) in &settings.headers {
                endpoint = endpoint.with_header(name, value.clone());
            }
            Ok(Client::Http(endpoint))
        }
        other => Err(TargetsCommandError::UnknownTransport {
            server: id.to_owned(),
            transport: other.to_owned(),
        }),
    }
}

/// A client that has not been constructed yet, so a config error is reported
/// before anything is spawned or connected.
enum Client {
    Stdio(ServerCommand),
    Http(HttpEndpoint),
}

/// Run `revlocal targets list`.
pub async fn run(config_path: &Path, json: bool) -> Result<(), TargetsCommandError> {
    let text =
        std::fs::read_to_string(config_path).map_err(|source| TargetsCommandError::Unreadable {
            path: config_path.display().to_string(),
            source,
        })?;

    let (config, warnings) =
        GlobalConfig::parse(&text).map_err(|source| TargetsCommandError::Malformed {
            path: config_path.display().to_string(),
            source,
        })?;

    // §18: unknown keys are surfaced rather than dropped. Under --json they go to
    // stderr, so stdout stays exactly one document.
    for warning in &warnings {
        eprintln!("revlocal: {}", warning.message());
    }

    let mut discovery = Discovery::new();
    for (id, settings) in &config.mcp_servers {
        match client(id, settings)? {
            Client::Stdio(command) => discovery.insert(StdioClient::new(command)),
            Client::Http(endpoint) => discovery.insert(HttpClient::new(endpoint)?),
        }
    }

    // Secrets are not resolved here. `targets list` reports on configuration; a
    // command that reads the keychain to print a summary is a command that reads
    // the keychain more often than anything needs to.
    let health = discovery.refresh_all(&NoSecrets).await;

    let mut reports = Vec::new();
    for (id, table) in &config.targets {
        let spec = TargetSpec::from_toml(id, table)?;

        let server_state = health
            .servers
            .iter()
            .find(|s| s.id == spec.mcp_server)
            .map(|s| s.state.clone());

        let mapping = match discovery.tools(&spec.mcp_server, &NoSecrets).await {
            Some(Ok(tools)) => Some(resolve(&spec, tools)),
            // A server that did not answer leaves its target unresolved rather
            // than unmapped: "we could not ask" and "we asked and it has not got
            // it" call for different remedies.
            _ => None,
        };

        reports.push(Report {
            spec,
            mapping,
            server_state,
        });
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&as_json(&reports))?);
    } else {
        print_human(&reports, &health.lines());
    }

    Ok(())
}

/// One target's resolution outcome.
struct Report {
    spec: TargetSpec,
    mapping: Option<revlocal_mcp::TargetMapping>,
    server_state: Option<ServerState>,
}

fn print_human(reports: &[Report], server_lines: &[String]) {
    println!("servers:");
    if server_lines.is_empty() {
        println!("  (none configured)");
    }
    for line in server_lines {
        println!("  {line}");
    }

    println!("targets:");
    if reports.is_empty() {
        println!("  (none configured)");
    }

    for report in reports {
        match &report.mapping {
            Some(mapping) => {
                println!("  {}", mapping.summary_line());
                for binding in &mapping.bound {
                    println!("    {} → {}", binding.capability, binding.tool);
                }
                for unmapped in &mapping.unmapped {
                    println!("    {}", unmapped.explain());
                }
            }
            None => {
                let reason = match &report.server_state {
                    Some(ServerState::Unreachable { reason, .. }) => reason.clone(),
                    _ => "the server was not reachable".to_owned(),
                };
                println!(
                    "  {} → {}: not resolved — {reason}",
                    report.spec.id, report.spec.mcp_server
                );
            }
        }
    }
}

fn as_json(reports: &[Report]) -> serde_json::Value {
    serde_json::json!({
        "targets": reports
            .iter()
            .map(|report| match &report.mapping {
                Some(mapping) => serde_json::json!({
                    "target": mapping.target,
                    "server": mapping.server,
                    "resolved": true,
                    "mapped": mapping.bound.iter().map(|b| serde_json::json!({
                        "capability": b.capability,
                        "tool": b.tool,
                    })).collect::<Vec<_>>(),
                    "unmapped": mapping.unmapped.iter().map(|u| serde_json::json!({
                        "capability": u.capability,
                        "candidates": u.candidates,
                        "available": u.available,
                    })).collect::<Vec<_>>(),
                }),
                None => serde_json::json!({
                    "target": report.spec.id,
                    "server": report.spec.mcp_server,
                    "resolved": false,
                    "reason": match &report.server_state {
                        Some(ServerState::Unreachable { reason, .. }) => reason.clone(),
                        _ => "the server was not reachable".to_owned(),
                    },
                }),
            })
            .collect::<Vec<_>>()
    })
}
