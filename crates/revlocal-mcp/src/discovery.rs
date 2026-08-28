//! Tool discovery cache and server health (RL-603, SPEC §11.2).
//!
//! §11.2 says the discovered tool list is cached with its input schemas and shown
//! in the UI — "Andare: 14 tools, 5 capabilities mapped". Two things follow from
//! that sentence that are easy to miss.
//!
//! # The cache is keyed on the connection, not on time
//!
//! A tool list describes *one server process*. Reconnecting may reach a different
//! build of the server with different tools, so a cache that survives a reconnect
//! is a cache that can hand out names the server no longer has — and the failure
//! arrives later, as an unknown-tool error from a capability that was bound at
//! startup and looked fine.
//!
//! So the cache records which connection generation it was read from, and a
//! generation mismatch is a miss. No TTL: time is not what invalidates this, and a
//! TTL would be both wrong (a server can be replaced inside the window) and
//! wasteful (a server that never restarts would be re-listed forever).
//!
//! # Health is per server, and one server's failure is its own
//!
//! rev-local publishes to several targets. An unreachable Trama must not stop
//! findings reaching GitHub, so [`Discovery::refresh_all`] returns a report rather
//! than a `Result`: every server is attempted, and a failure is recorded against
//! that server. There is no aggregate error to short-circuit on, because there is
//! no aggregate action to abandon.

use std::collections::BTreeMap;

use crate::http::{HttpClient, HttpError, SecretResolver};
use crate::protocol::Tool;
use crate::stdio::{McpError, StdioClient};

/// One MCP server, whichever transport it speaks.
///
/// An enum rather than a trait object: §11.2 fixes the transport set at stdio and
/// streamable HTTP, and a closed set is better described by an enum than by a
/// `dyn` that implies servers could speak something else. It also keeps the two
/// clients' distinct error types intact rather than erasing them behind a boxed
/// trait.
///
/// **Both** variants are boxed, not just the larger one. `StdioClient` holds a
/// child process and its pipes — 672 bytes on Windows against `HttpClient`'s 312,
/// enough of a gap for clippy's `large_enum_variant`. Boxing only the bigger one
/// inverts the imbalance rather than fixing it: 8 bytes against 312 trips the same
/// lint from the other side. Two boxes make the enum a pointer either way.
///
/// The gap is platform-dependent — the Windows leg caught this while Linux stayed
/// quiet — which is a reason to read CI's clippy output rather than only the local
/// one.
pub enum McpClient {
    /// A server spawned as a child process.
    ///
    Stdio(Box<StdioClient>),
    /// A server reached over HTTP.
    Http(Box<HttpClient>),
}

/// Written by hand rather than derived, for the reason `HttpClient`'s own `Debug`
/// is: an endpoint holds `SecretRef`s and a stdio client holds a live child
/// process, and neither belongs in a diagnostic dump. Identity and connection
/// state are what a log needs to be useful here.
impl std::fmt::Debug for McpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpClient")
            .field(
                "transport",
                &match self {
                    Self::Stdio(_) => "stdio",
                    Self::Http(_) => "http",
                },
            )
            .field("id", &self.id())
            .field("connected", &self.is_connected())
            .field("connects", &self.connect_count())
            .finish()
    }
}

impl From<StdioClient> for McpClient {
    fn from(client: StdioClient) -> Self {
        Self::Stdio(Box::new(client))
    }
}

impl From<HttpClient> for McpClient {
    fn from(client: HttpClient) -> Self {
        Self::Http(Box::new(client))
    }
}

impl McpClient {
    /// Which server this is.
    pub fn id(&self) -> &str {
        match self {
            Self::Stdio(c) => c.id(),
            Self::Http(c) => c.id(),
        }
    }

    /// How many times a connection has been established, including reconnects.
    ///
    /// This is the cache's generation counter. It is deliberately the client's own
    /// count rather than something this module maintains: the client is the only
    /// thing that knows a reconnect happened, and a second counter would be one
    /// missed increment away from serving a stale tool list.
    pub const fn connect_count(&self) -> u64 {
        match self {
            Self::Stdio(c) => c.connect_count(),
            Self::Http(c) => c.connect_count(),
        }
    }

    /// Whether a connection is currently held.
    pub const fn is_connected(&self) -> bool {
        match self {
            Self::Stdio(c) => c.is_connected(),
            Self::Http(c) => c.is_connected(),
        }
    }

    /// Ask the server for its tools, connecting first if necessary.
    pub async fn list_tools(
        &mut self,
        resolver: &dyn SecretResolver,
    ) -> Result<Vec<Tool>, DiscoveryError> {
        match self {
            Self::Stdio(c) => c.list_tools().await.map_err(DiscoveryError::from),
            Self::Http(c) => c.list_tools(resolver).await.map_err(DiscoveryError::from),
        }
    }
}

/// A discovery failure, from either transport.
///
/// The two transports fail in genuinely different ways — a binary that will not
/// spawn has no HTTP equivalent, and a 401 has no stdio equivalent — so the
/// variants stay whole rather than being flattened into one string. Callers that
/// only need to decide what to do next use [`Self::retryable`].
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    /// A stdio server failed.
    #[error(transparent)]
    Stdio(#[from] McpError),

    /// An HTTP server failed.
    #[error(transparent)]
    Http(#[from] HttpError),
}

impl DiscoveryError {
    /// Whether this is worth retrying, where the failure said so (§11.6).
    ///
    /// `None` where nothing said. Same rule as both transports: a guess here would
    /// turn a permanent failure into a slow one.
    pub fn retryable(&self) -> Option<bool> {
        match self {
            Self::Stdio(e) => e.retryable(),
            Self::Http(e) => e.retryable(),
        }
    }

    /// Whether the connection is gone and the next call should reconnect.
    pub fn is_transport(&self) -> bool {
        match self {
            Self::Stdio(e) => e.is_transport(),
            Self::Http(e) => e.is_transport(),
        }
    }
}

/// What is known about one server, for the UI and for `revlocal doctor`.
///
/// `mapped` and `unmapped` are counts of *capabilities*, not tools, and they are
/// filled in by the capability mapper rather than by discovery — discovery knows
/// how many tools a server has, and nothing about what rev-local wanted to do with
/// them. Until a mapper has run they are zero, which is honest: nothing has been
/// bound yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerHealth {
    /// Which server.
    pub id: String,
    /// Whether it answered, and with what.
    pub state: ServerState,
    /// Capabilities successfully bound to a tool on this server.
    pub mapped: usize,
    /// Capabilities that could not be bound (§11.2: reported, never guessed).
    pub unmapped: usize,
}

/// Whether a server answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerState {
    /// Never contacted.
    Unknown,
    /// Answered `tools/list`.
    Reachable {
        /// How many tools it reported.
        tools: usize,
    },
    /// Did not answer.
    Unreachable {
        /// What went wrong, rendered — the error itself is not `Clone`.
        reason: String,
        /// Whether it is worth trying again, where the failure said (§11.6).
        retryable: Option<bool>,
    },
}

impl ServerHealth {
    /// The line `revlocal doctor` prints for this server.
    ///
    /// Format is fixed by RL-603's criterion: `server: N tools, M capabilities
    /// mapped, K unmapped`. An unreachable server says so instead of printing
    /// `0 tools`, because zero tools is a server that answered and had none — a
    /// different problem with a different remedy.
    pub fn summary_line(&self) -> String {
        match &self.state {
            ServerState::Unknown => format!("{}: not contacted", self.id),
            ServerState::Reachable { tools } => format!(
                "{}: {} tools, {} capabilities mapped, {} unmapped",
                self.id, tools, self.mapped, self.unmapped
            ),
            ServerState::Unreachable { reason, .. } => {
                format!("{}: unreachable — {reason}", self.id)
            }
        }
    }

    /// Whether this server answered.
    pub const fn is_reachable(&self) -> bool {
        matches!(self.state, ServerState::Reachable { .. })
    }
}

/// Health for every configured server, in a stable order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HealthReport {
    /// One entry per configured server, ordered by id.
    pub servers: Vec<ServerHealth>,
}

impl HealthReport {
    /// One line per server, in id order.
    pub fn lines(&self) -> Vec<String> {
        self.servers
            .iter()
            .map(ServerHealth::summary_line)
            .collect()
    }

    /// Whether every server answered.
    ///
    /// Not used to gate publishing — §11.2's whole point is that one dead server
    /// degrades one target — but `doctor` needs to exit non-zero for *something*.
    pub fn all_reachable(&self) -> bool {
        self.servers.iter().all(ServerHealth::is_reachable)
    }

    /// The servers that did not answer.
    pub fn unreachable(&self) -> impl Iterator<Item = &ServerHealth> {
        self.servers.iter().filter(|s| !s.is_reachable())
    }
}

/// Whether this entry's cached tool list still describes the connection in hand.
///
/// Two conditions, and the second is the one that is easy to leave out: the
/// generation must match **and** the client must still be connected. A client that
/// has been shut down has not incremented its connect count yet — it does that on
/// the way back up — so a generation check alone would call a list read from a
/// closed connection fresh, and serve it right up until the reconnect that was
/// supposed to invalidate it.
fn is_fresh(entry: &Entry) -> bool {
    entry.client.is_connected()
        && entry
            .cached
            .as_ref()
            .is_some_and(|c| c.generation == entry.client.connect_count())
}

/// A tool list, and the connection generation it was read from.
#[derive(Debug)]
struct Cached {
    tools: Vec<Tool>,
    generation: u64,
}

/// One server and everything discovery knows about it.
#[derive(Debug)]
struct Entry {
    client: McpClient,
    cached: Option<Cached>,
    mapped: usize,
    unmapped: usize,
    /// The last failure, kept so health can report *why* rather than just "no".
    last_error: Option<(String, Option<bool>)>,
}

/// Every configured MCP server, with their cached tool lists.
///
/// Ordered by server id so `doctor` output and the UI's server list do not shuffle
/// between runs — the same reason the pipeline sorts findings.
#[derive(Debug, Default)]
pub struct Discovery {
    servers: BTreeMap<String, Entry>,
}

impl Discovery {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a server. Replaces any server already registered under the same id.
    pub fn insert(&mut self, client: impl Into<McpClient>) {
        let client = client.into();
        let id = client.id().to_owned();
        self.servers.insert(
            id,
            Entry {
                client,
                cached: None,
                mapped: 0,
                unmapped: 0,
                last_error: None,
            },
        );
    }

    /// The configured server ids, in order.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.servers.keys().map(String::as_str)
    }

    /// How many servers are configured.
    pub fn len(&self) -> usize {
        self.servers.len()
    }

    /// Whether no servers are configured.
    pub fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }

    /// This server's tools, from the cache when it is still valid.
    ///
    /// A cache entry read from an earlier connection is a miss, not a hit — see the
    /// module docs. `None` for a server that was never registered, which is a
    /// caller bug rather than a runtime condition and so is not an error variant.
    pub async fn tools(
        &mut self,
        id: &str,
        resolver: &dyn SecretResolver,
    ) -> Option<Result<&[Tool], DiscoveryError>> {
        let entry = self.servers.get_mut(id)?;

        let fresh = is_fresh(entry);

        if !fresh {
            match entry.client.list_tools(resolver).await {
                Ok(tools) => {
                    entry.cached = Some(Cached {
                        tools,
                        // Read *after* the call: the generation that produced this
                        // list is the one the client holds now, not the one it held
                        // before it reconnected to answer.
                        generation: entry.client.connect_count(),
                    });
                    entry.last_error = None;
                }
                Err(error) => {
                    entry.last_error = Some((error.to_string(), error.retryable()));
                    // A failed refresh drops the old list rather than serving it.
                    // Stale names bound to capabilities is the failure this module
                    // exists to prevent.
                    entry.cached = None;
                    return Some(Err(error));
                }
            }
        }

        Some(Ok(entry
            .cached
            .as_ref()
            .map_or(&[][..], |c| c.tools.as_slice())))
    }

    /// Whether this server's cached tool list is still valid for the current
    /// connection.
    ///
    /// Exposed for tests and for the UI's "refresh" affordance; callers reaching
    /// for tools should use [`Self::tools`], which checks this itself.
    pub fn is_cached(&self, id: &str) -> bool {
        self.servers.get(id).is_some_and(is_fresh)
    }

    /// Close one server's connection. The next ask reconnects and re-lists.
    ///
    /// The cached list is deliberately **not** cleared here. Freshness is decided
    /// by [`is_fresh`], and leaving the entry in place means the invalidation rule
    /// is what a test exercises — clearing it by hand would make the rule look
    /// correct without ever running it.
    pub async fn shutdown(&mut self, id: &str) {
        if let Some(entry) = self.servers.get_mut(id) {
            match &mut entry.client {
                McpClient::Stdio(c) => c.shutdown().await,
                McpClient::Http(c) => c.disconnect(),
            }
        }
    }

    /// Record what the capability mapper bound on this server (RL-604).
    ///
    /// Discovery owns the health line, the mapper owns the capability counts, and
    /// neither should have to know the other's internals to produce one sentence.
    pub fn set_capability_counts(&mut self, id: &str, mapped: usize, unmapped: usize) {
        if let Some(entry) = self.servers.get_mut(id) {
            entry.mapped = mapped;
            entry.unmapped = unmapped;
        }
    }

    /// Contact every server, and report on each.
    ///
    /// Never fails as a whole: an unreachable server is recorded against itself and
    /// the rest are still contacted. That is the criterion — one dead target must
    /// not take the others with it.
    pub async fn refresh_all(&mut self, resolver: &dyn SecretResolver) -> HealthReport {
        let ids: Vec<String> = self.servers.keys().cloned().collect();
        for id in ids {
            // The result is recorded on the entry by `tools`; the error is
            // deliberately not propagated.
            let _ = self.tools(&id, resolver).await;
        }
        self.health()
    }

    /// What is currently known, without contacting anything.
    pub fn health(&self) -> HealthReport {
        let servers = self
            .servers
            .values()
            .map(|entry| {
                let state = match (&entry.cached, &entry.last_error) {
                    (Some(cached), _) if is_fresh(entry) => ServerState::Reachable {
                        tools: cached.tools.len(),
                    },
                    (_, Some((reason, retryable))) => ServerState::Unreachable {
                        reason: reason.clone(),
                        retryable: *retryable,
                    },
                    _ => ServerState::Unknown,
                };
                ServerHealth {
                    id: entry.client.id().to_owned(),
                    state,
                    mapped: entry.mapped,
                    unmapped: entry.unmapped,
                }
            })
            .collect();

        HealthReport { servers }
    }
}
