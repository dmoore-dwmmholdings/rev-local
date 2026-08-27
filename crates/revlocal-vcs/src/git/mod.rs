//! The git adapter (SPEC §6.2).

pub mod cmd;
pub mod discover;

pub use cmd::{non_interactive_env, run, GitError, GitOutput, GitRunner, DEFAULT_TIMEOUT};
pub use discover::{discover_branch, merge_discoveries, resolve_branches};
