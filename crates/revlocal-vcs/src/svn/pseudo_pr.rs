//! Pseudo-PR synthesis on branch reintegration (RL-904, SPEC §6.4, decision D6).
//!
//! # Why a merge revision is the wrong thing to review
//!
//! Subversion has no pull request. When a branch is reintegrated, the revision
//! that lands on trunk contains the *result* of the merge — often a handful of
//! files, sometimes just a property change — while the work being merged is spread
//! across every revision on the branch. Reviewing the merge revision reviews the
//! merge, not the change.
//!
//! So a reintegration produces an **additional** change: same revision, but its
//! diff is `trunk@fork_rev` against `branch@rev`, which is the whole of the
//! branch's work as one reviewable unit. That is what a pull request would have
//! been. The per-revision change is still emitted — §6.4 says *additional*, not
//! *instead of* — and RL-905 decides which of the two is authoritative.
//!
//! # Three heuristics, in order, each able to fire alone
//!
//! §6.4 lists them and RL-202 built the fixture so they can be tested
//! independently: r10 trips both mergeinfo and the log message, r13 trips
//! mergeinfo with a message that deliberately does not match. Without that, a
//! test could not tell which heuristic was doing the work — and heuristic 1 could
//! rot behind heuristic 2 for a year.
//!
//! # Being wrong is cheap in one direction and not the other
//!
//! A missed reintegration means the branch's work is reviewed revision by
//! revision, which is worse but not wrong. A false positive invents a change that
//! never happened, files findings against it, and — once RL-905 lands — demotes
//! the *real* reviews in its favour. So the heuristics are ordered by how much
//! they prove, and the weakest one requires corroboration: a file count *and* a
//! branch path that actually exists.
//!
//! # What RL-906 found: gaining mergeinfo is not the same as reintegrating
//!
//! §6.4 states heuristic 1 as "`svn:mergeinfo` on the target path gained ranges
//! from a branch path". Measured against Subversion 1.14.5, that condition is true
//! of **four** different merge styles, and only one of them is a reintegration:
//!
//! | style | mergeinfo gained | content changed | source | range reaches branch head |
//! |---|---|---|---|---|
//! | reintegrate | `/branches/x:3-8` | yes | a branch | yes |
//! | sync merge (trunk → branch) | `/trunk:4-9` | yes | **trunk** | n/a |
//! | cherry-pick (`-c N`) | `/branches/x:7` | yes | a branch | **no** |
//! | `--record-only` | `/branches/x:8` | **no** | a branch | yes |
//!
//! Taken literally, heuristic 1 fires on all four. The last is the worst: the
//! `--record-only` idiom exists to mark a revision as *deliberately never to be
//! merged*, and treating it as a reintegration would synthesise a review of code
//! a human explicitly rejected. The cherry-pick case is nearly as bad — one
//! revision was taken, and the pseudo-PR diff would be the whole branch.
//!
//! So each style gets a discriminator, and [`MergeEvidence`] carries the three
//! facts they need. See ADR 0031.

use std::collections::BTreeMap;

use regex::Regex;
use revlocal_core::RepoConfig;

use super::cmd::{SvnError, SvnRunner};
use super::discover::SvnRevision;

/// SPEC §6.4's default for heuristic 3.
pub const DEFAULT_PSEUDO_PR_MIN_FILES: usize = 5;

/// A parsed `svn:mergeinfo` property.
///
/// Maps a branch path to the revision ranges merged from it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeInfo {
    /// Branch path to merged ranges, inclusive.
    pub branches: BTreeMap<String, Vec<(u64, u64)>>,
}

impl MergeInfo {
    /// Parse the property's text form: one `path:ranges` line per branch.
    ///
    /// Ranges are comma-separated, each either `N` or `M-N`, and svn appends `*`
    /// to a non-inheritable range. The `*` is stripped rather than rejected — a
    /// non-inheritable merge is still a merge, and refusing to parse one would
    /// turn an ordinary repository into an unreviewable one.
    pub fn parse(text: &str) -> Self {
        let mut branches = BTreeMap::new();

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Some((path, ranges)) = line.rsplit_once(':') else {
                continue;
            };

            let parsed: Vec<(u64, u64)> = ranges
                .split(',')
                .filter_map(|range| parse_range(range.trim()))
                .collect();

            if !parsed.is_empty() {
                branches.insert(path.trim().to_owned(), parsed);
            }
        }

        Self { branches }
    }

    /// The highest revision recorded for `branch`.
    pub fn highest_for(&self, branch: &str) -> Option<u64> {
        self.branches
            .get(branch)
            .and_then(|ranges| ranges.iter().map(|(_, end)| *end).max())
    }
}

/// `N` or `M-N`, with a trailing `*` allowed.
fn parse_range(range: &str) -> Option<(u64, u64)> {
    let range = range.trim_end_matches('*');
    match range.split_once('-') {
        Some((start, end)) => Some((start.trim().parse().ok()?, end.trim().parse().ok()?)),
        None => {
            let single: u64 = range.parse().ok()?;
            Some((single, single))
        }
    }
}

/// A branch whose ranges grew between two revisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GainedRange {
    /// The branch path, e.g. `/branches/feature-x`.
    pub branch: String,
    /// The highest revision now recorded for it.
    pub through: u64,
}

/// Branches that gained merged ranges between `before` and `after`.
///
/// "Gained", not "differs": a mergeinfo property that *lost* ranges is somebody
/// editing history, and treating that as a reintegration would invent a change
/// out of a correction.
pub fn gained_branches(before: &MergeInfo, after: &MergeInfo) -> Vec<GainedRange> {
    after
        .branches
        .keys()
        .filter_map(|branch| {
            let now = after.highest_for(branch)?;
            let was = before.highest_for(branch).unwrap_or(0);
            (now > was).then(|| GainedRange {
                branch: branch.clone(),
                through: now,
            })
        })
        .collect()
}

/// What a `svn:mergeinfo` gain actually represents (RL-906).
///
/// Only [`Reintegration`](MergeStyle::Reintegration) warrants a pseudo-PR. The
/// other three are named rather than lumped into a boolean because "we saw
/// mergeinfo move and did not synthesise a change" is worth being able to say out
/// loud — §18 — and because an operator debugging a missing pseudo-PR needs to
/// know which of the three it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeStyle {
    /// A branch merged into the watched path. The one that gets a pseudo-PR.
    Reintegration,
    /// The watched path merged *into* a branch — the opposite direction.
    SyncMerge,
    /// Individual revisions taken from a branch, not the branch itself.
    CherryPick,
    /// `--record-only`: mergeinfo written with no content, to mark a revision as
    /// never to be merged. Reviewing this would review rejected code.
    RecordOnly,
}

impl MergeStyle {
    /// Whether this style should produce a pseudo-PR.
    pub const fn is_reintegration(self) -> bool {
        matches!(self, Self::Reintegration)
    }

    /// Why no pseudo-PR was synthesised, for the run record.
    pub const fn explain_rejection(self) -> Option<&'static str> {
        match self {
            Self::Reintegration => None,
            Self::SyncMerge => Some(
                "the merge ran into a branch rather than out of one, so there is \
                 nothing new on the watched path to review",
            ),
            Self::CherryPick => Some(
                "only part of the branch was merged, so the branch-vs-trunk diff \
                 would contain work that was not taken",
            ),
            Self::RecordOnly => Some(
                "svn:mergeinfo was recorded with no content change (--record-only), \
                 which marks the revisions as deliberately not merged",
            ),
        }
    }
}

/// The facts heuristic 1 needs beyond the mergeinfo property itself (RL-906).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MergeEvidence {
    /// The watched trunk path. A gain *from* this is a sync merge.
    pub trunk: String,
    /// Whether the revision changed file content, not only `svn:mergeinfo`.
    ///
    /// From `svn diff --summarize`: see [`ChangedPath::is_property_only`].
    ///
    /// [`ChangedPath::is_property_only`]: super::materialize::ChangedPath::is_property_only
    pub changes_content: bool,
    /// Each branch's last-changed revision as of just before this one.
    ///
    /// A reintegration records a range reaching the branch's head; a cherry-pick
    /// records less. Absent means unknown, which is treated as "do not reject" —
    /// the completeness test is the one most likely to be wrong on an unusual
    /// history, and a missed rejection costs less than a missed reintegration.
    pub branch_last_changed: BTreeMap<String, u64>,
}

impl MergeEvidence {
    /// Evidence for a repository whose trunk is `trunk`, with content changed.
    pub fn new(trunk: &str) -> Self {
        Self {
            trunk: normalize_branch(trunk),
            changes_content: true,
            branch_last_changed: BTreeMap::new(),
        }
    }

    /// Record a branch's last-changed revision.
    #[must_use]
    pub fn with_branch_head(mut self, branch: &str, revision: u64) -> Self {
        self.branch_last_changed
            .insert(normalize_branch(branch), revision);
        self
    }

    /// Mark the revision as changing no file content (`--record-only`).
    #[must_use]
    pub const fn without_content(mut self) -> Self {
        self.changes_content = false;
        self
    }
}

/// Classify one mergeinfo gain (RL-906).
///
/// Order matters only for which reason is reported; the styles are disjoint in
/// practice. Direction is checked first because it is the one that does not depend
/// on any fact beyond the two paths.
pub fn classify_gain(gain: &GainedRange, evidence: &MergeEvidence) -> MergeStyle {
    // Trunk merged into a branch. Nothing new arrived on the watched path.
    if !evidence.trunk.is_empty() && normalize_branch(&gain.branch) == evidence.trunk {
        return MergeStyle::SyncMerge;
    }

    // Mergeinfo moved and nothing else did.
    if !evidence.changes_content {
        return MergeStyle::RecordOnly;
    }

    // Part of the branch, not the branch. Unknown head means do not reject.
    if let Some(head) = evidence
        .branch_last_changed
        .get(&normalize_branch(&gain.branch))
    {
        if gain.through < *head {
            return MergeStyle::CherryPick;
        }
    }

    MergeStyle::Reintegration
}

/// Which heuristics are enabled. All three, normally; one at a time in tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Heuristics {
    /// §6.4's first: `svn:mergeinfo` gained ranges from a branch path.
    pub mergeinfo: bool,
    /// §6.4's second: the log message matches `merge_detect_regex`.
    pub log_message: bool,
    /// §6.4's third: enough files, and a branch path that exists.
    pub file_count: bool,
    /// The threshold for the third.
    pub min_files: usize,
}

impl Default for Heuristics {
    fn default() -> Self {
        Self {
            mergeinfo: true,
            log_message: true,
            file_count: true,
            min_files: DEFAULT_PSEUDO_PR_MIN_FILES,
        }
    }
}

impl Heuristics {
    /// The heuristics a repository's configuration asks for (§6.4, §13.2).
    ///
    /// §6.4 names `pseudo_pr_min_files` and gives it a default of 5, and §13.2's
    /// document now carries it — before REVL-120 it did not, so the threshold
    /// lived only as [`DEFAULT_PSEUDO_PR_MIN_FILES`] and a repository could not
    /// tune the heuristic the spec says is tunable.
    ///
    /// All three heuristics stay on. §6.4 does not offer them as switches, and a
    /// config that could turn `mergeinfo` off would let somebody disable the
    /// strongest evidence and keep the weakest.
    pub fn for_repo(config: &RepoConfig) -> Self {
        Self {
            mergeinfo: true,
            log_message: true,
            file_count: true,
            // `usize` here, `u32` in config: the config document is a wire format
            // and 32 bits is plenty for a file count, while this is an index-like
            // comparison against `files.len()`.
            min_files: config.pseudo_pr_min_files as usize,
        }
    }

    /// Only the named heuristic, so a test can prove it works alone.
    pub const fn only_mergeinfo() -> Self {
        Self {
            mergeinfo: true,
            log_message: false,
            file_count: false,
            min_files: DEFAULT_PSEUDO_PR_MIN_FILES,
        }
    }

    /// Only the log-message heuristic.
    pub const fn only_log_message() -> Self {
        Self {
            mergeinfo: false,
            log_message: true,
            file_count: false,
            min_files: DEFAULT_PSEUDO_PR_MIN_FILES,
        }
    }

    /// Only the file-count heuristic.
    pub const fn only_file_count(min_files: usize) -> Self {
        Self {
            mergeinfo: false,
            log_message: false,
            file_count: true,
            min_files,
        }
    }
}

/// Which heuristic fired, and what it found.
///
/// Kept rather than reduced to a boolean: §18's spirit, and an operator asking
/// "why did rev-local invent this change?" deserves an answer more specific than
/// "it looked like a merge".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Detection {
    /// `svn:mergeinfo` gained ranges from this branch.
    MergeInfo {
        /// The branch merged from.
        branch: String,
        /// The highest revision now recorded for it.
        through: u64,
    },
    /// The log message named a branch.
    LogMessage {
        /// The branch the message named.
        branch: String,
    },
    /// Enough files changed, and the message named a branch that exists.
    FileCountAndName {
        /// The branch the message named.
        branch: String,
        /// How many files the revision touched.
        files: usize,
    },
}

impl Detection {
    /// The branch this reintegration is of.
    pub fn branch(&self) -> &str {
        match self {
            Self::MergeInfo { branch, .. }
            | Self::LogMessage { branch }
            | Self::FileCountAndName { branch, .. } => branch,
        }
    }

    /// A sentence for the run record and the UI.
    pub fn explain(&self) -> String {
        match self {
            Self::MergeInfo { branch, through } => {
                format!("svn:mergeinfo gained ranges from {branch} through r{through}")
            }
            Self::LogMessage { branch } => {
                format!("the log message names {branch} as merged")
            }
            Self::FileCountAndName { branch, files } => {
                format!("{files} files changed and the log message names {branch}, which exists")
            }
        }
    }
}

/// Decide whether a revision reintegrated a branch (§6.4).
///
/// Heuristics are tried in §6.4's order, which is the order of how much they
/// prove. `existing_branches` is what heuristic 3 corroborates against — a message
/// naming a branch that does not exist is somebody talking about a plan, not
/// recording a merge.
pub fn detect(
    revision: &SvnRevision,
    mergeinfo_before: &MergeInfo,
    mergeinfo_after: &MergeInfo,
    merge_detect: &Regex,
    existing_branches: &[String],
    heuristics: Heuristics,
    evidence: &MergeEvidence,
) -> Option<Detection> {
    if heuristics.mergeinfo {
        // RL-906: a gain is necessary but not sufficient. Three of the four merge
        // styles that move mergeinfo are not reintegrations.
        if let Some(gained) = gained_branches(mergeinfo_before, mergeinfo_after)
            .into_iter()
            .find(|gain| classify_gain(gain, evidence).is_reintegration())
        {
            return Some(Detection::MergeInfo {
                branch: gained.branch,
                through: gained.through,
            });
        }
    }

    if heuristics.log_message {
        if let Some(branch) = branch_from_message(&revision.message, merge_detect) {
            return Some(Detection::LogMessage { branch });
        }
    }

    if heuristics.file_count {
        let files = revision.paths.len();
        if files >= heuristics.min_files {
            // Corroboration, not a second guess: the weakest signal is the one
            // that most needs a branch that actually exists behind it.
            if let Some(branch) = named_existing_branch(&revision.message, existing_branches) {
                return Some(Detection::FileCountAndName { branch, files });
            }
        }
    }

    None
}

/// The branch path a merge message names, per `merge_detect_regex`.
///
/// The default pattern requires both a merge word **and** a branch path, which is
/// what stops "merge the config files" from inventing a change.
fn branch_from_message(message: &str, merge_detect: &Regex) -> Option<String> {
    let captures = merge_detect.captures(message)?;
    // The default pattern's second group is the branch path. A custom pattern
    // without one falls back to the whole match, which is still better than
    // claiming a branch nobody named.
    let branch = captures
        .get(2)
        .or_else(|| captures.get(1))
        .map(|m| m.as_str().to_owned())?;
    Some(normalize_branch(&branch))
}

/// A branch named in the message that is also in `existing`.
fn named_existing_branch(message: &str, existing: &[String]) -> Option<String> {
    existing
        .iter()
        .find(|branch| {
            let bare = branch.trim_start_matches('/');
            message.contains(bare) || message.contains(branch.as_str())
        })
        .cloned()
}

/// Leading slash, no trailing slash.
fn normalize_branch(branch: &str) -> String {
    let trimmed = branch.trim_end_matches('/');
    if trimmed.starts_with('/') {
        trimmed.to_owned()
    } else {
        format!("/{trimmed}")
    }
}

/// §6.4's identifier for a pseudo-PR: `{branch}@r{rev}`.
pub fn pseudo_pr_external_id(branch: &str, revision: u64) -> String {
    format!("{branch}@r{revision}")
}

/// Read `svn:mergeinfo` on a path at a revision.
///
/// An absent property is empty mergeinfo, not an error: most paths have none, and
/// most revisions do not change it.
pub async fn mergeinfo_at(
    runner: &SvnRunner,
    repo_url: &str,
    path: &str,
    revision: u64,
) -> Result<MergeInfo, SvnError> {
    let target = format!(
        "{}{}@{revision}",
        repo_url.trim_end_matches('/'),
        if path.starts_with('/') {
            path.to_owned()
        } else {
            format!("/{path}")
        }
    );
    let rev = revision.to_string();

    match runner
        .run(
            std::path::Path::new("."),
            &["propget", "svn:mergeinfo", "-r", &rev, &target],
        )
        .await
    {
        Ok(output) => Ok(MergeInfo::parse(&output.stdout)),
        // A path that did not exist at that revision, or a property that is not
        // set. Both mean "no mergeinfo", which is the ordinary case.
        Err(SvnError::Failed { .. }) => Ok(MergeInfo::default()),
        Err(other) => Err(other),
    }
}

/// The revision a branch was copied from (§6.4's fork point).
///
/// Read with `--stop-on-copy`, which stops the log at the copy that created the
/// branch — so the oldest entry is the branch's creation, and its `copyfrom-rev`
/// is where trunk was at the time.
///
/// **Not** the branch's own earliest revision: a branch with trunk merged into it
/// partway has revisions from trunk in its history, and taking the oldest revision
/// number would produce a diff containing trunk work the branch never authored.
pub async fn fork_point(
    runner: &SvnRunner,
    repo_url: &str,
    branch: &str,
) -> Result<Option<u64>, SvnError> {
    let target = format!(
        "{}{}",
        repo_url.trim_end_matches('/'),
        normalize_branch(branch)
    );

    let output = runner
        .run(
            std::path::Path::new("."),
            &["log", "--xml", "-v", "--stop-on-copy", &target],
        )
        .await?;

    let revisions = super::discover::parse_log_xml(&output.stdout)?;

    // The oldest entry is the copy that created the branch. Its path entry carries
    // `copyfrom-rev`, which is the fork point.
    Ok(revisions
        .iter()
        .min_by_key(|revision| revision.revision)
        .and_then(|creation| creation.paths.iter().find_map(|path| path.copyfrom_rev)))
}

/// The diff a pseudo-PR is reviewed on: the whole branch against its fork point.
///
/// `svn diff {trunk}@{fork} {branch}@{rev}` — §6.4, and the point of the whole
/// feature. The merge revision's own diff is a different thing and is reviewed
/// separately as the `svn_rev` change.
pub async fn pseudo_pr_diff(
    runner: &SvnRunner,
    repo_url: &str,
    trunk: &str,
    branch: &str,
    fork_rev: u64,
    revision: u64,
) -> Result<String, SvnError> {
    let base = repo_url.trim_end_matches('/');
    let from = format!("{base}{}@{fork_rev}", normalize_branch(trunk));
    let to = format!("{base}{}@{revision}", normalize_branch(branch));

    Ok(runner
        .run(std::path::Path::new("."), &["diff", &from, &to])
        .await?
        .stdout)
}
