//! Subversion support (SPEC §6.4).
//!
//! Nothing here runs unless a repository has `kind = 'svn'`. §6.4 makes a missing
//! `svn` binary a blocking prerequisite **for SVN repositories only**, so a
//! machine with none must keep reviewing its git repositories exactly as before —
//! which is a property of this module never being reached from the git path, not
//! of it handling absence gracefully.

pub mod cmd;
pub mod discover;
pub mod materialize;

pub use cmd::{
    doctor_line, is_available, non_interactive_env, CertFailure, SvnError, SvnOutput, SvnRunner,
    DEFAULT_TIMEOUT,
};
pub use discover::{discover, parse_log_xml, Discovery, SvnPath, SvnRevision, WatchedPaths};
pub use materialize::{
    export_path, materialize, parse_summary, render_property_only, BinarySummary, ChangedPath,
    EXPORT_SUBDIR,
};
