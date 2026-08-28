//! `GitAdapter` — the git implementation of [`VcsAdapter`] (SPEC §6.1).
//!
//! Assembly only. Every behaviour already lives in `discover`, `materialize` and
//! `skip_rules`; this type chooses a repository directory, calls them, and maps
//! `GitError` onto `VcsError`. **No VCS logic is added here**, because a second place
//! that decides what a change is would eventually disagree with the first.
//!
//! # Why the error mapping is not a blanket `From`
//!
//! `VcsError` names the repository in most of its variants and `GitError` does not
//! know one, so a derived `From` would produce messages with the repo missing where
//! it matters most. §18 requires a user-visible error to say what to do; "`git
//! rev-parse` failed" without naming the repository is not actionable when a user
//! has eleven of them.

use std::path::{Path, PathBuf};

use revlocal_core::{Change, Cursor, Repo, RepoConfig, RepoKind};

use crate::adapter::{
    DetectedChange, HookMode, HookReport, ProbeProblem, ProbeReport, Result, VcsAdapter, VcsError,
};
use crate::git::{self, GitError, GitRunner};
use crate::ChangeContext;

/// Reviews a git repository on the local filesystem.
#[derive(Debug, Default)]
pub struct GitAdapter {
    runner: GitRunner,
}

impl GitAdapter {
    /// A new adapter with a default runner.
    pub fn new() -> Self {
        Self::default()
    }

    /// The directory this repo lives in.
    ///
    /// A git repo with no `local_path` is a configuration error rather than a
    /// runtime one, and saying so beats a `NotARepository` about an empty path.
    fn dir(repo: &Repo) -> Result<PathBuf> {
        repo.local_path
            .as_ref()
            .map(PathBuf::from)
            .ok_or_else(|| VcsError::Unusable {
                repo: repo.name.clone(),
                problem: "no local_path is set, so there is nothing on disk to review".to_owned(),
                remediation: "set the repository's local path to its working copy or mirror"
                    .to_owned(),
            })
    }

    /// Map a `GitError` onto a `VcsError`, naming the repository.
    fn map_error(repo: &Repo, error: GitError) -> VcsError {
        match error {
            GitError::NotInstalled => VcsError::MissingTool {
                tool: "git",
                purpose: "reviewing a git repository",
                remediation: "install git and make sure it is on PATH",
            },
            GitError::NotARepository { path } => VcsError::Unusable {
                repo: repo.name.clone(),
                problem: format!("{} is not a git repository", path.display()),
                remediation: "check the repository's local path".to_owned(),
            },
            GitError::CredentialsRequired { .. } => VcsError::Unusable {
                repo: repo.name.clone(),
                problem: "git asked for credentials, and rev-local never supplies them".to_owned(),
                remediation: "configure a credential helper, or review a local clone".to_owned(),
            },
            other => VcsError::CommandFailed {
                command: "git".to_owned(),
                status: other.to_string(),
                stderr: String::new(),
            },
        }
    }

    /// The config this repo was configured with, or the defaults.
    ///
    /// A config document that no longer parses must not stop a review: the defaults
    /// are §13.2's and reviewing with them beats reviewing nothing. The caller that
    /// owns the repo row surfaces the parse warnings; this is the read path.
    fn config(repo: &Repo) -> RepoConfig {
        RepoConfig::parse_json(&repo.config_json)
            .map(|(config, _)| config)
            .unwrap_or_default()
    }
}

#[async_trait::async_trait]
impl VcsAdapter for GitAdapter {
    fn kind(&self) -> RepoKind {
        RepoKind::Git
    }

    async fn probe(&self, repo: &Repo) -> Result<ProbeReport> {
        let dir = Self::dir(repo)?;

        let version = match self.runner.run(&dir, &["--version"]).await {
            Ok(output) => output.stdout.trim().to_owned(),
            Err(GitError::NotInstalled) => {
                return Ok(ProbeReport {
                    usable: false,
                    tool_version: None,
                    default_branch: None,
                    problems: vec![ProbeProblem {
                        problem: "git is not on PATH".to_owned(),
                        remediation: "install git and make sure it is on PATH".to_owned(),
                    }],
                })
            }
            Err(e) => return Err(Self::map_error(repo, e)),
        };

        // Every problem at once, not the first: telling a user their repo is broken
        // one reason at a time turns one fix into three round trips.
        let mut problems = Vec::new();

        let config = Self::config(repo);
        let branches = match git::resolve_branches(&self.runner, &dir, &config.branches).await {
            Ok(branches) => branches,
            Err(GitError::NotARepository { path }) => {
                problems.push(ProbeProblem {
                    problem: format!("{} is not a git repository", path.display()),
                    remediation: "check the repository's local path".to_owned(),
                });
                Vec::new()
            }
            Err(e) => return Err(Self::map_error(repo, e)),
        };

        if problems.is_empty() && branches.is_empty() {
            problems.push(ProbeProblem {
                problem: format!(
                    "no branch matches {:?}, so nothing would ever be reviewed",
                    config.branches
                ),
                remediation: "adjust the repository's `branches` patterns".to_owned(),
            });
        }

        Ok(ProbeReport {
            usable: problems.is_empty(),
            tool_version: Some(version),
            default_branch: branches.first().cloned(),
            problems,
        })
    }

    async fn discover(
        &self,
        repo: &Repo,
        cursor: Option<&Cursor>,
        limit: usize,
    ) -> Result<Vec<DetectedChange>> {
        let dir = Self::dir(repo)?;
        let config = Self::config(repo);

        let branches = git::resolve_branches(&self.runner, &dir, &config.branches)
            .await
            .map_err(|e| Self::map_error(repo, e))?;

        let mut per_branch = Vec::with_capacity(branches.len());
        for branch in &branches {
            // Each branch keeps its own cursor; a shared one would let a commit on a
            // quiet branch be skipped by activity on a busy one.
            let position = cursor
                .filter(|c| c.scope == Cursor::commits_scope(branch))
                .map(|c| c.value.as_str());

            let found = git::discover_branch(&self.runner, &dir, branch, position, limit)
                .await
                .map_err(|e| Self::map_error(repo, e))?;
            per_branch.push(found);
        }

        Ok(git::merge_discoveries(per_branch))
    }

    async fn materialize(
        &self,
        repo: &Repo,
        change: &Change,
        into: &Path,
    ) -> Result<ChangeContext> {
        let dir = Self::dir(repo)?;

        git::materialize(&self.runner, &dir, change, into)
            .await
            .map_err(|e| match e {
                // A rev that does not resolve is the caller naming something that is
                // not there, not a broken repository. It gets its own variant so the
                // CLI can exit with a message about the rev rather than about git.
                // Wording taken from git, not guessed: `worktree add` says
                // "invalid reference", `archive` on a bare mirror says "not a valid
                // object name", and `rev-parse` says "unknown revision". The first
                // version of this list had three plausible strings and none of the
                // one that actually fires.
                GitError::Failed { ref stderr, .. }
                    if stderr.contains("invalid reference")
                        || stderr.contains("unknown revision")
                        || stderr.contains("bad revision")
                        || stderr.contains("not a valid object name") =>
                {
                    VcsError::NoSuchChange {
                        repo: repo.name.clone(),
                        kind: change.kind,
                        external_id: change.external_id.clone(),
                    }
                }
                other => Self::map_error(repo, other),
            })
    }

    async fn install_hooks(&self, repo: &Repo, mode: HookMode) -> Result<HookReport> {
        // RL-1201's item. Reporting "not installed" is honest for `Verify`; the two
        // mutating modes refuse rather than silently doing nothing, because a user
        // who runs `install` and is told nothing would reasonably believe it worked.
        match mode {
            HookMode::Verify => Ok(HookReport {
                installed: false,
                preserved: Vec::new(),
                problems: Vec::new(),
            }),
            HookMode::Install | HookMode::Uninstall => Err(VcsError::Unusable {
                repo: repo.name.clone(),
                problem: "git hook installation is not implemented yet".to_owned(),
                remediation: "use polling for now; hooks land with RL-1201".to_owned(),
            }),
        }
    }
}
