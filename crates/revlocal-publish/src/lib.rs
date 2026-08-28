//! `PublishTarget` trait plus the publish action queue (SPEC §11.1, §11.6).
//!
//! Publishing is the only part of rev-local that changes something outside the
//! machine it runs on. Everything here is shaped by that: an action is recorded
//! before it is attempted, delivery is at-least-once with an exactly-once effect,
//! and a target that is slow, rate limited or broken degrades itself and nothing
//! else.

pub mod queue;
pub mod retry;
pub mod target;

pub use queue::{DispatchReport, PublishQueue, QueueConfig, QueueError, DEFAULT_CONCURRENCY};
pub use retry::{RetryPolicy, BASE_DELAY, JITTER_FRACTION, MAX_ATTEMPTS, MAX_DELAY};
pub use target::{PublishError, PublishTarget};

/// The name of this crate, used by the workspace layout test in `revlocal-cli`.
pub const CRATE_NAME: &str = "revlocal-publish";
