//! Starting rev-local when somebody logs in (RL-1517, SPEC §4.2).
//!
//! # Why this exists at all
//!
//! §4.2 is explicit: the daemon runs in-process and there is no background
//! service in v1, so **the app must be running to review**. The loop, the
//! autopilot switch and its timer all live inside a window. Nothing arranged for
//! that window to open, which made "reviews unattended" true only for as long as
//! somebody happened to leave the app running — a reboot stopped reviewing and
//! nothing said so.
//!
//! # Opt-in, and visible
//!
//! Off unless asked for. Software that arranges to launch itself without being
//! asked is its own kind of rude, and a person who wanted that will say so once.
//!
//! Visible, because the failure mode of a login item is that it silently is not
//! there — the file was never written, or something else removed it — and the
//! only way to notice is to reboot and find nothing happened. [`status`] reads
//! the filesystem rather than a stored flag, so the answer is what is actually
//! installed rather than what was last requested.
//!
//! # The contents are pure functions
//!
//! [`launch_agent`] and [`desktop_entry`] take a path and return a string, so
//! what gets written can be asserted without writing it. Every function that does
//! touch the disk takes the home directory as a parameter for the same reason:
//! the tests use a temporary one and never go near the real login items.

use std::path::{Path, PathBuf};

/// The label the macOS agent and the Linux entry are both keyed by.
///
/// Matches the bundle identifier so that two rev-locals cannot both claim it and
/// so an operator grepping their login items finds the name they expect.
pub const LABEL: &str = "com.dwmmholdings.revlocal";

/// Whether rev-local is arranged to start at login.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// The entry is installed.
    Enabled,
    /// It is not, and it could be.
    Disabled,
    /// This platform has no path implemented here.
    ///
    /// Named rather than reported as `Disabled`, because "you have not turned it
    /// on" and "turning it on does nothing here" are different things to say to
    /// somebody, and only one of them is worth offering a switch for.
    Unsupported,
}

/// Where the login-item entry lives, relative to a home directory.
///
/// `None` on a platform this does not implement. Windows uses a registry key
/// rather than a file, which is a different mechanism and not one this module
/// pretends to cover — see [`Status::Unsupported`].
pub fn entry_path(home: &Path) -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        Some(
            home.join("Library/LaunchAgents")
                .join(format!("{LABEL}.plist")),
        )
    } else if cfg!(target_os = "linux") {
        Some(home.join(".config/autostart/rev-local.desktop"))
    } else {
        None
    }
}

/// The macOS `LaunchAgent` that opens the app at login.
///
/// `RunAtLoad` and no `KeepAlive`: rev-local is an app somebody may legitimately
/// quit, and an agent that restarted it would make the Quit menu item a lie.
pub fn launch_agent(app_path: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/bin/open</string>
    <string>-a</string>
    <string>{app_path}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
</dict>
</plist>
"#
    )
}

/// The XDG autostart entry.
pub fn desktop_entry(exe_path: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=rev-local\n\
         Comment=Local, autonomous code review\n\
         Exec={exe_path}\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n"
    )
}

/// What is actually installed right now.
///
/// Reads the filesystem rather than a stored preference: a flag says what was
/// asked for, and the question worth answering is whether it took.
pub fn status(home: &Path) -> Status {
    match entry_path(home) {
        None => Status::Unsupported,
        Some(path) if path.exists() => Status::Enabled,
        Some(_) => Status::Disabled,
    }
}

/// Why the login item could not be changed.
#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    /// This platform has no implementation here.
    #[error("starting at login is not implemented on this platform yet\n  try: launch rev-local yourself, or leave it running")]
    Unsupported,

    /// The entry could not be written or removed.
    #[error("could not {action} the login item at {path}: {source}\n  try: check the directory is writable")]
    Io {
        /// What was being attempted.
        action: &'static str,
        /// Where.
        path: String,
        /// Why.
        #[source]
        source: std::io::Error,
    },
}

/// Install the login item.
///
/// `target` is the `.app` bundle on macOS and the executable elsewhere — the two
/// platforms launch different things, and passing the wrong one produces an entry
/// that exists and does nothing, which is the worst of the three outcomes.
pub fn enable(home: &Path, target: &str) -> Result<(), StartupError> {
    let path = entry_path(home).ok_or(StartupError::Unsupported)?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| StartupError::Io {
            action: "create the directory for",
            path: parent.display().to_string(),
            source,
        })?;
    }

    let body = if cfg!(target_os = "macos") {
        launch_agent(target)
    } else {
        desktop_entry(target)
    };

    std::fs::write(&path, body).map_err(|source| StartupError::Io {
        action: "write",
        path: path.display().to_string(),
        source,
    })
}

/// Remove it.
///
/// An entry that is already absent is a success. Somebody switching this off
/// wants it off, and reporting "there was nothing to remove" as a failure would
/// leave the switch stuck on for the one person whose entry something else
/// already deleted.
pub fn disable(home: &Path) -> Result<(), StartupError> {
    let path = entry_path(home).ok_or(StartupError::Unsupported)?;

    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(StartupError::Io {
            action: "remove",
            path: path.display().to_string(),
            source,
        }),
    }
}
