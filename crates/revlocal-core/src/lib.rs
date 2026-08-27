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

mod audit;
pub mod budget;
mod change;
pub mod config;
mod enums;
mod error;
mod finding;
pub mod fingerprint;
pub mod ids;
mod publish;
pub mod redact;
mod repo;
pub mod risk;
mod run;

/// A point in time, as stored in SPEC §5's `TEXT` timestamp columns.
///
/// Always UTC. The domain crate represents instants but never reads the clock —
/// chrono is taken without its `clock` feature — so a timestamp is always supplied
/// by the caller and a test can pin one (SPEC §4.1).
pub type Timestamp = chrono::DateTime<chrono::Utc>;

pub use audit::{AuditEntry, BudgetLedgerEntry};
pub use budget::{BudgetDecision, BudgetLimits, ExhaustedLimit};
pub use change::{Change, DiffStat, FileDiff, FileStatus};
pub use config::{
    effective_autonomy, merge_in_repo, BudgetSettings, ConfigError, ConfigWarning, GlobalConfig,
    GlobalSettings, InRepoConfig, McpServerSettings, MergeOutcome, OnExhausted, RepoConfig,
    SecretRef,
};
pub use enums::{
    AutonomyMode, Capability, Category, ChangeKind, Depth, EngineKind, FindingState,
    PublishActionStatus, RepoKind, RiskClass, RunStatus, Severity, TriggerSource, Verdict,
};
pub use error::{DomainError, ParseEnumError, Result};
pub use finding::{Finding, Suppression, LOW_CONFIDENCE_THRESHOLD, TITLE_MAX_CHARS};
pub use fingerprint::{fingerprint, normalize_path, normalize_title, FINGERPRINT_HEX_LEN};
pub use ids::{AuditId, ChangeId, FindingId, PublishActionId, RepoId, RunId, SuppressionId};
pub use publish::{CapabilitySet, PublishAction, PublishReceipt, TargetHealth};
pub use redact::{is_sensitive_field, redact, redact_field, REDACTED};
pub use repo::{Cursor, Repo};
pub use risk::{
    classify, ActionIntent, CheckConclusion, RiskAssessment, RiskInputs, RiskReason,
    DEFAULT_BURST_THRESHOLD,
};
pub use run::{IllegalTransition, Run, Usage};
