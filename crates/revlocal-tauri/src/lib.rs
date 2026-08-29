//! The Tauri shell's IPC surface and event bridge (RL-1101, SPEC §4.2, §15).
//!
//! # Why this crate compiles without Tauri
//!
//! §15 requires the IPC surface to be a **thin delegation layer with no business
//! logic**. That is a property somebody has to be able to check, and it is far
//! easier to check when the layer can be compiled and exercised without a webview:
//! a function that cannot reach a `tauri::Window` cannot quietly grow a decision
//! that belongs in the daemon.
//!
//! So `tauri` is an optional dependency, off by default. Everything here is plain
//! Rust; the `#[tauri::command]` wrappers under the `desktop` feature do nothing
//! but call it.
//!
//! It also keeps CI honest. Linking Tauri on Linux pulls webkit2gtk and a dozen
//! system packages, and making that a precondition for `cargo test --workspace`
//! would have every leg of the matrix install a browser engine to run tests that
//! do not use one.
//!
//! # The UI never polls
//!
//! §15: *live updates come from Tauri events, not polling the DB*. The daemon
//! already emits [`RunEvent`] through [`RunEventSink`], a trait built so the daemon
//! can fan out to the UI and the CLI without knowing about either. The bridge here
//! is a sink — so "the UI updates live" is not a feature the UI implements, it is
//! a consequence of the daemon already announcing what it does.
//!
//! [`RunEvent`]: revlocal_daemon::state_machine::RunEvent
//! [`RunEventSink`]: revlocal_daemon::state_machine::RunEventSink

pub mod events;
pub mod ipc;

#[cfg(feature = "desktop")]
pub mod commands;

pub use events::{EventBridge, UiEvent, UiEventSink};
pub use ipc::{IpcError, IpcRequest, IpcResponse};
