//! Domain types, configuration, errors and the risk model for rev-local.
//!
//! This crate is the shared vocabulary of the workspace and has **no I/O
//! dependencies** — no tokio, no sqlx, no reqwest (SPEC §4.1, enforced by
//! `RL-104`). Everything here is a value that can be constructed and asserted on
//! in a unit test without a runtime, a database or a network.
//!
//! Two conventions run through the module:
//!
//! - **Enums carry their wire spelling explicitly.** The literal in each
//!   [`string_enum!`] declaration is the same string that appears in the SQLite
//!   `CHECK` constraint in SPEC §5 and in engine/MCP JSON, so renaming a Rust
//!   variant cannot silently change what is stored.
//! - **Ids are newtypes, and declaration order is meaningful.** See [`ids`] and
//!   ADR 0004.

#[macro_use]
mod macros;

mod enums;
mod error;
pub mod ids;

pub use enums::{
    AutonomyMode, Capability, Category, ChangeKind, Depth, EngineKind, FindingState,
    PublishActionStatus, RepoKind, RiskClass, RunStatus, Severity, TriggerSource, Verdict,
};
pub use error::{DomainError, ParseEnumError, Result};
pub use ids::{AuditId, ChangeId, FindingId, PublishActionId, RepoId, RunId, SuppressionId};
