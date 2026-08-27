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

    /// A rewrite was detected but its commits could not be content-compared.
    ///
    /// Without the pre-rewrite commits there is nothing to compare against, so
    /// everything above the merge-base is reviewed again. Recorded rather than
    /// passing silently: an operator seeing a burst of re-reviews after a rebase
    /// needs this line to know why (SPEC §18).
    RewriteDedupeUnavailable {
        /// The cursor before the rewrite.
        old_cursor: String,
        /// Why the comparison could not be made.
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
            Self::RewriteDedupeUnavailable { .. } => "rewrite_dedupe_unavailable",
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

/// Content hashes for a set of commits, keyed by SHA (SPEC §6.2).
///
/// `git patch-id --stable` hashes a diff with line numbers and whitespace
/// normalised away, so **the same change on a different base has the same
/// patch-id**. That is precisely the property a rebase preserves and a SHA does
/// not, and it is why resetting the cursor to the merge-base is necessary but not
/// sufficient: every commit above the merge-base comes back with a new SHA, and
/// without a content hash they all look new.
///
/// Two git calls for the whole set rather than two per commit: `git show` emits
/// every diff at once and `patch-id` reports one line per commit it saw.
pub async fn patch_ids(
    runner: &GitRunner,
    dir: &Path,
    revs: &[String],
) -> Result<std::collections::BTreeMap<String, String>, GitError> {
    use std::collections::BTreeMap;

    if revs.is_empty() {
        return Ok(BTreeMap::new());
    }

    let mut args: Vec<String> = vec![
        "show".to_owned(),
        "--no-walk".to_owned(),
        "--format=%H".to_owned(),
        "--patch".to_owned(),
        // Rename detection off: a rename detected on one base and not the other
        // would change the diff text and therefore the patch-id, making the same
        // change look different.
        "--no-renames".to_owned(),
    ];
    args.extend(revs.iter().cloned());

    let diffs = runner.run(dir, &args).await?;

    // `--stable` so the hash does not depend on the order hunks happen to appear
    // in; the unstable form is explicitly not comparable across invocations.
    let ids = runner
        .run_with_stdin(dir, &["patch-id", "--stable"], &diffs.stdout)
        .await?;

    let mut map = BTreeMap::new();
    for line in ids.lines() {
        let mut parts = line.split_whitespace();
        if let (Some(patch_id), Some(sha)) = (parts.next(), parts.next()) {
            map.insert(sha.to_owned(), patch_id.to_owned());
        }
    }
    Ok(map)
}

/// Mark discovered changes that merely reproduce an already-reviewed commit.
///
/// After a rebase, the commits above the merge-base are the *same work* with new
/// SHAs. Reviewing them again would re-file every finding on them, so a rebase
/// would spam the tracker — the failure this whole item exists to prevent.
///
/// Sets `skip_reason` rather than dropping them: SPEC §18 wants a skip recorded
/// with its reason, and "we already reviewed this as `<old sha>`" is exactly the
/// kind of thing an operator needs to see when they wonder why a rebased commit
/// has no review.
///
/// Returns the number marked. If the pre-rewrite commits are no longer reachable —
/// garbage-collected after the rewrite — nothing can be compared and every change
/// is reviewed again; that is reported as an event rather than passing silently.
pub async fn mark_superseded_by_rewrite(
    runner: &GitRunner,
    dir: &Path,
    old_cursor: &str,
    merge_base: &str,
    changes: &mut [crate::adapter::DetectedChange],
) -> Result<Vec<DiscoveryEvent>, GitError> {
    let mut events = Vec::new();

    // The commits that existed between the merge-base and the old cursor: what was
    // already reviewed and then rewritten.
    let old_range = format!("{merge_base}..{old_cursor}");
    let listed = match runner.run(dir, &["rev-list", &old_range]).await {
        Ok(output) => output,
        Err(_) => {
            // The old history is gone. Honest degradation: everything above the
            // merge-base is reviewed again, and the log says why.
            events.push(DiscoveryEvent::RewriteDedupeUnavailable {
                old_cursor: old_cursor.to_owned(),
                reason: "the pre-rewrite commits are no longer reachable".to_owned(),
            });
            return Ok(events);
        }
    };

    let old_shas: Vec<String> = listed.lines().iter().map(|s| (*s).to_owned()).collect();
    if old_shas.is_empty() {
        return Ok(events);
    }

    let old_ids = patch_ids(runner, dir, &old_shas).await?;
    // patch-id -> the old sha that had it, so the skip reason can name it.
    let by_content: std::collections::BTreeMap<&str, &str> = old_ids
        .iter()
        .map(|(sha, id)| (id.as_str(), sha.as_str()))
        .collect();

    let new_shas: Vec<String> = changes.iter().map(|c| c.external_id.clone()).collect();
    let new_ids = patch_ids(runner, dir, &new_shas).await?;

    for change in changes.iter_mut() {
        let Some(patch_id) = new_ids.get(&change.external_id) else {
            continue;
        };
        // An empty patch-id means an empty diff — a merge or an empty commit. Those
        // are not "the same change", they are "no change", and treating every one of
        // them as a match would suppress unrelated commits.
        if patch_id.chars().all(|c| c == '0') {
            continue;
        }
        if let Some(old_sha) = by_content.get(patch_id.as_str()) {
            change.skip_reason = Some(format!(
                "unchanged_after_rewrite: same content as already-reviewed {old_sha}"
            ));
        }
    }

    Ok(events)
}
