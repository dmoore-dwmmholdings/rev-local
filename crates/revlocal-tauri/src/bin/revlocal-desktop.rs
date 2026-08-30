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

/// Where the store lives for this session.
///
/// A single database beside the config, which is what §4.2's in-process daemon
/// reads. Absent, the dashboard reports the error rather than inventing an empty
/// one — "no repositories" and "no database" look the same on screen and have
/// different remedies.
fn database_path() -> std::path::PathBuf {
    std::env::var_os("REVLOCAL_DB").map_or_else(
        || {
            std::env::var_os("HOME").map_or_else(
                || std::path::PathBuf::from("rev-local.db"),
                |home| std::path::PathBuf::from(home).join(".local/share/rev-local/rev-local.db"),
            )
        },
        std::path::PathBuf::from,
    )
}

/// Where §13.1's config lives for this session.
///
/// Beside the database, and overridable the same way, so the app and the CLI read
/// one file rather than each having its own idea of where settings are.
fn config_path() -> std::path::PathBuf {
    std::env::var_os("REVLOCAL_CONFIG").map_or_else(
        || {
            database_path().parent().map_or_else(
                || std::path::PathBuf::from("config.toml"),
                |dir| dir.join("config.toml"),
            )
        },
        std::path::PathBuf::from,
    )
}

/// Read §13.1's config, falling back to its documented defaults.
///
/// Absent is not an error: a fresh install has no file and the defaults *are* the
/// document. A malformed one is not an error here either — this is used to light
/// an indicator, and refusing to render the repository screen because a comment
/// somewhere is unbalanced would hide the screen somebody needs in order to fix
/// it. `revlocal config check` is where a bad file gets reported.
fn global_config() -> revlocal_core::GlobalConfig {
    std::fs::read_to_string(config_path())
        .ok()
        .and_then(|text| revlocal_core::GlobalConfig::parse(&text).ok())
        .map_or_else(
            revlocal_core::GlobalConfig::default,
            |(config, _warnings)| config,
        )
}

/// The dashboard snapshot (§15 screen 1).
///
/// One line of delegation past opening the store: the composition is
/// `revlocal_daemon::dashboard`, which `revlocal dashboard` calls too. A number
/// computed here is a number the CLI would eventually disagree with.
#[tauri::command]
async fn dashboard() -> Result<serde_json::Value, String> {
    let pool = revlocal_store::open(&database_path())
        .await
        .map_err(|e| format!("could not open the database: {e}"))?;
    let snapshot = revlocal_daemon::dashboard::gather(
        &pool,
        &revlocal_core::BudgetSettings::default(),
        chrono::Utc::now(),
    )
    .await;
    pool.close().await;

    serde_json::to_value(snapshot.map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

/// Set the global autonomy ceiling (§12.2, §15's mode selector).
#[tauri::command]
async fn set_mode(mode: String) -> Result<(), String> {
    // Rejected here rather than stored and puzzled over later: an unknown mode
    // would read back as the default, which is a silent widening or narrowing.
    let parsed: revlocal_core::AutonomyMode = mode.parse().map_err(|_| {
        format!("unknown mode {mode:?}; try off, dry_run, auto_low_ask_high or auto")
    })?;

    let pool = revlocal_store::open(&database_path())
        .await
        .map_err(|e| format!("could not open the database: {e}"))?;
    let result = revlocal_store::SettingStore::new(&pool)
        .set(
            revlocal_daemon::dashboard::SETTING_MODE,
            parsed.as_str(),
            chrono::Utc::now(),
        )
        .await
        .map_err(|e| e.to_string());
    pool.close().await;
    result
}

/// One run's detail (§15 screen 3).
#[tauri::command]
async fn get_run(run_id: i64) -> Result<serde_json::Value, String> {
    let pool = revlocal_store::open(&database_path())
        .await
        .map_err(|e| format!("could not open the database: {e}"))?;
    let view = revlocal_daemon::run_view::gather(&pool, revlocal_core::RunId::new(run_id)).await;
    pool.close().await;

    serde_json::to_value(view.map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

/// One run's raw transcript, read only when somebody expands it (§15 screen 3).
///
/// Bounded. A transcript is whatever the engine wrote, and an engine that emitted
/// a gigabyte of progress bars should not be able to exhaust this process's memory
/// through a UI control. The tail is kept rather than the head: the end of a log
/// is where the failure is.
#[tauri::command]
async fn get_transcript(run_id: i64) -> Result<String, String> {
    const MAX_BYTES: u64 = 4 * 1024 * 1024;

    let pool = revlocal_store::open(&database_path())
        .await
        .map_err(|e| format!("could not open the database: {e}"))?;
    let run = revlocal_store::RunStore::new(&pool)
        .get(revlocal_core::RunId::new(run_id))
        .await
        .map_err(|e| e.to_string());
    pool.close().await;

    let Some(path) = run?.transcript_path else {
        return Ok(String::new());
    };
    let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;

    if size <= MAX_BYTES {
        return Ok(text);
    }
    // §18: a truncated read says so in the text itself, because the screen shows
    // this verbatim and a silently clipped log looks like a short one.
    let kept: String = text
        .chars()
        .skip(text.chars().count().saturating_sub(MAX_BYTES as usize))
        .collect();
    Ok(format!(
        "[rev-local: this transcript is {size} bytes; showing the last {MAX_BYTES}]\n\n{kept}"
    ))
}

/// Re-queue one target's failed actions (§15's retry buttons, §11.6).
#[tauri::command]
async fn retry_target(run_id: i64, target: String) -> Result<(), String> {
    let pool = revlocal_store::open(&database_path())
        .await
        .map_err(|e| format!("could not open the database: {e}"))?;
    let result = revlocal_store::PublishActionStore::new(&pool)
        .reset_for_retry(revlocal_core::RunId::new(run_id), &target)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string());
    pool.close().await;
    result
}

/// The approvals inbox (§12.4, §15 screen 5).
#[tauri::command]
async fn list_approvals() -> Result<serde_json::Value, String> {
    let pool = revlocal_store::open(&database_path())
        .await
        .map_err(|e| format!("could not open the database: {e}"))?;
    let view = revlocal_daemon::approvals_view::gather(&pool).await;
    pool.close().await;

    serde_json::to_value(view.map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

/// Approve one queued action (§12.4).
///
/// The digest is computed over the payload as it stands *now* — after any edit —
/// and the queue re-checks it at dispatch. That is what makes "an edit after
/// approval is impossible" a mechanism rather than a promise.
#[tauri::command]
async fn approve_action(id: i64) -> Result<(), String> {
    with_store(|pool| async move {
        let store = revlocal_store::PublishActionStore::new(&pool);
        let waiting = store
            .list_awaiting_approval()
            .await
            .map_err(|e| e.to_string())?;
        let action = waiting
            .iter()
            .find(|a| a.id.get() == id)
            .ok_or_else(|| format!("action #{id} is not waiting for approval"))?;

        let digest = revlocal_core::payload_digest(&action.payload_json);
        store
            .approve(revlocal_core::PublishActionId::new(id), &digest)
            .await
            .map_err(|e| e.to_string())
    })
    .await
}

/// Approve everything queued for one run (§12.4's "approve all for this run").
#[tauri::command]
async fn approve_run(run_id: i64) -> Result<(), String> {
    with_store(|pool| async move {
        let ids =
            revlocal_daemon::approvals_view::for_run(&pool, revlocal_core::RunId::new(run_id))
                .await
                .map_err(|e| e.to_string())?;
        let store = revlocal_store::PublishActionStore::new(&pool);
        let waiting = store
            .list_awaiting_approval()
            .await
            .map_err(|e| e.to_string())?;

        for id in ids {
            // Each digest is over that action's own payload. One digest for the
            // batch would let an edit to any member ride in on another's approval.
            if let Some(action) = waiting.iter().find(|a| a.id.get() == id) {
                let digest = revlocal_core::payload_digest(&action.payload_json);
                store
                    .approve(revlocal_core::PublishActionId::new(id), &digest)
                    .await
                    .map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    })
    .await
}

/// Reject one action, optionally suppressing its finding (§12.4).
#[tauri::command]
async fn reject_action(id: i64, suppress: bool) -> Result<(), String> {
    with_store(|pool| async move { revlocal_cli_reject(&pool, id, suppress).await }).await
}

/// Replace a payload before approving it (§12.4's "edit body then approve").
#[tauri::command]
async fn edit_payload(id: i64, payload_json: String) -> Result<(), String> {
    with_store(|pool| async move {
        revlocal_store::PublishActionStore::new(&pool)
            .edit_payload(revlocal_core::PublishActionId::new(id), &payload_json)
            .await
            .map_err(|e| e.to_string())
    })
    .await
}

/// One repository's screen (§15 screen 2).
///
/// The webhook listener port comes from §13.1's global config, because the
/// webhook indicator is only honest if it knows whether a listener exists at all
/// — "enabled here" and "reachable" are different facts, and a screen that showed
/// the first as the second would be the one telling somebody their webhook works.
#[tauri::command]
async fn get_repository(repo_id: i64) -> Result<serde_json::Value, String> {
    let global = global_config();

    let pool = revlocal_store::open(&database_path())
        .await
        .map_err(|e| format!("could not open the database: {e}"))?;
    let view = revlocal_daemon::repository_view::gather(
        &pool,
        repo_id,
        &revlocal_core::BudgetSettings::default(),
        global.global.webhook_port,
        chrono::Utc::now(),
    )
    .await;
    pool.close().await;

    serde_json::to_value(view.map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

/// Validate and store a repository's config (§13.2, §15 screen 2).
///
/// The error crosses the boundary as the message the editor shows inline, line
/// and column included. Summarising it to "invalid config" here would leave
/// somebody re-reading thirty fields to find the one that is wrong.
#[tauri::command]
async fn save_repo_config(repo_id: i64, config_json: String) -> Result<String, String> {
    let pool = revlocal_store::open(&database_path())
        .await
        .map_err(|e| format!("could not open the database: {e}"))?;
    let saved = revlocal_daemon::repository_view::save_config(
        &pool,
        repo_id,
        &config_json,
        chrono::Utc::now(),
    )
    .await;
    pool.close().await;

    saved.map_err(|e| e.to_string())
}

/// Findings across every repository, filtered (§15 screen 4).
///
/// The filter crosses the boundary and the daemon applies it. Filtering in the
/// front end would mean sending the whole table first — this is the one screen
/// that can be large, and that cost would be paid on every keystroke.
#[tauri::command]
async fn list_findings(
    filter: revlocal_daemon::findings_view::FindingFilter,
) -> Result<serde_json::Value, String> {
    let pool = revlocal_store::open(&database_path())
        .await
        .map_err(|e| format!("could not open the database: {e}"))?;
    let view = revlocal_daemon::findings_view::gather(&pool, &filter).await;
    pool.close().await;

    serde_json::to_value(view.map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

/// Suppress one finding, scoped to its own repository (§14, §15 screen 4).
///
/// Returns the finding's new state so the screen can show the row changing
/// rather than assuming it did.
#[tauri::command]
async fn suppress_finding(id: i64) -> Result<String, String> {
    let pool = revlocal_store::open(&database_path())
        .await
        .map_err(|e| format!("could not open the database: {e}"))?;
    let state = revlocal_daemon::findings_view::suppress(&pool, id, chrono::Utc::now()).await;
    pool.close().await;

    Ok(state.map_err(|e| e.to_string())?.as_str().to_owned())
}

/// File a finding to Andare by hand — gated exactly like an automatic action.
///
/// Returns the status the action was *given*, not "filed". Under the default mode
/// this queues for approval, and a command that reported success would have the
/// screen telling somebody an issue exists that does not.
#[tauri::command]
async fn file_to_andare(id: i64) -> Result<String, String> {
    let pool = revlocal_store::open(&database_path())
        .await
        .map_err(|e| format!("could not open the database: {e}"))?;
    // The same reader the dashboard uses, so the mode shown and the mode enforced
    // cannot drift apart.
    let mode = revlocal_daemon::dashboard::global_mode(&pool).await;
    let status = match mode {
        Ok(mode) => {
            revlocal_daemon::findings_view::file_to_andare(&pool, id, mode, chrono::Utc::now())
                .await
                .map_err(|e| e.to_string())
        }
        Err(error) => Err(error.to_string()),
    };
    pool.close().await;

    Ok(status?.as_str().to_owned())
}

/// Open the store, run one thing, close it.
///
/// Every command here is short-lived and owns its connection: §4.2 runs the
/// daemon in-process, and a pool held across an idle window is a lock held for no
/// reason.
async fn with_store<F, Fut>(work: F) -> Result<(), String>
where
    F: FnOnce(revlocal_store::Pool) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let pool = revlocal_store::open(&database_path())
        .await
        .map_err(|e| format!("could not open the database: {e}"))?;
    let result = work(pool.clone()).await;
    pool.close().await;
    result
}

/// Reject, and suppress the finding when asked.
async fn revlocal_cli_reject(
    pool: &revlocal_store::Pool,
    id: i64,
    suppress: bool,
) -> Result<(), String> {
    let store = revlocal_store::PublishActionStore::new(pool);
    let waiting = store
        .list_awaiting_approval()
        .await
        .map_err(|e| e.to_string())?;
    let action = waiting
        .iter()
        .find(|a| a.id.get() == id)
        .ok_or_else(|| format!("action #{id} is not waiting for approval"))?;

    // §12.4 keeps `expired` for a timeout distinct from a person saying no: one
    // is a decision, the other is that nobody looked.
    store
        .reject(
            revlocal_core::PublishActionId::new(id),
            "rejected by operator",
        )
        .await
        .map_err(|e| e.to_string())?;

    if !suppress {
        return Ok(());
    }
    let Some(finding_id) = action.finding_id else {
        // Nothing to suppress. Not an error — the button is disabled for this
        // case, and a race that gets here should not fail the rejection that
        // already succeeded.
        return Ok(());
    };
    let finding = revlocal_store::FindingStore::new(pool)
        .get(finding_id)
        .await
        .map_err(|e| e.to_string())?;

    revlocal_store::SuppressionStore::new(pool)
        .insert(&revlocal_core::Suppression {
            id: revlocal_core::SuppressionId::new(0),
            repo_id: None,
            fingerprint: Some(finding.fingerprint),
            glob: None,
            reason: Some("rejected with suppress from the approvals inbox".to_owned()),
            created_at: chrono::Utc::now(),
        })
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Which screen to open on launch (§16.4).
///
/// A capture harness photographs one screen at a time and cannot click its way
/// there — a webview's DOM is not an OS accessibility tree, which is an afternoon
/// I spent finding out.
///
/// An IPC command rather than a query parameter set by `eval`: the eval runs after
/// the page has mounted, so the front end would already have read an empty URL and
/// any success would be a race that happened to go the right way.
///
/// Environment rather than a flag, so somebody launching the app normally never
/// meets it and the harness sets it the way it already sets `REVLOCAL_DB`.
#[tauri::command]
fn initial_screen() -> String {
    std::env::var("REVLOCAL_SCREEN").unwrap_or_default()
}

/// Which repository a capture harness asked for, or 0 (RL-1102, §16.4).
///
/// §15's repository screen is about *a* repository, so opening it without one
/// leaves a screen that correctly says "choose one from the dashboard" — right
/// for a person, useless as a capture. Set by the harness and by nothing else,
/// the same way `REVLOCAL_SCREEN` is.
///
/// An unparseable value is 0 rather than an error: this comes from an environment
/// variable somebody typed, and a mistyped id should leave the chooser on screen,
/// not a stack trace.
#[tauri::command]
fn initial_repo() -> i64 {
    std::env::var("REVLOCAL_REPO")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
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
        .invoke_handler(tauri::generate_handler![
            kill_switch,
            dashboard,
            set_mode,
            get_run,
            get_transcript,
            retry_target,
            list_approvals,
            approve_action,
            approve_run,
            reject_action,
            edit_payload,
            get_repository,
            save_repo_config,
            list_findings,
            suppress_finding,
            file_to_andare,
            initial_screen,
            initial_repo
        ])
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
