//! The `VcsAdapter` trait and its supporting types (SPEC §6.1).
//!
//! One trait covers git, GitHub and Subversion. The pipeline never branches on
//! [`RepoKind`]; it asks an adapter for changes and for a materialized tree, and
//! the differences between "a commit", "a pull request at a head SHA" and "a
//! revision" live behind this boundary.

use std::path::{Path, PathBuf};

use revlocal_core::{Change, ChangeKind, Cursor, DiffStat, FileDiff, Repo, RepoKind, Timestamp};

/// What can go wrong in a VCS adapter.
#[derive(Debug, thiserror::Error)]
pub enum VcsError {
    /// The repository is not usable as configured.
    ///
    /// Carries remediation, because SPEC §18 requires every user-visible error to
    /// say what to do about it.
    #[error("{repo}: {problem}\n  try: {remediation}")]
    Unusable {
        /// The repository's name.
        repo: String,
        /// What is wrong.
        problem: String,
        /// What the user should do.
        remediation: String,
    },

    /// A required external tool is missing.
    #[error("`{tool}` is not on PATH, which {purpose} requires\n  try: {remediation}")]
    MissingTool {
        /// The executable that was not found.
        tool: &'static str,
        /// What needed it.
        purpose: &'static str,
        /// How to install it.
        remediation: &'static str,
    },

    /// A VCS command failed.
    #[error("`{command}` failed ({status}): {stderr}")]
    CommandFailed {
        /// The command that was run, without arguments that may carry secrets.
        command: String,
        /// Its exit status.
        status: String,
        /// What it wrote to stderr.
        stderr: String,
    },

    /// The change a caller named does not exist in this repository.
    #[error("{repo} has no {kind} `{external_id}`")]
    NoSuchChange {
        /// The repository's name.
        repo: String,
        /// What kind of change was looked for.
        kind: ChangeKind,
        /// Its identity in the originating system.
        external_id: String,
    },

    /// Filesystem trouble, including scratch directory creation.
    #[error("{context}: {source}")]
    Io {
        /// What was being attempted.
        context: String,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
}

/// The VCS layer's result alias.
pub type Result<T, E = VcsError> = std::result::Result<T, E>;

/// A cheap liveness and configuration check (SPEC §6.1).
///
/// `probe` never mutates, so this is safe to run on a schedule and on every
/// startup — which is what `revlocal doctor` does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeReport {
    /// Whether the repository can be reviewed at all right now.
    pub usable: bool,
    /// The VCS tool's version string, when one could be obtained.
    pub tool_version: Option<String>,
    /// The branch or path discovery will follow.
    pub default_branch: Option<String>,
    /// Everything wrong, each with remediation. Empty when `usable`.
    ///
    /// A list rather than one problem: telling a user their repo is broken one
    /// reason at a time turns a single fix into three round trips.
    pub problems: Vec<ProbeProblem>,
}

/// One reason a repository is not usable, and what to do about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeProblem {
    /// What is wrong.
    pub problem: String,
    /// What the user should do.
    pub remediation: String,
    /// Whether this is something wrong, as opposed to something not ready yet.
    ///
    /// A repository somebody has run `git init` in and not committed to is not
    /// misconfigured and has nothing for anybody to fix — it simply has nothing
    /// to review until there is a commit. Reporting that as a failure puts the
    /// word FAILED against four of the twenty-seven repositories on a real
    /// machine, none of which is broken, and a screen that shouts about things
    /// that are fine is one people stop reading (REVL-201).
    ///
    /// `usable` on the report stays false either way: neither kind can be
    /// reviewed. This says whether anybody should be alarmed about it.
    pub fault: bool,
}

impl ProbeReport {
    /// A report for a repository that is fine.
    pub fn usable(tool_version: Option<String>, default_branch: Option<String>) -> Self {
        Self {
            usable: true,
            tool_version,
            default_branch,
            problems: Vec::new(),
        }
    }

    /// A report for a repository that is not.
    pub fn unusable(problems: Vec<ProbeProblem>) -> Self {
        Self {
            usable: false,
            tool_version: None,
            default_branch: None,
            problems,
        }
    }
}

/// A change the adapter found that rev-local has not seen (SPEC §6.1).
///
/// Distinct from [`Change`] because a detected change has no `id` yet — it has not
/// been written to the store — and because an adapter may already know the change
/// should be skipped. Carrying `skip_reason` here rather than discovering it later
/// is what lets a skip be recorded with its reason (SPEC §18) instead of the change
/// simply never appearing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedChange {
    /// Which of the four kinds this is.
    pub kind: ChangeKind,
    /// Identity in the originating system.
    pub external_id: String,
    /// Commit subject, PR title, or SVN log summary.
    pub title: Option<String>,
    /// Author display name.
    pub author_name: Option<String>,
    /// Author email.
    pub author_email: Option<String>,
    /// When the author made the change.
    pub authored_at: Option<Timestamp>,
    /// Branch, where the concept applies.
    pub branch: Option<String>,
    /// PR base SHA or SVN merge base. The first parent, for a commit.
    pub base_ref: Option<String>,
    /// Every parent. Length > 1 means a merge (SPEC §9.4's `merge_commit` skip).
    ///
    /// Carried rather than recomputed: deciding whether a change is a merge is a
    /// skip rule, and shelling out again per change to answer a question discovery
    /// already knew the answer to would be a git call per commit.
    pub parents: Vec<String>,
    /// Repository-relative paths the change touches.
    ///
    /// Needed by the `ignore_globs` skip rule, which is about *which* paths a
    /// change touches rather than how many. Also what §9.4's truncation rules need
    /// in order to list omitted files in full.
    pub paths: Vec<String>,
    /// Head SHA or revision.
    pub head_ref: Option<String>,
    /// Web URL, if known.
    pub url: Option<String>,
    /// Size of the change.
    pub diff_stat: DiffStat,
    /// Why this change should not be reviewed, if it should not (SPEC §9.4).
    pub skip_reason: Option<String>,
    /// The cursor value to store once this change has been handled.
    ///
    /// The adapter owns what a cursor means for its VCS — a SHA, a PR
    /// `updated_at`, a revision number — so it says what to record rather than the
    /// caller inferring it.
    pub cursor_value: String,
}

impl DetectedChange {
    /// Whether the adapter has already decided this change is not reviewable.
    pub const fn is_skipped(&self) -> bool {
        self.skip_reason.is_some()
    }
}

/// Full reviewable context for one change (SPEC §6.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeContext {
    /// The checked-out state **at** the change. Always a scratch copy.
    pub worktree: PathBuf,
    /// Full unified diff, base..head.
    pub diff_unified: String,
    /// Per-file summary of the same diff.
    pub diff_files: Vec<FileDiff>,
    /// Commit message, PR body, or SVN log message.
    pub message: String,
    /// Parent SHAs or revisions.
    pub parents: Vec<String>,
    /// Size of the change.
    pub stat: DiffStat,
    /// Whether the diff exceeded limits and was reduced.
    ///
    /// SPEC §18: a review that saw 60% of the diff must never look like a review
    /// that saw all of it. Whoever sets this must also record what was omitted.
    pub truncated: bool,
    /// What was left out when `truncated`, for the prompt and the run record.
    pub omitted_files: Vec<String>,
}

impl ChangeContext {
    /// Whether this context is self-consistent.
    ///
    /// A truncated context with no omitted files is a silent cap: it claims
    /// something was dropped without saying what, and the UI would have nothing to
    /// show (SPEC §18).
    pub fn is_consistent(&self) -> bool {
        !self.truncated || !self.omitted_files.is_empty()
    }
}

/// What to do about local hook integration (SPEC §7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookMode {
    /// Install rev-local's hook, preserving any existing one.
    Install,
    /// Remove rev-local's hook, restoring any existing one byte-identically.
    Uninstall,
    /// Report what is installed without changing anything.
    Verify,
}

/// The result of a hook operation (SPEC §7.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookReport {
    /// Whether rev-local's hook is installed now.
    pub installed: bool,
    /// Hooks that were already present and have been preserved.
    ///
    /// M12's gate requires an existing user hook to survive install and uninstall
    /// byte-identically, so what was found has to be recorded rather than assumed
    /// absent.
    pub preserved: Vec<String>,
    /// Anything the user should know, each with remediation.
    pub problems: Vec<ProbeProblem>,
}

/// The adapter for a repository's kind.
///
/// # Why selection lives here
///
/// `GitAdapter::new()` was hardcoded in five places. RL-1558 put a shared
/// *check* beside the adapters, which stopped four of them lying — and then
/// wiring `SvnAdapter` into the autopilot alone reintroduced the same defect on
/// `backfill`, because a check for "is this kind supported" is not the same
/// thing as choosing what to run. One selector is (RL-1559).
///
/// Returns the error [`unsupported_kind`] describes for a kind with no adapter,
/// so a caller gets the remedy without composing it.
pub fn adapter_for(repo: &Repo) -> Result<Box<dyn VcsAdapter + Send + Sync>> {
    match repo.kind {
        RepoKind::Git => Ok(Box::new(crate::GitAdapter::new())),
        RepoKind::Svn => Ok(Box::new(crate::SvnAdapter::new())),
        // Not the git adapter. A GitHub repository is reviewed at pull-request
        // granularity (§6.3), and `github/pull_requests.rs` is the code for it —
        // still unrouted. Handing it to the git adapter would review its commits
        // instead, which is a quieter wrong answer than an error and therefore a
        // worse one.
        RepoKind::GitHub => Err(VcsError::Unusable {
            repo: repo.name.clone(),
            problem: "rev-local cannot review a `github` repository yet — pull-request discovery is not wired".to_owned(),
            remediation: "add it as a `git` checkout to review its commits, or disable it until GitHub support lands".to_owned(),
        }),
    }
}

/// Why this repository's kind cannot be reviewed yet, if it cannot.
///
/// `None` for git and Subversion. `github/` still carries an implementation —
/// pull-request discovery and its transport — that nothing routes to.
///
/// # Why this lives here rather than in each caller
///
/// `GitAdapter::new()` is hardcoded in five places: the autopilot's discovery
/// and executor, the desktop's add-repository preview, `backfill` and `review`.
/// The first version of this check went into one of them, and the other four went
/// on telling somebody with a Subversion working copy that it "is not a git
/// repository" — `backfill` went further and suggested `git rev-parse` in it
/// (RL-1558). A check beside the adapters is one every caller can reach.
///
/// The message carries no repository name: callers that have one prepend it.
pub fn unsupported_kind(repo: &Repo) -> Option<String> {
    match repo.kind {
        RepoKind::Git | RepoKind::Svn => None,
        RepoKind::GitHub => Some(
            "rev-local cannot review a `github` repository yet — pull-request discovery is not wired\n  try: add it as a `git` checkout to review its commits, or disable it until GitHub support lands"
                .to_owned(),
        ),

    }
}

/// One version-control system, behind one interface (SPEC §6.1).
#[async_trait::async_trait]
pub trait VcsAdapter: Send + Sync {
    /// Which VCS this adapter speaks.
    fn kind(&self) -> RepoKind;

    /// Cheap liveness and configuration check. **Never mutates.**
    async fn probe(&self, repo: &Repo) -> Result<ProbeReport>;

    /// Everything new since `cursor`, oldest first, bounded by `limit`.
    ///
    /// Oldest-first because reviews are published in the order changes happened;
    /// newest-first would put a fix's review above the bug's.
    async fn discover(
        &self,
        repo: &Repo,
        cursor: Option<&Cursor>,
        limit: usize,
    ) -> Result<Vec<DetectedChange>>;

    /// Full reviewable context for one change, materialized into `into`.
    ///
    /// **Must not mutate the user's working copy.** Git uses `git worktree add
    /// --detach` into the scratch directory, or `git archive` for a bare mirror;
    /// SVN uses `svn export`. M4's gate asserts the fixture's working tree is
    /// byte-identical afterwards.
    async fn materialize(&self, repo: &Repo, change: &Change, into: &Path)
        -> Result<ChangeContext>;

    /// Install, remove or verify local trigger integration. A no-op for adapters
    /// with no local hooks.
    async fn install_hooks(&self, repo: &Repo, mode: HookMode) -> Result<HookReport>;
}
