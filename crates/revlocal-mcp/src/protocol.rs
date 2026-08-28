//! The JSON-RPC 2.0 shapes MCP speaks (SPEC §11.2).
//!
//! Only what the client sends and reads. MCP is a large protocol and rev-local uses
//! a small corner of it — `initialize`, `tools/list`, `tools/call` — so modelling
//! the rest would be inventing obligations nothing checks.

use serde::{Deserialize, Serialize};

/// The protocol revision this client speaks.
///
/// Sent in `initialize` and compared against what the server answers. A server
/// speaking a different revision is a **warning**, not a failure: MCP servers are
/// third-party and a version skew that happens to work should not stop a review
/// from publishing. What must not happen is the skew going unrecorded.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// A request, as it goes out.
#[derive(Debug, Clone, Serialize)]
pub struct Request {
    /// Always `"2.0"`.
    pub jsonrpc: &'static str,
    /// Correlates the reply. Unique per connection.
    pub id: u64,
    /// The MCP method being called.
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Method arguments, when the method takes any.
    pub params: Option<serde_json::Value>,
}

impl Request {
    /// A request with the next id.
    pub fn new(id: u64, method: &str, params: Option<serde_json::Value>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            method: method.to_owned(),
            params,
        }
    }
}

/// A notification: no id, and no reply is expected or waited for.
#[derive(Debug, Clone, Serialize)]
pub struct Notification {
    /// Always `"2.0"`.
    pub jsonrpc: &'static str,
    /// The MCP method being notified.
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Method arguments, when the method takes any.
    pub params: Option<serde_json::Value>,
}

impl Notification {
    /// A notification for `method`.
    pub fn new(method: &str) -> Self {
        Self {
            jsonrpc: "2.0",
            method: method.to_owned(),
            params: None,
        }
    }
}

/// One line read back from the server.
#[derive(Debug, Clone, Deserialize)]
pub struct Response {
    /// Absent on a parse error the server could not attribute to a request.
    #[serde(default)]
    pub id: Option<u64>,
    /// Present on success.
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    /// Present on failure. Never both.
    #[serde(default)]
    pub error: Option<RpcError>,
}

/// A JSON-RPC error object.
///
/// `data` is carried whole rather than parsed into fields. Servers put different
/// things there — `retry_after_ms`, `http_status`, vendor keys — and a struct with
/// four optional fields would quietly drop the fifth.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct RpcError {
    /// The JSON-RPC error code.
    pub code: i64,
    /// The server's message.
    pub message: String,
    /// Server-supplied detail, carried whole.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl RpcError {
    /// Whether the server said this is worth retrying.
    ///
    /// **Only when it said so.** Guessing from the code would mean deciding that a
    /// server's `-32002` means the same as another's, and the cost of guessing wrong
    /// is a retry loop on a caller bug — §11.6's `invalid_params` case, which is not
    /// retryable and which backing off on turns a fast failure into a slow one.
    pub fn retryable(&self) -> Option<bool> {
        self.data.as_ref()?.get("retryable")?.as_bool()
    }

    /// How long the server asked us to wait, if it said.
    pub fn retry_after_ms(&self) -> Option<u64> {
        self.data.as_ref()?.get("retry_after_ms")?.as_u64()
    }
}

/// What `initialize` answered.
///
/// `rename_all` is load-bearing: MCP is camelCase on the wire, and without it
/// `protocol_version` deserialized to `None` against every real server — which meant
/// the version-skew warning below could never fire. Caught by
/// `stdio_the_handshake_is_recorded`; the same class of silently-dead path as a
/// guessed error string.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    /// The revision the server speaks. Compared, not enforced -- see [`PROTOCOL_VERSION`].
    #[serde(default)]
    pub protocol_version: Option<String>,
    /// Who the server says it is.
    #[serde(default)]
    pub server_info: Option<ServerInfo>,
    /// Guidance the server offers its callers.
    #[serde(default)]
    pub instructions: Option<String>,
}

/// Who the server says it is.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ServerInfo {
    #[serde(default)]
    /// The server's name.
    pub name: String,
    /// Its version string.
    #[serde(default)]
    pub version: String,
}

/// One tool, as `tools/list` reports it.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Tool {
    /// The name to pass to `tools/call`.
    pub name: String,
    #[serde(default)]
    /// What the server says the tool does.
    pub description: String,
    /// The tool's JSON Schema.
    ///
    /// Kept whole: §11.2 validates rendered args against it, and a partially
    /// modelled schema would validate against the parts we happened to model.
    #[serde(default, rename = "inputSchema")]
    pub input_schema: serde_json::Value,
}

/// What `tools/call` answered.
///
/// Note the two failure modes MCP deliberately keeps apart, and which this client
/// keeps apart too: a **protocol** error (the call did not happen — unknown tool,
/// bad params) arrives as an `RpcError`, while a **tool** error (the call happened
/// and the tool refused) arrives here with `is_error`. Collapsing them would make
/// "Trama refused to overwrite a page you had not read" indistinguishable from
/// "Trama has no such tool", which need opposite responses.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ToolResult {
    #[serde(default)]
    /// The blocks the tool returned.
    pub content: Vec<Content>,
    /// Whether the tool itself refused. See the type docs.
    #[serde(default, rename = "isError")]
    pub is_error: bool,
}

impl ToolResult {
    /// Every text block, joined. Non-text content is skipped.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|c| c.text.as_deref())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// One block of a tool's result.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Content {
    #[serde(default, rename = "type")]
    /// `text`, `image`, and so on.
    pub kind: String,
    /// The text, when this block carries any.
    #[serde(default)]
    pub text: Option<String>,
}
