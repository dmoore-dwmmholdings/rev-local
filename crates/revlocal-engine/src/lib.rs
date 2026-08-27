//! `Engine` trait plus the Claude Code, Codex and mock review runners.
//!
//! Scaffolded by `RL-101`; implementation lands in later work items.

pub mod engine;
pub mod mock;
pub mod schema;

pub use engine::{
    Engine, EngineError, EngineId, EngineOutcome, EngineProbe, EngineProblem, EngineTask,
    RawFinding, Result,
};
pub use mock::{MockBehaviour, MockEngine};
pub use schema::{
    validate, DroppedFinding, SchemaError, ValidatedResult, RESULT_SCHEMA_V1,
    SUPPORTED_SCHEMA_VERSION,
};

/// The name of this crate, used by the workspace layout test in `revlocal-cli`.
pub const CRATE_NAME: &str = "revlocal-engine";
