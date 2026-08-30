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
use revlocal_tauri::lifecycle::{on_close, CloseAction, CloseCause, TrayItem};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Emitter, Manager, WindowEvent};

/// Printed once the window and tray exist, when `REVLOCAL_SMOKE` is set.
///
/// The smoke test greps for exactly this, so it lives here rather than being
/// spelled twice.
pub const READY_LINE: &str = "revlocal-desktop: window ready";

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

/// Build the tray menu §15 requires.
///
/// The items and their order come from `TrayItem`, so the menu and the handler
/// cannot drift apart — adding one without handling it is a compile error rather
/// than a menu entry that does nothing.
fn tray_menu(app: &tauri::AppHandle<tauri::Wry>) -> tauri::Result<Menu<tauri::Wry>> {
    let menu = Menu::new(app)?;
    for item in TrayItem::ALL {
        menu.append(&MenuItem::with_id(
            app,
            item.id(),
            item.label(),
            true,
            None::<&str>,
        )?)?;
    }
    Ok(menu)
}

/// Exit code, not a panic (ADR 0003).
///
/// A window that fails to start is the one moment a desktop user has no window to
/// be told anything in, so the message goes to stderr and the shell gets a code.
/// A panic here would print a backtrace and the word "panicked" to somebody whose
/// actual problem is a missing webview.
fn main() -> std::process::ExitCode {
    if let Err(error) = run() {
        eprintln!("revlocal: the desktop app could not start: {error}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

fn run() -> tauri::Result<()> {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![kill_switch])
        .setup(|app| {
            let handle = app.handle().clone();

            let sink: Arc<dyn UiEventSink> = Arc::new(WindowSink {
                app: handle.clone(),
            });
            // The bridge is what the daemon will be handed as its RunEventSink.
            // Held in Tauri's state so it lives as long as the app does.
            app.manage(revlocal_tauri::EventBridge::new(sink));

            // §15: the kill switch is reachable from every screen and from the
            // tray. The tray is also what makes closing the window survivable —
            // hiding a window with no way back is just losing it.
            TrayIconBuilder::with_id("revlocal")
                .icon(
                    app.default_window_icon()
                        .cloned()
                        .ok_or("the app has no window icon to use for the tray")?,
                )
                .tooltip("rev-local")
                .menu(&tray_menu(&handle)?)
                .on_menu_event(|app, event| match TrayItem::from_id(event.id().as_ref()) {
                    Some(TrayItem::Show) => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    Some(TrayItem::KillSwitch) => {
                        let _ = kill_switch();
                    }
                    // Quit is a real exit. An app that can only be hidden is one
                    // people force-kill, and a force-killed daemon leaves runs
                    // stuck mid-stage for RL-501's recovery to find.
                    Some(TrayItem::Quit) => app.exit(0),
                    None => {}
                })
                .build(app)?;

            // A CI smoke test can see that a process exists; it cannot see that
            // the window was created. Without this, an app that *hung* inside
            // setup and one that started correctly look identical from outside —
            // and hanging is the failure mode this project has been bitten by
            // most.
            //
            // Behind an environment variable so a real user never sees it. Set by
            // the workflow's smoke step and by nothing else.
            if std::env::var_os("REVLOCAL_SMOKE").is_some() {
                println!("{READY_LINE}");
                // Unbuffered, because the smoke test greps for this while the
                // process is still running and stdout to a pipe is block-buffered.
                use std::io::Write as _;
                let _ = std::io::stdout().flush();
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                // One line of delegation. The rule lives in `lifecycle::on_close`,
                // where it can be asserted without driving a window.
                if on_close(CloseCause::WindowControl) == CloseAction::HideToTray {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .run(tauri::generate_context!())
}
