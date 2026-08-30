//! Scheduler, trigger sources, run orchestrator and budget guard.
//!
//! Scaffolded by `RL-101`; implementation lands in later work items.

pub mod approvals;
pub mod approvals_view;
pub mod autonomy;
pub mod backfill;
pub mod budgets;
pub mod dashboard;
pub mod depth;
pub mod findings_view;
pub mod gating;
pub mod hooks;
pub mod kill_switch;
pub mod logging;
pub mod normalize;
pub mod pipeline;
pub mod poll;
pub mod prompt;
pub mod repository_view;
pub mod run_view;
pub mod scheduler;
pub mod state_machine;
pub mod trigger_receiver;
pub mod triggers;
pub mod truncation;
pub mod tunnel;
pub mod view;
pub mod webhook;

pub use approvals::{
    decision_detail, expires_at, expiry_detail, payload_digest, payload_matches_approval,
    verify_before_send, ApprovalError, Decision, InboxItem, AUDIT_KIND_EXPIRED,
    DEFAULT_APPROVAL_TTL_HOURS, REASON_EXPIRED,
};
pub use autonomy::{
    disposition, mode_change_detail, reviews_run, widens, Disposition, AUDIT_KIND_MODE_CHANGED,
};
pub use budgets::{
    check as check_budget, day_of, exhausted_detail, records_the_change, resumes_after_reset,
    BudgetVerdict, Exhausted, RunSlots, AUDIT_KIND_BUDGET_EXHAUSTED, DEFAULT_MAX_CONCURRENT_RUNS,
};
pub use gating::{gate, GateContext, GatedAction};
pub use kill_switch::{
    cancels, may_dispatch, process_is_alive, reap, switch_detail, KillSwitch, PauseReport,
    AUDIT_KIND_PAUSED, AUDIT_KIND_RESUMED, CANCELLABLE,
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
