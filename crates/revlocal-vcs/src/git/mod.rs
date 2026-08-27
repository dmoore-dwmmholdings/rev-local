//! The git adapter (SPEC §6.2).

pub mod cmd;

pub use cmd::{non_interactive_env, run, GitError, GitOutput, GitRunner, DEFAULT_TIMEOUT};
