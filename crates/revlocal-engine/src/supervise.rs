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
use std::sync::{Arc, Mutex};
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

/// How long a **cancelled** engine gets before SIGKILL (SPEC §12.1, ADR 0030).
///
/// Shorter than [`GRACE`] on purpose. A timeout is "you have had long enough,
/// finish up", and ADR 0017 kept five seconds because a CLI cut off mid-write
/// loses a review whose tokens were already spent. A kill switch is a person
/// saying *stop now*, and they have already accepted losing the run — spending
/// five seconds of their emergency budget on a courtesy they explicitly declined
/// is the wrong trade, and §12.1 gives the whole cancellation three seconds.
///
/// Two seconds still lets a well-behaved engine flush; it only shortens the wait
/// for one that is ignoring SIGTERM, which is the case the budget is about.
pub const CANCEL_GRACE: Duration = Duration::from_secs(2);

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
    /// Whether reading stopped before the pipe closed (SPEC §18).
    ///
    /// Set when a drain hit its bound, which means something was still holding
    /// the pipe. The captured output is what arrived, not necessarily all of it,
    /// and §18 forbids that difference being invisible: a partial review that
    /// looks complete is worse than one that admits it is partial.
    pub output_truncated: bool,
    /// How long it ran.
    pub elapsed: Duration,
}

impl Supervised {
    /// Whether the process finished on its own.
    pub const fn completed(&self) -> bool {
        self.killed.is_none()
    }

    /// Whether this output can be trusted as the whole of what the engine said.
    ///
    /// Deliberately not the same question as [`completed`](Self::completed). A
    /// process can exit successfully and still leave a child holding the pipe, so
    /// "it finished" and "we read everything it wrote" are separate facts and only
    /// one of them is about the exit code.
    pub const fn output_is_complete(&self) -> bool {
        !self.output_truncated
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

    // ...with the exception of the handful Windows needs to start a process at
    // all. `SystemRoot` is where the loader finds the C runtime and Winsock;
    // `COMSPEC` is how a `.cmd` gets a shell; `PATHEXT` is how a bare program name
    // resolves. Clearing them does not sandbox the engine, it stops it running —
    // and the failure is a bare non-zero exit with no output to explain it.
    //
    // §8.5's denylist exists to keep secrets out of an engine's environment. The
    // path to `C:\WINDOWS` is not a secret, and an engine that cannot start has
    // not been secured, it has been broken.
    #[cfg(windows)]
    for name in WINDOWS_ESSENTIAL_ENV {
        if !env.contains_key(*name) {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
    }

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

    // §8.5's Job Object. Created after the spawn and assigned immediately, which
    // leaves a race the `job` module documents: a process that spawns a
    // grandchild in its first microseconds could have it escape. Closing that
    // window properly means not using `tokio::process` at all.
    //
    // A failure to create or assign is a warning, not an error. The engine is
    // already running and its review is worth more than the cleanup guarantee;
    // what must not happen is proceeding as if the guarantee held.
    #[cfg(windows)]
    let job = match crate::job::JobObject::new() {
        Ok(job) => match job.assign(&child) {
            Ok(()) => Some(job),
            Err(error) => {
                tracing::warn!(
                    %error,
                    "could not put the engine in a job object; a kill may leave \
                     grandchildren running (SPEC §8.5)"
                );
                None
            }
        },
        Err(error) => {
            tracing::warn!(
                %error,
                "could not create a job object; a kill may leave grandchildren \
                 running (SPEC §8.5)"
            );
            None
        }
    };

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
    let stdout_reader = child.stdout.take().map(read_into_shared);
    let stderr_reader = child.stderr.take().map(read_into_shared);

    let killed = tokio::select! {
        status = child.wait() => {
            let code = status.ok().and_then(|s| s.code());
            return finish(stdout_reader, stderr_reader, code, None, pid, started).await;
        }
        () = tokio::time::sleep(timeout) => KillReason::Timeout,
        () = cancel.cancelled() => KillReason::Cancelled,
    };

    let grace = match killed {
        KillReason::Timeout => GRACE,
        KillReason::Cancelled => CANCEL_GRACE,
    };
    #[cfg(windows)]
    terminate(&mut child, pid, grace, job.as_ref()).await;
    #[cfg(not(windows))]
    terminate(&mut child, pid, grace).await;

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

/// SIGTERM, `grace`, then SIGKILL — to the whole group.
///
/// The grace period is not politeness for its own sake: a CLI given no chance to
/// flush `result.json` loses a review whose tokens were already spent. How much
/// of it a kill gets depends on why: see [`GRACE`] and [`CANCEL_GRACE`].
async fn terminate(
    child: &mut tokio::process::Child,
    pid: Option<u32>,
    grace: Duration,
    #[cfg(windows)] job: Option<&crate::job::JobObject>,
) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        signal_group(pid, nix::sys::signal::Signal::SIGTERM);

        // Poll rather than `wait()`, so the grace period is bounded even if the
        // child ignores the signal — which the fixture's `hang` mode does on purpose.
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        signal_group(pid, nix::sys::signal::Signal::SIGKILL);
    }

    // Windows: the job goes first, before the wait below.
    //
    // Ordering is the whole fix. `child.wait()` returns when the direct child
    // exits, but the drains in `finish` do not end until every write handle on
    // the pipes is closed — and a surviving grandchild holds one. Killing the
    // child first and the tree second leaves a window where rev-local is waiting
    // on output from a process it has already decided to kill, which is exactly
    // the hang this was written to remove.
    #[cfg(windows)]
    {
        let _ = pid;
        match job {
            // This reaches grandchildren, which killing the direct child cannot.
            // Terminating a `.cmd` shim kills `cmd.exe` and leaves `node` holding
            // the pipes — which reads as a hang, not a failure.
            Some(job) => {
                if let Err(error) = job.terminate() {
                    tracing::warn!(
                        %error,
                        "terminating the job object failed; grandchildren of the \
                         engine may still be running"
                    );
                }
            }
            None => tracing::warn!(
                "no job object was attached to this engine, so only the direct \
                 child was killed; anything it spawned may still be running \
                 (SPEC §8.5)"
            ),
        }
    }

    // On every platform, and as the last word on Unix: kill the direct child.
    // `kill_on_drop` would do it eventually, but "eventually" is not a guarantee a
    // timeout can be built on.
    let _ = child.start_kill();
    let _ = child.wait().await;

    #[cfg(not(unix))]
    {
        let _ = pid;
        // `grace` has nothing to apply to here. Windows has no graceful-termination
        // signal a console process is obliged to honour, so `start_kill` above is
        // TerminateProcess — immediate by definition, with no window in which the
        // child could have chosen to flush. That is a real difference from the Unix
        // path, not an oversight: a Windows engine gets no chance to write
        // `result.json` on the way out, and §8.5's grace period is Unix-only in
        // practice because Windows offers nothing to spend it on.
        let _ = grace;
    }
}

/// Variables Windows needs present for a process to start.
///
/// Deliberately short, and deliberately not "everything". Each entry is here
/// because its absence stops a program running rather than merely inconveniencing
/// it:
///
/// - `SystemRoot` / `windir` — the loader resolves system DLLs relative to these.
///   Without them a process fails during CRT initialisation, before `main`.
/// - `COMSPEC` — the shell used to run a `.cmd` or `.bat`.
/// - `PATHEXT` — how a bare program name resolves to `foo.exe` rather than `foo`.
/// - `TEMP` / `TMP` — many toolchains write scratch files unconditionally and
///   abort if they cannot.
/// - `USERPROFILE` — Node and Git both look here for configuration.
///
/// None of them is a secret, which is the test for belonging on this list.
#[cfg(windows)]
pub const WINDOWS_ESSENTIAL_ENV: &[&str] = &[
    "SystemRoot",
    "windir",
    "COMSPEC",
    "PATHEXT",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "SystemDrive",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
];

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

/// How long the drain is given once the child has been killed.
///
/// The drain cannot be unbounded. A pipe stays open while **any** process holds
/// its write end, so a grandchild that survived the kill keeps a read waiting
/// forever — and on Windows that is not hypothetical: §8.5's Job Object is
/// RL-1303's, and until it lands, killing the shim leaves `node` running. The
/// Windows CI leg hung for half an hour on exactly this.
///
/// A supervisor that can hang forever waiting on a process it already killed is
/// worse than the timeout it was enforcing.
pub const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// How long to drain after a process exited **on its own** (SPEC §18).
///
/// A process that exits closes its pipes, so this normally elapses not at all.
/// It exists for the case where that is not true: a grandchild outliving its
/// parent keeps the write end open, and the read never ends.
///
/// The wait used to be unbounded there, on the reasoning that an exited process
/// has closed its pipes. That is true of the process and not of its children, and
/// the difference is a deadlock: no timeout wraps this, and on Windows the job
/// object that would reap the grandchild is not closed until after this returns.
///
/// Longer than [`DRAIN_GRACE`] because nothing is waiting on it — §12.1's
/// three-second budget is about *cancellation*, and this path was not cancelled.
/// The point is a bound, not a short one.
pub const EXIT_DRAIN_GRACE: Duration = Duration::from_secs(10);

/// A buffer a reader task fills and the supervisor can read at any point.
type SharedBuffer = Arc<Mutex<Vec<u8>>>;

/// Drain a pipe into a buffer the caller can read even if the task is abandoned.
///
/// Shared rather than returned, so output read *before* a hang is still
/// available. §8.2's ladder reads stdout and a killed engine may already have
/// emitted a usable fenced block; throwing that away because a grandchild held
/// the pipe open would lose a review that was recoverable.
fn read_into_shared<R>(mut reader: R) -> (tokio::task::JoinHandle<()>, SharedBuffer)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let buffer: SharedBuffer = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&buffer);

    let handle = tokio::spawn(async move {
        use tokio::io::AsyncReadExt as _;
        let mut chunk = [0_u8; 8192];
        loop {
            match reader.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    if let Ok(mut sink) = sink.lock() {
                        sink.extend_from_slice(&chunk[..read]);
                    }
                }
            }
        }
    });

    (handle, buffer)
}

/// Whatever has been read so far.
fn snapshot(buffer: &SharedBuffer) -> String {
    buffer
        .lock()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
}

/// Wait for a reader, giving up after `grace` and keeping what it read.
async fn drain(
    reader: Option<(tokio::task::JoinHandle<()>, SharedBuffer)>,
    grace: Duration,
    killed: bool,
) -> (String, bool) {
    let Some((handle, buffer)) = reader else {
        return (String::new(), false);
    };

    let gave_up = tokio::time::timeout(grace, handle).await.is_err();
    if gave_up {
        // Whoever is holding the pipe is not ours to wait for. Take what arrived
        // and say the reading stopped early, so truncated output is visible
        // rather than passed off as all of it (§18).
        if killed {
            tracing::warn!(
                bytes = snapshot(&buffer).len(),
                ?grace,
                "an engine's output pipe was still open after the process was \
                 killed; something it spawned is still holding it"
            );
        } else {
            // Worse than the killed case, and said differently. The engine
            // finished normally and something it spawned outlived it — so this
            // output is being treated as a complete review's, and it is not.
            tracing::warn!(
                bytes = snapshot(&buffer).len(),
                ?grace,
                "an engine exited on its own but its output pipe is still held by \
                 something it spawned; the captured output may be incomplete"
            );
        }
    }

    (snapshot(&buffer), gave_up)
}

/// Collect what the readers gathered, however the process ended.
///
/// Output is kept even for a killed process: §8.2's ladder reads stdout, and a
/// timed-out engine may still have emitted a usable fenced block before it hung.
async fn finish(
    stdout: Option<(tokio::task::JoinHandle<()>, SharedBuffer)>,
    stderr: Option<(tokio::task::JoinHandle<()>, SharedBuffer)>,
    exit_code: Option<i32>,
    killed: Option<KillReason>,
    pid: Option<u32>,
    started: Instant,
) -> Result<Supervised, EngineError> {
    // Always bounded. It used to be bounded only when the process was killed,
    // on the reasoning that one which exited has closed its pipes — true of the
    // process, false of anything it spawned, and the gap between those is an
    // unbounded await with no timeout around it.
    let grace = if killed.is_some() {
        DRAIN_GRACE
    } else {
        EXIT_DRAIN_GRACE
    };

    // Concurrently, not one after the other. The two drains wait on independent
    // pipes, and awaiting them in sequence makes the worst case the *sum* of their
    // grace periods rather than the longer of the two.
    //
    // That is not theoretical. A grandchild that survived the kill holds both
    // pipes, so both drains wait the full grace — 2s + 2s — and §12.1's
    // three-second cancellation budget is blown by a second. The Windows CI leg
    // failed on exactly this, at 4.0237226s, because Windows has no process-group
    // kill and the grandchild really does survive (see `terminate`). Unix passed
    // only because `killpg` reaps the grandchild and both pipes close at once.
    //
    // Sequential awaits are the easy thing to write and are wrong whenever the
    // things being awaited are independent and bounded by a timeout.
    let killed_flag = killed.is_some();
    let ((stdout, stdout_cut), (stderr, stderr_cut)) = tokio::join!(
        drain(stdout, grace, killed_flag),
        drain(stderr, grace, killed_flag)
    );

    Ok(Supervised {
        stdout,
        stderr,
        exit_code,
        killed,
        pid,
        output_truncated: stdout_cut || stderr_cut,
        elapsed: started.elapsed(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader that never yields and never ends — a grandchild holding the pipe.
    struct NeverEnds;

    impl tokio::io::AsyncRead for NeverEnds {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            _buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Pending
        }
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn a_killed_process_drains_both_pipes_within_one_grace_period() {
        // The regression this exists for: `finish` awaited the two drains in
        // sequence, so a process whose pipes are both held open cost
        // DRAIN_GRACE *twice*. §12.1 gives cancellation three seconds and the
        // grace is two, so 2 + 2 blew the budget by a second.
        //
        // Only the Windows CI leg ever caught it, because Windows has no
        // process-group kill and the grandchild genuinely survives; on Unix
        // `killpg` reaps it and both pipes close at once, so the sum was never
        // paid. A platform-specific test failure for a platform-independent bug.
        //
        // `start_paused` means this asserts the awaited duration rather than
        // wall-clock, so it is exact and takes no real time.
        let started = tokio::time::Instant::now();

        let result = finish(
            Some(read_into_shared(NeverEnds)),
            Some(read_into_shared(NeverEnds)),
            None,
            Some(KillReason::Cancelled),
            Some(1),
            Instant::now(),
        )
        .await;

        let waited = started.elapsed();
        assert!(result.is_ok());
        assert!(
            waited < DRAIN_GRACE * 2,
            "the drains ran in sequence: waited {waited:?} for a {DRAIN_GRACE:?} grace"
        );
        assert!(
            waited <= DRAIN_GRACE + Duration::from_millis(50),
            "draining two held pipes must cost one grace period, not two; \
             waited {waited:?}"
        );
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn a_process_that_exited_on_its_own_is_not_bounded_at_all() {
        // The grace applies only to a killed process. One that exited has closed
        // its pipes, so the drain finishes immediately and an unbounded await
        // costs nothing — bounding it would risk truncating output that was
        // already there.
        let started = tokio::time::Instant::now();

        let result = finish(None, None, Some(0), None, Some(1), Instant::now()).await;

        assert!(result.is_ok());
        assert!(started.elapsed() < Duration::from_millis(50));
    }
}
