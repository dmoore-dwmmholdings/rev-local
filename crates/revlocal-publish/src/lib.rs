//! `PublishTarget` trait plus the publish action queue (SPEC §11.1, §11.6).
//!
//! Publishing is the only part of rev-local that changes something outside the
//! machine it runs on. Everything here is shaped by that: an action is recorded
//! before it is attempted, delivery is at-least-once with an exactly-once effect,
//! and a target that is slow, rate limited or broken degrades itself and nothing
//! else.

pub mod check;
pub mod github;
pub mod queue;
pub mod report;
pub mod retry;
pub mod target;

pub use check::{
    conclusion_for, gh_commit_comment, gh_set_check, unresolved_check, CheckConclusion,
    CheckPayload, CheckStatus, CHECK_NAME,
};
pub use github::{
    compose, event_for, idempotency_key, DiffAnchors, ExistingReview, GitHubTarget, GitHubWriter,
    InlineComment, ReviewDraft, ReviewEvent, ReviewOptions, ReviewPayload,
};
pub use github::{
    find_own_review, gh_create_review, gh_list_reviews, gh_update_review, GhRequest, REVIEW_MARKER,
};
pub use queue::{DispatchReport, PublishQueue, QueueConfig, QueueError, DEFAULT_CONCURRENCY};
pub use report::{RunPublishReport, TargetOutcome, TargetState};
pub use retry::{RetryPolicy, BASE_DELAY, JITTER_FRACTION, MAX_ATTEMPTS, MAX_DELAY};
pub use target::{PublishError, PublishTarget};

/// The name of this crate, used by the workspace layout test in `revlocal-cli`.
pub const CRATE_NAME: &str = "revlocal-publish";
