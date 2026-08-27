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
mod scratch;

pub use adapter::{
    ChangeContext, DetectedChange, HookMode, HookReport, ProbeProblem, ProbeReport, Result,
    VcsAdapter, VcsError,
};
pub use git::{GitError, GitOutput, GitRunner};
pub use scratch::{RunOutcome, ScratchDir};
