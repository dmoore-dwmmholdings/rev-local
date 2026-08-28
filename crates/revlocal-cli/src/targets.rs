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
    parse_arg, resolve, Discovery, HttpClient, HttpEndpoint, NoSecrets, Override, Overrides,
    RenderContext, ServerCommand, ServerState, StdioClient, TargetSpec, Tool,
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
    ///
    /// The source is boxed: `toml::de::Error` is 96 bytes, and an error type that
    /// large travels in every `Result` this command returns. clippy's
    /// `result_large_err` is right about it.
    #[error("could not parse {path}: {source}")]
    Malformed {
        /// Which file.
        path: String,
        /// Why.
        #[source]
        source: Box<toml::de::Error>,
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
    ///
    /// Boxed for the same reason as `Malformed`: `HttpError` is 120 bytes.
    #[error(transparent)]
    Http(Box<revlocal_mcp::HttpError>),

    /// The report could not be serialized.
    #[error(transparent)]
    Json(#[from] serde_json::Error),

    /// An override could not be saved, loaded or validated.
    #[error(transparent)]
    Override(#[from] revlocal_mcp::OverrideError),

    /// The config names no such target.
    #[error("no target `{target}` in your config\n  try: one of [{}]", known.join(", "))]
    NoSuchTarget {
        /// What was asked for.
        target: String,
        /// What is configured.
        known: Vec<String>,
    },

    /// A server could not be contacted, or is not configured.
    #[error("{0}")]
    Discovery(String),

    /// An `--arg` was not `key=value`.
    #[error("`{text}` is not `key=value`\n  try: --arg summary={{finding.title}}")]
    BadArg {
        /// What was passed.
        text: String,
    },

    /// A dry run produced at least one capability that would not render.
    #[error("{failed} of {total} mapped capabilities would not render")]
    DryRunFailed {
        /// How many failed.
        failed: usize,
        /// How many were tried.
        total: usize,
    },
}

impl From<revlocal_mcp::HttpError> for TargetsCommandError {
    fn from(error: revlocal_mcp::HttpError) -> Self {
        Self::Http(Box::new(error))
    }
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
            source: Box::new(source),
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

/// Where overrides are kept when `--overrides` is not given: beside the config.
///
/// Predictable rather than clever. A user who moves their config moves the
/// overrides that go with it, and a test can point both at a temp directory.
pub fn default_overrides_path(config_path: &Path) -> std::path::PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("target-overrides.json")
}

/// Discover one target's server tools, for `map` and `test`.
async fn tools_for(
    config: &GlobalConfig,
    target: &str,
) -> Result<(TargetSpec, Vec<Tool>), TargetsCommandError> {
    let table = config
        .targets
        .get(target)
        .ok_or_else(|| TargetsCommandError::NoSuchTarget {
            target: target.to_owned(),
            known: config.targets.keys().cloned().collect(),
        })?;
    let spec = TargetSpec::from_toml(target, table)?;

    let mut discovery = Discovery::new();
    for (id, settings) in &config.mcp_servers {
        match client(id, settings)? {
            Client::Stdio(command) => discovery.insert(StdioClient::new(command)),
            Client::Http(endpoint) => discovery.insert(HttpClient::new(endpoint)?),
        }
    }

    let tools = match discovery.tools(&spec.mcp_server, &NoSecrets).await {
        Some(Ok(tools)) => tools.to_vec(),
        Some(Err(error)) => return Err(TargetsCommandError::Discovery(error.to_string())),
        None => {
            return Err(TargetsCommandError::Discovery(format!(
                "no MCP server `{}` is configured",
                spec.mcp_server
            )))
        }
    };

    Ok((spec, tools))
}

/// Read the config file.
fn read_config(config_path: &Path) -> Result<GlobalConfig, TargetsCommandError> {
    let text =
        std::fs::read_to_string(config_path).map_err(|source| TargetsCommandError::Unreadable {
            path: config_path.display().to_string(),
            source,
        })?;
    let (config, warnings) =
        GlobalConfig::parse(&text).map_err(|source| TargetsCommandError::Malformed {
            path: config_path.display().to_string(),
            source: Box::new(source),
        })?;
    for warning in &warnings {
        eprintln!("revlocal: {}", warning.message());
    }
    Ok(config)
}

/// Run `revlocal targets map`.
///
/// The override is checked against the tool's schema **before** it is written, so
/// a typo'd tool name or a template missing a required field is refused here
/// rather than at the first publish that needed it (RL-605 criterion 2).
pub async fn map(
    config_path: &Path,
    overrides_path: Option<&Path>,
    target: &str,
    capability: &str,
    tool: &str,
    args: &[String],
) -> Result<(), TargetsCommandError> {
    let config = read_config(config_path)?;
    let (_spec, tools) = tools_for(&config, target).await?;

    let mut template = serde_json::Map::new();
    for text in args {
        let (name, value) =
            parse_arg(text).ok_or_else(|| TargetsCommandError::BadArg { text: text.clone() })?;
        template.insert(name, value);
    }

    let entry = Override {
        target: target.to_owned(),
        capability: capability.to_owned(),
        tool: tool.to_owned(),
        args: serde_json::Value::Object(template),
    };
    entry.check_against(&tools)?;

    let path = overrides_path.map_or_else(
        || default_overrides_path(config_path),
        std::path::Path::to_path_buf,
    );
    let mut overrides = Overrides::load(&path)?;
    overrides.set(entry);
    overrides.save(&path)?;

    println!(
        "revlocal: {target}/{capability} → {tool} (saved to {})",
        path.display()
    );
    Ok(())
}

/// A representative context for a dry run.
///
/// Fixed rather than drawn from the database: `targets test` answers "would this
/// template render at all", and a real finding would make the answer depend on
/// which finding happened to be lying around.
fn sample_context() -> RenderContext {
    RenderContext::new(serde_json::json!({
        "finding": {
            "title": "Sample finding for a dry run",
            "body_md": "This is a dry run. No tool was called.",
            "category": "correctness",
            "severity": "high",
            "line": 1,
            "file": "src/main.rs",
        },
        "repo": { "name": "sample", "config": { "andare_project": "SAMPLE" } },
        "issue_ref": "SAMPLE-1",
        "status": "In Review",
        "query": "text ~ \"sample\"",
        "comment": { "body_md": "Sample comment." },
    }))
}

/// Run `revlocal targets test` — render every mapped capability, call nothing.
pub async fn test(
    config_path: &Path,
    overrides_path: Option<&Path>,
    target: &str,
) -> Result<(), TargetsCommandError> {
    let config = read_config(config_path)?;
    let (spec, tools) = tools_for(&config, target).await?;

    let mut mapping = resolve(&spec, &tools);
    let path = overrides_path.map_or_else(
        || default_overrides_path(config_path),
        std::path::Path::to_path_buf,
    );
    Overrides::load(&path)?.apply(&mut mapping, &tools);

    let context = sample_context();
    let mut failed = 0;

    println!("{}", mapping.summary_line());
    for binding in &mapping.bound {
        let source = if binding.from_override {
            " (override)"
        } else {
            ""
        };
        match binding.render(&context) {
            Ok(payload) => println!(
                "  ok   {} → {}{source}: {payload}",
                binding.capability, binding.tool
            ),
            Err(error) => {
                failed += 1;
                println!(
                    "  FAIL {} → {}{source}: {error}",
                    binding.capability, binding.tool
                );
            }
        }
    }
    for unmapped in &mapping.unmapped {
        println!("  --   {}", unmapped.explain());
    }

    println!("revlocal: dry run only — no tool was called");

    if failed == 0 {
        Ok(())
    } else {
        Err(TargetsCommandError::DryRunFailed {
            failed,
            total: mapping.bound.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::TargetsCommandError;

    /// clippy's `result_large_err` fires at 128 bytes, and it fired on CI (Rust
    /// 1.98) while this machine's older toolchain said nothing. The size is
    /// asserted here so the next large variant is caught by `cargo test` rather
    /// than by a CI leg three commits later.
    #[test]
    fn the_error_type_stays_small_enough_to_return_by_value() {
        let size = std::mem::size_of::<TargetsCommandError>();
        assert!(
            size < 128,
            "TargetsCommandError is {size} bytes; clippy::result_large_err fires at \
             128. Box the largest variant's payload rather than raising this bound."
        );
    }
}
