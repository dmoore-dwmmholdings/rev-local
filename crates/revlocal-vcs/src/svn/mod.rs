//! Subversion support (SPEC §6.4).
//!
//! Nothing here runs unless a repository has `kind = 'svn'`. §6.4 makes a missing
//! `svn` binary a blocking prerequisite **for SVN repositories only**, so a
//! machine with none must keep reviewing its git repositories exactly as before —
//! which is a property of this module never being reached from the git path, not
//! of it handling absence gracefully.

pub mod cmd;
pub mod demotion;
pub mod discover;
pub mod materialize;
pub mod pseudo_pr;

pub use cmd::{
    doctor_line, is_available, non_interactive_env, CertFailure, SvnError, SvnOutput, SvnRunner,
    DEFAULT_TIMEOUT,
};
pub use demotion::{
    constituent_revisions, plan, prior_context, DemotionPlan, Disposition, PlannedFinding,
};
pub use discover::{discover, parse_log_xml, Discovery, SvnPath, SvnRevision, WatchedPaths};
pub use materialize::{
    export_path, materialize, parse_summary, render_property_only, BinarySummary, ChangedPath,
    EXPORT_SUBDIR,
};
pub use pseudo_pr::{
    classify_gain, detect, fork_point, gained_branches, mergeinfo_at, pseudo_pr_diff,
    pseudo_pr_external_id, Detection, GainedRange, Heuristics, MergeEvidence, MergeInfo,
    MergeStyle, DEFAULT_PSEUDO_PR_MIN_FILES,
};
