//! The GitHub adapter (SPEC §6.3).
//!
//! A superset of the git adapter: everything git does, plus pull-request discovery
//! and PR-aware publishing. What differs first is *how it reaches GitHub at all*,
//! which is [`transport`].

pub mod pull_requests;
pub mod transport;

pub use pull_requests::{
    discover as discover_pull_requests, mark_covered_by_pr, parse_gh_pr_list,
    superseded_fingerprints, to_detected_change, GhCliSource, PullRequest, PullRequestSource,
    GH_PR_FIELDS,
};
pub use transport::{
    authorize, probe, select, GitHubTransport, GitHubWrite, RungReport, TransportProbes,
    TransportSelection, WriteRefused,
};
