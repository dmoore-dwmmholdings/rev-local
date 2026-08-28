//! `VcsAdapter` trait plus git, GitHub and Subversion implementations.
//!
//! The pipeline never branches on which VCS a repository uses. It asks an adapter
//! for changes and for a materialized tree; everything that differs between a git
//! commit, a GitHub pull request and an SVN revision lives behind [`VcsAdapter`].
//!
//! Two invariants run through this crate:
//!
//! - **Nothing mutates the repository under review.** Materialization happens in a
//!   [`ScratchDir`], never in the user's checkout.
//! - **Nothing is dropped without a reason.** A change an adapter decides not to
//!   review carries a `skip_reason`, so it appears in the run record rather than
//!   simply never showing up (SPEC §18).

mod adapter;
pub mod git;
pub mod github;
mod scratch;
pub mod skip_rules;
pub mod svn;

pub use adapter::{
    ChangeContext, DetectedChange, HookMode, HookReport, ProbeProblem, ProbeReport, Result,
    VcsAdapter, VcsError,
};
pub use git::{CursorState, DiscoveryEvent, GitAdapter, GitError, GitOutput, GitRunner};
pub use github::{GitHubTransport, GitHubWrite, TransportSelection, WriteRefused};
pub use scratch::{RunOutcome, ScratchDir};
pub use skip_rules::{evaluate as evaluate_skip, reviewable_paths, Skip, SkipReason};
pub use svn::{SvnError, SvnOutput, SvnRunner};

/// The `bash` that can actually run a POSIX script on this machine.
///
/// Not simply `"bash"`. On Windows, `bash` on `PATH` is
/// `C:\Windows\System32\bash.exe` — the **WSL launcher** — which on a machine
/// with no distribution installed prints "Windows Subsystem for Linux has no
/// installed distributions" (in UTF-16, on stdout) and exits 1. Every harness
/// that ran `fixtures/build.sh` through `Command::new("bash")` failed that way on
/// the Windows CI leg, and the message was invisible until the harnesses started
/// reporting stdout.
///
/// Order: an explicit `REVLOCAL_BASH` override, then Git for Windows' bash where
/// it exists, then plain `bash` — which is right everywhere else.
///
/// Lives here rather than in each test file because five harnesses need it, and
/// RL-205's lesson was that the duplication is the thing to remove.
pub fn bash_program() -> std::path::PathBuf {
    if let Some(explicit) = std::env::var_os("REVLOCAL_BASH") {
        return std::path::PathBuf::from(explicit);
    }

    #[cfg(windows)]
    {
        for candidate in [
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Program Files\Git\usr\bin\bash.exe",
            r"C:\Program Files (x86)\Git\bin\bash.exe",
        ] {
            let path = std::path::Path::new(candidate);
            if path.is_file() {
                return path.to_path_buf();
            }
        }
    }

    std::path::PathBuf::from("bash")
}
