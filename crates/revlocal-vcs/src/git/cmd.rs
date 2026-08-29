//! The single choke point for every `git` invocation (SPEC §6.2).
//!
//! §6.2 says to shell out rather than link libgit2 or gix, because shelling out
//! matches the user's own configuration exactly — credential helpers, submodules,
//! LFS. The cost of that decision is that `git` is an interactive program living
//! inside a daemon, and it will happily block forever waiting for a password that
//! nobody is there to type.
//!
//! So every invocation goes through [`run`], which:
//!
//! - sets `GIT_TERMINAL_PROMPT=0` and `GIT_ASKPASS=echo`, so a repository needing
//!   credentials **fails immediately** instead of hanging;
//! - applies a per-call timeout;
//! - captures stdout and stderr rather than letting them reach the daemon's own;
//! - maps exit status onto typed errors, so callers branch on a variant rather
//!   than string-matching git's prose.
//!
//! `no_module_spawns_git_directly` asserts this is the only place in the
//! workspace's production code that spawns `git`. That test is the decision: a
//! second call site is not a style problem, it is a call site with no timeout and
//! no prompt suppression, and it will be found when the daemon hangs.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

/// Default per-call timeout.
///
/// Generous for a local operation and short enough that a wedged `fetch` does not
/// hold a run open indefinitely. Callers that need longer (a large clone) pass
/// their own.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

/// Environment applied to every invocation, so git cannot block on a human.
///
/// Returned as data rather than applied inline so it can be asserted on directly:
/// this is the mechanism behind "fails fast instead of hanging", and a test that
/// only checked the end-to-end behaviour would not notice one of the two going
/// missing.
pub fn non_interactive_env() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        // Refuse to prompt on a terminal.
        ("GIT_TERMINAL_PROMPT", "0"),
        // `echo` answers any credential request with an empty line, so git gets a
        // definitive "no credentials" and gives up rather than retrying.
        ("GIT_ASKPASS", "echo"),
        // The same, for anything reaching ssh.
        ("SSH_ASKPASS", "echo"),
        ("SSH_ASKPASS_REQUIRE", "never"),
        // Stable, parseable output regardless of the user's locale.
        ("LC_ALL", "C"),
        // Never open a pager: it would wait for input that never comes.
        ("GIT_PAGER", "cat"),
        ("PAGER", "cat"),
    ])
}

/// What a `git` invocation can do other than succeed.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    /// `git` is not on PATH.
    #[error("`git` is not on PATH\n  try: install git and make sure it is on PATH")]
    NotInstalled,

    /// The call exceeded its timeout and was killed.
    #[error(
        "`git {args}` did not finish within {timeout:?} and was killed\n  \
         try: check the repository is reachable, or raise the timeout"
    )]
    Timeout {
        /// The arguments, for identifying which call hung.
        args: String,
        /// How long it was given.
        timeout: Duration,
    },

    /// git needed credentials and there were none.
    ///
    /// A distinct variant because the remedy is specific and the alternative — a
    /// generic failure — sends the user looking at the wrong thing.
    #[error(
        "`git {args}` needs credentials rev-local does not have\n  \
         try: authenticate the repository outside rev-local (a credential helper, \
         an ssh agent, or `gh auth login`); rev-local never prompts and stores no \
         credentials of its own"
    )]
    CredentialsRequired {
        /// The arguments.
        args: String,
        /// What git said.
        stderr: String,
    },

    /// The path is not a git repository.
    #[error("{path} is not a git repository\n  try: check the repo's local_path")]
    NotARepository {
        /// The directory that was tried.
        path: PathBuf,
    },

    /// git ran and reported failure.
    #[error("`git {args}` failed with exit code {code}: {stderr}")]
    Failed {
        /// The arguments.
        args: String,
        /// The exit code, or -1 if it was signalled.
        code: i32,
        /// What git said.
        stderr: String,
    },

    /// The process could not be spawned or waited on.
    #[error("running `git {args}`: {source}")]
    Spawn {
        /// The arguments.
        args: String,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
}

/// A completed invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOutput {
    /// Captured stdout, trailing newline trimmed.
    pub stdout: String,
    /// Captured stderr, trailing newline trimmed.
    pub stderr: String,
}

impl GitOutput {
    /// stdout split into lines, with empties dropped.
    ///
    /// Almost every caller wants this; doing it here keeps the same
    /// `lines().filter(...)` from being written a dozen times slightly differently.
    pub fn lines(&self) -> Vec<&str> {
        self.stdout
            .lines()
            .map(str::trim_end)
            .filter(|l| !l.is_empty())
            .collect()
    }
}

/// How to invoke git. Carries the seam that makes timeouts testable.
#[derive(Debug, Clone)]
pub struct GitRunner {
    program: PathBuf,
    timeout: Duration,
}

impl Default for GitRunner {
    fn default() -> Self {
        Self {
            program: PathBuf::from("git"),
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

impl GitRunner {
    /// A runner using `git` from PATH and the default timeout.
    pub fn new() -> Self {
        Self::default()
    }

    /// Use a different timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Use a different program.
    ///
    /// Two real uses beyond tests: a configured `git` that is not on PATH, and
    /// pointing the timeout and process-group machinery at a program that can be
    /// made to hang on demand — `git` itself cannot, offline and reliably.
    pub fn with_program(mut self, program: impl Into<PathBuf>) -> Self {
        self.program = program.into();
        self
    }

    /// This runner's timeout.
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Run git in `dir` with `args`.
    pub async fn run<S: AsRef<OsStr>>(
        &self,
        dir: &Path,
        args: &[S],
    ) -> Result<GitOutput, GitError> {
        self.run_inner(dir, args, None).await
    }

    /// Run git in `dir` with `args`, writing `input` to its stdin.
    ///
    /// `git patch-id` reads a diff from stdin and has no file-argument form, so
    /// without this the only way to use it would be a second call site spawning
    /// git — which is exactly what `no_module_spawns_git_directly` exists to
    /// prevent. The choke point owns piping too.
    pub async fn run_with_stdin<S: AsRef<OsStr>>(
        &self,
        dir: &Path,
        args: &[S],
        input: &str,
    ) -> Result<GitOutput, GitError> {
        self.run_inner(dir, args, Some(input)).await
    }

    async fn run_inner<S: AsRef<OsStr>>(
        &self,
        dir: &Path,
        args: &[S],
        input: Option<&str>,
    ) -> Result<GitOutput, GitError> {
        let rendered = args
            .iter()
            .map(|a| a.as_ref().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join(" ");

        let mut command = tokio::process::Command::new(&self.program);
        command
            .args(args)
            .current_dir(dir)
            // Null unless the caller is piping: nothing to type into, made explicit
            // so a command that reads stdin gets a fast EOF rather than blocking on
            // an inherited terminal.
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        for (key, value) in non_interactive_env() {
            command.env(key, value);
        }

        // A new process group, so a timeout can signal git AND anything it spawned
        // — a credential helper, an ssh, a submodule fetch. Killing only the child
        // leaves those running and holding the repository lock.
        #[cfg(unix)]
        command.process_group(0);

        let mut child = command.spawn().map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                GitError::NotInstalled
            } else {
                GitError::Spawn {
                    args: rendered.clone(),
                    source,
                }
            }
        })?;

        let pid = child.id();

        if let Some(input) = input {
            // Written before waiting, and the handle dropped so git sees EOF. A
            // `patch-id` that never sees EOF would hit the timeout instead of
            // answering.
            if let Some(mut stdin) = child.stdin.take() {
                use tokio::io::AsyncWriteExt as _;
                let write = stdin.write_all(input.as_bytes()).await;
                let shutdown = stdin.shutdown().await;
                drop(stdin);
                if let Err(source) = write.and(shutdown) {
                    return Err(GitError::Spawn {
                        args: rendered,
                        source,
                    });
                }
            }
        }

        let output = match tokio::time::timeout(self.timeout, child.wait_with_output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(source)) => {
                return Err(GitError::Spawn {
                    args: rendered,
                    source,
                })
            }
            Err(_elapsed) => {
                if let Some(pid) = pid {
                    kill_process_group(pid);
                }
                return Err(GitError::Timeout {
                    args: rendered,
                    timeout: self.timeout,
                });
            }
        };

        let stdout = String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_owned();
        let stderr = String::from_utf8_lossy(&output.stderr)
            .trim_end()
            .to_owned();

        if output.status.success() {
            return Ok(GitOutput { stdout, stderr });
        }

        Err(classify(
            &rendered,
            dir,
            output.status.code().unwrap_or(-1),
            stderr,
        ))
    }
}

/// Turn a failed invocation into the most specific error that fits.
///
/// Matching on git's prose is unpleasant, but the alternative is handing every
/// failure to the user as "git failed" and letting them work out that they needed
/// to log in. The patterns are checked case-insensitively and are additive: an
/// unrecognised failure still surfaces, just less specifically.
fn classify(args: &str, dir: &Path, code: i32, stderr: String) -> GitError {
    let lowered = stderr.to_lowercase();

    const CREDENTIAL_MARKERS: &[&str] = &[
        "could not read username",
        "could not read password",
        "authentication failed",
        "terminal prompts disabled",
        "no credentials",
        "permission denied (publickey",
    ];
    if CREDENTIAL_MARKERS.iter().any(|m| lowered.contains(m)) {
        return GitError::CredentialsRequired {
            args: args.to_owned(),
            stderr,
        };
    }

    const NOT_A_REPO_MARKERS: &[&str] = &[
        "not a git repository",
        "does not appear to be a git repository",
    ];
    if NOT_A_REPO_MARKERS.iter().any(|m| lowered.contains(m)) {
        return GitError::NotARepository {
            path: dir.to_path_buf(),
        };
    }

    GitError::Failed {
        args: args.to_owned(),
        code,
        stderr,
    }
}

/// Kill a process group after a timeout.
///
/// Best effort by nature: the group may already be gone, which is the outcome
/// wanted anyway. `SIGKILL` rather than `SIGTERM` because this path is only
/// reached after the call already had its full timeout to finish.
///
/// Uses `nix`'s safe wrapper rather than `libc::killpg` directly: the workspace
/// forbids `unsafe`, and that lint was right — there is a safe binding for this.
#[cfg(unix)]
fn kill_process_group(pid: u32) {
    // The pgid equals the child's pid, because it was spawned with
    // `process_group(0)`. `try_from` rather than `as`: a pid that does not fit an
    // i32 cannot be signalled, and truncating one would signal a different group.
    let Ok(raw) = i32::try_from(pid) else {
        tracing::warn!(pid, "pid does not fit in a pid_t; not signalling");
        return;
    };
    let pgid = nix::unistd::Pid::from_raw(raw);
    if let Err(errno) = nix::sys::signal::killpg(pgid, nix::sys::signal::Signal::SIGKILL) {
        tracing::debug!(pid, %errno, "process group was already gone when the timeout fired");
    }
}

/// Windows has no process groups in the POSIX sense.
///
/// `kill_on_drop` handles the child itself. Killing a whole tree needs a Job
/// Object, which SPEC §8.5 requires and `RL-1303` owns; until then a timed-out
/// grandchild can outlive its parent on Windows.
#[cfg(not(unix))]
fn kill_process_group(_pid: u32) {
    tracing::warn!(
        "process-group kill is not implemented on this platform; a timed-out git \
         subprocess may leave children running (see RL-1303)"
    );
}

/// Run git with the default runner.
pub async fn run<S: AsRef<OsStr>>(dir: &Path, args: &[S]) -> Result<GitOutput, GitError> {
    GitRunner::new().run(dir, args).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
    }

    // --- the non-interactive environment ------------------------------------

    #[test]
    fn git_cmd_sets_the_variables_that_stop_git_blocking_on_a_human() {
        // This is the mechanism behind "fails fast instead of hanging". Asserted
        // directly, because an end-to-end test would still pass with one of the two
        // missing — and the one that was missing would be the one that mattered on
        // whichever repository actually needed a password.
        let env = non_interactive_env();
        assert_eq!(env.get("GIT_TERMINAL_PROMPT"), Some(&"0"));
        assert_eq!(env.get("GIT_ASKPASS"), Some(&"echo"));
        assert_eq!(env.get("SSH_ASKPASS_REQUIRE"), Some(&"never"));
        assert_eq!(
            env.get("GIT_PAGER"),
            Some(&"cat"),
            "a pager would wait for input"
        );
    }

    #[tokio::test]
    async fn git_cmd_applies_that_environment_to_the_child() {
        // Setting the variables is not the same as the child receiving them, and
        // the difference is invisible until something hangs in production. `env`
        // reports what was actually inherited.
        let runner = GitRunner::new().with_program("env");
        let output = runner
            .run(&repo_root(), &[] as &[&str])
            .await
            .unwrap_or_else(|e| panic!("running env: {e}"));

        for expected in ["GIT_TERMINAL_PROMPT=0", "GIT_ASKPASS=echo", "GIT_PAGER=cat"] {
            assert!(
                output.stdout.lines().any(|l| l == expected),
                "the child did not receive {expected}"
            );
        }
    }

    #[tokio::test]
    async fn git_cmd_gives_the_child_no_stdin_to_read() {
        // Even with prompting disabled, a command that reads stdin would block on
        // an inherited terminal. Null stdin makes that a fast EOF instead.
        let runner = GitRunner::new()
            .with_program("cat")
            .with_timeout(Duration::from_secs(3));
        let output = runner
            .run(&repo_root(), &[] as &[&str])
            .await
            .unwrap_or_else(|e| panic!("cat with null stdin should return immediately: {e}"));
        assert_eq!(output.stdout, "");
    }

    // --- failing fast --------------------------------------------------------

    #[tokio::test]
    async fn git_cmd_a_repo_needing_credentials_fails_fast_rather_than_hanging() {
        // A generous timeout on purpose: if the call returns quickly, it returned
        // because git gave up, not because the timeout rescued it. Asserting on
        // elapsed time is what distinguishes those two.
        let timeout = Duration::from_secs(30);
        let runner = GitRunner::new().with_timeout(timeout);

        let started = std::time::Instant::now();
        let result = runner
            .run(
                &repo_root(),
                &["ls-remote", "https://127.0.0.1:9/needs-credentials.git"],
            )
            .await;
        let elapsed = started.elapsed();

        assert!(
            result.is_err(),
            "an unreachable authenticated remote must not succeed"
        );
        assert!(
            elapsed < Duration::from_secs(15),
            "it took {elapsed:?}, which is close enough to the timeout to suggest it hung"
        );
        assert!(
            !matches!(result, Err(GitError::Timeout { .. })),
            "it must fail on its own, not be rescued by the timeout: {result:?}"
        );
    }

    #[test]
    fn git_cmd_credential_failures_get_their_own_variant_with_a_remedy() {
        // Classified from git's own wording. Handing the user "git failed" when
        // they need to log in sends them looking at the wrong thing.
        let error = classify(
            "fetch origin",
            Path::new("/repo"),
            128,
            "fatal: could not read Username for 'https://github.com': terminal prompts disabled"
                .to_owned(),
        );
        assert!(
            matches!(error, GitError::CredentialsRequired { .. }),
            "{error:?}"
        );

        let message = error.to_string();
        assert!(
            message.contains("try:"),
            "every user-visible error carries a remedy: {message}"
        );
        assert!(
            message.contains("stores no credentials"),
            "and this one should say rev-local will not prompt: {message}"
        );
    }

    #[test]
    fn git_cmd_a_missing_repository_is_told_apart_from_a_failed_command() {
        let error = classify(
            "rev-parse HEAD",
            Path::new("/not/a/repo"),
            128,
            "fatal: not a git repository (or any of the parent directories): .git".to_owned(),
        );
        assert!(
            matches!(error, GitError::NotARepository { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn git_cmd_an_unrecognised_failure_still_surfaces_with_its_stderr() {
        // The classifier is additive: a failure it does not recognise must not be
        // swallowed or mislabelled.
        let error = classify(
            "bisect",
            Path::new("/repo"),
            3,
            "something new went wrong".to_owned(),
        );
        match error {
            GitError::Failed { code, stderr, .. } => {
                assert_eq!(code, 3);
                assert!(stderr.contains("something new"));
            }
            other => panic!("expected a generic failure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn git_cmd_a_missing_git_binary_says_so_rather_than_failing_obscurely() {
        let runner = GitRunner::new().with_program("definitely-not-a-real-git-binary");
        let result = runner.run(&repo_root(), &["--version"]).await;
        assert!(matches!(result, Err(GitError::NotInstalled)), "{result:?}");
    }

    // --- timeouts and process groups ----------------------------------------

    // Unix only, for two reasons that both land on the same place.
    //
    // The assertions are about a *process group* — that the timeout reaches the
    // backgrounded grandchild and not merely the direct child. Windows has no
    // process groups, `command.process_group(0)` is itself `#[cfg(unix)]` a few
    // hundred lines above, and §8.5's Job Object that would provide the equivalent
    // is unimplemented (REVL-106).
    //
    // It also cannot run there: the test drives a `.sh` script through `bash`, and
    // `bash` on a Windows PATH is the WSL launcher — which is why
    // `revlocal_vcs::bash_program()` exists. CI reported exactly that, a bare
    // `code: 1` with empty stderr, rather than the timeout the test expects.
    //
    // Gating rather than porting: rewriting it to use `bash_program()` would make
    // it *run* on Windows and then assert a guarantee the platform does not offer,
    // which is a test that fails for the wrong reason. The guarantee comes back
    // with the Job Object.
    #[cfg(unix)]
    #[tokio::test]
    async fn git_cmd_a_timeout_kills_the_child_and_its_whole_process_group() {
        // The criterion, and the part that is easy to fake. `git` cannot be made to
        // hang reliably offline, so the timeout machinery is pointed at a script
        // that hangs on demand AND spawns a grandchild — because killing only the
        // child is the bug this exists to prevent. A grandchild that survives holds
        // the repository lock and the next run fails for a reason nobody can trace.
        let dir = tempfile::TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let marker = dir.path().join("grandchild.pid");
        let script = dir.path().join("hang.sh");

        std::fs::write(
            &script,
            format!(
                "#!/usr/bin/env bash\n\
                 # a grandchild that outlives its parent unless the GROUP is killed\n\
                 ( sleep 120 ) &\n\
                 echo $! > {}\n\
                 sleep 120\n",
                marker.display()
            ),
        )
        .unwrap_or_else(|e| panic!("write: {e}"));

        // Run the script through `bash` rather than exec'ing it directly, and no
        // chmod. Exec'ing a file this process has just written races with any
        // other test thread that forks in the window before the write's descriptor
        // is out of every child's table: the kernel answers ETXTBSY, and the test
        // fails as `ExecutableFileBusy` for reasons that have nothing to do with
        // what it is testing. It failed exactly that way on CI. `bash` only
        // *opens* the file, which ETXTBSY does not apply to.
        //
        // The process shape is unchanged: bash is the child, the backgrounded
        // subshell is the grandchild, which is what the assertions are about.
        let runner = GitRunner::new()
            .with_program("bash")
            .with_timeout(Duration::from_millis(700));

        let started = std::time::Instant::now();
        let result = runner
            .run(dir.path(), &[script.display().to_string().as_str()])
            .await;
        let elapsed = started.elapsed();

        assert!(
            matches!(result, Err(GitError::Timeout { .. })),
            "{result:?}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "the timeout did not fire promptly: {elapsed:?}"
        );

        // Give the signal a moment to land, then check the grandchild is gone.
        tokio::time::sleep(Duration::from_millis(400)).await;

        let grandchild: i32 = std::fs::read_to_string(&marker)
            .unwrap_or_default()
            .trim()
            .parse()
            .unwrap_or(0);
        assert!(grandchild > 0, "the script did not record a grandchild pid");

        #[cfg(unix)]
        {
            let pid = nix::unistd::Pid::from_raw(grandchild);
            // Signal 0: an existence check, no signal delivered.
            let alive = nix::sys::signal::kill(pid, None).is_ok();

            // Clean up before asserting. A failing run must not leak the very
            // process it is complaining about — that would leave the next run of
            // the suite fighting a stray `sleep`.
            if alive {
                let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL);
            }

            assert!(
                !alive,
                "grandchild {grandchild} survived the timeout; only the direct child \
                 was killed, so a hung git leaves processes holding the repo lock"
            );
        }
    }

    #[tokio::test]
    async fn git_cmd_the_timeout_error_names_the_call_that_hung() {
        let runner = GitRunner::new()
            .with_program("sleep")
            .with_timeout(Duration::from_millis(300));
        let result = runner.run(&repo_root(), &["30"]).await;

        match result {
            Err(GitError::Timeout { args, timeout }) => {
                assert_eq!(args, "30", "a timeout with no arguments is undiagnosable");
                assert_eq!(timeout, Duration::from_millis(300));
            }
            other => panic!("expected a timeout, got {other:?}"),
        }
    }

    // --- the choke point ------------------------------------------------------

    #[test]
    fn git_cmd_no_module_spawns_git_directly() {
        // The decision, not a style rule. A second call site is a call site with no
        // timeout and no prompt suppression, and it will be found when the daemon
        // hangs on someone's private repository.
        //
        // Scoped to production code. Test code that inspects a fixture repository
        // is exempt: it is asserting on git's own state, is not running inside the
        // daemon, and has nothing to hang on.
        let crates_dir = repo_root().join("crates");
        let mut offenders = Vec::new();

        fn walk(dir: &Path, offenders: &mut Vec<String>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    // Only production code. `tests/` is exempt, see above.
                    if path.file_name().is_some_and(|n| n == "tests") {
                        continue;
                    }
                    walk(&path, offenders);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                // This module is the choke point; it is allowed to spawn git.
                if path.ends_with("git/cmd.rs") {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                for pattern in [
                    "Command::new(\"git\")",
                    "Command::new(\"git\".",
                    "process::Command::new(\"git\"",
                ] {
                    if text.contains(pattern) {
                        offenders.push(path.display().to_string());
                    }
                }
            }
        }

        walk(&crates_dir, &mut offenders);

        assert!(
            offenders.is_empty(),
            "these modules spawn git outside the choke point, so they have no timeout \
             and no prompt suppression: {offenders:?}"
        );
    }

    #[tokio::test]
    async fn git_cmd_captures_output_instead_of_letting_it_reach_the_daemons_own() {
        let output = run(&repo_root(), &["--version"])
            .await
            .unwrap_or_else(|e| panic!("git --version: {e}"));
        assert!(
            output.stdout.starts_with("git version"),
            "{}",
            output.stdout
        );
        assert_eq!(output.lines().len(), 1);
    }
}
