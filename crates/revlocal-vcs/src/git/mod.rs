//! The git adapter (SPEC §6.2).

pub mod adapter;
pub mod cmd;
pub mod discover;
pub mod materialize;
pub mod recover;

pub use adapter::GitAdapter;
pub use cmd::{non_interactive_env, run, GitError, GitOutput, GitRunner, DEFAULT_TIMEOUT};
pub use discover::{discover_branch, merge_discoveries, resolve_branches};
pub use materialize::{
    is_bare, materialize, prune_worktrees, release_worktree, worktree_path, WORKTREE_SUBDIR,
};
pub use recover::{
    classify_cursor, fetch, has_remote, mark_superseded_by_rewrite, patch_ids, CursorState,
    DiscoveryEvent, FetchOutcome,
};
