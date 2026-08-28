//! The `PublishTarget` trait and how a target fails (SPEC §11.1, §11.6).

use async_trait::async_trait;
use revlocal_core::{Capability, CapabilitySet, PublishAction, PublishReceipt, TargetHealth};

/// One place findings can be published to (SPEC §11.1).
///
/// `async_trait` rather than native `async fn`: the queue holds
/// `Arc<dyn PublishTarget>` so it can carry GitHub, Andare and Trama in one
/// collection, and native async fns in traits are not dyn-compatible.
///
/// `discover` and `health` are separate on purpose. A target can be reachable and
/// still unable to do what a run needs — an MCP server that answered but has no
/// tool bound to `CreateIssue` is healthy and incapable, and collapsing the two
/// would report "target down" for a configuration problem.
#[async_trait]
pub trait PublishTarget: Send + Sync {
    /// `github` | `andare` | `trama` | a custom id.
    fn id(&self) -> &str;

    /// What this target can actually do right now.
    async fn discover(&self) -> Result<CapabilitySet, PublishError>;

    /// Perform one action.
    ///
    /// Takes the action rather than a payload so a target can read
    /// `idempotency_key` — §11.6's exactly-once *effect* is the target's to
    /// honour where the remote system supports it.
    async fn execute(&self, action: &PublishAction) -> Result<PublishReceipt, PublishError>;

    /// Whether the target is reachable, and why not when it is not.
    async fn health(&self) -> Result<TargetHealth, PublishError>;
}

/// Why an action could not be delivered.
///
/// The variants exist to answer exactly one question — should this be tried
/// again? — because §11.6 makes that structural: transport, 5xx and rate limits
/// retry; 4xx is terminal. Flattening these into one string would push that
/// decision into pattern-matching on error text, which is the thing ADR 0023 was
/// written about.
#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    /// The target could not be reached at all.
    #[error(
        "could not reach `{target}`: {detail}\n  try: check the target is configured and running"
    )]
    Transport {
        /// Which target.
        target: String,
        /// What happened.
        detail: String,
    },

    /// The target asked for less traffic.
    #[error("`{target}` is rate limiting; {}", match retry_after_secs { Some(s) => format!("it asked to wait {s}s"), None => "it did not say for how long".to_owned() })]
    RateLimited {
        /// Which target.
        target: String,
        /// What the target asked for, where it said.
        retry_after_secs: Option<u64>,
    },

    /// The target failed on its own side.
    #[error("`{target}` failed{}: {detail}", match status { Some(code) => format!(" with {code}"), None => String::new() })]
    Server {
        /// Which target.
        target: String,
        /// The HTTP status, where there was one.
        status: Option<u16>,
        /// What it said.
        detail: String,
    },

    /// The target refused the request, and would refuse it again.
    #[error("`{target}` refused the request{}: {detail}\n  try: this will not succeed on a retry — fix the request", match status { Some(code) => format!(" with {code}"), None => String::new() })]
    Rejected {
        /// Which target.
        target: String,
        /// The HTTP status, where there was one.
        status: Option<u16>,
        /// What it said.
        detail: String,
    },

    /// The target does not do this.
    #[error("`{target}` cannot {capability}\n  try: remove the capability from this target, or map it (see `revlocal targets map`)")]
    Unsupported {
        /// Which target.
        target: String,
        /// What was asked for.
        capability: Capability,
    },
}

impl PublishError {
    /// Whether §11.6 says to try again.
    ///
    /// Deliberately total rather than a `_ =>` catch-all: a new variant should
    /// make this fail to compile, so somebody decides, rather than inheriting
    /// "terminal" by accident.
    pub const fn is_retryable(&self) -> bool {
        match self {
            Self::Transport { .. } | Self::RateLimited { .. } | Self::Server { .. } => true,
            Self::Rejected { .. } | Self::Unsupported { .. } => false,
        }
    }

    /// How long the target asked to be left alone, where it said.
    pub const fn retry_after_secs(&self) -> Option<u64> {
        match self {
            Self::RateLimited {
                retry_after_secs, ..
            } => *retry_after_secs,
            _ => None,
        }
    }

    /// Which target this is about.
    pub fn target(&self) -> &str {
        match self {
            Self::Transport { target, .. }
            | Self::RateLimited { target, .. }
            | Self::Server { target, .. }
            | Self::Rejected { target, .. }
            | Self::Unsupported { target, .. } => target,
        }
    }
}
