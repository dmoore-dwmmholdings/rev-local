//! The Subversion adapter (SPEC §6.4, decision D6).
//!
//! # Why this did not exist until now
//!
//! Everything under it did. `discover.rs`, `materialize.rs`, `cmd.rs`,
//! `demotion.rs` and `pseudo_pr.rs` were written, unit-tested and exported —
//! and `GitAdapter` was the only `impl VcsAdapter`, so nothing could reach
//! them. A repository added as `--kind svn` was handed to the git adapter and
//! told its working copy "is not a git repository" (RL-1556, RL-1558).
//!
//! # How an SVN repository is addressed
//!
//! By its root URL. SPEC §5 says `remote_url` holds the "svn root URL", and
//! `repo add <path|url>` puts a URL there. Added by path instead — a working
//! copy — the URL is asked of the working copy itself, because requiring
//! somebody to know their own repository's URL to add a checkout they are
//! standing in would be a worse answer than running one command.

use std::path::{Path, PathBuf};

use revlocal_core::{Change, ChangeKind, Repo, RepoKind};

use crate::adapter::{
    ChangeContext, DetectedChange, HookMode, HookReport, ProbeProblem, ProbeReport, Result,
    VcsAdapter, VcsError,
};
use crate::svn::{self, SvnError, SvnRunner, WatchedPaths};

/// Reviews a Subversion repository.
#[derive(Debug, Default)]
pub struct SvnAdapter {
    runner: SvnRunner,
}

impl SvnAdapter {
    /// A new adapter over the default runner.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Where to run `svn` from.
    ///
    /// The working copy when there is one, otherwise anywhere — a URL-addressed
    /// repository needs no local directory, and the current directory is as good
    /// a place as any to run a command that ignores it.
    fn dir(repo: &Repo) -> PathBuf {
        repo.local_path
            .as_deref()
            .map_or_else(|| PathBuf::from("."), PathBuf::from)
    }

    fn map_error(repo: &Repo, error: SvnError) -> VcsError {
        VcsError::Unusable {
            repo: repo.name.clone(),
            problem: error.to_string(),
            remediation: "check the repository's URL and that `svn` can reach it".to_owned(),
        }
    }

    /// The repository's root URL.
    ///
    /// `remote_url` when it is set. Otherwise asked of the working copy, since
    /// somebody who added a checkout by path should not have to know its URL.
    async fn root_url(&self, repo: &Repo) -> Result<String> {
        if let Some(url) = repo.remote_url.as_deref().filter(|u| !u.trim().is_empty()) {
            return Ok(url.to_owned());
        }

        let dir = Self::dir(repo);
        let out = self
            .runner
            .run(&dir, &["info", "--show-item", "url"])
            .await
            .map_err(|e| Self::map_error(repo, e))?;

        let url = out.stdout.trim().to_owned();
        if url.is_empty() {
            return Err(VcsError::Unusable {
                repo: repo.name.clone(),
                problem: format!(
                    "`{}` is not a working copy and the repository has no remote URL",
                    dir.display()
                ),
                remediation: "add it by its root URL instead of by path".to_owned(),
            });
        }
        Ok(url)
    }

    /// The revision a cursor names, or 0 for "from the beginning".
    ///
    /// A cursor that is not a number is treated as absent rather than fatal: the
    /// alternative is a repository that can never be discovered again because one
    /// row is malformed, and re-discovery is bounded — the store upserts by
    /// `(repo_id, kind, external_id)`.
    fn revision_of(cursor: Option<&revlocal_core::Cursor>) -> u64 {
        cursor
            .and_then(|c| c.value.trim().parse::<u64>().ok())
            .unwrap_or(0)
    }
}

#[async_trait::async_trait]
impl VcsAdapter for SvnAdapter {
    fn kind(&self) -> RepoKind {
        RepoKind::Svn
    }

    async fn probe(&self, repo: &Repo) -> Result<ProbeReport> {
        if !svn::is_available().await {
            return Ok(ProbeReport {
                usable: false,
                tool_version: None,
                default_branch: None,
                problems: vec![ProbeProblem {
                    problem: "svn is not on PATH".to_owned(),
                    remediation: "install Subversion and make sure `svn` is on PATH".to_owned(),
                }],
            });
        }

        let dir = Self::dir(repo);
        let version = self
            .runner
            .run(&dir, &["--version", "--quiet"])
            .await
            .ok()
            .map(|out| out.stdout.trim().to_owned());

        // Every problem at once rather than the first, for the same reason the git
        // adapter does it: one fix should not take three round trips.
        let mut problems = Vec::new();
        if let Err(error) = self.root_url(repo).await {
            problems.push(ProbeProblem {
                problem: error.to_string(),
                remediation: "add the repository by its root URL, or point it at a working copy"
                    .to_owned(),
            });
        }

        Ok(ProbeReport {
            usable: problems.is_empty(),
            tool_version: version,
            default_branch: Some(WatchedPaths::default().trunk),
            problems,
        })
    }

    async fn discover(
        &self,
        repo: &Repo,
        cursor: Option<&revlocal_core::Cursor>,
        limit: usize,
    ) -> Result<Vec<DetectedChange>> {
        let url = self.root_url(repo).await?;
        let watched = WatchedPaths::default();
        let limit = u32::try_from(limit).unwrap_or(u32::MAX);

        let found = svn::discover(
            &self.runner,
            &url,
            Self::revision_of(cursor),
            limit,
            &watched,
        )
        .await
        .map_err(|e| Self::map_error(repo, e))?;

        Ok(found
            .reviewable
            .into_iter()
            .map(|rev| DetectedChange {
                // `SvnRev`, not `Commit`: §9.4's `disabled_kind` refuses a
                // commit when `review_commits` is off and deliberately never
                // refuses a Subversion kind — "a repository watched over
                // Subversion has nothing else to review". Calling a revision a
                // commit made that setting skip everything (RL-1561).
                kind: ChangeKind::SvnRev,
                // `r1234` is how a Subversion revision is written everywhere else
                // in this codebase and in svn's own output; a bare number would
                // read as a database id in the report and the issue title.
                external_id: format!("r{}", rev.revision),
                title: rev.message.lines().next().map(str::to_owned),
                author_name: rev.author.clone(),
                author_email: None,
                authored_at: None,
                branch: Some(watched.trunk.clone()),
                base_ref: None,
                parents: Vec::new(),
                paths: rev.paths.iter().map(|p| p.path.clone()).collect(),
                head_ref: None,
                url: None,
                diff_stat: revlocal_core::DiffStat::default(),
                skip_reason: None,
                // The cursor is the revision number alone: `discover` parses it
                // back with `parse::<u64>()`, and `r1234` would not round-trip.
                cursor_value: rev.revision.to_string(),
            })
            .collect())
    }

    async fn materialize(
        &self,
        repo: &Repo,
        change: &Change,
        into: &Path,
    ) -> Result<ChangeContext> {
        let url = self.root_url(repo).await?;
        let revision = change
            .external_id
            .trim_start_matches('r')
            .parse::<u64>()
            .map_err(|_| VcsError::Unusable {
                repo: repo.name.clone(),
                problem: format!("`{}` is not a Subversion revision", change.external_id),
                remediation: "revisions look like `r1234`".to_owned(),
            })?;

        svn::materialize(&self.runner, &url, revision, into)
            .await
            .map_err(|e| Self::map_error(repo, e))
    }

    async fn install_hooks(&self, repo: &Repo, _mode: HookMode) -> Result<HookReport> {
        // Subversion hooks live on the server, in the repository's `hooks/`
        // directory — not in a working copy. There is nothing this machine can
        // install, and §18 says so rather than reporting a success nobody got.
        Ok(HookReport {
            installed: false,
            preserved: Vec::new(),
            problems: vec![ProbeProblem {
                problem: format!(
                    "{}: Subversion hooks live on the server, not in a working copy",
                    repo.name
                ),
                remediation:
                    "poll on a timer, or add a server-side post-commit hook that calls `revlocal trigger`"
                        .to_owned(),
            }],
        })
    }
}
