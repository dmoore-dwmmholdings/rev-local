//! The desktop shell (RL-1101, SPEC §4.2, §15).
//!
//! §4.2: the daemon runs **in-process** inside both the Tauri app and the CLI.
//! There is no background service in v1 — the app must be running to review — so
//! this binary owns the runtime and hands the daemon an event bridge pointed at
//! the window.
//!
//! Everything this file does is wiring. The commands delegate to
//! [`revlocal_tauri::ipc`], which compiles without a webview and is tested without
//! one; if a decision needs making it belongs in the daemon, where the CLI can
//! reach it too.

use std::sync::Arc;

use revlocal_tauri::events::{UiEvent, UiEventSink, RUN_EVENT};
use tauri::{Emitter, Manager};

/// Delivers events to the window.
///
/// The bridge does not know it is talking to a webview; this is the only place
/// that does.
struct WindowSink {
    app: tauri::AppHandle<tauri::Wry>,
}

impl UiEventSink for WindowSink {
    fn deliver(&self, event: UiEvent) {
        // A window that has gone away is not an error worth failing a run over —
        // §15's rule is that the app stays usable while a review runs, and the
        // review outliving a closed window is the same principle.
        if let Err(error) = self.app.emit(RUN_EVENT, &event) {
            eprintln!("revlocal: no window to deliver a run event to: {error}");
        }
    }
}

/// Stop everything (SPEC §12.1).
///
/// One line of delegation, like every command here.
#[tauri::command]
fn kill_switch() -> Result<(), String> {
    // TODO(RL-1201): hand this to the daemon's KillSwitch once the app owns one.
    eprintln!("revlocal: kill switch invoked from the UI");
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![kill_switch])
        .setup(|app| {
            let sink: Arc<dyn UiEventSink> = Arc::new(WindowSink {
                app: app.handle().clone(),
            });
            // The bridge is what the daemon will be handed as its RunEventSink.
            // Held in Tauri's state so it lives as long as the app does.
            app.manage(revlocal_tauri::EventBridge::new(sink));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("the rev-local window could not start");
}
