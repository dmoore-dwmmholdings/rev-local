//! The `#[tauri::command]` wrappers (RL-1101, SPEC §15).
//!
//! Every function here is one line of delegation. That is the point: §15 requires
//! the IPC surface to hold no business logic, and the cheapest way to guarantee it
//! is for the layer that *can* reach a window to be too thin to hold any.
//!
//! Everything these call lives in [`crate::ipc`], which compiles without Tauri and
//! is tested without one. If a decision needs making it belongs in the daemon,
//! where the CLI can reach it too — a number computed here is a number the CLI
//! will eventually disagree with.

use crate::ipc::{IpcError, IpcRequest, IpcResponse};

/// Dispatch a request. The single entry point the wrappers share.
///
/// A caller supplies the handler, so this module never learns what a repository
/// is. That is what keeps it a boundary rather than a layer with opinions.
pub fn dispatch<H>(request: IpcRequest, handler: &H) -> Result<IpcResponse, IpcError>
where
    H: Fn(IpcRequest) -> Result<IpcResponse, IpcError>,
{
    handler(request)
}

/// The Tauri event name the front end subscribes to.
pub use crate::events::RUN_EVENT;
