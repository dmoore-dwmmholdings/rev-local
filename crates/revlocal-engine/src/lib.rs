//! `Engine` trait plus the Claude Code, Codex and mock review runners.
//!
//! Scaffolded by `RL-101`; implementation lands in later work items.

pub mod engine;
/// The review prompt template (SPEC §9.2).
///
/// Compiled in rather than read from disk: a packaged desktop app has no `crates/`
/// directory beside the binary, so a runtime read would work in every test and fail
/// on every install.
pub const REVIEW_TEMPLATE: &str = include_str!("../prompts/review.md.hbs");

pub mod ladder;
pub mod mock;
pub mod runner;
pub mod schema;
pub mod supervise;
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
pub use runner::{CliEngine, PROMPT_FILE};
pub use schema::{
    validate, DroppedFinding, SchemaError, ValidatedResult, RESULT_SCHEMA_V1,
    SUPPORTED_SCHEMA_VERSION,
};
pub use supervise::{
    filtered_env, is_denied, supervise, timeout_for, withheld_auth_remediation,
    withheld_auth_variables, KillReason, Supervised, GRACE,
};
pub use template::{Invocation, InvocationTemplate, RenderContext, TemplateError, PLACEHOLDERS};

/// The name of this crate, used by the workspace layout test in `revlocal-cli`.
pub const CRATE_NAME: &str = "revlocal-engine";
