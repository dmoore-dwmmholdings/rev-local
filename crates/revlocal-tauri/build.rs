//! Build script for the desktop shell (RL-1101).
//!
//! Tauri's build step runs only when the `desktop` feature is on. With it off the
//! crate is the plain-Rust IPC layer, and CI builds it without a webview toolchain
//! — which is the point of the feature.

fn main() {
    #[cfg(feature = "desktop")]
    tauri_build::build();
}
