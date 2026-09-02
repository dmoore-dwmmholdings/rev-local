//! Starting at login (RL-1517, SPEC §4.2).
//!
//! Every test uses a temporary home directory, so nothing here goes near the real
//! login items.
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use revlocal_daemon::startup::{self, Status};
use tempfile::TempDir;

#[test]
fn it_is_off_until_somebody_asks_for_it() {
    // Software that arranges to launch itself without being asked is its own kind
    // of rude.
    let home = TempDir::new().unwrap_or_else(|e| panic!("{e}"));

    let status = startup::status(home.path());

    assert!(
        matches!(status, Status::Disabled | Status::Unsupported),
        "a fresh home must not already start rev-local: {status:?}"
    );
}

#[test]
fn enabling_writes_an_entry_and_status_reads_it_back() -> Result<(), Box<dyn std::error::Error>> {
    let home = TempDir::new()?;
    if startup::entry_path(home.path()).is_none() {
        // This platform has no implementation; `unsupported_is_not_just_off`
        // covers what it must say instead.
        return Ok(());
    }

    startup::enable(home.path(), "/Applications/rev-local.app")?;

    assert_eq!(startup::status(home.path()), Status::Enabled);
    Ok(())
}

#[test]
fn status_reads_the_filesystem_rather_than_what_was_asked_for(
) -> Result<(), Box<dyn std::error::Error>> {
    // The failure mode of a login item is that it silently is not there. A stored
    // flag would say "on" and the machine would not start rev-local, and the only
    // way to find out is to reboot.
    let home = TempDir::new()?;
    let Some(path) = startup::entry_path(home.path()) else {
        return Ok(());
    };

    startup::enable(home.path(), "/Applications/rev-local.app")?;
    std::fs::remove_file(&path)?;

    assert_eq!(
        startup::status(home.path()),
        Status::Disabled,
        "something else removed the entry, and the switch must say so"
    );
    Ok(())
}

#[test]
fn disabling_something_already_gone_is_a_success() -> Result<(), Box<dyn std::error::Error>> {
    // Somebody switching this off wants it off. Reporting "there was nothing to
    // remove" as a failure leaves the switch stuck on for the one person whose
    // entry something else already deleted.
    let home = TempDir::new()?;
    if startup::entry_path(home.path()).is_none() {
        return Ok(());
    }

    startup::disable(home.path())?;
    startup::disable(home.path())?;

    assert_eq!(startup::status(home.path()), Status::Disabled);
    Ok(())
}

#[test]
fn enabling_twice_leaves_one_entry() -> Result<(), Box<dyn std::error::Error>> {
    let home = TempDir::new()?;
    let Some(path) = startup::entry_path(home.path()) else {
        return Ok(());
    };

    startup::enable(home.path(), "/Applications/rev-local.app")?;
    startup::enable(home.path(), "/Applications/rev-local.app")?;

    let siblings = std::fs::read_dir(path.parent().ok_or("a parent")?)?.count();
    assert_eq!(siblings, 1, "a second enable must replace, not accumulate");
    Ok(())
}

// --- what gets written ------------------------------------------------------

#[test]
fn the_agent_names_what_it_launches() {
    let plist = startup::launch_agent("/Applications/rev-local.app");

    assert!(plist.contains("/Applications/rev-local.app"), "{plist}");
    assert!(plist.contains(startup::LABEL), "{plist}");
    assert!(plist.contains("RunAtLoad"), "{plist}");
}

#[test]
fn the_agent_does_not_restart_an_app_somebody_quit() {
    // §15: quitting is a real exit. An agent with `KeepAlive` would relaunch it
    // and make the Quit menu item a lie.
    let plist = startup::launch_agent("/Applications/rev-local.app");

    assert!(!plist.contains("KeepAlive"), "{plist}");
}

#[test]
fn the_desktop_entry_names_the_executable() {
    let entry = startup::desktop_entry("/usr/bin/revlocal-desktop");

    assert!(entry.contains("Exec=/usr/bin/revlocal-desktop"), "{entry}");
    assert!(entry.contains("Type=Application"), "{entry}");
}

#[test]
fn unsupported_is_not_just_off() {
    // "You have not turned it on" and "turning it on does nothing here" are
    // different things to tell somebody, and only one deserves a switch.
    let home = TempDir::new().unwrap_or_else(|e| panic!("{e}"));

    if startup::entry_path(home.path()).is_none() {
        assert_eq!(startup::status(home.path()), Status::Unsupported);
        let error = startup::enable(home.path(), "anything")
            .err()
            .unwrap_or_else(|| panic!("an unsupported platform must refuse"));
        assert!(error.to_string().contains("not implemented"), "{error}");
    }
}
