//! Window and tray lifecycle (RL-1101, SPEC §4.2, §15).
//!
//! # Why closing the window must not stop the daemon
//!
//! §4.2: the daemon runs **in-process**, and there is no background service in
//! v1 — *the app must be running to review*. So the window closing and the app
//! quitting are two different things, and conflating them means every review stops
//! the first time somebody hits Cmd-W out of habit.
//!
//! The inverse matters as much. An app that cannot be quit, only hidden, is one
//! people force-kill — and a force-killed daemon leaves runs stuck mid-stage for
//! RL-501's recovery to clean up on the next start. Quitting has to be a real,
//! reachable, clean exit.
//!
//! # The decision is a function, not a closure inside a GUI callback
//!
//! §15's third criterion — closing to tray keeps the daemon running, quitting
//! stops it cleanly — is exactly the kind of behaviour that is normally only
//! testable by driving a window. Keeping the *decision* here, as a pure function,
//! means the rule can be asserted without a webview; the Tauri handler is one line
//! that calls it.

/// What should happen when the window's close button is pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseAction {
    /// Hide the window and keep running. The daemon keeps reviewing.
    HideToTray,
    /// Actually exit.
    Exit,
}

impl CloseAction {
    /// Whether the daemon keeps running after this.
    pub const fn keeps_daemon_running(self) -> bool {
        matches!(self, Self::HideToTray)
    }
}

/// Why the window is closing.
///
/// Named rather than a boolean, because "the user pressed the red button" and "the
/// user chose Quit" look identical to a window and mean opposite things.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseCause {
    /// The window's own close control, or Cmd-W.
    WindowControl,
    /// Quit from the tray menu, the app menu, or Cmd-Q.
    QuitRequested,
    /// The OS is shutting down or logging out.
    SystemShutdown,
}

/// Decide what a close means (§15).
///
/// Only the window control hides. Everything else is somebody saying *stop*, and a
/// tool that will not stop when asked is one people learn to force-kill — which
/// leaves runs stuck mid-stage for RL-501's recovery to find on the next start.
pub const fn on_close(cause: CloseCause) -> CloseAction {
    match cause {
        CloseCause::WindowControl => CloseAction::HideToTray,
        CloseCause::QuitRequested | CloseCause::SystemShutdown => CloseAction::Exit,
    }
}

/// The tray menu items §15 requires.
///
/// "The kill switch is reachable from every screen **and from the tray**" — so it
/// is here rather than only in the window, and it is listed before Quit because
/// somebody reaching for the tray in a hurry is more likely to want the first than
/// the second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayItem {
    /// Bring the window back.
    Show,
    /// Stop everything now (§12.1).
    KillSwitch,
    /// Exit cleanly.
    Quit,
}

impl TrayItem {
    /// Every item, in menu order.
    pub const ALL: [Self; 3] = [Self::Show, Self::KillSwitch, Self::Quit];

    /// The id the menu uses.
    pub const fn id(self) -> &'static str {
        match self {
            Self::Show => "show",
            Self::KillSwitch => "kill_switch",
            Self::Quit => "quit",
        }
    }

    /// The label a person reads.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Show => "Show rev-local",
            Self::KillSwitch => "Kill switch — stop everything",
            Self::Quit => "Quit rev-local",
        }
    }

    /// Parse a menu id back.
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|item| item.id() == id)
    }
}
