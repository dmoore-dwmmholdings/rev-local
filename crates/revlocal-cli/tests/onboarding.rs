//! The path a new user takes, and the safety property at the end of it (RL-1205).
//!
//! §15's onboarding is a desktop flow. Two of its four criteria are not about the
//! desktop at all — they are properties of the binary underneath, and they are
//! checkable here whatever the wizard ends up looking like:
//!
//! 1. A user with no configuration reaches a completed dry-run review without
//!    editing a file.
//! 2. A newly added repository is never on `auto`.
//!
//! The second is the one worth a test on its own. It is a one-word change away
//! from being false, the change would look harmless in a diff, and the
//! consequence is a repository that starts writing to somebody's issue tracker
//! because it was added.

use std::path::PathBuf;
use std::process::Command;

fn binary() -> PathBuf {
    let mut path = std::env::current_exe().unwrap_or_default();
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join(if cfg!(windows) {
        "revlocal.exe"
    } else {
        "revlocal"
    })
}

/// Run `revlocal` with a home directory that contains nothing.
///
/// The point of the whole criterion is "without editing a file", and a test that
/// inherited the developer's real config would be checking a machine that has
/// already been set up — the one case nobody needs help with.
fn revlocal(home: &std::path::Path, args: &[&str]) -> Result<(bool, String), String> {
    let out = Command::new(binary())
        .args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("USERPROFILE", home)
        .env("APPDATA", home.join("AppData"))
        .output()
        .map_err(|e| format!("running {args:?}: {e}"))?;

    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Ok((out.status.success(), text))
}

/// A git repository with one commit.
fn a_repository(at: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(at).map_err(|e| e.to_string())?;
    let run = |args: &[&str]| -> Result<(), String> {
        let out = Command::new("git")
            .args(args)
            .current_dir(at)
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).into_owned());
        }
        Ok(())
    };
    run(&["init", "--quiet", "-b", "main", "."])?;
    run(&["config", "user.email", "t@e.invalid"])?;
    run(&["config", "user.name", "T"])?;
    std::fs::write(at.join("main.rs"), "fn main() {}\n").map_err(|e| e.to_string())?;
    run(&["add", "main.rs"])?;
    run(&["commit", "--quiet", "-m", "add a main"])?;
    Ok(())
}

#[test]
fn a_user_with_no_configuration_reaches_a_review() -> Result<(), String> {
    // Criterion 1, as far as the binary owns it. Two commands, no file edited,
    // no database prepared: `doctor` says whether anything is blocking, and
    // `review` produces a result.
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    let repo = dir.path().join("acme");
    a_repository(&repo)?;

    let (ok, report) = revlocal(&home, &["doctor"])?;
    assert!(ok, "doctor must not block a fresh install:\n{report}");

    let (ok, output) = revlocal(
        &home,
        &[
            "review",
            "--repo",
            &repo.display().to_string(),
            "--rev",
            "HEAD",
        ],
    )?;
    assert!(ok, "review failed on a fresh install:\n{output}");
    assert!(
        output.contains("reviewed"),
        "no review result reached the user:\n{output}"
    );

    // And it said which engine it used. §18: a mock result that looked like a
    // real one would make the first thing a new user sees a lie.
    assert!(
        output.contains("mock engine"),
        "the mock engine must announce itself:\n{output}"
    );
    Ok(())
}

#[test]
fn a_newly_added_repository_is_never_on_auto() -> Result<(), String> {
    // Criterion 2. A repository added a moment ago has never been reviewed and
    // nobody has seen its findings; the first thing it does must not be to
    // publish them.
    //
    // Asserted through the binary rather than by reading the `default_value`,
    // because the default reaching the stored row is the part that matters and
    // the part a refactor can break.
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    let repo = dir.path().join("acme");
    a_repository(&repo)?;
    let db = dir.path().join("rl.db");

    revlocal(
        &home,
        &["db", "migrate", "--database", &db.display().to_string()],
    )?;
    let (ok, added) = revlocal(
        &home,
        &[
            "repo",
            "add",
            &repo.display().to_string(),
            "--kind",
            "git",
            "--name",
            "acme",
            "--database",
            &db.display().to_string(),
        ],
    )?;
    assert!(ok, "repo add failed:\n{added}");

    let (ok, listed) = revlocal(
        &home,
        &[
            "repo",
            "show",
            "acme",
            "--database",
            &db.display().to_string(),
            "--json",
        ],
    )?;
    assert!(ok, "repo show failed:\n{listed}");

    let stored: serde_json::Value =
        serde_json::from_str(&listed).map_err(|e| format!("{e}\n{listed}"))?;
    let autonomy = stored
        .pointer("/autonomy")
        .or_else(|| stored.pointer("/0/autonomy"))
        .and_then(|value| value.as_str())
        .ok_or_else(|| format!("no autonomy in the stored row:\n{listed}"))?;

    assert_ne!(
        autonomy, "auto",
        "a newly added repository is on `auto`; it would publish before anybody \
         had seen a finding from it"
    );
    assert_eq!(
        autonomy, "dry_run",
        "the default has moved; if that is deliberate, this test is the place to \
         say so"
    );

    // And the user is told, rather than having to go and look.
    assert!(
        added.contains("dry_run"),
        "`repo add` must say what autonomy it chose:\n{added}"
    );
    assert!(
        added.contains("nothing is published"),
        "and what that means:\n{added}"
    );
    Ok(())
}

#[test]
fn asking_for_auto_explicitly_still_works() -> Result<(), String> {
    // The rule is about the default, not a prohibition. Somebody who types
    // `--autonomy auto` has chosen it, and a safety property that cannot be
    // switched off is a bug report waiting to happen.
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    let repo = dir.path().join("acme");
    a_repository(&repo)?;
    let db = dir.path().join("rl.db");

    revlocal(
        &home,
        &["db", "migrate", "--database", &db.display().to_string()],
    )?;
    let (ok, added) = revlocal(
        &home,
        &[
            "repo",
            "add",
            &repo.display().to_string(),
            "--kind",
            "git",
            "--name",
            "acme",
            "--autonomy",
            "auto",
            "--database",
            &db.display().to_string(),
        ],
    )?;

    assert!(ok, "an explicit --autonomy auto was refused:\n{added}");
    assert!(added.contains("auto"), "and it must say so:\n{added}");
    Ok(())
}
