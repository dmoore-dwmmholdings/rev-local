//! `Engine` trait plus the Claude Code, Codex and mock review runners.
//!
//! Scaffolded by `RL-101`; implementation lands in later work items.

pub mod engine;
pub mod ladder;
pub mod mock;
pub mod schema;
pub mod template;

pub use engine::{
    Engine, EngineError, EngineId, EngineOutcome, EngineProbe, EngineProblem, EngineTask,
    RawFinding, Result,
};
pub use ladder::{
    last_fenced_json_block, resolve, LadderOutcome, RepairPass, RepairResult, Rung, OUT_DIR_ENV,
    RESULT_FILE,
};
pub use mock::{MockBehaviour, MockEngine};
pub use schema::{
    validate, DroppedFinding, SchemaError, ValidatedResult, RESULT_SCHEMA_V1,
    SUPPORTED_SCHEMA_VERSION,
};
pub use template::{Invocation, InvocationTemplate, RenderContext, TemplateError, PLACEHOLDERS};

/// The name of this crate, used by the workspace layout test in `revlocal-cli`.
pub const CRATE_NAME: &str = "revlocal-engine";
