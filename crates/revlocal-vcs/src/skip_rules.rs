//! The skip table of SPEC §9.4.
//!
//! A skipped change is **not an invisible one**. Every rule here produces a
//! `skip_reason`, the change is still recorded, and a `skipped` run is still
//! created carrying that reason (SPEC §18, and `Run::is_consistent` refuses a
//! skipped run without one). The point of a skip is to avoid *engine spend*, not
//! to avoid leaving a trace — a user who wonders why their lockfile bump has no
//! review must be able to find out.
//!
//! Evaluation is pure: a change, its file list, and the repo's config. Nothing here
//! reads git or the database, which is what lets every rule be tested against a
//! constructed change rather than a repository shaped to produce it.

use globset::{Glob, GlobSetBuilder};
use revlocal_core::RepoConfig;

use crate::adapter::DetectedChange;

/// Why a change will not be reviewed (SPEC §9.4).
///
/// The wire spellings are what land in `run.skip_reason` and what the UI groups by,
/// so they are fixed here rather than being formatted at each call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// Every path the change touches matches `ignore_globs`.
    IgnoredPaths,
    /// The change touches nothing, or nothing survived ignore filtering.
    EmptyDiff,
    /// The author matches `ignore_authors`.
    IgnoredAuthor,
    /// A merge commit, with `review_merge_commits = false`.
    MergeCommit,
    /// A draft pull request, with `review_draft_prs = false` (SPEC §6.3, §13.2).
    ///
    /// Decided by the GitHub adapter, which is the only layer that knows a change
    /// is a pull request at all.
    DraftPr,
    /// The commit is already covered by an open pull request (SPEC §6.3).
    ///
    /// Not decided here: it needs the GitHub adapter's view of open PRs. `RL-306`
    /// sets it.
    CoveredByPr,
    /// A `done` run already exists for the same content (SPEC §9.4).
    ///
    /// Not decided here: it needs the store. The pipeline sets it, using the content
    /// hashes `git::patch_ids` produces.
    AlreadyReviewed,
}

impl SkipReason {
    /// Every reason, so a test can assert the set is covered.
    pub const ALL: &'static [Self] = &[
        Self::IgnoredPaths,
        Self::EmptyDiff,
        Self::IgnoredAuthor,
        Self::MergeCommit,
        Self::DraftPr,
        Self::CoveredByPr,
        Self::AlreadyReviewed,
    ];

    /// The wire spelling stored in `run.skip_reason`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::IgnoredPaths => "ignored_paths",
            Self::EmptyDiff => "empty_diff",
            Self::IgnoredAuthor => "ignored_author",
            Self::MergeCommit => "merge_commit",
            Self::DraftPr => "draft_pr",
            Self::CoveredByPr => "covered_by_pr",
            Self::AlreadyReviewed => "already_reviewed",
        }
    }

    /// Whether this reason is decided by [`evaluate`], as opposed to by a layer
    /// with more context.
    ///
    /// `DraftPr` and `CoveredByPr` are decided by the GitHub adapter, which is the
    /// only layer that knows a change is a pull request; `AlreadyReviewed` needs the
    /// store.
    pub const fn is_decided_by_vcs(self) -> bool {
        matches!(
            self,
            Self::IgnoredPaths | Self::EmptyDiff | Self::IgnoredAuthor | Self::MergeCommit
        )
    }
}

impl std::fmt::Display for SkipReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A decision not to review, with enough detail to explain it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skip {
    /// Which rule fired.
    pub reason: SkipReason,
    /// A sentence for the UI and the audit log.
    pub detail: String,
}

impl Skip {
    /// The value stored in `run.skip_reason`.
    ///
    /// The machine-readable reason first, so grouping by prefix works, then the
    /// human detail. One column has to serve both, and losing the detail would make
    /// "ignored_paths" unactionable — the user needs to know *which* glob.
    pub fn to_skip_reason(&self) -> String {
        format!("{}: {}", self.reason, self.detail)
    }
}

/// Rules this module can decide on its own, in the order they are checked.
///
/// Order is by how cheap the answer is to explain, not by cost to compute: a merge
/// that is also bot-authored is more usefully reported as a merge, because that is
/// the property the user configured.
const _ORDER: () = ();

/// Decide whether `change` should be reviewed, given `config`.
///
/// Returns `None` when the change should be reviewed. Two of §9.4's six rules are
/// not decided here and are documented on [`SkipReason`]: `covered_by_pr` needs the
/// GitHub adapter and `already_reviewed` needs the store.
pub fn evaluate(change: &DetectedChange, config: &RepoConfig) -> Option<Skip> {
    if !config.review_merge_commits && change.parents.len() > 1 {
        return Some(Skip {
            reason: SkipReason::MergeCommit,
            detail: format!(
                "{} parents; review_merge_commits is off, so its constituent commits \
                 are reviewed instead",
                change.parents.len()
            ),
        });
    }

    if let Some(author) = matched_author(change, config) {
        return Some(Skip {
            reason: SkipReason::IgnoredAuthor,
            detail: format!("author `{author}` matches ignore_authors"),
        });
    }

    if change.paths.is_empty() {
        return Some(Skip {
            reason: SkipReason::EmptyDiff,
            detail: "the change touches no files".to_owned(),
        });
    }

    let remaining = reviewable_paths(&change.paths, config);
    if remaining.is_empty() {
        return Some(Skip {
            reason: SkipReason::IgnoredPaths,
            detail: format!("all {} path(s) match ignore_globs", change.paths.len()),
        });
    }

    None
}

/// The paths that survive `ignore_globs` filtering.
///
/// Exposed because §9.4's truncation rules operate on the same filtered set, and
/// because a caller building an engine prompt needs to know what was excluded in
/// order to say so.
pub fn reviewable_paths(paths: &[String], config: &RepoConfig) -> Vec<String> {
    let Some(ignored) = build_globset(&config.ignore_globs) else {
        // A malformed glob must not silently ignore everything — that would skip
        // every change in the repository and look like rev-local doing nothing.
        // Reviewing more than asked is the safe direction.
        tracing::warn!(
            globs = ?config.ignore_globs,
            "ignore_globs did not compile; reviewing all paths rather than skipping them"
        );
        return paths.to_vec();
    };

    paths
        .iter()
        .filter(|path| !ignored.is_match(path.as_str()))
        .cloned()
        .collect()
}

/// Whether the change's author matches `ignore_authors`.
///
/// Matched against both the display name and the email, because a bot is spelled
/// one way in one field and another way in the other — `dependabot[bot]` as a name,
/// `…@users.noreply.github.com` as an email — and a rule that only checked one
/// would miss half the bots it was written for.
fn matched_author<'a>(change: &'a DetectedChange, config: &RepoConfig) -> Option<&'a str> {
    let candidates = [
        change.author_name.as_deref(),
        change.author_email.as_deref(),
    ];

    for candidate in candidates.into_iter().flatten() {
        let lowered = candidate.to_lowercase();
        if config
            .ignore_authors
            .iter()
            .any(|pattern| author_matches(&pattern.to_lowercase(), &lowered))
        {
            return Some(candidate);
        }
    }
    None
}

/// Match one `ignore_authors` entry.
///
/// Exact match, or a `*` glob. Substring matching was rejected: an entry of `bot`
/// would then skip every commit by anyone called Abbott.
fn author_matches(pattern: &str, candidate: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == candidate,
        Some((prefix, suffix)) => {
            candidate.len() >= prefix.len() + suffix.len()
                && candidate.starts_with(prefix)
                && candidate.ends_with(suffix)
        }
    }
}

/// Compile a glob set, or `None` if any pattern is invalid.
fn build_globset(patterns: &[String]) -> Option<globset::GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern).ok()?);
    }
    builder.build().ok()
}
