//! The IPC surface and the event bridge (RL-1101, SPEC §4.2, §15).
//!
//! Criterion 4 says the IPC layer is "asserted by review". Review is the weakest
//! guard there is for a property that erodes gradually, so
//! `the_ipc_layer_holds_no_business_logic` reads the source and fails on the
//! shapes that erosion takes. Review still happens; it just is not the only thing
//! standing between this layer and a `filter` somebody added at 6pm.
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use std::sync::Arc;

use revlocal_core::{RunId, RunStatus};
use revlocal_daemon::state_machine::{RunEvent, RunEventSink};
use revlocal_tauri::events::{RecordingSink, RUN_EVENT};
use revlocal_tauri::{EventBridge, IpcError, IpcRequest, UiEvent};

#[test]
fn a_daemon_event_reaches_the_ui_without_a_poll() {
    // Criterion 2, and §15's rule that live updates come from events rather than
    // polling the DB. Nothing here reads a database: the daemon announces a
    // transition and the bridge forwards it, which is the whole mechanism.
    let sink = Arc::new(RecordingSink::new());
    let bridge = EventBridge::new(Arc::clone(&sink) as Arc<_>);

    bridge.emit(RunEvent::StageChanged {
        run: RunId::new(7),
        from: RunStatus::Queued,
        to: RunStatus::Reviewing,
    });

    let events = sink.events();
    assert_eq!(events.len(), 1, "the UI must see the transition");
    assert_eq!(
        events[0],
        UiEvent::StageChanged {
            run_id: 7,
            from: "queued".to_owned(),
            to: "reviewing".to_owned(),
        }
    );
}

#[test]
fn every_run_event_variant_reaches_the_ui() {
    // Filtering in the bridge would mean the UI silently missing a state the
    // daemon considered worth announcing. The interesting events are the ones
    // nobody predicted wanting.
    let sink = Arc::new(RecordingSink::new());
    let bridge = EventBridge::new(Arc::clone(&sink) as Arc<_>);

    for event in [
        RunEvent::StageChanged {
            run: RunId::new(1),
            from: RunStatus::Queued,
            to: RunStatus::Reviewing,
        },
        RunEvent::Interrupted {
            run: RunId::new(2),
            stuck_in: RunStatus::Reviewing,
        },
        RunEvent::ReEnqueued {
            previous: RunId::new(2),
            run: RunId::new(3),
            attempt: 2,
        },
        RunEvent::GivenUp {
            run: RunId::new(4),
            reason: "attempt ceiling reached".to_owned(),
        },
    ] {
        bridge.emit(event);
    }

    let events = sink.events();
    assert_eq!(events.len(), 4, "no variant may be dropped by the bridge");
    assert_eq!(
        events.iter().map(UiEvent::run_id).collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
}

#[test]
fn the_ui_event_is_a_wire_format_not_the_daemons_enum() {
    // Serialising the daemon's own enum would make a refactor inside the daemon a
    // silent breakage in a front end written against its field names.
    let event = UiEvent::from(RunEvent::GivenUp {
        run: RunId::new(9),
        reason: "attempt ceiling reached".to_owned(),
    });

    let json = serde_json::to_value(&event).unwrap_or_default();
    assert_eq!(json["kind"], "given_up");
    assert_eq!(json["run_id"], 9);
    assert_eq!(json["reason"], "attempt ceiling reached");

    // And it round-trips, because the front end is not the only consumer — the
    // CLI's follow mode reads the same stream.
    let back: UiEvent = serde_json::from_value(json).unwrap_or(UiEvent::GivenUp {
        run_id: 0,
        reason: String::new(),
    });
    assert_eq!(back, event);
}

#[test]
fn there_is_one_event_channel_not_one_per_variant() {
    // A front end that must subscribe to six channels will miss the seventh when
    // it is added, and the discriminant is in the payload anyway.
    assert_eq!(RUN_EVENT, "revlocal://run-event");
}

#[test]
fn every_command_names_itself_and_declares_whether_it_mutates() {
    // §15 requires every destructive or outbound action to name its target. A
    // screen cannot enforce that for a command it cannot tell apart from a read,
    // so the classification lives with the command.
    let reads = [
        IpcRequest::ListRepos,
        IpcRequest::GetRepo { repo_id: 1 },
        IpcRequest::ListRuns {
            repo_id: None,
            limit: 50,
        },
        IpcRequest::GetRun { run_id: 1 },
        IpcRequest::ListFindings {
            repo_id: None,
            limit: 50,
        },
        IpcRequest::ListApprovals,
        IpcRequest::KillSwitchState,
    ];

    for request in &reads {
        assert!(!request.name().is_empty());
        assert!(
            !request.mutates(),
            "{} is a read and must not be classified as mutating",
            request.name()
        );
    }

    assert!(
        IpcRequest::KillSwitch.mutates(),
        "the kill switch changes the world and must say so"
    );

    // Names are distinct, or the front end cannot address them.
    let mut names: Vec<&str> = reads.iter().map(IpcRequest::name).collect();
    names.push(IpcRequest::KillSwitch.name());
    let unique: std::collections::BTreeSet<&str> = names.iter().copied().collect();
    assert_eq!(unique.len(), names.len(), "command names must be distinct");
}

#[test]
fn requests_round_trip_as_tagged_json() {
    // The front end constructs these. An untagged shape would make `get_repo` and
    // `get_run` indistinguishable once both carry a single integer.
    let request = IpcRequest::GetRun { run_id: 42 };
    let json = serde_json::to_value(&request).unwrap_or_default();

    assert_eq!(json["command"], "get_run");
    assert_eq!(json["run_id"], 42);

    let back: IpcRequest = serde_json::from_value(json).unwrap_or(IpcRequest::ListRepos);
    assert_eq!(back, request);
}

#[test]
fn errors_cross_the_boundary_as_data_not_as_a_sentence() {
    // A command returning Err(String) gives the front end something to display and
    // nothing to branch on. §18 wants an error to say what to do, and a UI can
    // only do that if it can tell these apart.
    let unavailable = IpcError::DaemonUnavailable {
        remediation: "restart rev-local".to_owned(),
    };
    let missing = IpcError::NoSuchRepo { repo_id: 3 };

    assert!(
        unavailable.is_retryable(),
        "a stopped daemon can be restarted"
    );
    assert!(
        !missing.is_retryable(),
        "retrying will not conjure a repository"
    );
    assert_eq!(unavailable.remediation(), Some("restart rev-local"));
    assert_eq!(
        missing.remediation(),
        None,
        "a stale window is not something the user can act on"
    );

    let json = serde_json::to_value(&unavailable).unwrap_or_default();
    assert_eq!(json["error"], "daemon_unavailable");
}

#[test]
fn the_ipc_layer_holds_no_business_logic() -> Result<(), String> {
    // Criterion 4, which the issue says is "asserted by review". Review is the
    // weakest guard there is for a property that erodes gradually — the first time
    // a screen needs a number the daemon does not expose, the cheap fix is to
    // compute it here, and six months later the UI and the CLI disagree about what
    // "queue depth" means because one of them does its own arithmetic.
    //
    // So the property is also checked mechanically. Review still happens; it is
    // just no longer the only thing standing between this layer and a `filter`
    // somebody added at 6pm.
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ipc.rs"),
    )
    .map_err(|e| format!("reading src/ipc.rs: {e}"))?;

    // Shapes that mean a decision is being made here rather than delegated.
    let banned = [
        ".filter(", ".fold(", ".sum()", ".count()", ".sort", ".retain(", "sqlx::", "SELECT ",
    ];

    let mut offenders = Vec::new();
    for (number, line) in source.lines().enumerate() {
        // Prose about the rule is not a violation of it.
        let code = line.split("//").next().unwrap_or(line);
        for needle in banned {
            if code.contains(needle) {
                offenders.push(format!("{}: {}", number + 1, line.trim()));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "the IPC layer must delegate, not decide. Move this into the daemon, where \
         the CLI can reach it too:\n{}",
        offenders.join("\n")
    );
    Ok(())
}

#[test]
fn the_ipc_layer_does_not_reach_a_webview() -> Result<(), String> {
    // The structural half of the same property. A layer that cannot touch a
    // window cannot grow a UI-shaped decision, and it is what lets these tests run
    // without linking a browser engine.
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ipc.rs"),
    )
    .map_err(|e| format!("reading src/ipc.rs: {e}"))?;

    assert!(
        !source.contains("tauri::"),
        "src/ipc.rs must compile without Tauri; the command wrappers live in \
         commands.rs behind the `desktop` feature"
    );
    Ok(())
}

// --- window and tray lifecycle (criterion 3) ------------------------------

use revlocal_tauri::lifecycle::{on_close, CloseAction, CloseCause, TrayItem};

#[test]
fn closing_the_window_keeps_the_daemon_running() {
    // Criterion 3, first half. §4.2 runs the daemon in-process and says the app
    // must be running to review — so the window closing and the app quitting are
    // different things. Conflating them stops every review the first time somebody
    // hits Cmd-W out of habit.
    let action = on_close(CloseCause::WindowControl);

    assert_eq!(action, CloseAction::HideToTray);
    assert!(action.keeps_daemon_running());
}

#[test]
fn quitting_actually_quits() {
    // Criterion 3, second half, and the one that is tempting to get wrong in the
    // safe-looking direction. An app that can only be hidden is one people
    // force-kill — and a force-killed daemon leaves runs stuck mid-stage for
    // RL-501's recovery to find on the next start.
    for cause in [CloseCause::QuitRequested, CloseCause::SystemShutdown] {
        let action = on_close(cause);
        assert_eq!(action, CloseAction::Exit, "{cause:?} must exit");
        assert!(!action.keeps_daemon_running(), "{cause:?}");
    }
}

#[test]
fn the_kill_switch_is_in_the_tray() {
    // §15: "the kill switch is reachable from every screen and from the tray."
    // The window has it in the header; this is the other half.
    assert!(
        TrayItem::ALL.contains(&TrayItem::KillSwitch),
        "the tray must carry the kill switch"
    );

    // Before Quit, because somebody reaching for the tray in a hurry is likelier
    // to want the first than the second.
    let order: Vec<&str> = TrayItem::ALL.iter().map(|item| item.id()).collect();
    let kill = order.iter().position(|id| *id == "kill_switch");
    let quit = order.iter().position(|id| *id == "quit");
    assert!(kill < quit, "kill switch must come before quit: {order:?}");
}

#[test]
fn every_tray_id_round_trips() {
    // The menu is built from `TrayItem::ALL` and dispatched through `from_id`, so
    // an item that does not round-trip is a menu entry that silently does nothing.
    for item in TrayItem::ALL {
        assert_eq!(TrayItem::from_id(item.id()), Some(item), "{item:?}");
        assert!(!item.label().is_empty(), "{item:?} needs a label");
    }
    assert_eq!(TrayItem::from_id("not-a-menu-item"), None);
}

#[test]
fn tray_labels_say_what_they_do() {
    // §15: every destructive action names what it does. "Quit" on its own is fine;
    // a kill switch labelled "Kill switch" alone is not obviously a global stop.
    assert!(
        TrayItem::KillSwitch.label().contains("stop everything"),
        "got {:?}",
        TrayItem::KillSwitch.label()
    );
    assert!(TrayItem::Quit.label().contains("Quit"));
}

// --- the front end's types mirror these (RL-1101) --------------------------

#[test]
fn every_ui_event_variant_is_present_in_the_typescript_union() -> Result<(), String> {
    // The front end declares `UiEvent` as a discriminated union in `ui/src/ipc.ts`.
    // Nothing in either language checks the other, so a variant added here and
    // forgotten there does not fail a build — it produces a row the UI renders as
    // `undefined`, at exactly the moment somebody is trying to understand why a
    // run stopped.
    //
    // Cheap to check, and this is the check.
    let source = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ui/src/ipc.ts"),
    )
    .map_err(|e| format!("reading ui/src/ipc.ts: {e}"))?;

    // The serde tag for each variant, taken from a real serialisation rather than
    // from a list written by hand — a hand-written list drifts the same way the
    // TypeScript does.
    let variants = [
        UiEvent::StageChanged {
            run_id: 1,
            from: "queued".to_owned(),
            to: "reviewing".to_owned(),
        },
        UiEvent::Interrupted {
            run_id: 1,
            stuck_in: "reviewing".to_owned(),
        },
        UiEvent::ReEnqueued {
            previous_run_id: 1,
            run_id: 2,
            attempt: 2,
        },
        UiEvent::GivenUp {
            run_id: 1,
            reason: "ceiling".to_owned(),
        },
    ];

    for variant in variants {
        let json = serde_json::to_value(&variant).map_err(|e| e.to_string())?;
        let tag = json["kind"]
            .as_str()
            .ok_or("every UiEvent must carry a kind")?;
        assert!(
            source.contains(&format!("kind: '{tag}'")),
            "ui/src/ipc.ts has no case for `{tag}`; the UI would render it as \
             undefined. Add it to the UiEvent union and to `describe`."
        );

        // Every field too. A renamed field is the quieter version of the same bug.
        if let Some(fields) = json.as_object() {
            for name in fields.keys().filter(|name| *name != "kind") {
                assert!(
                    source.contains(&format!("{name}:")),
                    "ui/src/ipc.ts never mentions `{name}`, a field of `{tag}`"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn the_front_end_does_not_poll() -> Result<(), String> {
    // §15: "live updates come from Tauri events, not polling the DB." That is a
    // rule about the front end, so it is checked in the front end's source — the
    // Rust side cannot enforce it, and a reviewer reading a React component will
    // not notice a `setInterval` added to fix a refresh bug six months from now.
    let app = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("ui/src/App.tsx"),
    )
    .map_err(|e| format!("reading ui/src/App.tsx: {e}"))?;

    for polling in ["setInterval(", "setTimeout(", "fetch("] {
        let code: String = app
            .lines()
            .filter(|line| {
                !line.trim_start().starts_with("//") && !line.trim_start().starts_with('*')
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !code.contains(polling),
            "§15: the UI updates from events, never by polling — found `{polling}` \
             in App.tsx"
        );
    }
    Ok(())
}
