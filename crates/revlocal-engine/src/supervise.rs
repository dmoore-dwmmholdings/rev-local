//! Process supervision for engine invocations (SPEC §8.5).
//!
//! An engine is somebody else's program, run on a schedule, with nobody watching.
//! Three things follow, and each is a rule here rather than a hope:
//!
//! - **It gets a hard wall-clock limit**, scaled by review depth (§8.5).
//! - **It is killed politely first.** SIGTERM, five seconds of grace, then SIGKILL.
//!   A CLI given no chance to finish writing `result.json` loses a review that had
//!   already been paid for.
//! - **Its children die with it.** An engine that shells out — and they do, to
//!   `git`, to language servers — leaves a process tree. Killing only the direct
//!   child leaves the rest holding the scratch worktree open, and the next run
//!   fails for a reason nobody can trace back here.
//!
//! # The environment is filtered, not inherited
//!
//! §8.5: the environment is inherited **minus a denylist**, because "the review
//! engine has no business acting on remotes; only rev-local's publish layer does".
//! A review engine holding a `GITHUB_TOKEN` can push. That is not a hypothetical
//! risk with an AI agent that has been asked to fix things.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use revlocal_core::Depth;
use tokio_util::sync::CancellationToken;

use crate::engine::{EngineError, EngineId};
use crate::template::Invocation;

/// How long an engine gets, by depth (SPEC §8.5, §9.3).
pub const fn timeout_for(depth: Depth) -> Duration {
    match depth {
        Depth::Summary => Duration::from_secs(3 * 60),
        Depth::Standard => Duration::from_secs(10 * 60),
        Depth::Deep => Duration::from_secs(25 * 60),
    }
}

/// How long a process gets between SIGTERM and SIGKILL (SPEC §8.5).
pub const GRACE: Duration = Duration::from_secs(5);

/// Environment variables never passed to an engine (SPEC §8.5).
///
/// Exact names, plus the suffix rules below. A review engine has no business
/// acting on remotes.
const DENIED_EXACT: &[&str] = &["GITHUB_TOKEN", "GH_TOKEN"];

/// Suffixes that make a variable a secret, whatever it is called.
///
/// Suffixes rather than a fixed list because the interesting ones are named after
/// whatever service a user happens to use, and a list would always be one service
/// behind.
const DENIED_SUFFIXES: &[&str] = &["_API_KEY", "_SECRET", "_PASSWORD", "_TOKEN"];

/// Whether a variable is withheld from the engine.
///
/// `pass_env` is the escape hatch: a user who genuinely needs one names it, which
/// is a decision they make explicitly rather than one rev-local makes for them.
pub fn is_denied(name: &str, pass_env: &[String]) -> bool {
    if pass_env.iter().any(|allowed| allowed == name) {
        return false;
    }
    let upper = name.to_uppercase();
    DENIED_EXACT.contains(&upper.as_str())
        || DENIED_SUFFIXES.iter().any(|suffix| upper.ends_with(suffix))
}

/// The environment an engine should receive, given the current one.
///
/// Takes the source rather than reading `std::env` so the filtering is testable
/// without mutating the test process's own environment — which is a global, and
/// mutating it under a parallel test runner is a race.
pub fn filtered_env<'a>(
    source: impl IntoIterator<Item = (&'a str, &'a str)>,
    pass_env: &[String],
) -> BTreeMap<String, String> {
    source
        .into_iter()
        .filter(|(name, _)| !is_denied(name, pass_env))
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .collect()
}

/// Variables that are denied by §8.5 but that an engine may need to authenticate.
///
/// This is the sharp edge of the denylist. Decision D9 says engines authenticate via
/// the user's existing CLI logins, and the common case is a credential file — which
/// needs `HOME`, and `HOME` is passed. But a user who authenticates with an API key
/// instead will find the engine unauthenticated, with **nothing on screen connecting
/// that to rev-local withholding a variable they set themselves**.
///
/// `revlocal doctor` uses this to say so out loud.
const LIKELY_AUTH_VARIABLES: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "CLAUDE_API_KEY",
    "OPENAI_API_KEY",
    "AZURE_OPENAI_API_KEY",
];

/// Variables present in `source` that are withheld and look like engine credentials.
///
/// Returned for `revlocal doctor` to report. Withholding them is correct — §8.5 is
/// explicit — but doing it silently turns a two-word fix (`pass_env`) into an
/// afternoon of confusion.
pub fn withheld_auth_variables<'a>(
    source: impl IntoIterator<Item = (&'a str, &'a str)>,
    pass_env: &[String],
) -> Vec<String> {
    source
        .into_iter()
        .map(|(name, _)| name)
        .filter(|name| {
            let upper = name.to_uppercase();
            LIKELY_AUTH_VARIABLES.contains(&upper.as_str()) && is_denied(name, pass_env)
        })
        .map(str::to_owned)
        .collect()
}

/// How `revlocal doctor` should explain a withheld credential.
pub fn withheld_auth_remediation(name: &str, engine: &str) -> String {
    format!(
        "`{name}` is set in your environment but rev-local withholds it from review \
         engines (SPEC §8.5: a review engine has no business acting on remotes). If \
         `{engine}` needs it to authenticate, add it to `engines.{engine}.pass_env` \
         in config.toml. Most CLIs authenticate from their own login instead, which \
         needs no environment variable."
    )
}

/// Why a process was killed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillReason {
    /// It exceeded its wall-clock limit.
    Timeout,
    /// The kill switch or a user cancelled the run (§12.1).
    Cancelled,
}

/// What a supervised run produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Supervised {
    /// Captured stdout.
    pub stdout: String,
    /// Captured stderr.
    pub stderr: String,
    /// The exit code, if it exited on its own.
    pub exit_code: Option<i32>,
    /// Why it was killed, if it was.
    pub killed: Option<KillReason>,
    /// The child's pid, recorded so a test — and a bug report — can check for
    /// survivors.
    pub pid: Option<u32>,
    /// How long it ran.
    pub elapsed: Duration,
}

impl Supervised {
    /// Whether the process finished on its own.
    pub const fn completed(&self) -> bool {
        self.killed.is_none()
    }
}

/// Run `invocation` under supervision.
pub async fn supervise(
    id: EngineId,
    invocation: &Invocation,
    cwd: &Path,
    env: &BTreeMap<String, String>,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<Supervised, EngineError> {
    let started = Instant::now();

    let mut command = tokio::process::Command::new(&invocation.program);
    command
        .args(&invocation.args)
        .current_dir(cwd)
        .stdin(if invocation.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        // Cleared, not merely overridden: the point of §8.5's denylist is that a
        // variable rev-local did not choose to pass is not there at all.
        .env_clear()
        .envs(env);

    // A new process group, so a kill can reach anything the engine spawned.
    #[cfg(unix)]
    command.process_group(0);

    let mut child = command.spawn().map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            EngineError::NotInstalled {
                id,
                remediation: format!(
                    "`{}` is not on PATH; install it, or set `bin` in this engine's \
                     config to its full path",
                    invocation.program
                ),
            }
        } else {
            EngineError::Failed {
                id,
                detail: format!("spawning `{}`: {source}", invocation.program),
            }
        }
    })?;

    let pid = child.id();

    if let Some(input) = invocation.stdin.clone() {
        if let Some(mut stdin) = child.stdin.take() {
            use tokio::io::AsyncWriteExt as _;
            // Failure here is not fatal: the engine may simply not be reading. The
            // timeout still bounds it.
            let _ = stdin.write_all(input.as_bytes()).await;
            let _ = stdin.shutdown().await;
        }
    }

    // Output is drained concurrently rather than with `wait_with_output`, which
    // consumes the child and would leave nothing to signal. Draining also matters
    // on its own: a child filling a full pipe blocks forever, and would look
    // exactly like a hang.
    let stdout_reader = child.stdout.take().map(read_to_string);
    let stderr_reader = child.stderr.take().map(read_to_string);

    let killed = tokio::select! {
        status = child.wait() => {
            let code = status.ok().and_then(|s| s.code());
            return finish(stdout_reader, stderr_reader, code, None, pid, started).await;
        }
        () = tokio::time::sleep(timeout) => KillReason::Timeout,
        () = cancel.cancelled() => KillReason::Cancelled,
    };

    terminate(&mut child, pid).await;

    finish(
        stdout_reader,
        stderr_reader,
        None,
        Some(killed),
        pid,
        started,
    )
    .await
}

/// SIGTERM, five seconds of grace, then SIGKILL — to the whole group.
///
/// The grace period is not politeness for its own sake: a CLI given no chance to
/// flush `result.json` loses a review whose tokens were already spent.
async fn terminate(child: &mut tokio::process::Child, pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        signal_group(pid, nix::sys::signal::Signal::SIGTERM);

        // Poll rather than `wait()`, so the grace period is bounded even if the
        // child ignores the signal — which the fixture's `hang` mode does on purpose.
        let deadline = Instant::now() + GRACE;
        while Instant::now() < deadline {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        signal_group(pid, nix::sys::signal::Signal::SIGKILL);
    }

    // On every platform, and as the last word on Unix: kill the direct child.
    // `kill_on_drop` would do it eventually, but "eventually" is not a guarantee a
    // timeout can be built on.
    let _ = child.start_kill();
    let _ = child.wait().await;

    #[cfg(not(unix))]
    {
        let _ = pid;
        tracing::warn!(
            "process-group kill is not implemented on this platform; a timed-out \
             engine may leave grandchildren running. SPEC §8.5 requires a Job \
             Object here — see RL-1303."
        );
    }
}

/// Signal a whole process group.
#[cfg(unix)]
fn signal_group(pid: u32, signal: nix::sys::signal::Signal) {
    // try_from rather than `as`: a pid that does not fit an i32 cannot be
    // signalled, and truncating one would signal a different group.
    let Ok(raw) = i32::try_from(pid) else {
        tracing::warn!(pid, "pid does not fit a pid_t; not signalling");
        return;
    };
    let group = nix::unistd::Pid::from_raw(raw);
    if let Err(errno) = nix::sys::signal::killpg(group, signal) {
        tracing::debug!(pid, %errno, ?signal, "process group was already gone");
    }
}

/// Read a pipe to a string on its own task.
fn read_to_string<R>(mut reader: R) -> tokio::task::JoinHandle<String>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt as _;
        let mut buffer = Vec::new();
        let _ = reader.read_to_end(&mut buffer).await;
        String::from_utf8_lossy(&buffer).into_owned()
    })
}

/// Collect what the readers gathered, however the process ended.
///
/// Output is kept even for a killed process: §8.2's ladder reads stdout, and a
/// timed-out engine may still have emitted a usable fenced block before it hung.
async fn finish(
    stdout: Option<tokio::task::JoinHandle<String>>,
    stderr: Option<tokio::task::JoinHandle<String>>,
    exit_code: Option<i32>,
    killed: Option<KillReason>,
    pid: Option<u32>,
    started: Instant,
) -> Result<Supervised, EngineError> {
    let stdout = match stdout {
        Some(handle) => handle.await.unwrap_or_default(),
        None => String::new(),
    };
    let stderr = match stderr {
        Some(handle) => handle.await.unwrap_or_default(),
        None => String::new(),
    };

    Ok(Supervised {
        stdout,
        stderr,
        exit_code,
        killed,
        pid,
        elapsed: started.elapsed(),
    })
}
