//! Scheduler, trigger sources, run orchestrator and budget guard.
//!
//! Scaffolded by `RL-101`; implementation lands in later work items.

pub mod autonomy;
pub mod depth;
pub mod logging;
pub mod normalize;
pub mod pipeline;
pub mod prompt;
pub mod state_machine;
pub mod truncation;

pub use autonomy::{
    disposition, mode_change_detail, reviews_run, widens, Disposition, AUDIT_KIND_MODE_CHANGED,
};
pub use logging::{
    init as init_logging, LoggingError, LoggingHandle, RedactingJsonLayer, RedactingVisitor,
};
pub use state_machine::{
    recover_interrupted, transition, NullSink, RecoveryReport, RunEvent, RunEventSink,
    DEFAULT_MAX_ATTEMPTS, INTERRUPTED,
};

/// The name of this crate, used by the workspace layout test in `revlocal-cli`.
pub const CRATE_NAME: &str = "revlocal-daemon";
