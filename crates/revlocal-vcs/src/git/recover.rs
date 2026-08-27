//! Fetching, and recovering from a rewritten history (SPEC §6.2).
//!
//! A cursor is a SHA on a branch. Both assumptions behind that can stop holding:
//! the branch can be force-pushed so the cursor is no longer an ancestor, and the
//! cursor's object can be garbage-collected so it is not in the repository at all.
//! Neither is exotic — a rebase-and-force-push is a normal day on many teams — and
//! both would otherwise surface as `rev-list` failing and discovery quietly finding
//! nothing.
//!
//! **Nothing here writes to the store.** `revlocal-vcs` does not depend on
//! `revlocal-store`, and it should not: the VCS layer's job is to notice, not to
//! record. Recovery returns [`DiscoveryEvent`]s as data and the caller writes them
//! to the audit log. That also makes the events assertable without a database.

use std::path::Path;

use super::cmd::{GitError, GitRunner};

/// Something a caller must record in the audit log (SPEC §5, decision D7).
///
/// Each of these is a moment where rev-local's idea of what it had reviewed turned
/// out to be wrong. SPEC §18: none of them may pass silently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveryEvent {
    /// The branch was force-pushed: the cursor is no longer an ancestor.
    HistoryRewritten {
        /// The branch that was rewritten.
        branch: String,
        /// Where the cursor was.
        old_cursor: String,
        /// The merge-base it was reset to, and where re-discovery resumes.
        reset_to: String,
    },

    /// The cursor's object is not in the repository at all.
    ///
    /// Distinct from [`HistoryRewritten`](Self::HistoryRewritten) because there is
    /// no merge-base to compute and therefore no safe resume point — the recovery
    /// is different and so is what an operator should do about it.
    CursorObjectMissing {
        /// The branch.
        branch: String,
        /// The cursor value that no longer resolves.
        old_cursor: String,
    },

    /// A fetch did not happen, and why.
    FetchSkipped {
        /// Why it was skipped.
        reason: String,
    },
}

impl DiscoveryEvent {
    /// The audit log `kind` for this event (SPEC §5).
    pub const fn audit_kind(&self) -> &'static str {
        match self {
            Self::HistoryRewritten { .. } => "history_rewritten",
            Self::CursorObjectMissing { .. } => "cursor_object_missing",
            Self::FetchSkipped { .. } => "fetch_skipped",
        }
    }
}

/// What a cursor turned out to be, checked against the branch as it is now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorState {
    /// No cursor: the branch has never been discovered.
    Fresh,
    /// The cursor is still an ancestor of the branch. The normal case.
    Valid(String),
    /// The branch was rewritten. Resume from the merge-base.
    Rewritten {
        /// Where the cursor was.
        old_cursor: String,
        /// The merge-base, and the new cursor.
        merge_base: String,
    },
    /// The cursor's object is gone.
    Missing {
        /// The cursor value that no longer resolves.
        old_cursor: String,
    },
}

impl CursorState {
    /// The cursor discovery should actually use.
    ///
    /// `None` for [`Fresh`](Self::Fresh) and [`Missing`](Self::Missing): with no
    /// resume point, the whole branch is re-discovered. That is deliberate. The
    /// alternative — skipping forward to the tip — would silently drop every change
    /// between the lost cursor and now, and losing a change is the one outcome this
    /// layer must not have. Re-discovery is bounded in cost because the store
    /// upserts by `(repo_id, kind, external_id)`, so a commit already reviewed is
    /// recognised rather than re-filed.
    pub fn effective(&self) -> Option<&str> {
        match self {
            Self::Fresh | Self::Missing { .. } => None,
            Self::Valid(sha) => Some(sha),
            Self::Rewritten { merge_base, .. } => Some(merge_base),
        }
    }

    /// The audit event this state implies, if any.
    pub fn event(&self, branch: &str) -> Option<DiscoveryEvent> {
        match self {
            Self::Fresh | Self::Valid(_) => None,
            Self::Rewritten {
                old_cursor,
                merge_base,
            } => Some(DiscoveryEvent::HistoryRewritten {
                branch: branch.to_owned(),
                old_cursor: old_cursor.clone(),
                reset_to: merge_base.clone(),
            }),
            Self::Missing { old_cursor } => Some(DiscoveryEvent::CursorObjectMissing {
                branch: branch.to_owned(),
                old_cursor: old_cursor.clone(),
            }),
        }
    }
}

/// Exit code `git merge-base --is-ancestor` uses for "no".
///
/// It reports the answer through the exit status rather than stdout, so the
/// difference between "not an ancestor" (1) and "that is not an object" (128) is
/// the difference between a rewrite and a missing cursor.
const NOT_AN_ANCESTOR: i32 = 1;

/// Check a cursor against a branch as it is now.
pub async fn classify_cursor(
    runner: &GitRunner,
    dir: &Path,
    branch: &str,
    cursor: Option<&str>,
) -> Result<CursorState, GitError> {
    let Some(cursor) = cursor else {
        return Ok(CursorState::Fresh);
    };

    // Does the object still exist? Asked first, because `--is-ancestor` on a
    // missing object fails in a way that looks like any other git error.
    let exists = runner
        .run(dir, &["cat-file", "-e", &format!("{cursor}^{{commit}}")])
        .await
        .is_ok();
    if !exists {
        return Ok(CursorState::Missing {
            old_cursor: cursor.to_owned(),
        });
    }

    match runner
        .run(dir, &["merge-base", "--is-ancestor", cursor, branch])
        .await
    {
        Ok(_) => Ok(CursorState::Valid(cursor.to_owned())),
        Err(GitError::Failed { code, .. }) if code == NOT_AN_ANCESTOR => {
            // Rewritten. The merge-base is the last commit the old and new
            // histories share — the newest point that is definitely still reviewed.
            //
            // Resetting to the branch root instead would re-review every commit
            // that survived the rewrite and re-file every finding on them.
            let merge_base = runner.run(dir, &["merge-base", cursor, branch]).await?;
            let base = merge_base.stdout.trim().to_owned();

            if base.is_empty() {
                // Unrelated histories: no shared commit at all.
                return Ok(CursorState::Missing {
                    old_cursor: cursor.to_owned(),
                });
            }
            Ok(CursorState::Rewritten {
                old_cursor: cursor.to_owned(),
                merge_base: base,
            })
        }
        Err(other) => Err(other),
    }
}

/// Whether the repository has any remote configured.
pub async fn has_remote(runner: &GitRunner, dir: &Path) -> Result<bool, GitError> {
    let output = runner.run(dir, &["remote"]).await?;
    Ok(!output.lines().is_empty())
}

/// What a fetch attempt did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchOutcome {
    /// Refs were fetched and stale ones pruned.
    Fetched,
    /// There was no remote to fetch from.
    NoRemote,
}

/// `git fetch --all --prune`, if there is a remote (SPEC §6.2).
///
/// A repository with no remote — a local-only repo, or the offline fixture — is
/// **not** an error. Treating it as one would stop reviewing repositories that work
/// perfectly well.
///
/// `--prune` so a deleted branch stops being discovered. Without it, a
/// long-abandoned `release/*` branch stays in the watched set forever.
///
/// A credential failure propagates as [`GitError::CredentialsRequired`] rather than
/// being swallowed: a fetch that silently did nothing looks exactly like a repo with
/// no new commits, and the user would see reviews stop with no explanation.
pub async fn fetch(
    runner: &GitRunner,
    dir: &Path,
) -> Result<(FetchOutcome, Vec<DiscoveryEvent>), GitError> {
    if !has_remote(runner, dir).await? {
        return Ok((
            FetchOutcome::NoRemote,
            vec![DiscoveryEvent::FetchSkipped {
                reason: "the repository has no remote configured".to_owned(),
            }],
        ));
    }

    runner.run(dir, &["fetch", "--all", "--prune"]).await?;
    Ok((FetchOutcome::Fetched, Vec::new()))
}
