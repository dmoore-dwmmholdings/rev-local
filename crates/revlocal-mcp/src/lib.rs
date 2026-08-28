//! MCP client over stdio and streamable HTTP, with tool discovery (SPEC §11.2).
//!
//! rev-local speaks a small corner of MCP — `initialize`, `tools/list`,
//! `tools/call` — against servers it did not write and cannot fix. That shapes
//! everything here: a third-party server may be absent, may crash, may speak a
//! version we do not, and may name its tools whatever it likes. None of those is
//! allowed to take the daemon down with it.

pub mod capability;
pub mod discovery;
pub mod http;
pub mod overrides;
pub mod protocol;
pub mod stdio;

pub use capability::{
    builtin_target, resolve, Binding, CapabilitySpec, MappingError, RenderContext, SpecError,
    TargetMapping, TargetSpec, Unmapped,
};
pub use discovery::{
    Discovery, DiscoveryError, HealthReport, McpClient, ServerHealth, ServerState,
};
pub use http::{
    parse_sse, HttpClient, HttpEndpoint, HttpError, NoSecrets, SecretResolver, SESSION_HEADER,
};
pub use overrides::{parse_arg, Override, OverrideError, Overrides};
pub use protocol::{
    Content, InitializeResult, Notification, Request, Response, RpcError, ServerInfo, Tool,
    ToolResult, PROTOCOL_VERSION,
};
pub use stdio::{McpError, ServerCommand, StdioClient};

/// The name of this crate, used by the workspace layout test in `revlocal-cli`.
pub const CRATE_NAME: &str = "revlocal-mcp";
