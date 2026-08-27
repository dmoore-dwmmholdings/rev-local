//! The `repo` and `cursor` tables (SPEC §5).

use crate::{AutonomyMode, EngineKind, RepoId, RepoKind, Timestamp};
use serde::{Deserialize, Serialize};

/// A repository rev-local watches (`repo`, SPEC §5).
///
/// `local_path` and `remote_url` are both optional because which one is meaningful
/// depends on [`RepoKind`]: a `git` repo needs a working copy, a `github` repo needs
/// a URL, and an `svn` repo may have either a mirror or just a root URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repo {
    /// Primary key.
    pub id: RepoId,
    /// Unique human-facing name.
    pub name: String,
    /// Which VCS backs it.
    pub kind: RepoKind,
    /// Working copy, clone or mirror on disk.
    pub local_path: Option<String>,
    /// Origin, GitHub URL, or SVN root URL.
    pub remote_url: Option<String>,
    /// `main` for git; the trunk path for SVN.
    pub default_branch: Option<String>,
    /// Which engine reviews this repo (decision D3 — per repo, not global).
    pub engine: EngineKind,
    /// This repo's requested autonomy. The effective mode is capped by the global
    /// ceiling; see [`AutonomyMode::effective`].
    pub autonomy: AutonomyMode,
    /// Whether triggers fire for this repo at all.
    pub enabled: bool,
    /// `RepoConfig` (SPEC §13.2), stored as JSON in `repo.config_json`.
    pub config_json: String,
    /// When the row was created.
    pub created_at: Timestamp,
    /// When the row last changed.
    pub updated_at: Timestamp,
}

/// How far rev-local has already looked, per scope (`cursor`, SPEC §5).
///
/// The primary key is `(repo_id, scope)`, so there is one cursor per branch, per
/// PR stream, or per SVN path — not one per repo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    /// Which repo this cursor belongs to.
    pub repo_id: RepoId,
    /// `commits:<branch>` | `prs` | `svn:<path>`.
    pub scope: String,
    /// A SHA, a PR `updated_at`, or a revision number — interpreted by the adapter.
    pub value: String,
    /// When the cursor last advanced.
    pub updated_at: Timestamp,
}

impl Cursor {
    /// The `commits:<branch>` scope string for a git branch.
    pub fn commits_scope(branch: &str) -> String {
        format!("commits:{branch}")
    }

    /// The `svn:<path>` scope string for a Subversion path.
    pub fn svn_scope(path: &str) -> String {
        format!("svn:{path}")
    }

    /// The scope string for a repository's pull-request stream.
    pub const PRS_SCOPE: &'static str = "prs";
}
