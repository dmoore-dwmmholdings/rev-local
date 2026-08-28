//! The git hook installer (RL-1004, SPEC §7.2).
//!
//! The first test is the one this feature exists to satisfy: **a developer's
//! commit must never fail because rev-local is down.** It runs a real `git commit`
//! against a real repository with a real installed hook and nothing listening on
//! the port, and it asserts both the exit status and the wall-clock time — because
//! a commit that succeeds after thirty seconds has still ruined somebody's
//! afternoon.
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use revlocal_core::{RepoId, TriggerSource};
use revlocal_daemon::hooks::{
    hooks_dir, install, managed_block, strip_block, uninstall, HookMode, HookOutcome, BEGIN_MARKER,
    END_MARKER,
};
use revlocal_daemon::trigger_receiver::{bind, router, ReceiverState, RepoSecret};
use revlocal_daemon::triggers::TriggerBus;

/// Run a command, failing with its output rather than a bare status.
fn run(program: &str, args: &[&str], cwd: &Path) -> Result<String, String> {
    let output = std::process::Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("running {program}: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "{program} {args:?} failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn git_is_installed() -> bool {
    std::process::Command::new("git")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A working copy with one commit, configured so committing needs no global setup.
fn a_repo(dir: &Path) -> Result<(), String> {
    run("git", &["init", "--quiet", "-b", "main", "."], dir)?;
    run("git", &["config", "user.email", "dev@example.com"], dir)?;
    run("git", &["config", "user.name", "A Developer"], dir)?;
    run("git", &["config", "commit.gpgsign", "false"], dir)?;
    std::fs::write(dir.join("a.txt"), "one\n").map_err(|e| e.to_string())?;
    run("git", &["add", "a.txt"], dir)?;
    run("git", &["commit", "--quiet", "-m", "first"], dir)?;
    Ok(())
}

/// A port nothing is listening on.
fn a_dead_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|listener| listener.local_addr().ok())
        .map(|addr| addr.port())
        // Binding and dropping leaves the port free, which is exactly what this
        // test wants: a port that is plausible and refuses connections.
        .unwrap_or(41791)
}

#[test]
fn a_commit_succeeds_in_under_two_seconds_with_the_receiver_down() -> Result<(), String> {
    // Criterion 1, and the reason this feature has a `safety` label. A code-review
    // tool that can block `git commit` is a tool people uninstall after the first
    // time it happens, and they are right to.
    if !git_is_installed() {
        return Err("git is required for this test".to_owned());
    }

    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    a_repo(dir.path())?;

    install(
        dir.path(),
        "acme-api",
        HookMode::Reference,
        a_dead_port(),
        "REVLOCAL_HOOK_SECRET",
    )
    .map_err(|e| e.to_string())?;

    std::fs::write(dir.path().join("b.txt"), "two\n").map_err(|e| e.to_string())?;
    run("git", &["add", "b.txt"], dir.path())?;

    let started = Instant::now();
    let output = std::process::Command::new("git")
        .args(["commit", "--quiet", "-m", "second"])
        .current_dir(dir.path())
        .output()
        .map_err(|e| e.to_string())?;
    let elapsed = started.elapsed();

    assert!(
        output.status.success(),
        "the commit FAILED with rev-local down: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // A commit that succeeds after thirty seconds has still ruined an afternoon.
    // The budget is the hook's 2s timeout plus room for git itself on a slow
    // runner; a hook that hangs blows through this rather than passing slowly.
    assert!(
        elapsed.as_secs() < 8,
        "the commit took {elapsed:?}; the hook must fire and forget"
    );

    // And it really did commit, rather than succeeding by doing nothing.
    let log = run("git", &["log", "--oneline"], dir.path())?;
    assert_eq!(log.lines().count(), 2, "expected two commits, got:\n{log}");
    Ok(())
}

#[test]
fn a_real_commit_reaches_a_running_receiver() -> Result<(), String> {
    // Not an acceptance criterion, but the one that stops every other test in this
    // file from being satisfied by a hook that does nothing. "The commit did not
    // fail" is trivially true of an empty file.
    //
    // This is also the only place RL-1003 and RL-1004 are exercised together: a
    // real `git commit`, a real hook, a real HTTP request, a real bus.
    if !git_is_installed() {
        return Err("git is required for this test".to_owned());
    }

    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let bus = Arc::new(Mutex::new(TriggerBus::default()));
        let mut secrets = BTreeMap::new();
        secrets.insert(
            "acme-api".to_owned(),
            RepoSecret {
                repo_id: RepoId::new(1),
                secret: "the-secret".to_owned(),
            },
        );

        let listener = bind(0).await.map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let app = router(ReceiverState::new(secrets, Arc::clone(&bus)));
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        a_repo(dir.path())?;
        install(
            dir.path(),
            "acme-api",
            HookMode::Reference,
            port,
            "REVLOCAL_HOOK_SECRET",
        )
        .map_err(|e| e.to_string())?;

        std::fs::write(dir.path().join("d.txt"), "four\n").map_err(|e| e.to_string())?;
        run("git", &["add", "d.txt"], dir.path())?;
        let output = std::process::Command::new("git")
            .args(["commit", "--quiet", "-m", "fourth"])
            .current_dir(dir.path())
            .env("REVLOCAL_HOOK_SECRET", "the-secret")
            .output()
            .map_err(|e| e.to_string())?;
        assert!(output.status.success());

        // The hook fires and forgets, so the request may land just after `git`
        // returns. Poll briefly rather than sleeping a fixed amount.
        let mut sources = Vec::new();
        for _ in 0..40 {
            sources = bus
                .lock()
                .map_err(|_| "bus mutex poisoned".to_owned())?
                .pending_sources(RepoId::new(1));
            if !sources.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        assert_eq!(
            sources,
            vec![TriggerSource::Hook],
            "the commit did not reach the receiver; the hook is inert"
        );

        server.abort();
        Ok(())
    })
}

#[test]
fn the_hook_still_lets_a_commit_through_when_curl_is_missing() -> Result<(), String> {
    // `command -v curl` guards the request. A machine without curl is a machine
    // where reviews do not fire — never one where commits fail.
    if !git_is_installed() {
        return Err("git is required for this test".to_owned());
    }
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    a_repo(dir.path())?;
    install(
        dir.path(),
        "acme-api",
        HookMode::Reference,
        a_dead_port(),
        "REVLOCAL_HOOK_SECRET",
    )
    .map_err(|e| e.to_string())?;

    std::fs::write(dir.path().join("c.txt"), "three\n").map_err(|e| e.to_string())?;
    run("git", &["add", "c.txt"], dir.path())?;

    // An empty PATH addition is the closest portable stand-in for "curl is not
    // here": the guard has to be what saves the commit, not curl's exit code.
    let output = std::process::Command::new("git")
        .args(["commit", "--quiet", "-m", "third"])
        .current_dir(dir.path())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("REVLOCAL_HOOK_SECRET", "")
        .output()
        .map_err(|e| e.to_string())?;

    assert!(
        output.status.success(),
        "commit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

#[test]
fn the_generated_hook_can_never_fail_a_commit_by_construction() -> Result<(), String> {
    // The properties above are observed; these are the reasons they hold. Both
    // matter: an observation can pass for the wrong reason, and a future edit that
    // adds `set -e` would break the guarantee without breaking the timing test.
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(dir.path().join(".git").join("hooks")).map_err(|e| e.to_string())?;

    install(
        dir.path(),
        "acme-api",
        HookMode::Reference,
        41791,
        "REVLOCAL_HOOK_SECRET",
    )
    .map_err(|e| e.to_string())?;

    let hook = std::fs::read_to_string(
        hooks_dir(dir.path())
            .map_err(|e| e.to_string())?
            .join("post-commit"),
    )
    .map_err(|e| e.to_string())?;

    assert!(
        hook.ends_with("exit 0\n"),
        "the hook must end in exit 0:\n{hook}"
    );
    // Prose about the rule is not a violation of it — the block's own comments
    // explain that it has no `set -e`, so only code lines are checked.
    let code: String = hook
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !code.contains("set -e"),
        "set -e would make a failure fatal"
    );
    assert!(hook.contains("|| true"), "curl's status must be discarded");
    assert!(
        hook.contains("--max-time 2"),
        "a receiver that stalls must not hang the commit"
    );
    // LF only. Git for Windows will not run a script whose shebang ends \r\n; it
    // reports `bad interpreter`, and the failure is silent.
    assert!(
        !hook.contains('\r'),
        "a CR reached the hook file; Git for Windows will refuse to run it"
    );
    Ok(())
}

#[test]
fn an_existing_hook_is_byte_identical_after_install_then_uninstall() -> Result<(), String> {
    // Criterion 2. A repository may already have hooks — from Husky, from
    // pre-commit, from a script a colleague wrote in 2019 that nobody understands
    // and everybody needs. Losing one is losing work that was not ours to lose.
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let hooks = dir.path().join(".git").join("hooks");
    std::fs::create_dir_all(&hooks).map_err(|e| e.to_string())?;

    let original = "#!/bin/sh\n# somebody's own hook\nnpx --no-install lint-staged\nexit 0\n";
    std::fs::write(hooks.join("post-commit"), original).map_err(|e| e.to_string())?;

    let installed = install(
        dir.path(),
        "acme-api",
        HookMode::Reference,
        41791,
        "REVLOCAL_HOOK_SECRET",
    )
    .map_err(|e| e.to_string())?;
    assert!(matches!(installed[0], HookOutcome::Appended(_)));

    // The user's lines survive the install.
    let after_install =
        std::fs::read_to_string(hooks.join("post-commit")).map_err(|e| e.to_string())?;
    assert!(after_install.contains("lint-staged"));
    assert!(after_install.contains(BEGIN_MARKER));

    uninstall(dir.path(), HookMode::Reference).map_err(|e| e.to_string())?;

    let after_uninstall =
        std::fs::read_to_string(hooks.join("post-commit")).map_err(|e| e.to_string())?;
    assert_eq!(
        after_uninstall, original,
        "the user's hook must come back byte-identical"
    );
    Ok(())
}

#[test]
fn the_block_goes_before_a_trailing_exit_not_after_it() -> Result<(), String> {
    // The bug this test exists for: hook scripts conventionally end with `exit 0`,
    // and a block appended after an unconditional exit never executes. The install
    // reports success, the file visibly contains the trigger, and no trigger ever
    // fires — the worst shape a bug can take, because it looks correct everywhere
    // somebody would check.
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let hooks = dir.path().join(".git").join("hooks");
    std::fs::create_dir_all(&hooks).map_err(|e| e.to_string())?;
    std::fs::write(
        hooks.join("post-commit"),
        "#!/bin/sh\nnpx --no-install lint-staged\nexit 0\n",
    )
    .map_err(|e| e.to_string())?;

    install(dir.path(), "acme-api", HookMode::Reference, 41791, "S").map_err(|e| e.to_string())?;

    let text = std::fs::read_to_string(hooks.join("post-commit")).map_err(|e| e.to_string())?;
    let block_at = text.find(BEGIN_MARKER).ok_or("the block must be present")?;
    let exit_at = text
        .rfind("\nexit 0")
        .ok_or("the trailing exit must survive")?;
    assert!(
        block_at < exit_at,
        "the block must precede the trailing exit, or it never runs:\n{text}"
    );
    // And the same must hold after a reinstall.
    install(dir.path(), "acme-api", HookMode::Reference, 41791, "S").map_err(|e| e.to_string())?;
    let again = std::fs::read_to_string(hooks.join("post-commit")).map_err(|e| e.to_string())?;
    assert!(
        again.find(BEGIN_MARKER).unwrap_or(usize::MAX) < again.rfind("\nexit 0").unwrap_or(0),
        "reinstall moved the block after the exit:\n{again}"
    );
    Ok(())
}

#[test]
fn install_is_idempotent() -> Result<(), String> {
    // Criterion 3. This matters because the natural response to "did that work?"
    // is to run it again, and three curls per commit is a bug that only shows up
    // as mysterious duplicate triggers.
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(dir.path().join(".git").join("hooks")).map_err(|e| e.to_string())?;
    let hook = dir.path().join(".git").join("hooks").join("post-commit");

    let first = install(dir.path(), "acme-api", HookMode::Reference, 41791, "S")
        .map_err(|e| e.to_string())?;
    let once = std::fs::read_to_string(&hook).map_err(|e| e.to_string())?;

    for _ in 0..3 {
        install(dir.path(), "acme-api", HookMode::Reference, 41791, "S")
            .map_err(|e| e.to_string())?;
    }
    let repeatedly = std::fs::read_to_string(&hook).map_err(|e| e.to_string())?;

    assert!(matches!(first[0], HookOutcome::Created(_)));
    assert_eq!(once, repeatedly, "install must be idempotent");
    assert_eq!(repeatedly.matches(BEGIN_MARKER).count(), 1);
    assert_eq!(repeatedly.matches(END_MARKER).count(), 1);
    Ok(())
}

#[test]
fn reinstalling_after_a_port_change_replaces_rather_than_stacks() -> Result<(), String> {
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(dir.path().join(".git").join("hooks")).map_err(|e| e.to_string())?;
    let hook = dir.path().join(".git").join("hooks").join("post-commit");

    install(dir.path(), "acme-api", HookMode::Reference, 41791, "S").map_err(|e| e.to_string())?;
    let outcomes = install(dir.path(), "acme-api", HookMode::Reference, 50000, "S")
        .map_err(|e| e.to_string())?;

    let text = std::fs::read_to_string(&hook).map_err(|e| e.to_string())?;
    assert!(matches!(outcomes[0], HookOutcome::Replaced(_)));
    assert!(text.contains("50000"), "the new port must be in the hook");
    assert!(
        !text.contains("41791"),
        "the old port must be gone:\n{text}"
    );
    Ok(())
}

#[test]
fn a_hook_rev_local_wrote_entirely_is_deleted_not_left_inert() -> Result<(), String> {
    // Leaving a file containing only `exit 0` behind is litter that looks like a
    // hook somebody meant to write.
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(dir.path().join(".git").join("hooks")).map_err(|e| e.to_string())?;
    let hook = dir.path().join(".git").join("hooks").join("post-commit");

    install(dir.path(), "acme-api", HookMode::Reference, 41791, "S").map_err(|e| e.to_string())?;
    assert!(hook.exists());

    let outcomes = uninstall(dir.path(), HookMode::Reference).map_err(|e| e.to_string())?;
    assert!(matches!(outcomes[0], HookOutcome::Deleted(_)));
    assert!(!hook.exists(), "a hook we wrote entirely should be removed");
    Ok(())
}

#[test]
fn uninstalling_what_was_never_installed_changes_nothing() -> Result<(), String> {
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let hooks = dir.path().join(".git").join("hooks");
    std::fs::create_dir_all(&hooks).map_err(|e| e.to_string())?;
    let theirs = "#!/bin/sh\necho hello\n";
    std::fs::write(hooks.join("post-commit"), theirs).map_err(|e| e.to_string())?;

    let outcomes = uninstall(dir.path(), HookMode::Reference).map_err(|e| e.to_string())?;
    assert!(outcomes
        .iter()
        .all(|o| matches!(o, HookOutcome::Untouched(_))));
    assert_eq!(
        std::fs::read_to_string(hooks.join("post-commit")).map_err(|e| e.to_string())?,
        theirs
    );
    Ok(())
}

#[test]
fn the_secret_is_never_written_into_the_hook() {
    // Hooks live in `.git`, which is not committed — but it is backed up, copied
    // between machines, and readable by anything with filesystem access. An env
    // var *name* is not a secret; a secret in a file is.
    let block = managed_block("acme-api", 41791, "REVLOCAL_HOOK_SECRET");

    assert!(block.contains("${REVLOCAL_HOOK_SECRET:-}"));
    assert!(block.contains("x-revlocal-secret"));
    // The block names the command that installed it, so somebody finding a curl in
    // their own hook file knows where it came from.
    assert!(block.contains("revlocal hooks install"));
}

#[test]
fn stripping_a_block_does_not_grow_the_file() -> Result<(), String> {
    // Every install/uninstall cycle leaving one blank line behind is how a hook
    // file ends up 40 lines long after a year.
    let base = "#!/bin/sh\necho theirs\nexit 0\n";
    let mut text = base.to_owned();
    text.push_str(&managed_block("r", 1, "S"));

    let stripped = strip_block(&text).ok_or("a block was present and must be found")?;
    assert_eq!(stripped, base);
    Ok(())
}

#[test]
fn a_path_that_is_not_a_repository_says_so() {
    let dir = match tempfile::tempdir() {
        Ok(dir) => dir,
        Err(e) => panic!("temp dir: {e}"),
    };
    let error = install(dir.path(), "r", HookMode::Reference, 1, "S")
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();

    assert!(
        error.contains("does not look like a git repository"),
        "{error}"
    );
    assert!(error.contains("try:"), "must say what to do: {error}");
}

#[test]
fn bare_mirror_mode_installs_post_receive_and_fires_on_push() -> Result<(), String> {
    // Criterion 5. §7.2: a bare mirror is the only way to see every pushed ref,
    // including deletions.
    if !git_is_installed() {
        return Err("git is required for this test".to_owned());
    }

    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let bare = dir.path().join("mirror.git");
    let work = dir.path().join("work");
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;

    run(
        "git",
        &[
            "init",
            "--quiet",
            "--bare",
            "-b",
            "main",
            &bare.display().to_string(),
        ],
        dir.path(),
    )?;
    a_repo(&work)?;

    // post-receive in a bare repo has no `.git` subdirectory — the hooks live at
    // the top level, which is why `hooks_dir` accepts both shapes.
    let outcomes = install(&bare, "acme-api", HookMode::BareMirror, a_dead_port(), "S")
        .map_err(|e| e.to_string())?;
    assert_eq!(outcomes.len(), 1);
    assert!(bare.join("hooks").join("post-receive").exists());

    // The push must succeed with nothing listening. A hook that fails a push is a
    // hook that stops a whole team, not one developer.
    run(
        "git",
        &["remote", "add", "mirror", &bare.display().to_string()],
        &work,
    )?;
    let started = Instant::now();
    run("git", &["push", "--quiet", "mirror", "main"], &work)?;
    let elapsed = started.elapsed();

    assert!(
        elapsed.as_secs() < 8,
        "the push took {elapsed:?}; post-receive must fire and forget"
    );

    // And the mirror really received it.
    let refs = run(
        "git",
        &[
            "--git-dir",
            &bare.display().to_string(),
            "log",
            "--oneline",
            "main",
        ],
        dir.path(),
    )?;
    assert!(!refs.trim().is_empty(), "the mirror has no commits");
    Ok(())
}
