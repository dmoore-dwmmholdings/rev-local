//! Bridging daemon events to the UI (RL-1101, SPEC §4.2, §15).
//!
//! §15's rule is that live updates come from events, not from polling the
//! database. The daemon already emits [`RunEvent`]; this turns each one into a
//! payload the front end can render, and hands it to whatever is listening.
//!
//! [`RunEvent`]: revlocal_daemon::state_machine::RunEvent

use std::sync::{Arc, Mutex};

use revlocal_daemon::state_machine::{RunEvent, RunEventSink};
use serde::{Deserialize, Serialize};

/// The Tauri event name the front end subscribes to.
///
/// One name for every run event rather than one per variant: a front end that has
/// to subscribe to six channels will miss the seventh when it is added, and the
/// discriminant is in the payload anyway.
pub const RUN_EVENT: &str = "revlocal://run-event";

/// One run event, in the shape the front end receives it.
///
/// Deliberately a separate type from [`RunEvent`] rather than serialising that
/// one directly. The daemon's enum is free to change shape for the daemon's
/// reasons; this is a wire format a front end is written against, and coupling
/// them would make a refactor inside the daemon a silent breakage in the UI.
///
/// [`RunEvent`]: revlocal_daemon::state_machine::RunEvent
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiEvent {
    /// A run moved between stages.
    StageChanged {
        /// Which run.
        run_id: i64,
        /// Where it was.
        from: String,
        /// Where it is now.
        to: String,
    },
    /// A run was found abandoned.
    Interrupted {
        /// Which run.
        run_id: i64,
        /// The stage it was stuck in — where the daemon died.
        stuck_in: String,
    },
    /// An interrupted run was re-enqueued.
    ReEnqueued {
        /// The run that was interrupted.
        previous_run_id: i64,
        /// Its successor.
        run_id: i64,
        /// Which attempt the successor is.
        attempt: u32,
    },
    /// An interrupted run was not re-enqueued, and why.
    GivenUp {
        /// Which run.
        run_id: i64,
        /// Why no further attempt was made.
        reason: String,
    },
}

impl From<RunEvent> for UiEvent {
    fn from(event: RunEvent) -> Self {
        match event {
            RunEvent::StageChanged { run, from, to } => Self::StageChanged {
                run_id: run.get(),
                from: from.to_string(),
                to: to.to_string(),
            },
            RunEvent::Interrupted { run, stuck_in } => Self::Interrupted {
                run_id: run.get(),
                stuck_in: stuck_in.to_string(),
            },
            RunEvent::ReEnqueued {
                previous,
                run,
                attempt,
            } => Self::ReEnqueued {
                previous_run_id: previous.get(),
                run_id: run.get(),
                attempt,
            },
            RunEvent::GivenUp { run, reason } => Self::GivenUp {
                run_id: run.get(),
                reason,
            },
        }
    }
}

impl UiEvent {
    /// The run this concerns.
    pub const fn run_id(&self) -> i64 {
        match self {
            Self::StageChanged { run_id, .. }
            | Self::Interrupted { run_id, .. }
            | Self::ReEnqueued { run_id, .. }
            | Self::GivenUp { run_id, .. } => *run_id,
        }
    }
}

/// Something that can deliver a [`UiEvent`] to a front end.
///
/// A trait so the bridge can be tested without a window, and so the CLI can reuse
/// it to print the same stream.
pub trait UiEventSink: Send + Sync {
    /// Deliver one event. Must not block or fail the transition that produced it.
    fn deliver(&self, event: UiEvent);
}

/// Collects events in memory. For tests, and for the CLI's `--follow`.
#[derive(Debug, Default)]
pub struct RecordingSink {
    events: Mutex<Vec<UiEvent>>,
}

impl RecordingSink {
    /// An empty sink.
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything delivered so far, in order.
    pub fn events(&self) -> Vec<UiEvent> {
        self.events
            .lock()
            .map(|held| held.clone())
            .unwrap_or_default()
    }
}

impl UiEventSink for RecordingSink {
    fn deliver(&self, event: UiEvent) {
        if let Ok(mut held) = self.events.lock() {
            held.push(event);
        }
    }
}

/// Plugs into the daemon as a [`RunEventSink`] and forwards to the UI.
///
/// This is the whole of "the UI updates live": the daemon already announces every
/// transition, so the front end receiving them is a consequence of that rather
/// than a feature the UI implements.
pub struct EventBridge {
    sink: Arc<dyn UiEventSink>,
}

impl EventBridge {
    /// Bridge daemon events to `sink`.
    pub fn new(sink: Arc<dyn UiEventSink>) -> Self {
        Self { sink }
    }
}

impl RunEventSink for EventBridge {
    fn emit(&self, event: RunEvent) {
        // Every event is forwarded. Filtering here would mean the UI silently
        // missing a state the daemon considered worth announcing — and §18's
        // point is that the interesting events are the ones nobody predicted
        // wanting.
        self.sink.deliver(UiEvent::from(event));
    }
}

impl std::fmt::Debug for EventBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventBridge").finish_non_exhaustive()
    }
}
