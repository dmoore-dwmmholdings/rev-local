//! Building the publish targets a config asks for (SPEC §11.2, §11.5, §13.1).
//!
//! # Two of three destinations were reachable only by clicking
//!
//! `AndareTarget::new` and `TramaTarget::new` had exactly one caller in the
//! workspace between them, and it was the desktop binary. `GitHubTarget::new` had
//! none outside its own tests. So `revlocal watch` — the headless daemon, the
//! thing that goes on a timer or a launchd job — reviewed, recorded findings,
//! queued publish actions, and could not route a single one of them anywhere but
//! a local markdown file. The autopilot even had a line for it: "N action(s) name
//! a target that is not configured, so nothing was sent" (REVL-223).
//!
//! Wiring lives here rather than in either surface because the defect was
//! duplication in the first place: the app grew its own two builders, the CLI
//! grew none, and "it publishes to Andare" was true of one binary and false of
//! the other. One builder means a destination reachable from the app is reachable
//! from a timer, by construction.
//!
//! # What is *not* built here
//!
//! The local report target. It needs no configuration and is registered by
//! whoever owns the queue — `autopilot::tick` does it for every pass — so
//! including it here would register it twice and make "which targets did the
//! config ask for?" unanswerable.

use std::sync::Arc;

use revlocal_core::{GlobalConfig, McpServerSettings};
use revlocal_mcp::{HttpClient, HttpEndpoint, MacKeychain, McpClient, ServerCommand, StdioClient};

use crate::{
    gh::GhWriter, AndareTarget, AndareToolNames, GitHubTarget, McpAndareWriter, McpTramaWriter,
    PublishTarget, TramaTarget, TramaToolNames,
};

/// A destination that could not be built, and the reason a person can act on.
///
/// Kept beside the targets rather than logged and dropped: §18 forbids a silent
/// cap, and "Andare is not configured" is the single most likely explanation for
/// a queue that never drains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unavailable {
    /// Which destination — `andare`, `trama`, `github`.
    pub target: String,
    /// Why it could not be built, phrased as something to do about it.
    pub reason: String,
}

/// The destinations this process can deliver to, and the ones it cannot.
#[derive(Default)]
pub struct TargetSet {
    /// Built and ready to register on a queue.
    pub targets: Vec<Arc<dyn PublishTarget>>,
    /// Named by the config or by a repository, and not built.
    pub unavailable: Vec<Unavailable>,
}

/// Hand-written: `dyn PublishTarget` is not `Debug`, and the useful thing to
/// print is which destinations are wired, not their innards.
impl std::fmt::Debug for TargetSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TargetSet")
            .field("targets", &self.ids().collect::<Vec<_>>())
            .field("unavailable", &self.unavailable)
            .finish()
    }
}

impl TargetSet {
    /// The ids that were built, in registration order.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.targets.iter().map(|target| target.id())
    }

    /// Whether a destination was built.
    pub fn has(&self, id: &str) -> bool {
        self.ids().any(|built| built == id)
    }

    /// Why `id` is not deliverable, if it is not.
    ///
    /// This is what turns the queue's `unroutable` count into a sentence: the
    /// count knows an action had no target, and only the config knows why.
    pub fn reason(&self, id: &str) -> Option<&str> {
        self.unavailable
            .iter()
            .find(|entry| entry.target == id)
            .map(|entry| entry.reason.as_str())
    }

    /// One line per destination that is not deliverable.
    pub fn notes(&self) -> Vec<String> {
        self.unavailable
            .iter()
            .map(|entry| format!("{}: {}", entry.target, entry.reason))
            .collect()
    }
}

/// Build every destination the config makes buildable.
///
/// Nothing here connects, spawns or reads a secret: an `HttpEndpoint` holds
/// deferred `{{keychain:…}}` references and a `StdioClient` does not launch its
/// child until something calls a tool. A config error is therefore reported by
/// this function, and a *reachability* problem is reported by the delivery that
/// hit it — which are different questions with different remedies.
pub fn from_config(config: &GlobalConfig) -> TargetSet {
    let mut set = TargetSet::default();

    // GitHub needs nothing from config.toml. Its credentials are `gh`'s own, and
    // which repository to file against comes from the repository's remote, so a
    // repository that names `github` in its targets is fully specified without a
    // `[mcpServers]` entry. Registering it unconditionally is what makes the
    // action a repository already queues actually deliverable.
    //
    // Deliberately *not* checked for here, though it would put a nicer sentence
    // in `unavailable`: whether `gh` is on PATH. Answering that means spawning a
    // process, and the promise this function makes — that building targets
    // contacts nothing — is what lets it run in the inner loop and lets a caller
    // build targets it may never use. `GhCli::run` already gives a missing `gh`
    // its own sentence ("install the GitHub CLI and run `gh auth login`") on the
    // action's own row, and the retry ladder bounds how often it says it. That is
    // a better answer than the one an unrouted action used to get, which told
    // everybody to add a suite bearer in Settings whatever the actual cause.
    set.targets
        .push(Arc::new(GitHubTarget::new(GhWriter::new())) as Arc<dyn PublishTarget>);

    match mcp_client(config, "andare") {
        Ok(client) => set
            .targets
            .push(Arc::new(AndareTarget::new(McpAndareWriter::new(
                client,
                Arc::new(MacKeychain),
                AndareToolNames::default(),
            )))),
        Err(reason) => set.unavailable.push(Unavailable {
            target: "andare".to_owned(),
            reason,
        }),
    }

    match mcp_client(config, "trama") {
        Ok(client) => set
            .targets
            .push(Arc::new(TramaTarget::new(McpTramaWriter::new(
                client,
                Arc::new(MacKeychain),
                TramaToolNames::default(),
            )))),
        Err(reason) => set.unavailable.push(Unavailable {
            target: "trama".to_owned(),
            reason,
        }),
    }

    set
}

/// Build the MCP client for one `[mcpServers.<id>]` entry.
fn mcp_client(config: &GlobalConfig, id: &str) -> Result<McpClient, String> {
    let settings = config.mcp_servers.get(id).ok_or_else(|| {
        format!(
            "no [mcpServers.{id}] in your config, so nothing can be filed there\n  \
             try: add the server and its bearer, then `revlocal targets list`"
        )
    })?;
    client_for(id, settings)
}

/// Turn one server entry into a client, without connecting to it.
fn client_for(id: &str, settings: &McpServerSettings) -> Result<McpClient, String> {
    match settings.transport.as_str() {
        // Supported here and refused by the desktop app's own wiring, which took
        // http only. A stdio server is how the suite is run locally, and a daemon
        // that could not speak to one would have sent the person who configured
        // it back to the GUI.
        "stdio" => {
            let command = settings.command.as_deref().filter(|c| !c.is_empty()).ok_or_else(|| {
                format!("mcpServers.{id} is a stdio server with no `command`\n  try: add `command = \"…\"`")
            })?;
            let args: Vec<&str> = settings.args.iter().map(String::as_str).collect();
            Ok(McpClient::from(StdioClient::new(ServerCommand::new(
                id, command, &args,
            ))))
        }
        "http" => {
            let url = settings.url.as_deref().filter(|u| !u.is_empty()).ok_or_else(|| {
                format!("mcpServers.{id} is an http server with no `url`\n  try: add `url = \"https://…\"`")
            })?;
            let mut endpoint = HttpEndpoint::new(id, url);
            for (name, value) in &settings.headers {
                endpoint = endpoint.with_header(name, value.clone());
            }
            HttpClient::new(endpoint)
                .map(McpClient::from)
                .map_err(|error| format!("mcpServers.{id} could not be prepared: {error}"))
        }
        other => Err(format!(
            "mcpServers.{id} has type `{other}`\n  try: `stdio` or `http`"
        )),
    }
}
