//! `Engine` trait plus the Claude Code, Codex and mock review runners.
//!
//! Scaffolded by `RL-101`; implementation lands in later work items.

pub mod engine;
pub mod mock;

pub use engine::{
    Engine, EngineError, EngineId, EngineOutcome, EngineProbe, EngineProblem, EngineTask,
    RawFinding, Result,
};
pub use mock::{MockBehaviour, MockEngine};

/// The name of this crate, used by the workspace layout test in `revlocal-cli`.
pub const CRATE_NAME: &str = "revlocal-engine";
