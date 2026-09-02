//! The local report target (RL-1507, SPEC §11.1).
//!
//! # Why a file is a publish target and not a side effect
//!
//! "GitHub issues, Andare issues, **or local reports**" was in the product's first
//! sentence, and the local report was the one output that never existed. Findings
//! sat in a SQLite table with no way to read them outside the app — which meant a
//! machine with no tracker configured reviewed its commits and produced nothing
//! anybody, or any other agent, could act on.
//!
//! Making it a [`PublishTarget`] rather than a step inside the executor is what
//! gets it the rest of §11 for free: risk gating, the approvals inbox, the audit
//! log, retries, and — the one that matters here — §11.6's idempotency. The
//! fingerprint is the filename, so re-reviewing a change that still has the same
//! problem rewrites one file instead of accumulating a directory of duplicates.
//!
//! # It writes, then renames
//!
//! A reader — the coding agent this output exists for — must never see half a
//! report. Writing to a temporary name in the same directory and renaming over
//! the destination is atomic on every platform the app ships to, so a file that
//! exists is a file that is complete.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use revlocal_core::{Capability, CapabilitySet, PublishAction, PublishReceipt, TargetHealth};
use serde::{Deserialize, Serialize};

use crate::target::{PublishError, PublishTarget};

/// The id this target is registered and stored under.
pub const TARGET_ID: &str = "report";

/// Everything the stored action carries for one report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportPayload {
    /// Which repository the finding is in, used as the directory name.
    pub repo: String,
    /// The finding's title, which becomes the report's heading.
    pub title: String,
    /// The finding's body — the same text an issue would have carried.
    pub body: String,
    /// The fingerprint, which becomes the filename.
    pub fingerprint: String,
}

impl ReportPayload {
    /// The markdown written to disk.
    pub fn render(&self) -> String {
        format!("# {}\n\n{}", self.title.trim(), self.body.trim_end())
    }
}

/// A directory name that survives a repository called `foo/bar (old)`.
///
/// Anything outside the safe set becomes `-`, and runs collapse, so two
/// repositories cannot produce a path that escapes the root. A name that reduced
/// to nothing — all punctuation — falls back to a constant rather than to the
/// empty string, which would write the file into the root and look like a bug in
/// the reader.
fn slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches(['-', '.']).to_owned();
    if trimmed.is_empty() {
        "repository".to_owned()
    } else {
        trimmed
    }
}

/// Findings written to disk as markdown, one file per finding.
#[derive(Debug, Clone)]
pub struct ReportTarget {
    root: PathBuf,
}

impl ReportTarget {
    /// Reports under `root`, one directory per repository.
    pub const fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// The conventional location beside the database.
    pub fn beside(data_dir: &Path) -> Self {
        Self::new(data_dir.join("reports"))
    }

    /// Where this repository's reports live.
    pub fn directory_for(&self, repo: &str) -> PathBuf {
        self.root.join(slug(repo))
    }

    /// Where one finding's report lives.
    pub fn path_for(&self, repo: &str, fingerprint: &str) -> PathBuf {
        self.directory_for(repo)
            .join(format!("{}.md", slug(fingerprint)))
    }

    fn rejected(detail: String) -> PublishError {
        PublishError::Rejected {
            target: TARGET_ID.to_owned(),
            status: None,
            detail,
        }
    }

    fn transport(detail: String) -> PublishError {
        PublishError::Transport {
            target: TARGET_ID.to_owned(),
            detail,
        }
    }
}

#[async_trait]
impl PublishTarget for ReportTarget {
    fn id(&self) -> &str {
        TARGET_ID
    }

    async fn discover(&self) -> Result<CapabilitySet, PublishError> {
        // A file can hold a finding and nothing else. Claiming `SetStatus` here
        // would let the executor queue work this target silently drops.
        Ok(CapabilitySet::new(vec![Capability::CreateIssue]))
    }

    async fn execute(&self, action: &PublishAction) -> Result<PublishReceipt, PublishError> {
        if action.capability != Capability::CreateIssue {
            return Err(PublishError::Unsupported {
                target: TARGET_ID.to_owned(),
                capability: action.capability,
            });
        }

        let payload: ReportPayload = serde_json::from_str(&action.payload_json)
            .map_err(|e| Self::rejected(format!("the stored payload is not a report: {e}")))?;

        let path = self.path_for(&payload.repo, &payload.fingerprint);
        let directory = self.directory_for(&payload.repo);
        // §11.6: the same finding rewrites its own file. Recorded so the audit log
        // can tell a fresh report from a redelivery, the same as Andare's.
        let existed = path.exists();

        let destination = path.clone();
        let rendered = payload.render();
        let write = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            std::fs::create_dir_all(&directory)?;
            // Same directory as the destination, so the rename cannot cross a
            // filesystem boundary and fall back to a copy.
            let temporary = destination.with_extension("md.part");
            std::fs::write(&temporary, rendered)?;
            std::fs::rename(&temporary, &destination)
        })
        .await
        .map_err(|e| Self::transport(format!("the write task did not finish: {e}")))?;

        write.map_err(|e| Self::transport(format!("could not write the report: {e}")))?;

        Ok(PublishReceipt {
            external_ref: Some(path.display().to_string()),
            response_json: None,
            deduplicated: existed,
        })
    }

    async fn health(&self) -> Result<TargetHealth, PublishError> {
        let root = self.root.clone();
        let reachable = tokio::task::spawn_blocking(move || std::fs::create_dir_all(&root))
            .await
            .map_err(|e| Self::transport(format!("the health check did not finish: {e}")))?;

        Ok(match reachable {
            Ok(()) => TargetHealth {
                reachable: true,
                capabilities: CapabilitySet::new(vec![Capability::CreateIssue]),
                detail: None,
            },
            Err(error) => TargetHealth {
                reachable: false,
                capabilities: CapabilitySet::new(Vec::new()),
                detail: Some(format!(
                    "could not create {}: {error}\n  try: check the directory is writable",
                    self.root.display()
                )),
            },
        })
    }
}
