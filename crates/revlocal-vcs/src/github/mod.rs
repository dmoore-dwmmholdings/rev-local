//! The GitHub adapter (SPEC §6.3).
//!
//! A superset of the git adapter: everything git does, plus pull-request discovery
//! and PR-aware publishing. What differs first is *how it reaches GitHub at all*,
//! which is [`transport`].

pub mod transport;

pub use transport::{
    authorize, probe, select, GitHubTransport, GitHubWrite, RungReport, TransportProbes,
    TransportSelection, WriteRefused,
};
