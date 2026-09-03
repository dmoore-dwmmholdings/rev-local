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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use revlocal_tauri::events::{UiEvent, UiEventSink, RUN_EVENT};
use revlocal_tauri::lifecycle::{on_close, CloseAction, CloseCause, TrayItem};
use revlocal_tauri::review;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Emitter, Manager, WindowEvent};

/// The rate limiter, shared across every call.
///
/// One per process because the limit is about a person's attention, not about a
/// screen: two callers each with their own budget would show twice as many.
static NOTIFIER: std::sync::Mutex<revlocal_daemon::notify::Notifier> =
    std::sync::Mutex::new(revlocal_daemon::notify::Notifier::new());

/// Prevent two dashboard clicks from running the same queue concurrently.
static QUEUE_DRAINING: AtomicBool = AtomicBool::new(false);

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

/// Build the real Andare target from the configured HTTP MCP endpoint.
///
/// The bearer remains a deferred Keychain reference until `HttpClient` connects;
/// this wiring never reads or logs it itself.
fn andare_target(
    config: &revlocal_core::GlobalConfig,
) -> Result<Arc<dyn revlocal_publish::PublishTarget>, String> {
    let server = config.mcp_servers.get("andare").ok_or_else(|| {
        "Andare MCP is not configured; add the suite bearer in Settings".to_owned()
    })?;
    if server.transport != "http" {
        return Err("Andare MCP must use the HTTP transport".to_owned());
    }
    let url = server
        .url
        .as_deref()
        .filter(|url| !url.is_empty())
        .ok_or_else(|| "Andare MCP has no endpoint URL".to_owned())?;
    let endpoint = revlocal_mcp::HttpEndpoint {
        id: "andare".to_owned(),
        url: url.to_owned(),
        headers: server.headers.clone(),
    };
    let client = revlocal_mcp::HttpClient::new(endpoint).map_err(|error| error.to_string())?;
    let writer = revlocal_publish::McpAndareWriter::new(
        revlocal_mcp::McpClient::from(client),
        Arc::new(revlocal_mcp::MacKeychain),
        revlocal_publish::AndareToolNames::default(),
    );
    Ok(Arc::new(revlocal_publish::AndareTarget::new(writer)))
}

/// Build the Trama target from the configured HTTP MCP endpoint.
///
/// Same shape as `andare_target`, and separate for the same reason the two
/// targets are separate: a repository can want a wiki page and no issue, or the
/// reverse, and one failing to build must not take the other with it.
fn trama_target(
    config: &revlocal_core::GlobalConfig,
) -> Result<Arc<dyn revlocal_publish::PublishTarget>, String> {
    let server = config
        .mcp_servers
        .get("trama")
        .ok_or_else(|| "Trama MCP is not configured".to_owned())?;
    if server.transport != "http" {
        return Err("Trama MCP must use the HTTP transport".to_owned());
    }
    let url = server
        .url
        .as_deref()
        .filter(|url| !url.is_empty())
        .ok_or_else(|| "Trama MCP has no endpoint URL".to_owned())?;
    let endpoint = revlocal_mcp::HttpEndpoint {
        id: "trama".to_owned(),
        url: url.to_owned(),
        headers: server.headers.clone(),
    };
    let client = revlocal_mcp::HttpClient::new(endpoint).map_err(|error| error.to_string())?;
    let writer = revlocal_publish::McpTramaWriter::new(
        revlocal_mcp::McpClient::from(client),
        Arc::new(revlocal_mcp::MacKeychain),
        revlocal_publish::TramaToolNames::default(),
    );
    Ok(Arc::new(revlocal_publish::TramaTarget::new(writer)))
}

/// Deliver pending and approved Andare actions, leaving durable receipts/errors
/// on their individual queue rows.
async fn dispatch_andare(
    pool: revlocal_store::Pool,
    config: &revlocal_core::GlobalConfig,
) -> Result<revlocal_publish::DispatchReport, String> {
    let mut queue =
        revlocal_publish::PublishQueue::new(pool, revlocal_publish::QueueConfig::default());
    queue.register(andare_target(config)?);
    queue
        .dispatch_pending(chrono::Utc::now())
        .await
        .map_err(|error| error.to_string())
}

/// Try to deliver, and say what happened — without turning a delivery problem
/// into a failed approval.
///
/// Approving and delivering are two things, and they fail for unrelated reasons.
/// Reporting a missing Andare bearer as "could not approve #12" was wrong twice
/// over: the approval had already been recorded, and the person reading it went
/// looking at the approval rather than at Settings. The action stays approved and
/// the queue redelivers it (§11.6 makes that safe), so the honest report is that
/// it is approved and not yet sent.
async fn deliver_note(pool: &revlocal_store::Pool, what: &str) -> String {
    match dispatch_andare(pool.clone(), &global_config()).await {
        Ok(_) => String::new(),
        Err(error) => format!("{what}, but not delivered yet — {error}"),
    }
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

/// One run as the queue panel shows it.
///
/// `status` is the run's own status rather than a re-used trigger field: a panel
/// that labelled a *reviewing* run with the word `poll` told somebody watching it
/// what started the run and nothing about what it is doing now.
#[derive(serde::Serialize)]
struct QueueItem {
    run_id: i64,
    repo: String,
    repo_id: i64,
    change: String,
    title: Option<String>,
    status: String,
    trigger: String,
    created_at: String,
    started_at: Option<String>,
    finished_at: Option<String>,
    verdict: Option<String>,
    error: Option<String>,
}

/// What the queue is doing, as opposed to what this window last asked it to do.
#[derive(serde::Serialize)]
struct QueueStatus {
    /// A review is executing — read from the runs, not from a local flag.
    running: bool,
    /// This window is driving the queue right now.
    draining: bool,
    /// The kill switch is engaged, so nothing will start.
    paused: bool,
    queued_total: u32,
    /// How many of those cannot run because their checkout is gone (RL-1554).
    queued_blocked: u32,
    active: Vec<QueueItem>,
    /// The head of the queue, oldest first — the order they will run in.
    waiting: Vec<QueueItem>,
    /// Waiting runs past the ones listed, so a short list never reads as the whole queue.
    waiting_hidden: u32,
    /// The most recently finished runs, so "nothing is running" is not the only thing said.
    recent: Vec<QueueItem>,
}

/// Statuses that mean a run is being worked on right now.
const ACTIVE_STATUSES: [revlocal_core::RunStatus; 4] = [
    revlocal_core::RunStatus::Preparing,
    revlocal_core::RunStatus::Reviewing,
    revlocal_core::RunStatus::Synthesizing,
    revlocal_core::RunStatus::Publishing,
];

/// Resolve runs into panel rows, reading each change and repository once.
///
/// The obvious loop re-opens `ChangeStore` per run and re-scans the repository
/// list per run; at a hundred queued runs that is the difference between a panel
/// that appears and one that people assume is broken.
async fn queue_items(
    pool: &revlocal_store::Pool,
    repos: &[revlocal_core::Repo],
    runs: &[revlocal_core::Run],
    changes: &mut std::collections::HashMap<i64, revlocal_core::Change>,
) -> Result<Vec<QueueItem>, String> {
    let mut items = Vec::with_capacity(runs.len());
    for run in runs {
        let key = run.change_id.get();
        if let std::collections::hash_map::Entry::Vacant(slot) = changes.entry(key) {
            let change = revlocal_store::ChangeStore::new(pool)
                .get(run.change_id)
                .await
                .map_err(|e| e.to_string())?;
            slot.insert(change);
        }
        // Inserted immediately above when absent.
        let Some(change) = changes.get(&key) else {
            continue;
        };
        let repo = repos.iter().find(|repo| repo.id == change.repo_id);
        items.push(QueueItem {
            run_id: run.id.get(),
            // Named rather than omitted: a blank cell reads as a panel that failed
            // to load, and a repository can genuinely have been removed.
            repo: repo.map_or_else(|| "removed repository".to_owned(), |repo| repo.name.clone()),
            repo_id: repo.map_or(0, |repo| repo.id.get()),
            change: change.external_id.clone(),
            title: change.title.clone(),
            status: run.status.as_str().to_owned(),
            trigger: run.trigger.as_str().to_owned(),
            created_at: run.created_at.to_rfc3339(),
            started_at: run.started_at.map(|at| at.to_rfc3339()),
            finished_at: run.finished_at.map(|at| at.to_rfc3339()),
            verdict: run.verdict.map(|v| v.as_str().to_owned()),
            error: run.error.clone(),
        });
    }
    Ok(items)
}

/// Read what the queue is actually doing (§15 screen 1).
#[tauri::command]
async fn queue_status() -> Result<serde_json::Value, String> {
    /// How many waiting runs the panel lists before it starts counting instead.
    const SHOW_WAITING: usize = 8;
    /// How many finished runs the panel keeps in view.
    const SHOW_RECENT: usize = 8;

    let pool = revlocal_store::open(&database_path())
        .await
        .map_err(|e| format!("could not open the database: {e}"))?;

    let result = async {
        let runs = revlocal_store::RunStore::new(&pool);
        let repos = revlocal_store::RepoStore::new(&pool)
            .list()
            .await
            .map_err(|e| e.to_string())?;
        let paused = revlocal_store::SettingStore::new(&pool)
            .is_paused()
            .await
            .map_err(|e| e.to_string())?;

        let queued_total = runs
            .count_matching(None, Some(revlocal_core::RunStatus::Queued))
            .await
            .map_err(|e| e.to_string())?;
        // How much of that total can actually run. A run whose checkout is gone
        // is held every tick forever, so a panel that folds it into "waiting"
        // shows a number that never goes down (RL-1554).
        let (_runnable, queued_blocked) = revlocal_daemon::repos::queued_split(&pool)
            .await
            .map_err(|e| e.to_string())?;

        // The same 200-run window the executor drains, so the head of this list is
        // genuinely the run that goes next — the newest 8 queued runs would be the
        // 8 that go *last*.
        let mut queued = runs
            .list_recent(None, Some(revlocal_core::RunStatus::Queued), 200)
            .await
            .map_err(|e| e.to_string())?;
        queued.reverse();
        queued.truncate(SHOW_WAITING);

        // A bounded window, because this is re-read on every run event and the
        // store fetches each row individually. Wide enough to hold the active
        // runs and the last few finished ones; a queue long enough to push a
        // finished run past it is one where the queue itself is the news.
        let latest = runs
            .list_recent(None, None, 50)
            .await
            .map_err(|e| e.to_string())?;
        let active: Vec<_> = latest
            .iter()
            .filter(|run| ACTIVE_STATUSES.contains(&run.status))
            .cloned()
            .collect();
        let recent: Vec<_> = latest
            .iter()
            .filter(|run| run.finished_at.is_some())
            .take(SHOW_RECENT)
            .cloned()
            .collect();

        let mut changes = std::collections::HashMap::new();
        let status = QueueStatus {
            running: !active.is_empty(),
            draining: QUEUE_DRAINING.load(Ordering::Acquire),
            paused,
            queued_total,
            queued_blocked,
            waiting_hidden: queued_total
                .saturating_sub(u32::try_from(queued.len()).unwrap_or(u32::MAX)),
            active: queue_items(&pool, &repos, &active, &mut changes).await?,
            waiting: queue_items(&pool, &repos, &queued, &mut changes).await?,
            recent: queue_items(&pool, &repos, &recent, &mut changes).await?,
        };
        serde_json::to_value(status).map_err(|e| e.to_string())
    }
    .await;

    pool.close().await;
    result
}

/// Work through the queued runs in the background, keeping the window responsive.
///
/// Runs until the queue stops making progress rather than stopping after one: a
/// button that emptied the queue one press at a time would need pressing once per
/// queued review, and the queue is what the person pressing it wants cleared.
#[tauri::command]
async fn start_queued_runs(app: tauri::AppHandle) -> Result<(), String> {
    if QUEUE_DRAINING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err("this window is already working through the queue".to_owned());
    }

    let db = database_path();
    let config = global_config();
    let data = data_dir();
    tauri::async_runtime::spawn(async move {
        let result = async {
            let pool = revlocal_store::open(&db).await.map_err(|e| e.to_string())?;
            // The database setting is the operational global ceiling selected in
            // the UI. The file supplies every other configuration default.
            let mut config = config;
            config.global.mode = revlocal_daemon::dashboard::global_mode(&pool)
                .await
                .map_err(|e| e.to_string())?;
            let sink = revlocal_tauri::events::EventBridge::new(Arc::new(WindowSink { app }));
            let cancel = cancel_token();
            let result: Result<(), String> = loop {
                let before = revlocal_store::RunStore::new(&pool)
                    .count_matching(None, Some(revlocal_core::RunStatus::Queued))
                    .await
                    .map_err(|e| e.to_string())?;
                let report = revlocal_daemon::executor::drain(
                    &pool,
                    &config,
                    &sink,
                    &data,
                    1,
                    chrono::Utc::now(),
                    &cancel,
                )
                .await
                .map_err(|e| e.to_string())?;
                let after = revlocal_store::RunStore::new(&pool)
                    .count_matching(None, Some(revlocal_core::RunStatus::Queued))
                    .await
                    .map_err(|e| e.to_string())?;
                // Stop only when there is no immediately runnable item. A held
                // budget/disabled run must not make a background task spin.
                if report.paused || after == 0 || after >= before {
                    break Ok(());
                }
            };
            pool.close().await;
            result
        }
        .await;
        if let Err(error) = result {
            eprintln!("rev-local: queued review could not run: {error}");
        }
        QUEUE_DRAINING.store(false, Ordering::Release);
    });
    Ok(())
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
    let view = revlocal_daemon::approvals_view::gather(
        &pool,
        i64::from(global_config().global.approval_ttl_hours),
        chrono::Utc::now(),
    )
    .await;
    pool.close().await;

    serde_json::to_value(view.map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

/// Approve one queued action (§12.4).
///
/// The digest is computed over the payload as it stands *now* — after any edit —
/// and the queue re-checks it at dispatch. That is what makes "an edit after
/// approval is impossible" a mechanism rather than a promise.
#[tauri::command]
async fn approve_action(id: i64) -> Result<String, String> {
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
    .await?;
    let pool = revlocal_store::open(&database_path())
        .await
        .map_err(|e| format!("could not open the database: {e}"))?;
    let note = deliver_note(&pool, &format!("Action #{id} is approved")).await;
    pool.close().await;
    Ok(note)
}

/// Approve everything queued for one run (§12.4's "approve all for this run").
#[tauri::command]
async fn approve_run(run_id: i64) -> Result<String, String> {
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
    .await?;
    let pool = revlocal_store::open(&database_path())
        .await
        .map_err(|e| format!("could not open the database: {e}"))?;
    let note = deliver_note(
        &pool,
        &format!("Run #{run_id}\u{2019}s actions are approved"),
    )
    .await;
    pool.close().await;
    Ok(note)
}

/// Reject one action, optionally suppressing its finding (§12.4).
#[tauri::command]
async fn reject_action(id: i64, suppress: bool) -> Result<(), String> {
    with_store(|pool| async move { revlocal_cli_reject(&pool, id, suppress).await }).await
}

/// Replace a payload before approving it (§12.4's "edit body then approve").
///
/// Checked before it is stored. Editing a body is the one place a person can
/// hand rev-local a payload its target cannot read, and the whole point of §12.4
/// is that approving is the deliberate moment — finding out at dispatch that the
/// edit broke the shape makes the approval a formality (RL-1516).
#[tauri::command]
async fn edit_payload(id: i64, payload_json: String) -> Result<(), String> {
    with_store(|pool| async move {
        let store = revlocal_store::PublishActionStore::new(&pool);
        let action = store
            .get(revlocal_core::PublishActionId::new(id))
            .await
            .map_err(|e| e.to_string())?;

        revlocal_publish::validate_payload(&action.target, action.capability, &payload_json)?;

        store
            .edit_payload(revlocal_core::PublishActionId::new(id), &payload_json)
            .await
            .map_err(|e| e.to_string())
    })
    .await
}

/// Show a native notification, if the limiter says it is worth showing.
///
/// The *decision* is `revlocal_daemon::notify`, which is plain Rust and tested
/// without a window. This is delivery, and delivery only: a rate limit
/// implemented in the layer that draws things is one that cannot be tested and
/// that a second caller would bypass.
///
/// The limiter is process-wide state, because the limit is about a *person's*
/// attention rather than about any one screen.
#[tauri::command]
async fn notify(
    app: tauri::AppHandle<tauri::Wry>,
    reason: revlocal_daemon::notify::Reason,
) -> Result<serde_json::Value, String> {
    use tauri_plugin_notification::NotificationExt as _;

    let decision = {
        let mut notifier = NOTIFIER
            .lock()
            .map_err(|e| format!("the notifier is poisoned: {e}"))?;
        notifier.consider(&reason, chrono::Utc::now())
    };

    if let revlocal_daemon::notify::Decision::Show { title, body }
    | revlocal_daemon::notify::Decision::Summarise { title, body, .. } = &decision
    {
        // A notification that could not be shown is reported, not swallowed: the
        // caller is telling somebody something, and "we tried" is the answer it
        // needs when the OS refused permission.
        app.notification()
            .builder()
            .title(title)
            .body(body)
            .show()
            .map_err(|e| format!("could not show a notification: {e}"))?;
    }

    serde_json::to_value(decision).map_err(|e| e.to_string())
}

/// Update the tray to match the paused state (§12.1, §15).
///
/// The kill switch's effect has to be visible without opening the window. One
/// somebody pressed that left no trace in the only part of the app still on
/// screen is one they press again to be sure.
#[tauri::command]
async fn refresh_tray(app: tauri::AppHandle<tauri::Wry>) -> Result<String, String> {
    let pool = revlocal_store::open(&database_path())
        .await
        .map_err(|e| format!("could not open the database: {e}"))?;
    let paused = revlocal_store::SettingStore::new(&pool)
        .is_paused()
        .await
        .map_err(|e| e.to_string());
    // What is waiting for a human belongs here rather than only on the approvals
    // screen: the window is closed most of the time, and an approval expires
    // whether or not anybody opened it (RL-1542). A count that cannot be read is
    // reported as none — a tooltip is not the place to raise a database error,
    // and the paused state below is the more important thing to still get right.
    let waiting = revlocal_store::PublishActionStore::new(&pool)
        .list_awaiting_approval()
        .await
        .map_or(0, |actions| actions.len());
    pool.close().await;

    let status = revlocal_daemon::notify::TrayStatus::of(paused?);
    let tooltip = status.tooltip_with_waiting(waiting);
    if let Some(tray) = app.tray_by_id("revlocal") {
        let _ = tray.set_tooltip(Some(&tooltip));
    }
    Ok(tooltip)
}

/// Where manual capability overrides live: beside the config (§11.2, RL-605).
fn overrides_path() -> std::path::PathBuf {
    config_path().parent().map_or_else(
        || std::path::PathBuf::from("target-overrides.json"),
        |dir| dir.join("target-overrides.json"),
    )
}

/// Whether this installation has never been set up (§15, RL-1205).
#[tauri::command]
async fn is_first_run() -> Result<bool, String> {
    let pool = revlocal_store::open(&database_path())
        .await
        .map_err(|e| format!("could not open the database: {e}"))?;
    let first = revlocal_daemon::onboarding::is_first_run(&pool).await;
    pool.close().await;

    first.map_err(|e| e.to_string())
}

/// Add the repository onboarding drafted (§14's `repo add`, through the wizard).
#[tauri::command]
async fn onboard_add_repo(
    draft: revlocal_daemon::onboarding::Draft,
) -> Result<serde_json::Value, String> {
    let pool = revlocal_store::open(&database_path())
        .await
        .map_err(|e| format!("could not open the database: {e}"))?;
    let added = revlocal_daemon::onboarding::add_repo(&pool, &draft, chrono::Utc::now()).await;
    pool.close().await;

    serde_json::to_value(added.map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

/// Discover and review one change, and hand back the result (§15's last step).
///
/// Runs through the same executor `watch` uses. A first review that worked
/// differently from every later one would demonstrate something that does not
/// exist, and the failure would arrive on the second day.
#[tauri::command]
async fn onboard_first_review(repo: String) -> Result<serde_json::Value, String> {
    let pool = revlocal_store::open(&database_path())
        .await
        .map_err(|e| format!("could not open the database: {e}"))?;

    // Discovery first: a repository added a moment ago has no changes recorded,
    // and "nothing to review" would be a confusing end to a walkthrough.
    let discovered = discover_for(&pool, &repo).await;
    let review = match discovered {
        Ok(()) => revlocal_daemon::onboarding::first_review(
            &pool,
            &global_config(),
            &data_dir(),
            &repo,
            chrono::Utc::now(),
        )
        .await
        .map_err(|e| e.to_string()),
        Err(detail) => Err(detail),
    };
    pool.close().await;

    serde_json::to_value(review?).map_err(|e| e.to_string())
}

/// §4.1's data directory: scratch worktrees live under it, keyed by run id.
fn data_dir() -> std::path::PathBuf {
    database_path().parent().map_or_else(
        || std::path::PathBuf::from("."),
        std::path::Path::to_path_buf,
    )
}

/// Record what this repository has, so the first review has something to look at.
async fn discover_for(pool: &revlocal_store::Pool, name: &str) -> Result<(), String> {
    use revlocal_vcs::VcsAdapter as _;

    let repos = revlocal_store::RepoStore::new(pool)
        .list()
        .await
        .map_err(|e| e.to_string())?;
    let repo = repos
        .iter()
        .find(|r| r.name == name)
        .ok_or_else(|| format!("no repository called {name}"))?;

    let changes = revlocal_vcs::GitAdapter::new()
        .discover(repo, None, 50)
        .await
        .map_err(|e| e.to_string())?;

    let at = chrono::Utc::now();
    for change in &changes {
        revlocal_store::ChangeStore::new(pool)
            .upsert(&revlocal_core::Change {
                id: revlocal_core::ChangeId::new(0),
                repo_id: repo.id,
                kind: change.kind,
                external_id: change.external_id.clone(),
                title: change.title.clone(),
                author_name: change.author_name.clone(),
                author_email: change.author_email.clone(),
                authored_at: change.authored_at,
                branch: change.branch.clone(),
                base_ref: change.base_ref.clone(),
                head_ref: change.head_ref.clone(),
                url: change.url.clone(),
                diff_stat: change.diff_stat,
                detected_at: at,
            })
            .await
            .map_err(|e| e.to_string())?;
    }

    Ok(())
}

/// The settings screen (§15 screen 6).
///
/// `doctor` is **not** run here. It shells out to every configured engine, and a
/// screen that did that on every open would make opening settings the slowest
/// thing in the app. The report starts empty — which the screen renders as
/// "doctor has not run yet", not as a pass — and the button runs it.
#[tauri::command]
async fn settings() -> Result<serde_json::Value, String> {
    let config = global_config();
    let view = revlocal_daemon::settings_view::gather_with_resolver(
        &config,
        &config_path().display().to_string(),
        &overrides_path().display().to_string(),
        revlocal_daemon::doctor::DoctorReport::default(),
        &revlocal_mcp::MacKeychain,
    )
    .await;

    serde_json::to_value(view).map_err(|e| e.to_string())
}

/// Re-run `doctor` and return the whole screen with its output (§15 screen 6).
///
/// The whole view, not just the report: doctor checks the same engines and
/// targets the rest of the screen describes, and refreshing half of it would
/// leave two answers about one machine on screen at once.
///
/// `spawn_blocking` because `doctor::gather` shells out and this is an async
/// command — blocking the runtime here would freeze the window while it ran,
/// which §15 forbids: the app stays usable while work happens.
#[tauri::command]
async fn run_doctor() -> Result<serde_json::Value, String> {
    let mut report = tokio::task::spawn_blocking(|| revlocal_daemon::doctor::gather(0))
        .await
        .map_err(|e| format!("doctor did not finish: {e}"))?;

    let config = global_config();

    // The install's own state, not just its tooling (RL-1551). Every
    // prerequisite can pass while no work moves at all, and this screen is where
    // somebody looks when it does not. A database that cannot be opened leaves
    // the section empty rather than failing the screen: the engine and target
    // checks above it are still worth showing.
    if let Ok(pool) = revlocal_store::open(&database_path()).await {
        report.install = revlocal_daemon::doctor::install_checks(
            &pool,
            i64::from(config.global.approval_ttl_hours),
            chrono::Utc::now(),
        )
        .await;
        pool.close().await;
    }
    let view = revlocal_daemon::settings_view::gather_with_resolver(
        &config,
        &config_path().display().to_string(),
        &overrides_path().display().to_string(),
        report,
        &revlocal_mcp::MacKeychain,
    )
    .await;

    serde_json::to_value(view).map_err(|e| e.to_string())
}

/// Configure the known HTTP MCP services using bearer values held only in Keychain.
///
/// Endpoint locations are product defaults discovered from this machine's existing
/// MCP configuration. The config file stores only deferred Keychain references.
#[tauri::command]
fn configure_mcp(suite_bearer: String) -> Result<(), String> {
    if suite_bearer.trim().is_empty() {
        return Err("enter the suite MCP bearer token".to_owned());
    }

    store_keychain_bearer("suite-mcp-bearer", &suite_bearer)?;

    let mut config = global_config();
    for (id, url, keychain_entry) in [
        (
            "andare",
            "https://us-central1-business-suite-7996a.cloudfunctions.net/claudemcp",
            "suite-mcp-bearer",
        ),
        (
            "trama",
            "https://us-central1-business-suite-7996a.cloudfunctions.net/tramamcp",
            "suite-mcp-bearer",
        ),
    ] {
        let mut headers = std::collections::BTreeMap::new();
        headers.insert(
            "Authorization".to_owned(),
            revlocal_core::SecretRef::parse(&format!("{{{{keychain:{keychain_entry}}}}}")),
        );
        config.mcp_servers.insert(
            id.to_owned(),
            revlocal_core::McpServerSettings {
                transport: "http".to_owned(),
                command: None,
                args: Vec::new(),
                url: Some(url.to_owned()),
                headers,
                extra: revlocal_core::config::Extra::default(),
            },
        );
    }

    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("could not create the configuration directory: {error}"))?;
    }
    let document = toml::to_string_pretty(&config)
        .map_err(|error| format!("could not encode the MCP configuration: {error}"))?;
    std::fs::write(&path, document)
        .map_err(|error| format!("could not write {}: {error}", path.display()))
}

/// Let the platform's folder chooser select a repository directory.
///
/// Returns an empty string when the chooser was dismissed — that is a decision,
/// not a failure, and reporting it as an error would put "could not open the
/// folder picker" on screen every time somebody changed their mind.
///
/// Each platform's chooser is a separate program that may not be installed. When
/// none is found the error says so and says what to do instead, because the path
/// field beside the button still works.
#[tauri::command]
fn pick_repository() -> Result<String, String> {
    #[cfg(target_os = "macos")]
    let attempts: [(&str, Vec<String>); 1] = [(
        "osascript",
        vec![
            "-e".to_owned(),
            "POSIX path of (choose folder with prompt \"Choose a repository\")".to_owned(),
        ],
    )];

    #[cfg(target_os = "windows")]
    let attempts: [(&str, Vec<String>); 1] = [(
        "powershell",
        vec![
            "-NoProfile".to_owned(),
            "-Command".to_owned(),
            "Add-Type -AssemblyName System.Windows.Forms; \
             $d = New-Object System.Windows.Forms.FolderBrowserDialog; \
             $d.Description = 'Choose a repository'; \
             if ($d.ShowDialog() -eq 'OK') { $d.SelectedPath }"
                .to_owned(),
        ],
    )];

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let attempts: [(&str, Vec<String>); 2] = [
        (
            "zenity",
            vec![
                "--file-selection".to_owned(),
                "--directory".to_owned(),
                "--title=Choose a repository".to_owned(),
            ],
        ),
        (
            "kdialog",
            vec![
                "--getexistingdirectory".to_owned(),
                ".".to_owned(),
                "--title".to_owned(),
                "Choose a repository".to_owned(),
            ],
        ),
    ];

    let mut missing = Vec::new();
    for (program, args) in attempts {
        match std::process::Command::new(program).args(&args).output() {
            Ok(output) if output.status.success() => {
                return String::from_utf8(output.stdout)
                    .map(|path| path.trim().to_owned())
                    .map_err(|_| "the folder picker returned a non-text path".to_owned());
            }
            // A non-zero exit is the chooser being dismissed.
            Ok(_) => return Ok(String::new()),
            Err(_) => missing.push(program),
        }
    }

    Err(format!(
        "no folder chooser is available ({} not found)\n  try: type or paste the repository path into the field instead",
        missing.join(", ")
    ))
}

/// Put a bearer into the login Keychain through stdin, never an argument.
#[cfg(target_os = "macos")]
fn store_keychain_bearer(account: &str, bearer: &str) -> Result<(), String> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    let mut child = Command::new("security")
        .args([
            "add-generic-password",
            "-U",
            "-s",
            "rev-local",
            "-a",
            account,
            "-w",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("could not access the macOS Keychain: {error}"))?;
    let Some(mut stdin) = child.stdin.take() else {
        return Err("could not open the private Keychain input channel".to_owned());
    };
    stdin
        .write_all(bearer.as_bytes())
        .map_err(|error| format!("could not save the bearer in Keychain: {error}"))?;
    // `security -w` reads until EOF. Waiting while this handle is alive leaves
    // the setup action spinning forever, with no configuration ever written.
    drop(stdin);
    let status = child
        .wait()
        .map_err(|error| format!("could not finish the Keychain update: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(
            "macOS Keychain rejected the bearer; unlock your login keychain and try again"
                .to_owned(),
        )
    }
}

#[cfg(not(target_os = "macos"))]
fn store_keychain_bearer(_account: &str, _bearer: &str) -> Result<(), String> {
    Err("bearer setup is currently available on macOS only".to_owned())
}

/// Bind a capability to a tool by hand (§11.2, RL-605).
///
/// The override is checked against the tool's own schema before it is written, so
/// a name the server does not expose is refused here rather than at the first
/// publish that needed it — which would be a review that silently did not publish.
#[tauri::command]
async fn set_override(
    target: String,
    capability: String,
    tool: String,
    args_json: String,
) -> Result<(), String> {
    let args: serde_json::Value =
        serde_json::from_str(&args_json).map_err(|e| format!("the arguments are not JSON: {e}"))?;

    revlocal_daemon::settings_view::set_override(
        &global_config(),
        &overrides_path().display().to_string(),
        &target,
        &capability,
        &tool,
        args,
    )
    .await
    .map_err(|e| e.to_string())
}

/// Remove a manual binding. Resolution takes over again.
#[tauri::command]
fn clear_override(target: String, capability: String) -> Result<(), String> {
    revlocal_daemon::settings_view::clear_override(
        &overrides_path().display().to_string(),
        &target,
        &capability,
    )
    .map(|_removed| ())
    .map_err(|e| e.to_string())
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

async fn git_for_repo(repo_id: i64) -> Result<(revlocal_core::Repo, std::path::PathBuf), String> {
    let pool = revlocal_store::open(&database_path())
        .await
        .map_err(|e| format!("could not open the database: {e}"))?;
    let repo = revlocal_store::RepoStore::new(&pool)
        .list()
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|repo| repo.id.get() == repo_id)
        .ok_or_else(|| format!("no repository with id {repo_id}"));
    pool.close().await;
    let repo = repo?;
    if !matches!(
        repo.kind,
        revlocal_core::RepoKind::Git | revlocal_core::RepoKind::GitHub
    ) {
        return Err(
            "starting a review by hand is currently available for Git repositories".to_owned(),
        );
    }
    let path = repo
        .local_path
        .as_deref()
        .map(std::path::PathBuf::from)
        .ok_or_else(|| "this repository has no local checkout to review".to_owned())?;
    Ok((repo, path))
}

/// Run one read-only git command against the repository under review.
///
/// Through `GitRunner`, never by spawning the binary here. The choke point is what
/// supplies the timeout and the non-interactive environment, and both matter
/// here specifically: this runs on a Tauri command, so a `git` that sat waiting
/// for a credential prompt would hang the window with nothing on screen to say
/// why. `git/cmd.rs` has a test that enforces this.
async fn git_text(path: &std::path::Path, args: &[&str]) -> Result<String, String> {
    revlocal_vcs::GitRunner::new()
        .run(path, args)
        .await
        .map(|output| output.stdout)
        .map_err(|error| error.to_string())
}

/// Branch heads in a local Git repository, without changing that repository.
#[tauri::command]
async fn review_branches(repo_id: i64) -> Result<serde_json::Value, String> {
    let (_, path) = git_for_repo(repo_id).await?;
    // `--show-current` is empty on a detached HEAD, which is a state and not an
    // error: the branch list is still worth showing.
    let current = git_text(&path, &["branch", "--show-current"])
        .await
        .map(|name| name.trim().to_owned())
        .ok()
        .filter(|name| !name.is_empty());

    let text = git_text(
        &path,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            &format!("--format={}", review::BRANCH_FORMAT),
            "refs/heads",
        ],
    )
    .await?;

    serde_json::to_value(review::branch_list(&text, current.as_deref()))
        .map_err(|error| error.to_string())
}

/// Recent commits on one selected branch. The fixed limit is declared in the UI.
#[tauri::command]
async fn review_commits(repo_id: i64, branch: String) -> Result<serde_json::Value, String> {
    let (_, path) = git_for_repo(repo_id).await?;
    let text = git_text(
        &path,
        &[
            "log",
            "--max-count=100",
            &format!("--format={}", review::COMMIT_FORMAT),
            "--no-decorate",
            &branch,
        ],
    )
    .await?;
    serde_json::to_value(review::parse_commits(&text)).map_err(|error| error.to_string())
}

/// Resolve a ref to the commit it names right now.
async fn resolve(path: &std::path::Path, revision: &str) -> Result<String, String> {
    // Named in the error, because git's own is "fatal: Needed a single revision",
    // which says nothing about which revision or where it came from.
    git_text(
        path,
        &["rev-parse", "--verify", &format!("{revision}^{{commit}}")],
    )
    .await
    .map(|sha| sha.trim().to_owned())
    .map_err(|error| format!("could not resolve `{revision}` in this repository: {error}"))
}

/// Queue a review the user asked for, and start it in the background.
///
/// Returns as soon as the run exists. The review itself takes as long as the
/// engine takes, and a command that only answered once the engine had finished
/// left the window looking hung with no run to open — which is the opposite of
/// what "start a review now" is for.
///
/// `revision` is the commit to review. `base` turns it into a range: a branch's
/// own work when it is another branch, and the whole repository when it is git's
/// empty tree.
#[tauri::command]
async fn start_review(
    app: tauri::AppHandle,
    repo_id: i64,
    branch: Option<String>,
    revision: String,
    base: Option<String>,
) -> Result<serde_json::Value, String> {
    let (repo, path) = git_for_repo(repo_id).await?;

    let sha = resolve(&path, &revision).await?;
    // Resolved here rather than at review time so a branch moving between the
    // click and the run cannot change what was asked for. The empty tree is not a
    // ref and resolves to itself.
    let base = match base.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
        None => None,
        Some(review::EMPTY_TREE) => Some(review::EMPTY_TREE.to_owned()),
        Some(other) => Some(resolve(&path, other).await?),
    };

    let subject = git_text(&path, &["log", "-1", "--format=%s", &sha])
        .await
        .unwrap_or_default();
    let author = git_text(&path, &["log", "-1", "--format=%an%x1f%ae%x1f%aI", &sha])
        .await
        .unwrap_or_default();
    let mut fields = author.trim().split('\u{1f}');
    let author = (
        fields.next().map(str::to_owned).filter(|a| !a.is_empty()),
        fields.next().map(str::to_owned).filter(|a| !a.is_empty()),
        fields.next().and_then(|at| {
            chrono::DateTime::parse_from_rfc3339(at)
                .ok()
                .map(|at| at.with_timezone(&chrono::Utc))
        }),
    );

    let branch = branch.filter(|branch| !branch.trim().is_empty());
    let scope = review::scope_of(base.as_deref(), branch.as_deref(), &sha);
    let change = review::change_for(&review::ResolvedRequest {
        repo_id: repo.id,
        sha: &sha,
        subject: subject.trim(),
        branch: branch.as_deref(),
        base: base.as_deref(),
        author,
        at: chrono::Utc::now(),
    });

    let pool = revlocal_store::open(&database_path())
        .await
        .map_err(|e| format!("could not open the database: {e}"))?;
    let queued =
        revlocal_daemon::executor::enqueue_manual(&pool, &repo, &change, chrono::Utc::now()).await;
    pool.close().await;
    let run_id = queued.map_err(|error| error.to_string())?.id;

    let db = database_path();
    let config = global_config();
    let data = data_dir();
    tauri::async_runtime::spawn(async move {
        let result = async {
            let pool = revlocal_store::open(&db).await.map_err(|e| e.to_string())?;
            let mut config = config;
            config.global.mode = revlocal_daemon::dashboard::global_mode(&pool)
                .await
                .map_err(|e| e.to_string())?;
            let sink = revlocal_tauri::events::EventBridge::new(Arc::new(WindowSink { app }));
            let outcome = revlocal_daemon::executor::execute_run(
                &pool,
                &config,
                &sink,
                &data,
                run_id,
                chrono::Utc::now(),
                &cancel_token(),
            )
            .await
            .map_err(|error| error.to_string());
            pool.close().await;
            outcome
        }
        .await;
        // The run row carries the outcome either way — this is for a terminal, not
        // for the screen, which reads the run.
        match result {
            Err(error) => eprintln!("rev-local: run #{} could not run: {error}", run_id.get()),
            Ok(Err(held)) => eprintln!("rev-local: {held}"),
            Ok(Ok(_)) => {}
        }
    });

    serde_json::to_value(review::StartedReview {
        run_id: run_id.get(),
        status: revlocal_core::RunStatus::Queued.as_str().to_owned(),
        scope,
    })
    .map_err(|error| error.to_string())
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
    // A `pending` action is safe to send under the selected autonomy mode. The
    // queue records its receipt or failure; a network problem therefore never
    // turns into a lost finding, and never turns into an error about the filing
    // — which succeeded.
    let status = match status {
        Ok(status) => {
            if status == revlocal_core::PublishActionStatus::Pending {
                let _ = dispatch_andare(pool.clone(), &global_config()).await;
            }
            Ok(status)
        }
        Err(error) => Err(error),
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

/// The step a scripted flow capture is currently on (RL-1103, §16.4).
///
/// Reads the **last line of the driver's own labels file** — the same file
/// `framewatch watch --labels-file` is tailing to caption frames. That is not a
/// convenience: §16.4 wants captions and actions that cannot drift apart, and one
/// file written by one process is the only way to get that mechanically. A
/// separate "advance" channel would eventually caption a frame with the step
/// before or after the one it shows, which is worse than no caption at all.
///
/// Returns "" when no flow is running, which is every real user, and
/// [`FLOW_IDLE`] when one is configured but has not named a step yet.
///
/// Those two have to be distinguishable. The first version returned "" for both,
/// so the front end — which starts polling only once it sees a flow — never
/// started when the driver had not written its first label yet. The capture hung
/// waiting for frames from a window that was never going to change.
#[tauri::command]
fn flow_step() -> String {
    let Some(path) = std::env::var_os("REVLOCAL_FLOW_FILE") else {
        return String::new();
    };

    // A missing or unreadable file is "no step yet", not an error: the driver
    // creates it and the app may look before the first write.
    let last = std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .rfind(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .trim()
        .to_owned();

    if last.is_empty() {
        FLOW_IDLE.to_owned()
    } else {
        last
    }
}

/// What [`flow_step`] answers while a flow is configured and has named no step.
pub const FLOW_IDLE: &str = "idle";

/// Which onboarding step a capture harness asked for, or "" (RL-1102, §16.4).
///
/// Onboarding is a *flow*, and §16.4's single-shot harness photographs one screen
/// at a time. Until RL-1103's scripted flow capture exists, this is how each step
/// gets a frame — one shot per step rather than one shot of a wizard nobody
/// clicked through.
#[tauri::command]
fn initial_onboarding_step() -> String {
    std::env::var("REVLOCAL_ONBOARDING").unwrap_or_default()
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

/// Which run a capture harness asked for, or 0 (RL-1102, §16.4).
///
/// The run-detail screen has the same shape as the repository screen: it is about
/// *a* run, and opening it without one photographs the words "select a run".
#[tauri::command]
fn initial_run() -> i64 {
    std::env::var("REVLOCAL_RUN")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

/// Stop everything (SPEC §12.1).
///
/// One line of delegation, like every command here.
#[tauri::command]
async fn kill_switch() -> Result<(), String> {
    // Both halves, and in this order. The database is the source of truth across
    // restarts and is what stops the *next* pass starting; the token is what
    // stops the engine running *now*. Doing only the second would leave a paused
    // app that resumes reviewing a minute later.
    with_store(|pool| async move {
        revlocal_store::SettingStore::new(&pool)
            .set_paused(true, chrono::Utc::now())
            .await
            .map_err(|e| e.to_string())
    })
    .await?;

    KILL.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .engage();
    Ok(())
}

/// Release the kill switch (§12.1: it has to be reversible).
///
/// There was no way to do this from the app at all. A switch that stops
/// everything and cannot be released is one people fix by deleting the database.
#[tauri::command]
async fn resume() -> Result<(), String> {
    with_store(|pool| async move {
        revlocal_store::SettingStore::new(&pool)
            .set_paused(false, chrono::Utc::now())
            .await
            .map_err(|e| e.to_string())
    })
    .await?;

    // A fresh token for new work. The old one stays cancelled, so anything still
    // holding it does not quietly come back to life.
    let mut switch = KILL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *switch = switch.released();
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

// --- the autopilot (RL-1501, RL-1506) ---------------------------------------

/// The database key holding whether the loop runs.
///
/// In the database rather than in memory so it survives a restart: an app that
/// forgets it was switched off is one you switch off twice.
/// The daemon owns this key: `doctor` reads the same setting this toggle writes,
/// and two definitions of one key is how they drift apart (RL-1551).
use revlocal_daemon::autopilot::SETTING_AUTOPILOT;

/// What the loop is doing, as a screen needs to read it at any moment.
///
/// §15 says live updates arrive as events, and they do — but a screen that opens
/// *between* two ticks would otherwise have nothing to show for a minute, which
/// reads exactly like an app that is not running. So the last tick is kept here
/// and served on demand as well as pushed.
#[derive(Debug, Clone, serde::Serialize)]
struct AutopilotState {
    /// Whether the loop is switched on.
    enabled: bool,
    /// Whether a tick is executing right now.
    ticking: bool,
    /// How long between ticks.
    interval_secs: u64,
    /// When the last tick started, RFC 3339.
    last_tick_at: Option<String>,
    /// One sentence of plain English about the last tick.
    last_line: String,
    /// Why the last tick could not run at all, if it could not.
    last_error: Option<String>,
    /// Everything the last tick did not do, and why (§18).
    notes: Vec<String>,
}

impl AutopilotState {
    /// Off until the database says otherwise.
    ///
    /// The reverse default would have a first launch start reviewing before the
    /// window has finished asking which repositories to watch.
    const fn new() -> Self {
        Self {
            enabled: false,
            ticking: false,
            interval_secs: revlocal_daemon::autopilot::DEFAULT_INTERVAL_SECS,
            last_tick_at: None,
            last_line: String::new(),
            last_error: None,
            notes: Vec::new(),
        }
    }
}

static AUTOPILOT: std::sync::Mutex<AutopilotState> = std::sync::Mutex::new(AutopilotState::new());

/// The kill switch this process's work actually watches (§12.1, RL-1522).
///
/// One for the whole app. Every drain is handed `token()`, so engaging this
/// cancels whatever is running — `supervise` watches the token and terminates the
/// engine's process *group*, which is why stopping a live review needs no pids.
///
/// Behind a lock because a cancelled `CancellationToken` cannot be un-cancelled,
/// and that is the right shape: work told to stop must not silently un-stop.
/// Resuming installs a fresh one, and anything still holding the old token stays
/// cancelled.
static KILL: std::sync::LazyLock<std::sync::Mutex<revlocal_daemon::KillSwitch>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(revlocal_daemon::KillSwitch::new()));

/// The token to hand to work that should stop when the switch is pulled.
fn cancel_token() -> tokio_util::sync::CancellationToken {
    KILL.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .token()
        .clone()
}

/// Held for the duration of a pass, so two never overlap.
///
/// Separate from `AutopilotState::ticking`, which is what a screen reads. A flag
/// somebody reads and then acts on is a race: the timer and the "check now"
/// button both looked, both saw `false`, and both drained the same queue. The
/// executor would have run one queued review twice.
static AUTOPILOT_RUNNING: AtomicBool = AtomicBool::new(false);

/// Change the shared state and return the new value.
///
/// A poisoned lock is recovered from rather than propagated: the state is a
/// status line, and refusing to report status because a previous reporter
/// panicked is the worst possible time to go quiet.
fn update_autopilot(change: impl FnOnce(&mut AutopilotState)) -> AutopilotState {
    let mut state = AUTOPILOT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    change(&mut state);
    state.clone()
}

/// Push the current state to the window.
fn publish_autopilot(app: &tauri::AppHandle, state: &AutopilotState) {
    if let Err(error) = app.emit(revlocal_tauri::events::AUTOPILOT_EVENT, state) {
        eprintln!("revlocal: no window to deliver autopilot status to: {error}");
    }
}

/// Run one pass of the loop and record what it did.
///
/// Everything that decides lives in `revlocal_daemon::autopilot::tick`, which
/// `revlocal watch` runs too. This opens the store, builds the publish target and
/// reports the result — the three things that need the app's own configuration.
async fn autopilot_tick(app: &tauri::AppHandle) {
    if AUTOPILOT_RUNNING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    publish_autopilot(app, &update_autopilot(|state| state.ticking = true));

    let outcome = async {
        let pool = revlocal_store::open(&database_path())
            .await
            .map_err(|e| format!("could not open the database: {e}"))?;

        // The database setting is the operational ceiling chosen in the UI; the
        // file supplies every other default. Same resolution the manual drain
        // uses, so a run started by the loop and one started by a button are
        // governed identically.
        let mut config = global_config();
        config.global.mode = revlocal_daemon::dashboard::global_mode(&pool)
            .await
            .map_err(|e| e.to_string())?;

        // A missing bearer is not a reason to skip reviewing, or to skip
        // publishing: the local report target needs no configuration and is
        // registered by the tick itself. This adds Andare when it can be built,
        // and the pass reports any action it could not route.
        // Each target built independently: one that cannot be configured must not
        // stop the others delivering. A repository can want a wiki page and no
        // issue, or the reverse.
        let targets: Vec<Arc<dyn revlocal_publish::PublishTarget>> = andare_target(&config)
            .into_iter()
            .chain(trama_target(&config))
            .collect();

        let sink =
            revlocal_tauri::events::EventBridge::new(Arc::new(WindowSink { app: app.clone() }));
        let report = revlocal_daemon::autopilot::tick(
            &pool,
            &config,
            &sink,
            &data_dir(),
            &targets,
            chrono::Utc::now(),
            &cancel_token(),
        )
        .await
        .map_err(|error| error.to_string());

        pool.close().await;
        report
    }
    .await;

    AUTOPILOT_RUNNING.store(false, Ordering::Release);
    let state = update_autopilot(|state| {
        state.ticking = false;
        state.last_tick_at = Some(chrono::Utc::now().to_rfc3339());
        match &outcome {
            Ok(report) => {
                state.last_line = report.line();
                state.notes.clone_from(&report.notes);
                state.last_error = None;
            }
            Err(error) => {
                state.last_error = Some(error.clone());
                state.last_line = "The last pass could not run.".to_owned();
            }
        }
    });
    publish_autopilot(app, &state);
}

/// Start the loop. Called once, from `setup`.
///
/// The task lives as long as the process and checks the switch each time round
/// rather than being started and stopped: a task that is torn down and rebuilt
/// has a window in which "off" and "starting" are indistinguishable, and this is
/// the one part of the app whose whole job is to be unambiguous about whether it
/// is running.
fn spawn_autopilot(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        // Read the stored switch before the first tick, so a restart resumes
        // whatever was chosen rather than the default.
        if let Ok(pool) = revlocal_store::open(&database_path()).await {
            let stored = revlocal_store::SettingStore::new(&pool)
                .get(SETTING_AUTOPILOT)
                .await
                .ok()
                .flatten();
            pool.close().await;
            let enabled = stored.as_deref() == Some("on");
            publish_autopilot(&app, &update_autopilot(|state| state.enabled = enabled));
        }

        let interval =
            std::time::Duration::from_secs(revlocal_daemon::autopilot::DEFAULT_INTERVAL_SECS);
        loop {
            let enabled = AUTOPILOT
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .enabled;
            if enabled {
                autopilot_tick(&app).await;
            }
            tokio::time::sleep(interval).await;
        }
    });
}

/// The home directory the login item lives under.
fn home_dir() -> std::path::PathBuf {
    std::env::var_os("HOME").map_or_else(|| std::path::PathBuf::from("."), std::path::PathBuf::from)
}

/// What this app should ask the OS to launch at login.
///
/// The `.app` bundle on macOS, the executable elsewhere. `current_exe` inside a
/// bundle points at `Contents/MacOS/revlocal-desktop`, and a login item pointing
/// there launches the binary without its bundle — which starts, has no icon, no
/// bundle identifier and no notification permissions, and is the kind of failure
/// that looks like it worked.
fn launch_target() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("could not locate this app: {e}"))?;

    if cfg!(target_os = "macos") {
        // .../rev-local.app/Contents/MacOS/revlocal-desktop -> .../rev-local.app
        if let Some(bundle) = exe
            .ancestors()
            .find(|path| path.extension().is_some_and(|ext| ext == "app"))
        {
            return Ok(bundle.display().to_string());
        }
        // Running from `cargo run`, with no bundle to point at. Reported rather
        // than guessed: a login item for a debug binary in `target/` is one that
        // breaks the next time somebody runs `cargo clean`.
        return Err(
            "this build is not in an app bundle, so it cannot be started at login
  try: install the packaged app first"
                .to_owned(),
        );
    }

    Ok(exe.display().to_string())
}

/// Whether rev-local starts when you log in (§4.2, RL-1517).
#[tauri::command]
fn startup_status() -> Result<serde_json::Value, String> {
    serde_json::to_value(revlocal_daemon::startup::status(&home_dir())).map_err(|e| e.to_string())
}

/// Turn starting-at-login on or off.
#[tauri::command]
fn set_startup(enabled: bool) -> Result<serde_json::Value, String> {
    let home = home_dir();
    if enabled {
        revlocal_daemon::startup::enable(&home, &launch_target()?).map_err(|e| e.to_string())?;
    } else {
        revlocal_daemon::startup::disable(&home).map_err(|e| e.to_string())?;
    }

    // Read back rather than assume. The whole point of this setting is that its
    // failure mode is silently not being there.
    serde_json::to_value(revlocal_daemon::startup::status(&home)).map_err(|e| e.to_string())
}

/// What the loop is doing (§15's "what is it doing right now").
#[tauri::command]
fn autopilot_status() -> Result<serde_json::Value, String> {
    let state = AUTOPILOT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    serde_json::to_value(state).map_err(|e| e.to_string())
}

/// Switch the loop on or off, and remember which.
#[tauri::command]
async fn set_autopilot(enabled: bool, app: tauri::AppHandle) -> Result<serde_json::Value, String> {
    let pool = revlocal_store::open(&database_path())
        .await
        .map_err(|e| format!("could not open the database: {e}"))?;
    let stored = revlocal_store::SettingStore::new(&pool)
        .set(
            SETTING_AUTOPILOT,
            if enabled { "on" } else { "off" },
            chrono::Utc::now(),
        )
        .await
        .map_err(|e| e.to_string());
    pool.close().await;
    stored?;

    let state = update_autopilot(|state| state.enabled = enabled);
    publish_autopilot(&app, &state);

    // Switching it on and then waiting a minute for the first pass is the
    // difference between "it works" and "nothing happened".
    if enabled {
        let handle = app.clone();
        tauri::async_runtime::spawn(async move { autopilot_tick(&handle).await });
    }

    serde_json::to_value(state).map_err(|e| e.to_string())
}

/// Run one pass now, without waiting for the interval.
#[tauri::command]
async fn autopilot_now(app: tauri::AppHandle) -> Result<(), String> {
    if AUTOPILOT_RUNNING.load(Ordering::Acquire) {
        return Err("a pass is already running".to_owned());
    }
    // The check above is for the message; `autopilot_tick` holds the real guard.
    tauri::async_runtime::spawn(async move { autopilot_tick(&app).await });
    Ok(())
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
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![
            kill_switch,
            resume,
            dashboard,
            queue_status,
            start_queued_runs,
            set_mode,
            get_run,
            get_transcript,
            retry_target,
            list_approvals,
            approve_action,
            approve_run,
            reject_action,
            edit_payload,
            notify,
            refresh_tray,
            is_first_run,
            onboard_add_repo,
            onboard_first_review,
            settings,
            run_doctor,
            configure_mcp,
            pick_repository,
            set_override,
            clear_override,
            get_repository,
            review_branches,
            review_commits,
            start_review,
            save_repo_config,
            list_findings,
            suppress_finding,
            file_to_andare,
            initial_screen,
            initial_repo,
            initial_run,
            initial_onboarding_step,
            flow_step,
            autopilot_status,
            set_autopilot,
            autopilot_now,
            startup_status,
            set_startup
        ])
        .setup(|app| {
            let handle = app.handle().clone();

            let sink: Arc<dyn UiEventSink> = Arc::new(WindowSink {
                app: handle.clone(),
            });
            // The bridge is what the daemon will be handed as its RunEventSink.
            // Held in Tauri's state so it lives as long as the app does.
            app.manage(revlocal_tauri::EventBridge::new(sink));

            // §4.2: the daemon runs in-process, so "the app is open" has to be
            // the same thing as "rev-local is watching". Until this existed it
            // was not — discovery and the queue both waited for a button.
            spawn_autopilot(handle.clone());

            // §15: the kill switch is reachable from every screen and from the
            // tray. The tray is also what makes closing the window survivable —
            // hiding a window with no way back is just losing it.
            TrayIconBuilder::with_id("revlocal")
                .icon(
                    app.default_window_icon()
                        .cloned()
                        .ok_or("the app has no window icon to use for the tray")?,
                )
                // Replaced as soon as the front end asks, but a tray that said
                // nothing until then would be blank at exactly the moment
                // somebody looks — startup after a kill switch.
                .tooltip(revlocal_daemon::notify::TrayStatus::Running.tooltip())
                .menu(&tray_menu(&handle)?)
                .on_menu_event(|app, event| match TrayItem::from_id(event.id().as_ref()) {
                    Some(TrayItem::Show) => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    Some(TrayItem::KillSwitch) => {
                        // Spawned rather than blocked on: this runs on the menu
                        // thread, and the tooltip is set from inside so it can
                        // only say "paused" once something actually paused.
                        let tray = app.tray_by_id("revlocal");
                        tauri::async_runtime::spawn(async move {
                            match kill_switch().await {
                                Ok(()) => {
                                    if let Some(tray) = tray {
                                        let _ = tray.set_tooltip(Some(
                                            revlocal_daemon::notify::TrayStatus::Paused.tooltip(),
                                        ));
                                    }
                                }
                                // A kill switch that failed leaves the tray saying
                                // "reviewing", which is the truth: nothing stopped.
                                Err(error) => {
                                    eprintln!("revlocal: the kill switch failed: {error}");
                                }
                            }
                        });
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
