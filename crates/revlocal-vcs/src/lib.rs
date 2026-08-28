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

pub use adapter::{
    ChangeContext, DetectedChange, HookMode, HookReport, ProbeProblem, ProbeReport, Result,
    VcsAdapter, VcsError,
};
pub use git::{CursorState, DiscoveryEvent, GitAdapter, GitError, GitOutput, GitRunner};
pub use github::{GitHubTransport, GitHubWrite, TransportSelection, WriteRefused};
pub use scratch::{RunOutcome, ScratchDir};
pub use skip_rules::{evaluate as evaluate_skip, reviewable_paths, Skip, SkipReason};
