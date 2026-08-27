//! MCP over streamable HTTP (SPEC §11.2).
//!
//! The transport Trama uses, so unlike stdio it talks to a machine rev-local does not
//! control, over a network that fails in more ways than a pipe does.
//!
//! # Secrets are resolved at connect time and never earlier
//!
//! §13.1: a header value may be `{{keychain:name}}`, resolved from the OS keychain
//! **at connect time**, never at load time. That is not a preference. A config
//! resolved at load time holds every token for the process's lifetime, in a struct
//! that gets logged wholesale by every diagnostic dump ever written.
//!
//! So [`HttpClient`] holds `SecretRef`s — whose `Debug` redacts — and resolves them
//! into a header map that is **not reachable from any `Debug` impl on this type**.
//! [`HttpError`] carries header *names* and never values, structurally: there is no
//! variant a token could be put in even by accident.
//!
//! # Reading the body whole
//!
//! MCP's streamable HTTP allows a long-lived SSE stream for server-initiated
//! messages. rev-local never exposes tools *to* a server, so it never needs one: for
//! `initialize`, `tools/list` and `tools/call` the server ends the stream with the
//! response, and reading to completion is correct and much simpler than a streaming
//! parser. If a server-initiated channel is ever needed, that is a new transport
//! mode, not a change to this one.
//!
//! Both content types are handled because servers genuinely differ: some answer
//! `application/json`, some answer a one-event `text/event-stream`, and a client that
//! reads only the first works against exactly the servers it was tested on.

use std::collections::BTreeMap;
use std::time::Duration;

use revlocal_core::SecretRef;

use crate::protocol::{
    InitializeResult, Notification, Request, Response, RpcError, Tool, ToolResult, PROTOCOL_VERSION,
};

/// How long a single HTTP request waits.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The header MCP uses to carry a session between calls.
pub const SESSION_HEADER: &str = "mcp-session-id";

/// One HTTP MCP endpoint (§13.1's `mcpServers` entry with `type = "http"`).
///
/// `headers` holds [`SecretRef`]s, not strings. A struct that held resolved values
/// would be one `{:?}` away from printing a bearer token, and the derived `Debug`
/// here is safe precisely because it cannot reach one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpEndpoint {
    /// A name for logs and the UI.
    pub id: String,
    /// The endpoint URL.
    pub url: String,
    /// Headers, which may be `{{keychain:name}}` placeholders.
    pub headers: BTreeMap<String, SecretRef>,
}

impl HttpEndpoint {
    /// An endpoint with no headers.
    pub fn new(id: &str, url: &str) -> Self {
        Self {
            id: id.to_owned(),
            url: url.to_owned(),
            headers: BTreeMap::new(),
        }
    }

    /// Add a header.
    #[must_use]
    pub fn with_header(mut self, name: &str, value: SecretRef) -> Self {
        self.headers.insert(name.to_owned(), value);
        self
    }
}

/// Resolves `{{keychain:name}}` placeholders.
///
/// A trait so the keychain implementation can land separately and so tests can
/// inject without touching the developer's real keychain — which a test that read
/// the OS keychain would, and which is not a thing a test suite may do.
pub trait SecretResolver: Send + Sync {
    /// Look up one keychain entry.
    ///
    /// The error is a *reason*, never the value, and is shown to the user.
    fn resolve(&self, name: &str) -> Result<String, String>;
}

/// A resolver that fails every lookup, for endpoints that need no secrets.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoSecrets;

impl SecretResolver for NoSecrets {
    fn resolve(&self, name: &str) -> Result<String, String> {
        Err(format!(
            "no keychain is configured, so `{{{{keychain:{name}}}}}` cannot be resolved"
        ))
    }
}

/// What can go wrong over HTTP.
///
/// **No variant carries a header value.** That is structural, not a convention: a
/// token cannot be put in one of these even by mistake, so no future edit to an error
/// message can leak one.
#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    /// A `{{keychain:name}}` header could not be resolved.
    #[error("MCP server `{id}`: header `{header}` needs keychain entry `{entry}`, which could not be read: {reason}\n  try: add the entry to your keychain, or replace the placeholder with a literal value")]
    Secret {
        /// Which server.
        id: String,
        /// Which header needed it. The name, never the value.
        header: String,
        /// The keychain entry. Names are not secret; values are.
        entry: String,
        /// Why the lookup failed.
        reason: String,
    },

    /// A header name or resolved value was not valid for HTTP.
    ///
    /// The value is *not* included even though it is the thing that was wrong — a
    /// pasted token with a trailing newline is the common cause, and printing it to
    /// explain the newline would defeat the point.
    #[error("MCP server `{id}`: header `{header}` is not a valid HTTP header\n  try: check for stray whitespace or newlines in the value")]
    BadHeader {
        /// Which server.
        id: String,
        /// Which header. The name, never the value.
        header: String,
    },

    /// The request never reached the server, or the connection failed.
    #[error("MCP server `{id}` at {url} could not be reached: {detail}\n  try: check the URL and that the server is running")]
    Transport {
        /// Which server.
        id: String,
        /// Where it was.
        url: String,
        /// What happened.
        detail: String,
    },

    /// The server did not answer in time.
    #[error("MCP server `{id}` did not answer `{method}` within {seconds}s")]
    Timeout {
        /// Which server.
        id: String,
        /// What was asked.
        method: String,
        /// How long was waited.
        seconds: u64,
    },

    /// The credentials were missing, wrong, or insufficient.
    ///
    /// 401 and 403 are separated in the message because the remedies differ: one is
    /// "your token is wrong", the other "your token is fine and lacks permission",
    /// and telling a user to re-issue a working token wastes their afternoon.
    #[error("MCP server `{id}` rejected rev-local's credentials ({status})\n  try: {remedy}")]
    Unauthorized {
        /// Which server.
        id: String,
        /// The HTTP status.
        status: u16,
        /// What to do about it.
        remedy: String,
    },

    /// The server answered with a status that is not success.
    #[error("MCP server `{id}` answered {status} for `{method}`{}\n  try: {remedy}", detail_suffix(.detail))]
    Status {
        /// Which server.
        id: String,
        /// What was asked.
        method: String,
        /// The HTTP status.
        status: u16,
        /// The body, truncated, when the server sent an explanatory one.
        detail: Option<String>,
        /// What to do about it.
        remedy: String,
        /// How long to wait, when the server said.
        retry_after_ms: Option<u64>,
    },

    /// The server answered, with a JSON-RPC error.
    #[error("MCP server `{id}` refused `{method}`: {} (code {})", .error.message, .error.code)]
    Protocol {
        /// Which server.
        id: String,
        /// What was asked.
        method: String,
        /// The server's error object.
        error: RpcError,
    },

    /// The body could not be read as MCP.
    #[error("MCP server `{id}` sent a reply this client could not read: {detail}")]
    Malformed {
        /// Which server.
        id: String,
        /// What was wrong.
        detail: String,
    },
}

/// `": <detail>"`, or nothing.
fn detail_suffix(detail: &Option<String>) -> String {
    detail
        .as_ref()
        .map_or_else(String::new, |d| format!(": {d}"))
}

impl HttpError {
    /// Whether the connection should be re-established before the next call.
    pub const fn is_transport(&self) -> bool {
        matches!(self, Self::Transport { .. } | Self::Timeout { .. })
    }

    /// Whether this is worth retrying (§11.6).
    ///
    /// Unlike the JSON-RPC case, HTTP status codes *do* have agreed meanings, so
    /// these are deductions rather than guesses. 429 and 5xx are retryable; 4xx is
    /// the caller's fault and retrying loops.
    pub fn retryable(&self) -> Option<bool> {
        match self {
            Self::Transport { .. } | Self::Timeout { .. } => Some(true),
            Self::Status { status, .. } => Some(*status == 429 || *status >= 500),
            Self::Unauthorized { .. } | Self::Secret { .. } | Self::BadHeader { .. } => Some(false),
            Self::Protocol { error, .. } => error.retryable(),
            Self::Malformed { .. } => Some(false),
        }
    }

    /// How long the server asked us to wait, when it said.
    pub const fn retry_after_ms(&self) -> Option<u64> {
        match self {
            Self::Status { retry_after_ms, .. } => *retry_after_ms,
            _ => None,
        }
    }
}

/// A resolved session: headers built once, at connect time.
///
/// Deliberately **not** `Debug`. This is the one place a token exists in memory as a
/// string, and the type system is what keeps it out of a log line.
struct Session {
    headers: reqwest::header::HeaderMap,
    session_id: Option<String>,
    handshake: InitializeResult,
}

/// An MCP client speaking to one HTTP endpoint.
pub struct HttpClient {
    endpoint: HttpEndpoint,
    http: reqwest::Client,
    session: Option<Session>,
    next_id: u64,
    connects: u64,
}

// Hand-written, not derived: a derived `Debug` would reach `Session` the moment
// someone gave it one, and `{:?}` on a client is exactly what a diagnostic dump does.
impl std::fmt::Debug for HttpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpClient")
            .field("id", &self.endpoint.id)
            .field("url", &self.endpoint.url)
            .field("connected", &self.session.is_some())
            .finish_non_exhaustive()
    }
}

impl HttpClient {
    /// A client for `endpoint`. Nothing is sent and no secret is read until the
    /// first call.
    pub fn new(endpoint: HttpEndpoint) -> Result<Self, HttpError> {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| HttpError::Transport {
                id: endpoint.id.clone(),
                url: endpoint.url.clone(),
                detail: format!("could not build an HTTP client: {e}"),
            })?;

        Ok(Self {
            endpoint,
            http,
            session: None,
            next_id: 1,
            connects: 0,
        })
    }

    /// Which server this is.
    pub fn id(&self) -> &str {
        &self.endpoint.id
    }

    /// Whether a session has been established.
    pub const fn is_connected(&self) -> bool {
        self.session.is_some()
    }

    /// How many times a session has been established, including reconnects.
    pub const fn connect_count(&self) -> u64 {
        self.connects
    }

    /// What `initialize` reported.
    pub fn handshake(&self) -> Option<&InitializeResult> {
        self.session.as_ref().map(|s| &s.handshake)
    }

    /// The session id the server assigned, if any.
    pub fn session_id(&self) -> Option<&str> {
        self.session.as_ref().and_then(|s| s.session_id.as_deref())
    }

    /// Resolve every configured header **now** (§13.1).
    fn build_headers(
        &self,
        resolver: &dyn SecretResolver,
    ) -> Result<reqwest::header::HeaderMap, HttpError> {
        use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        // Say what we can read. A server that supports both picks one; a client that
        // claimed only JSON would never see the SSE path it must handle.
        headers.insert(
            reqwest::header::ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );

        for (name, secret) in &self.endpoint.headers {
            let value = match secret {
                SecretRef::Literal(literal) => literal.clone(),
                SecretRef::Keychain { name: entry } => {
                    resolver
                        .resolve(entry)
                        .map_err(|reason| HttpError::Secret {
                            id: self.endpoint.id.clone(),
                            header: name.clone(),
                            entry: entry.clone(),
                            reason,
                        })?
                }
            };

            let header_name =
                HeaderName::try_from(name.as_str()).map_err(|_| HttpError::BadHeader {
                    id: self.endpoint.id.clone(),
                    header: name.clone(),
                })?;
            let mut header_value =
                HeaderValue::try_from(value).map_err(|_| HttpError::BadHeader {
                    id: self.endpoint.id.clone(),
                    header: name.clone(),
                })?;

            // Marks the value sensitive: hyper omits it from its own debug output,
            // and it costs nothing. Belt as well as braces — the braces being that
            // this map is unreachable from any Debug impl here.
            header_value.set_sensitive(true);
            headers.insert(header_name, header_value);
        }

        Ok(headers)
    }

    /// Establish a session: resolve secrets, `initialize`, `notifications/initialized`.
    pub async fn connect(&mut self, resolver: &dyn SecretResolver) -> Result<(), HttpError> {
        let headers = self.build_headers(resolver)?;

        let mut session = Session {
            headers,
            session_id: None,
            handshake: InitializeResult {
                protocol_version: None,
                server_info: None,
                instructions: None,
            },
        };

        let (result, assigned) = self
            .send(
                &session,
                "initialize",
                Some(serde_json::json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "rev-local", "version": env!("CARGO_PKG_VERSION") },
                })),
            )
            .await?;

        let handshake: InitializeResult =
            serde_json::from_value(result).map_err(|e| HttpError::Malformed {
                id: self.endpoint.id.clone(),
                detail: format!("initialize result: {e}"),
            })?;

        if let Some(theirs) = &handshake.protocol_version {
            if theirs != PROTOCOL_VERSION {
                tracing::warn!(
                    server = %self.endpoint.id,
                    theirs = %theirs,
                    ours = PROTOCOL_VERSION,
                    "MCP protocol version differs; continuing"
                );
            }
        }

        session.session_id = assigned;
        session.handshake = handshake;

        // A notification: no reply, and a failure to deliver it is not fatal to a
        // session that has already initialized.
        if let Err(e) = self.notify(&session, "notifications/initialized").await {
            tracing::warn!(server = %self.endpoint.id, error = %e, "initialized notification failed");
        }

        self.session = Some(session);
        self.connects += 1;
        Ok(())
    }

    /// Post one request and read the reply. Returns the result and any session id.
    async fn send(
        &self,
        session: &Session,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<(serde_json::Value, Option<String>), HttpError> {
        let request = Request::new(self.next_id, method, params);
        let body = serde_json::to_string(&request).map_err(|e| HttpError::Malformed {
            id: self.endpoint.id.clone(),
            detail: format!("could not encode {method}: {e}"),
        })?;

        let mut builder = self
            .http
            .post(&self.endpoint.url)
            .headers(session.headers.clone())
            .body(body);

        if let Some(id) = &session.session_id {
            builder = builder.header(SESSION_HEADER, id);
        }

        let response = builder.send().await.map_err(|e| {
            if e.is_timeout() {
                HttpError::Timeout {
                    id: self.endpoint.id.clone(),
                    method: method.to_owned(),
                    seconds: REQUEST_TIMEOUT.as_secs(),
                }
            } else {
                HttpError::Transport {
                    id: self.endpoint.id.clone(),
                    url: self.endpoint.url.clone(),
                    // `e` here is reqwest's error, which describes the connection.
                    // It cannot contain a header value: reqwest does not put request
                    // headers in its errors, and the sensitive flag above covers the
                    // paths that print them.
                    detail: e.to_string(),
                }
            }
        })?;

        let status = response.status();
        let assigned = response
            .headers()
            .get(SESSION_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let retry_after_ms = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .map(|secs| secs.saturating_mul(1000));
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();

        let text = response.text().await.map_err(|e| HttpError::Transport {
            id: self.endpoint.id.clone(),
            url: self.endpoint.url.clone(),
            detail: format!("reading the response body: {e}"),
        })?;

        if !status.is_success() {
            return Err(self.status_error(method, status.as_u16(), &text, retry_after_ms));
        }

        let payload = if content_type.starts_with("text/event-stream") {
            parse_sse(&text).ok_or_else(|| HttpError::Malformed {
                id: self.endpoint.id.clone(),
                detail: "an event stream with no data events".to_owned(),
            })?
        } else {
            text
        };

        let parsed: Response =
            serde_json::from_str(payload.trim()).map_err(|e| HttpError::Malformed {
                id: self.endpoint.id.clone(),
                detail: format!("reply to `{method}` was not JSON-RPC: {e}"),
            })?;

        if let Some(error) = parsed.error {
            return Err(HttpError::Protocol {
                id: self.endpoint.id.clone(),
                method: method.to_owned(),
                error,
            });
        }

        Ok((parsed.result.unwrap_or(serde_json::Value::Null), assigned))
    }

    /// Map a non-2xx status onto something a user can act on.
    fn status_error(
        &self,
        method: &str,
        status: u16,
        body: &str,
        retry_after_ms: Option<u64>,
    ) -> HttpError {
        let id = self.endpoint.id.clone();

        // 401 and 403 mean different things and have different remedies. Telling
        // someone to re-issue a token that is already correct wastes their afternoon.
        match status {
            401 => {
                return HttpError::Unauthorized {
                    id,
                    status,
                    remedy: "check the token in the keychain entry this server's headers refer to; it is missing, expired, or wrong".to_owned(),
                }
            }
            403 => {
                return HttpError::Unauthorized {
                    id,
                    status,
                    remedy: "the credentials were accepted but lack permission for this operation; check the token's scopes".to_owned(),
                }
            }
            _ => {}
        }

        let remedy = match status {
            404 => "check the server's URL — this endpoint does not exist".to_owned(),
            405 | 415 => "this endpoint does not accept an MCP POST; check the URL points at the MCP endpoint and not the web UI".to_owned(),
            429 => "the server is rate limiting; rev-local will back off and retry".to_owned(),
            s if s >= 500 => "the server failed; this is usually temporary".to_owned(),
            _ => "check the server's URL and configuration".to_owned(),
        };

        HttpError::Status {
            id,
            method: method.to_owned(),
            status,
            // Truncated: an HTML error page is 40 KB of nothing, and a log line that
            // long is a log line nobody reads.
            detail: body_excerpt(body),
            remedy,
            retry_after_ms,
        }
    }

    /// Send a notification; nothing is read back beyond the status.
    async fn notify(&self, session: &Session, method: &str) -> Result<(), HttpError> {
        let body = serde_json::to_string(&Notification::new(method)).map_err(|e| {
            HttpError::Malformed {
                id: self.endpoint.id.clone(),
                detail: format!("could not encode {method}: {e}"),
            }
        })?;

        let mut builder = self
            .http
            .post(&self.endpoint.url)
            .headers(session.headers.clone())
            .body(body);
        if let Some(id) = &session.session_id {
            builder = builder.header(SESSION_HEADER, id);
        }

        builder
            .send()
            .await
            .map(|_| ())
            .map_err(|e| HttpError::Transport {
                id: self.endpoint.id.clone(),
                url: self.endpoint.url.clone(),
                detail: e.to_string(),
            })
    }

    /// Run one call, connecting first if needed.
    async fn call(
        &mut self,
        resolver: &dyn SecretResolver,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, HttpError> {
        if self.session.is_none() {
            self.connect(resolver).await?;
        }

        self.next_id += 1;

        let Some(session) = self.session.as_ref() else {
            return Err(HttpError::Transport {
                id: self.endpoint.id.clone(),
                url: self.endpoint.url.clone(),
                detail: "no session after connecting".to_owned(),
            });
        };

        let outcome = self.send(session, method, params).await;

        if outcome.as_ref().is_err_and(HttpError::is_transport) {
            // Same rule as stdio: drop the session, reconnect on next use, never
            // eagerly. See `stdio`'s module docs.
            self.session = None;
        }

        outcome.map(|(result, _)| result)
    }

    /// Every tool the server offers.
    pub async fn list_tools(
        &mut self,
        resolver: &dyn SecretResolver,
    ) -> Result<Vec<Tool>, HttpError> {
        let result = self.call(resolver, "tools/list", None).await?;
        let tools = result
            .get("tools")
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        serde_json::from_value(tools).map_err(|e| HttpError::Malformed {
            id: self.endpoint.id.clone(),
            detail: format!("tools/list: {e}"),
        })
    }

    /// Call one tool. A tool that ran and refused is `Ok` with `is_error` set.
    pub async fn call_tool(
        &mut self,
        resolver: &dyn SecretResolver,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<ToolResult, HttpError> {
        let result = self
            .call(
                resolver,
                "tools/call",
                Some(serde_json::json!({ "name": name, "arguments": arguments })),
            )
            .await?;

        serde_json::from_value(result).map_err(|e| HttpError::Malformed {
            id: self.endpoint.id.clone(),
            detail: format!("tools/call `{name}`: {e}"),
        })
    }
}

/// How much of an error body is worth keeping.
const BODY_EXCERPT_BYTES: usize = 400;

/// A short, single-line excerpt of a response body.
fn body_excerpt(body: &str) -> Option<String> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut end = trimmed.len().min(BODY_EXCERPT_BYTES);
    while end > 0 && !trimmed.is_char_boundary(end) {
        end -= 1;
    }

    let mut excerpt = trimmed[..end].replace(['\n', '\r'], " ");
    if end < trimmed.len() {
        excerpt.push('…');
    }
    Some(excerpt)
}

/// The last `data:` payload in an SSE body.
///
/// The **last**, not the first, for the same reason §8.2 takes the last fenced block
/// of an engine's output: a stream may carry progress events before the answer, and
/// taking the first would return a progress notification as the result.
///
/// Multi-line `data:` fields are joined with newlines, per the SSE spec.
pub fn parse_sse(body: &str) -> Option<String> {
    let mut last: Option<String> = None;
    let mut current: Vec<&str> = Vec::new();

    let flush = |current: &mut Vec<&str>, last: &mut Option<String>| {
        if !current.is_empty() {
            *last = Some(current.join("\n"));
            current.clear();
        }
    };

    for line in body.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);

        if line.is_empty() {
            flush(&mut current, &mut last);
            continue;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            current.push(rest.strip_prefix(' ').unwrap_or(rest));
        }
        // `event:`, `id:`, `retry:` and comments are not needed: MCP puts the whole
        // JSON-RPC message in `data`.
    }
    flush(&mut current, &mut last);

    last.filter(|d| !d.trim().is_empty())
}
