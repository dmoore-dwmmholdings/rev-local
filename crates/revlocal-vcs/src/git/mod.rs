//! The git adapter (SPEC §6.2).

pub mod cmd;
pub mod discover;
pub mod recover;

pub use cmd::{non_interactive_env, run, GitError, GitOutput, GitRunner, DEFAULT_TIMEOUT};
pub use discover::{discover_branch, merge_discoveries, resolve_branches};
pub use recover::{
    classify_cursor, fetch, has_remote, mark_superseded_by_rewrite, patch_ids, CursorState,
    DiscoveryEvent, FetchOutcome,
};
