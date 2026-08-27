//! Scheduler, trigger sources, run orchestrator and budget guard.
//!
//! Scaffolded by `RL-101`; implementation lands in later work items.

pub mod logging;

pub use logging::{
    init as init_logging, LoggingError, LoggingHandle, RedactingJsonLayer, RedactingVisitor,
};

/// The name of this crate, used by the workspace layout test in `revlocal-cli`.
pub const CRATE_NAME: &str = "revlocal-daemon";
