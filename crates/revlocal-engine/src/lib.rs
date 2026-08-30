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

pub mod job;
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
    withheld_auth_variables, KillReason, Supervised, CANCEL_GRACE, GRACE,
};
pub use template::{Invocation, InvocationTemplate, RenderContext, TemplateError, PLACEHOLDERS};

/// The name of this crate, used by the workspace layout test in `revlocal-cli`.
pub const CRATE_NAME: &str = "revlocal-engine";

/// The mock engine launcher for this platform.
///
/// `fixtures/mock-engine/run` is a bash shim and `run.cmd` is its Windows
/// equivalent — RL-203 wrote both, and every caller then hardcoded the POSIX one.
/// On Windows that produces `%1 is not a valid Win32 application` (os error 193),
/// because `CreateProcess` will not exec a file with a shebang.
///
/// The two shims exist so the *process shape* matches on both platforms: each one
/// hands off to `mock-engine.mjs` so the process the runner spawned is node, which
/// is what makes the `hang` mode's ignore-SIGTERM behaviour testable.
pub fn mock_engine_program() -> std::path::PathBuf {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join("mock-engine");

    if cfg!(windows) {
        fixtures.join("run.cmd")
    } else {
        fixtures.join("run")
    }
}
