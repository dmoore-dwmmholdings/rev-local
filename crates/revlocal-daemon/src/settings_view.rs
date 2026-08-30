//! The settings screen (RL-1110, SPEC §15 screen 6).
//!
//! # Presence, never the secret
//!
//! §13.1 lets a header hold a literal value as well as a `{{keychain:name}}`
//! reference, and this screen renders headers. The rule here is that a secret
//! never crosses the IPC boundary at all — not redacted on the way out, not
//! starred in the front end, not present in the JSON somebody could read in a
//! devtools panel. [`SecretPresence`] has no field that could hold one.
//!
//! That is stricter than "do not display it" on purpose. Redacting at the point
//! of rendering means the value travelled, and every future renderer has to
//! remember. A type that cannot carry it only has to be right once.
//!
//! A literal is reported as *configured in the file* rather than as a name,
//! because the name is the thing worth telling somebody: `{{keychain:andare}}`
//! points at an entry they can go and check, and a literal points at a line they
//! should probably move into the keychain.
//!
//! # An unmapped capability is a finding, not an absence
//!
//! §11.2: rev-local reports what it could not bind, and never guesses. So an
//! unmapped capability carries what was looked for *and* what the server actually
//! has — everything needed to offer a manual override without asking the server
//! again, which is the "fix affordance" §15 wants.
//!
//! The two ways a capability ends up unbound are kept apart. A server that was
//! never contacted has *unknown* mappings, not unmapped ones: reporting four
//! unmapped capabilities for a server nobody has spoken to would send somebody
//! writing overrides for tools that are probably there.

use revlocal_core::{GlobalConfig, McpServerSettings, SecretRef};
use revlocal_mcp::{Discovery, NoSecrets, Override, Overrides};
use serde::{Deserialize, Serialize};

use crate::doctor::DoctorReport;

/// Why the settings could not be assembled.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    /// A target's `[targets.*]` table could not be read.
    #[error("{detail}")]
    Spec {
        /// What is wrong, in the terms the screen can show.
        detail: String,
    },

    /// An override was refused (RL-605).
    #[error("{detail}")]
    Override {
        /// Why, including what the schema wanted.
        detail: String,
    },
}

/// That a secret is configured, and where it comes from. Never what it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretPresence {
    /// Which header carries it.
    pub header: String,
    /// `keychain` or `literal`.
    pub source: String,
    /// The keychain entry's name, when it has one.
    ///
    /// A name is not a secret and is the useful half: it points at something
    /// somebody can go and check. `None` for a literal, whose value is exactly
    /// what must not travel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keychain_entry: Option<String>,
    /// What to do about it, when there is something to do.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub advice: Option<String>,
}

impl SecretPresence {
    /// Describe one configured header without carrying its value.
    pub fn of(header: &str, secret: &SecretRef) -> Self {
        match secret {
            SecretRef::Keychain { name } => Self {
                header: header.to_owned(),
                source: "keychain".to_owned(),
                keychain_entry: Some(name.clone()),
                advice: None,
            },
            // The value is deliberately not read. There is no branch here that
            // could put it in the struct, which is the property that makes this
            // safe rather than careful.
            SecretRef::Literal(_) => Self {
                header: header.to_owned(),
                source: "literal".to_owned(),
                keychain_entry: None,
                advice: Some(
                    "written in the config file; consider {{keychain:name}} instead".to_owned(),
                ),
            },
        }
    }
}

/// One configured MCP server (§11.2, §15 screen 6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerPanel {
    /// The server's id, as configured.
    pub id: String,
    /// `stdio` or `http`.
    pub transport: String,
    /// The command or URL, whichever this transport uses.
    ///
    /// A URL can carry a token in a query string, so this is the configured
    /// string and nothing is resolved into it.
    pub endpoint: String,
    /// Which headers hold secrets, and where from. Never the secrets.
    pub secrets: Vec<SecretPresence>,
    /// The tools it reported, by name.
    pub tools: Vec<String>,
    /// The doctor line for this server: `id: N tools, M mapped, K unmapped`.
    pub summary: String,
    /// Whether it answered at all.
    pub contacted: bool,
    /// Why it did not, when it did not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One capability that bound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundRow {
    /// What rev-local wanted to do.
    pub capability: String,
    /// The tool it resolved to.
    pub tool: String,
    /// Whether a person said so, or resolution worked it out.
    ///
    /// ADR 0015: "you told us to" and "we worked it out" are different answers to
    /// the question this table exists to answer, and a table that showed them
    /// alike would make an override impossible to find again.
    pub from_override: bool,
}

/// One capability that did not bind, and everything needed to fix it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnmappedRow {
    /// What rev-local wanted to do.
    pub capability: String,
    /// The tool names that were looked for, in order.
    pub candidates: Vec<String>,
    /// What the server has instead — the list a manual override picks from.
    pub available: Vec<String>,
    /// The sentence `revlocal targets list` prints.
    pub explanation: String,
}

/// One target's capability mapping (§11.2, §15's mapping table).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetPanel {
    /// Which target.
    pub target: String,
    /// Which server it resolves against.
    pub server: String,
    /// Capabilities that bound.
    pub bound: Vec<BoundRow>,
    /// Capabilities that did not.
    pub unmapped: Vec<UnmappedRow>,
    /// Whether the server has answered.
    ///
    /// When false, `bound` and `unmapped` are both empty and the screen says the
    /// mapping is unknown. Four unmapped capabilities for a server nobody has
    /// spoken to would send somebody writing overrides for tools that are there.
    pub server_contacted: bool,
}

impl TargetPanel {
    /// Whether anything here needs attention.
    pub fn has_unmapped(&self) -> bool {
        !self.unmapped.is_empty()
    }
}

/// Budgets and retention, as §13.1 has them.
// No `Eq`: a USD allowance is an `f64`, and a type that claims total equality
// over floats is claiming something false about NaN.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Limits {
    /// Daily token allowance per repository.
    pub daily_tokens_per_repo: u64,
    /// Daily run allowance per repository.
    pub daily_runs_per_repo: u32,
    /// Daily USD allowance; `0` means unlimited, which the screen spells out.
    pub daily_cost_usd_per_repo: f64,
    /// What happens when an allowance runs out. Never a silent drop.
    pub on_exhausted: String,
    /// How long transcripts are kept (§5.1).
    pub transcript_retention_days: u32,
}

impl Limits {
    /// Read the limits out of a config.
    pub fn of(config: &GlobalConfig) -> Self {
        Self {
            daily_tokens_per_repo: config.budgets.daily_tokens_per_repo,
            daily_runs_per_repo: config.budgets.daily_runs_per_repo,
            daily_cost_usd_per_repo: config.budgets.daily_cost_usd_per_repo,
            on_exhausted: config.budgets.on_exhausted.as_str().to_owned(),
            transcript_retention_days: config.global.transcript_retention_days,
        }
    }
}

/// The screen's data (§15 screen 6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingsView {
    /// `doctor`'s checks, inline. §15 asks for the output, not a link to it.
    pub doctor: DoctorReport,
    /// Configured MCP servers with their discovered tools.
    pub servers: Vec<ServerPanel>,
    /// Capability mapping, one panel per target.
    pub targets: Vec<TargetPanel>,
    /// Budgets and retention.
    pub limits: Limits,
    /// Where the config being shown came from, so the screen can name the file.
    pub config_path: String,
    /// Where manual overrides are stored.
    pub overrides_path: String,
    /// `[targets.*]` tables that would not parse.
    ///
    /// Shown rather than swallowed: a target that will not parse is one whose
    /// publishes are silently not happening, and a settings screen that omitted
    /// it would be the last place somebody would think to look.
    pub target_errors: Vec<String>,
}

impl SettingsView {
    /// How many capabilities are unmapped across every target.
    ///
    /// So the screen can lead with it. §15's criterion is that an unmapped
    /// capability is *visible*, and a count that has to be assembled by reading
    /// four panels is not.
    pub fn unmapped_count(&self) -> usize {
        self.targets.iter().map(|t| t.unmapped.len()).sum()
    }
}

/// Describe one configured server, without contacting it.
///
/// The tools and the reachability come from a discovery pass the caller has
/// already made; this is the part that is pure, and the part the secrets rule
/// lives in.
pub fn server_panel(
    id: &str,
    settings: &McpServerSettings,
    state: &revlocal_mcp::ServerState,
    mapped: usize,
    unmapped: usize,
    tools: &[String],
) -> ServerPanel {
    let endpoint = match settings.transport.as_str() {
        "http" => settings.url.clone().unwrap_or_default(),
        _ => {
            let mut parts = vec![settings.command.clone().unwrap_or_default()];
            parts.extend(settings.args.iter().cloned());
            parts.join(" ")
        }
    };

    let health = revlocal_mcp::ServerHealth {
        id: id.to_owned(),
        state: state.clone(),
        mapped,
        unmapped,
    };

    let error = match state {
        revlocal_mcp::ServerState::Unreachable { reason, .. } => Some(reason.clone()),
        _ => None,
    };

    ServerPanel {
        id: id.to_owned(),
        transport: settings.transport.clone(),
        endpoint,
        secrets: settings
            .headers
            .iter()
            .map(|(name, secret)| SecretPresence::of(name, secret))
            .collect(),
        tools: tools.to_vec(),
        summary: health.summary_line(),
        contacted: !matches!(state, revlocal_mcp::ServerState::Unknown),
        error,
    }
}

/// Turn a resolved mapping into the screen's panel.
pub fn target_panel(mapping: &revlocal_mcp::TargetMapping, server_contacted: bool) -> TargetPanel {
    TargetPanel {
        target: mapping.target.clone(),
        server: mapping.server.clone(),
        bound: mapping
            .bound
            .iter()
            .map(|b| BoundRow {
                capability: b.capability.clone(),
                tool: b.tool.clone(),
                from_override: b.from_override,
            })
            .collect(),
        unmapped: mapping
            .unmapped
            .iter()
            .map(|u| UnmappedRow {
                capability: u.capability.clone(),
                candidates: u.candidates.clone(),
                available: u.available.clone(),
                explanation: u.explain(),
            })
            .collect(),
        server_contacted,
    }
}

/// A target whose server has not answered.
///
/// Not "everything is unmapped". Nobody has asked the server what it has, and
/// saying four capabilities are unmapped would send somebody writing overrides
/// for tools that are probably there.
pub fn unknown_target_panel(target: &str, server: &str) -> TargetPanel {
    TargetPanel {
        target: target.to_owned(),
        server: server.to_owned(),
        bound: Vec::new(),
        unmapped: Vec::new(),
        server_contacted: false,
    }
}

/// Build a client for one configured server (§13.1's `[mcp_servers.*]`).
///
/// A server whose transport is unknown, or which is missing the field its
/// transport needs, is reported rather than skipped: a settings screen that
/// silently omitted a configured server would be the screen somebody stares at
/// wondering where it went.
fn client_for(id: &str, settings: &McpServerSettings) -> Result<revlocal_mcp::McpClient, String> {
    match settings.transport.as_str() {
        "stdio" => {
            let command = settings
                .command
                .as_deref()
                .ok_or_else(|| format!("server `{id}` is stdio and has no `command`"))?;
            let args: Vec<&str> = settings.args.iter().map(String::as_str).collect();
            Ok(
                revlocal_mcp::StdioClient::new(revlocal_mcp::ServerCommand::new(
                    id, command, &args,
                ))
                .into(),
            )
        }
        "http" => {
            let url = settings
                .url
                .as_deref()
                .ok_or_else(|| format!("server `{id}` is http and has no `url`"))?;
            let mut endpoint = revlocal_mcp::HttpEndpoint::new(id, url);
            for (name, value) in &settings.headers {
                endpoint = endpoint.with_header(name, value.clone());
            }
            revlocal_mcp::HttpClient::new(endpoint)
                .map(Into::into)
                .map_err(|error| format!("server `{id}`: {error}"))
        }
        other => Err(format!(
            "server `{id}` has transport `{other}`; §13.1 knows `stdio` and `http`"
        )),
    }
}

/// Assemble §15's settings screen.
///
/// Contacts every configured server once. That is a real cost and it is the
/// point: the mapping table is only worth showing if it reflects what the servers
/// actually expose today, and a table drawn from config alone would show a
/// capability as bound because somebody once wrote its name down.
///
/// `overrides` are applied before resolution is read, so a manually-bound
/// capability shows as bound and says a person did it (ADR 0015).
pub async fn gather(
    config: &GlobalConfig,
    config_path: &str,
    overrides_path: &str,
    doctor: DoctorReport,
) -> SettingsView {
    let overrides = Overrides::load(std::path::Path::new(overrides_path)).unwrap_or_default();

    let mut discovery = Discovery::new();
    let mut build_errors: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for (id, settings) in &config.mcp_servers {
        match client_for(id, settings) {
            Ok(client) => discovery.insert(client),
            Err(detail) => {
                build_errors.insert(id.clone(), detail);
            }
        }
    }

    // Tools first, per server, so each target's mapping is resolved against a
    // list that was actually fetched rather than assumed.
    let mut tools_by_server: std::collections::BTreeMap<String, Vec<revlocal_mcp::Tool>> =
        std::collections::BTreeMap::new();
    let mut state_by_server: std::collections::BTreeMap<String, revlocal_mcp::ServerState> =
        std::collections::BTreeMap::new();

    for id in config.mcp_servers.keys() {
        if let Some(detail) = build_errors.get(id) {
            state_by_server.insert(
                id.clone(),
                revlocal_mcp::ServerState::Unreachable {
                    reason: detail.clone(),
                    // A misconfigured server is not worth retrying: nothing about
                    // trying again changes what the file says.
                    retryable: Some(false),
                },
            );
            continue;
        }

        match discovery.tools(id, &NoSecrets).await {
            Some(Ok(tools)) => {
                let tools = tools.to_vec();
                state_by_server.insert(
                    id.clone(),
                    revlocal_mcp::ServerState::Reachable { tools: tools.len() },
                );
                tools_by_server.insert(id.clone(), tools);
            }
            Some(Err(error)) => {
                state_by_server.insert(
                    id.clone(),
                    revlocal_mcp::ServerState::Unreachable {
                        reason: error.to_string(),
                        retryable: error.retryable(),
                    },
                );
            }
            None => {
                state_by_server.insert(id.clone(), revlocal_mcp::ServerState::Unknown);
            }
        }
    }

    // Targets: config's own tables first, then the built-in profiles for any
    // target §11 ships and the config did not override.
    let mut specs: Vec<revlocal_mcp::TargetSpec> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for (id, table) in &config.targets {
        match revlocal_mcp::TargetSpec::from_toml(id, table) {
            Ok(spec) => specs.push(spec),
            // Reported on the screen rather than swallowed: a target that will not
            // parse is one whose publishes are silently not happening.
            Err(error) => errors.push(error.to_string()),
        }
    }
    if !specs.iter().any(|s| s.id == "andare") {
        if let Some(spec) = revlocal_mcp::builtin_target("andare") {
            specs.push(spec);
        }
    }

    let mut targets = Vec::new();
    let mut mapped_counts: std::collections::BTreeMap<String, (usize, usize)> =
        std::collections::BTreeMap::new();
    for spec in &specs {
        let contacted = matches!(
            state_by_server.get(&spec.mcp_server),
            Some(revlocal_mcp::ServerState::Reachable { .. })
        );

        if !contacted {
            targets.push(unknown_target_panel(&spec.id, &spec.mcp_server));
            continue;
        }

        let tools = tools_by_server
            .get(&spec.mcp_server)
            .map_or(&[][..], Vec::as_slice);
        let mut mapping = revlocal_mcp::resolve(spec, tools);
        overrides.apply(&mut mapping, tools);

        let entry = mapped_counts
            .entry(spec.mcp_server.clone())
            .or_insert((0, 0));
        entry.0 += mapping.bound.len();
        entry.1 += mapping.unmapped.len();

        targets.push(target_panel(&mapping, true));
    }

    let servers = config
        .mcp_servers
        .iter()
        .map(|(id, settings)| {
            let state = state_by_server
                .get(id)
                .cloned()
                .unwrap_or(revlocal_mcp::ServerState::Unknown);
            let (mapped, unmapped) = mapped_counts.get(id).copied().unwrap_or((0, 0));
            let names: Vec<String> = tools_by_server
                .get(id)
                .map(|tools| tools.iter().map(|t| t.name.clone()).collect())
                .unwrap_or_default();
            server_panel(id, settings, &state, mapped, unmapped, &names)
        })
        .collect();

    SettingsView {
        doctor,
        servers,
        targets,
        limits: Limits::of(config),
        config_path: config_path.to_owned(),
        overrides_path: overrides_path.to_owned(),
        target_errors: errors,
    }
}

/// Record a manual binding, checked against the server's own schema first.
///
/// RL-605's rule: an override is refused *here* rather than at the first publish
/// that needed it. A typo'd tool name discovered at dispatch time is a review that
/// silently did not publish.
pub async fn set_override(
    config: &GlobalConfig,
    overrides_path: &str,
    target: &str,
    capability: &str,
    tool: &str,
    args: serde_json::Value,
) -> Result<(), SettingsError> {
    let spec = config
        .targets
        .get(target)
        .map(|table| revlocal_mcp::TargetSpec::from_toml(target, table))
        .transpose()
        .map_err(|error| SettingsError::Spec {
            detail: error.to_string(),
        })?
        .or_else(|| revlocal_mcp::builtin_target(target))
        .ok_or_else(|| SettingsError::Spec {
            detail: format!("no target `{target}` is configured"),
        })?;

    let settings = config
        .mcp_servers
        .get(&spec.mcp_server)
        .ok_or_else(|| SettingsError::Spec {
            detail: format!("no MCP server `{}` is configured", spec.mcp_server),
        })?;

    let client =
        client_for(&spec.mcp_server, settings).map_err(|detail| SettingsError::Spec { detail })?;
    let mut discovery = Discovery::new();
    discovery.insert(client);

    let tools = match discovery.tools(&spec.mcp_server, &NoSecrets).await {
        Some(Ok(tools)) => tools.to_vec(),
        Some(Err(error)) => {
            return Err(SettingsError::Override {
                detail: error.to_string(),
            })
        }
        None => {
            return Err(SettingsError::Spec {
                detail: format!("no MCP server `{}` is configured", spec.mcp_server),
            })
        }
    };

    let entry = Override {
        target: target.to_owned(),
        capability: capability.to_owned(),
        tool: tool.to_owned(),
        args,
    };
    entry
        .check_against(&tools)
        .map_err(|error| SettingsError::Override {
            detail: error.to_string(),
        })?;

    let path = std::path::Path::new(overrides_path);
    let mut overrides = Overrides::load(path).unwrap_or_default();
    overrides.set(entry);
    overrides
        .save(path)
        .map_err(|error| SettingsError::Override {
            detail: error.to_string(),
        })
}

/// Remove a manual binding. Resolution takes over again on the next read.
pub fn clear_override(
    overrides_path: &str,
    target: &str,
    capability: &str,
) -> Result<bool, SettingsError> {
    let path = std::path::Path::new(overrides_path);
    let mut overrides = Overrides::load(path).unwrap_or_default();
    let removed = overrides.clear(target, capability);
    overrides
        .save(path)
        .map_err(|error| SettingsError::Override {
            detail: error.to_string(),
        })?;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http_server(header_value: &str) -> McpServerSettings {
        let mut headers = std::collections::BTreeMap::new();
        headers.insert("Authorization".to_owned(), SecretRef::parse(header_value));
        McpServerSettings {
            transport: "http".to_owned(),
            url: Some("https://andare.example/mcp".to_owned()),
            headers,
            ..McpServerSettings::default()
        }
    }

    #[test]
    fn settings_a_literal_secret_never_reaches_the_screen() {
        // The criterion, tested where it can actually fail: not "is it starred in
        // the UI" but "is it in the JSON at all". A redacted-on-render secret has
        // already travelled, and every future renderer has to remember.
        let settings = http_server("hunter2-the-real-token");

        let panel = server_panel(
            "andare",
            &settings,
            &revlocal_mcp::ServerState::Reachable { tools: 3 },
            2,
            1,
            &["create_issue".to_owned()],
        );

        let json = serde_json::to_string(&panel).expect("serialises");
        assert!(
            !json.contains("hunter2"),
            "the secret crossed the boundary: {json}"
        );
        // Presence is reported, and what to do about it.
        assert_eq!(panel.secrets.len(), 1);
        assert_eq!(panel.secrets[0].source, "literal");
        assert!(panel.secrets[0].advice.is_some());
    }

    #[test]
    fn settings_a_keychain_reference_shows_its_name() {
        // The name is not a secret and is the useful half: it points at an entry
        // somebody can go and check.
        let settings = http_server("{{keychain:andare-token}}");

        let panel = server_panel(
            "andare",
            &settings,
            &revlocal_mcp::ServerState::Reachable { tools: 3 },
            2,
            1,
            &[],
        );

        assert_eq!(panel.secrets[0].source, "keychain");
        assert_eq!(
            panel.secrets[0].keychain_entry.as_deref(),
            Some("andare-token")
        );
        // Nothing to advise: this is the arrangement being recommended.
        assert!(panel.secrets[0].advice.is_none());
    }

    #[test]
    fn settings_an_unreachable_server_says_why_rather_than_zero_tools() {
        // RL-603's distinction, carried to the screen. Zero tools is a server
        // that answered and had none — a different problem with a different fix.
        let panel = server_panel(
            "andare",
            &http_server("{{keychain:t}}"),
            &revlocal_mcp::ServerState::Unreachable {
                reason: "connection refused".to_owned(),
                retryable: Some(true),
            },
            0,
            0,
            &[],
        );

        assert!(panel.contacted);
        assert_eq!(panel.error.as_deref(), Some("connection refused"));
        assert!(panel.summary.contains("unreachable"));
        assert!(!panel.summary.contains("0 tools"));
    }

    #[test]
    fn settings_a_server_never_contacted_is_not_reported_as_unmapped() {
        // The failure this avoids: four unmapped capabilities shown for a server
        // nobody has spoken to, sending somebody to write overrides for tools
        // that are probably there.
        let panel = unknown_target_panel("andare", "andare");

        assert!(!panel.server_contacted);
        assert!(!panel.has_unmapped());
        assert_eq!(panel.unmapped.len(), 0);
    }

    #[test]
    fn settings_an_unmapped_capability_carries_what_a_fix_needs() {
        // §11.2: reported, never guessed — and the report has to be enough to act
        // on without asking the server again, which is §15's "fix affordance".
        let mapping = revlocal_mcp::TargetMapping {
            target: "andare".to_owned(),
            server: "andare".to_owned(),
            bound: Vec::new(),
            unmapped: vec![revlocal_mcp::Unmapped {
                capability: "comment".to_owned(),
                candidates: vec!["comment_on_issue".to_owned(), "add_comment".to_owned()],
                available: vec!["create_work_item".to_owned(), "transition_issue".to_owned()],
            }],
        };

        let panel = target_panel(&mapping, true);

        assert!(panel.has_unmapped());
        let row = &panel.unmapped[0];
        // Both lists: what was wanted, and what there is to choose from.
        assert_eq!(row.candidates.len(), 2);
        assert_eq!(row.available.len(), 2);
        assert!(row.explanation.contains("comment"));
    }

    #[test]
    fn settings_a_manual_binding_is_distinguishable_from_a_resolved_one() {
        // ADR 0015: "you told us to" and "we worked it out" are different answers
        // to the question this table exists to answer. A table that showed them
        // alike would make an override impossible to find again.
        let mapping = revlocal_mcp::TargetMapping {
            target: "andare".to_owned(),
            server: "andare".to_owned(),
            bound: vec![
                revlocal_mcp::Binding {
                    capability: "create_issue".to_owned(),
                    tool: "create_work_item".to_owned(),
                    candidate_index: 1,
                    schema: serde_json::json!({}),
                    args: serde_json::json!({}),
                    from_override: false,
                },
                revlocal_mcp::Binding {
                    capability: "comment".to_owned(),
                    tool: "leave_note".to_owned(),
                    candidate_index: 0,
                    schema: serde_json::json!({}),
                    args: serde_json::json!({}),
                    from_override: true,
                },
            ],
            unmapped: Vec::new(),
        };

        let panel = target_panel(&mapping, true);

        assert!(!panel.bound[0].from_override);
        assert!(panel.bound[1].from_override);
    }

    #[test]
    fn settings_a_stdio_server_shows_the_command_it_runs() {
        let settings = McpServerSettings {
            transport: "stdio".to_owned(),
            command: Some("node".to_owned()),
            args: vec!["server.js".to_owned(), "--profile".to_owned()],
            ..McpServerSettings::default()
        };

        let panel = server_panel(
            "mock",
            &settings,
            &revlocal_mcp::ServerState::Reachable { tools: 1 },
            1,
            0,
            &["ping".to_owned()],
        );

        assert_eq!(panel.endpoint, "node server.js --profile");
        assert!(panel.secrets.is_empty());
    }

    #[test]
    fn settings_the_unmapped_count_is_across_every_target() {
        // §15's criterion is that an unmapped capability is *visible*, and a count
        // somebody has to assemble by reading four panels is not.
        let view = SettingsView {
            doctor: DoctorReport::default(),
            servers: Vec::new(),
            targets: vec![
                TargetPanel {
                    target: "andare".to_owned(),
                    server: "andare".to_owned(),
                    bound: Vec::new(),
                    unmapped: vec![UnmappedRow {
                        capability: "comment".to_owned(),
                        candidates: Vec::new(),
                        available: Vec::new(),
                        explanation: String::new(),
                    }],
                    server_contacted: true,
                },
                unknown_target_panel("trama", "trama"),
            ],
            limits: Limits::of(&GlobalConfig::default()),
            config_path: "/tmp/config.toml".to_owned(),
            overrides_path: "/tmp/target-overrides.json".to_owned(),
            target_errors: Vec::new(),
        };

        assert_eq!(view.unmapped_count(), 1);
    }

    #[test]
    fn settings_limits_come_from_the_config_document() {
        let limits = Limits::of(&GlobalConfig::default());

        // §13.1's document, so a fresh install shows what the spec says.
        assert_eq!(limits.daily_runs_per_repo, 200);
        assert_eq!(limits.daily_tokens_per_repo, 2_000_000);
        assert_eq!(limits.transcript_retention_days, 30);
        // Never a silent drop when an allowance runs out.
        assert_eq!(limits.on_exhausted, "pause");
    }
}
