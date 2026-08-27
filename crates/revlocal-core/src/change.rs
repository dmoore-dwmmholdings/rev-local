//! The `change` table and its diff payloads (SPEC §5, §6.1).

use crate::{ChangeId, ChangeKind, RepoId, Timestamp};
use serde::{Deserialize, Serialize};

/// The atomic thing being reviewed (`change`, SPEC §3).
///
/// `external_id` is the change's identity in its own system — a SHA, `pr#:headsha`,
/// `r1234`, or `branch@r1234` — and `(repo_id, kind, external_id)` is unique, which
/// is what makes rediscovering the same change idempotent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    /// Primary key.
    pub id: ChangeId,
    /// Which repo it landed in.
    pub repo_id: RepoId,
    /// Which of the four kinds of change this is.
    pub kind: ChangeKind,
    /// Identity in the originating system. Unique per `(repo_id, kind)`.
    pub external_id: String,
    /// Commit subject, PR title, or SVN log summary.
    pub title: Option<String>,
    /// Author display name, if the VCS reports one.
    pub author_name: Option<String>,
    /// Author email, if the VCS reports one.
    pub author_email: Option<String>,
    /// When the author made the change, as distinct from when it was detected.
    pub authored_at: Option<Timestamp>,
    /// Branch the change is on, where the concept applies.
    pub branch: Option<String>,
    /// PR base SHA, or the SVN merge base.
    pub base_ref: Option<String>,
    /// Head SHA or revision.
    pub head_ref: Option<String>,
    /// Web URL, if one is known.
    pub url: Option<String>,
    /// Size of the change; stored as JSON in `change.diff_stat_json`.
    pub diff_stat: DiffStat,
    /// When rev-local first saw it.
    pub detected_at: Timestamp,
}

/// How large a change is (`change.diff_stat_json`, SPEC §5).
///
/// Drives depth selection (SPEC §9.3) and the skip rules (§9.4), so it is recorded
/// even for changes that are never reviewed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffStat {
    /// Number of files touched.
    pub files: u32,
    /// Lines added.
    pub insertions: u64,
    /// Lines removed.
    pub deletions: u64,
}

impl DiffStat {
    /// Total lines changed, which is what SPEC §9.3's 20k threshold measures.
    pub const fn changed_lines(&self) -> u64 {
        self.insertions + self.deletions
    }
}

/// What happened to one file in a change (SPEC §6.1, `ChangeContext::diff_files`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiff {
    /// Path relative to the repository root, in the change's *new* state.
    ///
    /// For a deletion this is the path as it last existed.
    pub path: String,
    /// Previous path, set only when the file was renamed or copied.
    pub previous_path: Option<String>,
    /// How the file changed.
    pub status: FileStatus,
    /// Lines added in this file.
    pub insertions: u64,
    /// Lines removed in this file.
    pub deletions: u64,
    /// Whether the VCS reports the file as binary. Binary files carry no hunks.
    pub binary: bool,
}

string_enum! {
    /// What happened to a file in a change.
    pub enum FileStatus {
        /// The file did not exist before.
        Added => "added",
        /// The file existed and its contents changed.
        Modified => "modified",
        /// The file no longer exists.
        Deleted => "deleted",
        /// The file moved; `previous_path` says from where.
        Renamed => "renamed",
        /// The file was copied from `previous_path`.
        Copied => "copied",
        /// Only the file's mode changed.
        TypeChanged => "type_changed",
    }
}
