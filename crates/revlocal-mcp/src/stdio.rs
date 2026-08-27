//! MCP over stdio: spawn a server, initialize, list tools, call them (SPEC §11.2).
//!
//! # The connection is disposable, and that is the design
//!
//! An MCP server is a third-party process rev-local did not write and cannot fix.
//! It may be absent, it may crash mid-call, and it may crash *on startup* every
//! time. So [`StdioClient`] holds **at most one** connection and treats it as
//! disposable: any transport failure drops it, and the next call spawns a fresh one.
//!
//! **Reconnection happens on next use, never eagerly.** A client that reconnected
//! the moment a connection died would, against a server that crashes on startup,
//! spin — spawning processes as fast as the OS allows, forever, while reporting
//! nothing. Waiting for a caller who wants something bounds the retry rate to the
//! rate of real work, which is the only bound that holds for a failure we cannot
//! diagnose.
//!
//! # Two failure modes that must not be collapsed
//!
//! A **protocol** error means the call did not happen: no such tool, bad params. A
//! **tool** error means it happened and the tool refused — Trama declining to
//! overwrite a page nobody read (§11.5) is the important instance. They need
//! opposite responses from a caller, so [`McpError`] keeps them apart and
//! `call_tool` returns tool errors as an `Ok` [`ToolResult`] with `is_error` set.
//!
//! # Reaping
//!
//! A leaked server process outlives the daemon and holds whatever the daemon held.
//! `Drop` cannot await, so shutdown is best-effort there and explicit in
//! [`StdioClient::shutdown`]; `kill_on_drop` is the backstop, not the plan.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::protocol::{
    InitializeResult, Notification, Request, Response, RpcError, Tool, ToolResult, PROTOCOL_VERSION,
};

/// How long any single request waits before the connection is declared dead.
///
/// A server that has stopped answering is indistinguishable from one that is slow,
/// and waiting forever on the difference hangs the daemon. Thirty seconds is longer
/// than any MCP call rev-local makes and shorter than a user's patience.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a server gets to exit after its stdin closes, before it is killed.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// How to start one MCP server (§13.1's `mcpServers` map).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerCommand {
    /// A name for logs and the UI. Not the process name.
    pub id: String,
    /// The executable.
    pub command: String,
    /// Its arguments.
    pub args: Vec<String>,
    /// Extra environment. Added to the inherited environment, not replacing it.
    pub env: BTreeMap<String, String>,
    /// Working directory, when the server needs one.
    pub cwd: Option<PathBuf>,
}

impl ServerCommand {
    /// A server with no extra environment and no working directory.
    pub fn new(id: &str, command: &str, args: &[&str]) -> Self {
        Self {
            id: id.to_owned(),
            command: command.to_owned(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            env: BTreeMap::new(),
            cwd: None,
        }
    }
}

/// Everything that can go wrong talking to an MCP server.
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    /// The server binary could not be started.
    #[error("could not start MCP server `{id}` (`{command}`): {source}\n  try: check the server's command in your config, and that it is on PATH")]
    Spawn {
        /// Which server.
        id: String,
        /// What was run.
        command: String,
        /// Why it failed.
        #[source]
        source: std::io::Error,
    },

    /// The server exited, or its pipes closed, mid-conversation.
    ///
    /// Distinct from every other variant because it is the one that means *the
    /// connection is gone*: the client drops it and the next call reconnects.
    #[error("MCP server `{id}` stopped responding: {detail}")]
    Disconnected {
        /// Which server.
        id: String,
        /// What was observed.
        detail: String,
    },

    /// The server did not answer within [`REQUEST_TIMEOUT`].
    #[error("MCP server `{id}` did not answer `{method}` within {seconds}s")]
    Timeout {
        /// Which server.
        id: String,
        /// What was asked.
        method: String,
        /// How long was waited.
        seconds: u64,
    },

    /// The server answered, with an error.
    ///
    /// The call did not happen. A tool that ran and refused is **not** this — see
    /// the module docs.
    #[error("MCP server `{id}` refused `{method}`: {} (code {})", .error.message, .error.code)]
    Protocol {
        /// Which server.
        id: String,
        /// What was asked.
        method: String,
        /// The server's error object, whole.
        error: RpcError,
    },

    /// The server said something this client could not read.
    #[error("MCP server `{id}` sent a reply this client could not read: {detail}")]
    Malformed {
        /// Which server.
        id: String,
        /// What was wrong.
        detail: String,
    },
}

impl McpError {
    /// Whether the connection is gone and the next call should reconnect.
    pub const fn is_transport(&self) -> bool {
        matches!(self, Self::Disconnected { .. } | Self::Timeout { .. })
    }

    /// Whether the server said this is worth retrying (§11.6).
    ///
    /// `None` where the server did not say. Guessing would mean deciding one
    /// server's `-32002` means what another's does, and guessing wrong on a
    /// non-retryable error turns a caller bug into a slow failure.
    pub fn retryable(&self) -> Option<bool> {
        match self {
            Self::Protocol { error, .. } => error.retryable(),
            // A dead connection is always worth one more attempt: reconnecting is
            // exactly what would fix it.
            Self::Disconnected { .. } | Self::Timeout { .. } => Some(true),
            Self::Spawn { .. } | Self::Malformed { .. } => Some(false),
        }
    }
}

/// One live conversation with a server process.
struct Connection {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    /// What `initialize` reported, kept for the UI and for health reporting.
    handshake: InitializeResult,
}

/// An MCP client speaking to one stdio server.
pub struct StdioClient {
    server: ServerCommand,
    connection: Option<Connection>,
    /// How many times a connection has been established, including reconnects.
    ///
    /// Exposed so a test can assert a reconnect *happened* rather than inferring it
    /// from a call that succeeded — which it would have done anyway if the first
    /// connection had never died.
    connects: u64,
}

impl StdioClient {
    /// A client for `server`. Nothing is spawned until the first call.
    ///
    /// Lazy because a daemon configures every server at startup and may never use
    /// most of them; spawning eagerly would start processes for targets no repo
    /// publishes to.
    pub fn new(server: ServerCommand) -> Self {
        Self {
            server,
            connection: None,
            connects: 0,
        }
    }

    /// Which server this is.
    pub fn id(&self) -> &str {
        &self.server.id
    }

    /// How many times a connection has been established.
    pub const fn connect_count(&self) -> u64 {
        self.connects
    }

    /// Whether a connection is currently held.
    pub const fn is_connected(&self) -> bool {
        self.connection.is_some()
    }

    /// The server process's pid, when connected.
    pub fn pid(&self) -> Option<u32> {
        self.connection.as_ref().and_then(|c| c.child.id())
    }

    /// What `initialize` reported, when connected.
    pub fn handshake(&self) -> Option<&InitializeResult> {
        self.connection.as_ref().map(|c| &c.handshake)
    }

    /// Spawn the server and complete the handshake.
    async fn connect(&mut self) -> Result<(), McpError> {
        let mut command = Command::new(&self.server.command);
        command
            .args(&self.server.args)
            .envs(&self.server.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Inherited, not captured: a server's diagnostics belong in the
            // daemon's log where a user can see them. Capturing without draining
            // would fill the pipe and wedge the server, which is a hang that looks
            // like a slow server.
            .stderr(Stdio::inherit())
            .kill_on_drop(true);

        if let Some(cwd) = &self.server.cwd {
            command.current_dir(cwd);
        }

        let mut child = command.spawn().map_err(|source| McpError::Spawn {
            id: self.server.id.clone(),
            command: self.server.command.clone(),
            source,
        })?;

        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            return Err(McpError::Disconnected {
                id: self.server.id.clone(),
                detail: "the spawned process had no stdin or stdout".to_owned(),
            });
        };

        let mut connection = Connection {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
            handshake: InitializeResult {
                protocol_version: None,
                server_info: None,
                instructions: None,
            },
        };

        let result = Self::request(
            &self.server.id,
            &mut connection,
            "initialize",
            Some(serde_json::json!({
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "rev-local", "version": env!("CARGO_PKG_VERSION") },
            })),
        )
        .await?;

        let handshake: InitializeResult =
            serde_json::from_value(result).map_err(|e| McpError::Malformed {
                id: self.server.id.clone(),
                detail: format!("initialize result: {e}"),
            })?;

        // A version skew is recorded, not enforced. These are third-party servers,
        // and refusing to talk to one that happens to work would break publishing
        // for a mismatch that costs nothing.
        if let Some(theirs) = &handshake.protocol_version {
            if theirs != PROTOCOL_VERSION {
                tracing::warn!(
                    server = %self.server.id,
                    theirs = %theirs,
                    ours = PROTOCOL_VERSION,
                    "MCP protocol version differs; continuing"
                );
            }
        }

        // The spec requires this after a successful initialize. It is a
        // notification, so nothing is waited for.
        Self::notify(
            &self.server.id,
            &mut connection,
            "notifications/initialized",
        )
        .await?;

        connection.handshake = handshake;
        self.connection = Some(connection);
        self.connects += 1;
        Ok(())
    }

    /// Ensure a connection exists, reconnecting if the last one died.
    async fn ensure_connected(&mut self) -> Result<(), McpError> {
        if self.connection.is_some() {
            return Ok(());
        }
        self.connect().await
    }

    /// Send one request and read its reply.
    async fn request(
        id: &str,
        connection: &mut Connection,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, McpError> {
        let request_id = connection.next_id;
        connection.next_id += 1;

        let line =
            serde_json::to_string(&Request::new(request_id, method, params)).map_err(|e| {
                McpError::Malformed {
                    id: id.to_owned(),
                    detail: format!("could not encode {method}: {e}"),
                }
            })?;

        Self::write_line(id, connection, &line).await?;

        // Replies are read until the matching id arrives. A server may interleave
        // notifications and, in a future revision, server-initiated requests;
        // treating the next line as *the* answer would misattribute one of those to
        // whatever call happened to be in flight.
        let deadline = tokio::time::Instant::now() + REQUEST_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(McpError::Timeout {
                    id: id.to_owned(),
                    method: method.to_owned(),
                    seconds: REQUEST_TIMEOUT.as_secs(),
                });
            }

            let mut line = String::new();
            let read = tokio::time::timeout(remaining, connection.stdout.read_line(&mut line))
                .await
                .map_err(|_| McpError::Timeout {
                    id: id.to_owned(),
                    method: method.to_owned(),
                    seconds: REQUEST_TIMEOUT.as_secs(),
                })?;

            match read {
                Ok(0) => {
                    return Err(McpError::Disconnected {
                        id: id.to_owned(),
                        detail: format!("stdout closed while waiting for `{method}`"),
                    })
                }
                Ok(_) => {}
                Err(e) => {
                    return Err(McpError::Disconnected {
                        id: id.to_owned(),
                        detail: format!("reading a reply to `{method}`: {e}"),
                    })
                }
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let response: Response =
                serde_json::from_str(trimmed).map_err(|e| McpError::Malformed {
                    id: id.to_owned(),
                    detail: format!("reply to `{method}` was not JSON-RPC: {e}"),
                })?;

            if response.id != Some(request_id) {
                // Not ours. A notification, or a reply to a request that already
                // timed out.
                continue;
            }

            if let Some(error) = response.error {
                return Err(McpError::Protocol {
                    id: id.to_owned(),
                    method: method.to_owned(),
                    error,
                });
            }

            return Ok(response.result.unwrap_or(serde_json::Value::Null));
        }
    }

    /// Send a notification. Nothing is read back.
    async fn notify(id: &str, connection: &mut Connection, method: &str) -> Result<(), McpError> {
        let line =
            serde_json::to_string(&Notification::new(method)).map_err(|e| McpError::Malformed {
                id: id.to_owned(),
                detail: format!("could not encode {method}: {e}"),
            })?;
        Self::write_line(id, connection, &line).await
    }

    /// Write one newline-delimited message.
    async fn write_line(id: &str, connection: &mut Connection, line: &str) -> Result<(), McpError> {
        let disconnected = |e: std::io::Error| McpError::Disconnected {
            id: id.to_owned(),
            detail: format!("writing to the server: {e}"),
        };

        connection
            .stdin
            .write_all(line.as_bytes())
            .await
            .map_err(disconnected)?;
        connection
            .stdin
            .write_all(b"\n")
            .await
            .map_err(disconnected)?;
        connection.stdin.flush().await.map_err(disconnected)
    }

    /// Run one request, reconnecting first if needed and dropping the connection if
    /// it dies.
    async fn call(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, McpError> {
        self.ensure_connected().await?;

        let Some(connection) = self.connection.as_mut() else {
            return Err(McpError::Disconnected {
                id: self.server.id.clone(),
                detail: "no connection after connecting".to_owned(),
            });
        };

        let outcome = Self::request(&self.server.id, connection, method, params).await;

        if outcome.as_ref().is_err_and(McpError::is_transport) {
            // Dropped here, not reconnected here. See the module docs: eager
            // reconnection against a server that crashes on startup spins.
            self.connection = None;
        }

        outcome
    }

    /// Every tool the server offers (§11.2).
    pub async fn list_tools(&mut self) -> Result<Vec<Tool>, McpError> {
        let result = self.call("tools/list", None).await?;

        let tools = result
            .get("tools")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        serde_json::from_value(tools).map_err(|e| McpError::Malformed {
            id: self.server.id.clone(),
            detail: format!("tools/list: {e}"),
        })
    }

    /// Call one tool.
    ///
    /// A tool that ran and refused comes back as `Ok` with `is_error` set — that is
    /// an answer, not a failure to get one. Only a call that did not happen is an
    /// `Err`.
    pub async fn call_tool(
        &mut self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<ToolResult, McpError> {
        let result = self
            .call(
                "tools/call",
                Some(serde_json::json!({ "name": name, "arguments": arguments })),
            )
            .await?;

        serde_json::from_value(result).map_err(|e| McpError::Malformed {
            id: self.server.id.clone(),
            detail: format!("tools/call `{name}`: {e}"),
        })
    }

    /// Close stdin, give the server a moment, then make sure it is gone.
    ///
    /// Explicit rather than left to `Drop`, because `Drop` cannot await and so
    /// cannot wait for a clean exit. A server killed without warning may leave its
    /// own side half-done; one whose stdin closes gets to finish.
    pub async fn shutdown(&mut self) {
        let Some(mut connection) = self.connection.take() else {
            return;
        };

        drop(connection.stdin);

        let exited = tokio::time::timeout(SHUTDOWN_GRACE, connection.child.wait()).await;
        if exited.is_err() {
            let _ = connection.child.start_kill();
            let _ = connection.child.wait().await;
        }
    }
}

impl Drop for StdioClient {
    fn drop(&mut self) {
        // `kill_on_drop(true)` on the `Child` is what actually reaps this, and it
        // needs a runtime alive to do it. Prefer `shutdown().await`; this is the
        // backstop for a client dropped on an error path.
        if let Some(connection) = &mut self.connection {
            let _ = connection.child.start_kill();
        }
    }
}
