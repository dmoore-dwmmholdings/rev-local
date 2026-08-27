//! Pull-request discovery (SPEC §6.3).
//!
//! # A pull request is not one change
//!
//! `external_id = "{number}:{head_sha}"`, so **a re-push is a distinct Change**.
//! That is the whole design and it is worth being explicit about why: a PR is a
//! moving target, and a review of PR #7 means nothing without saying *which* PR #7.
//! Keying on the number alone would let a re-push mutate the row a finding points
//! at, so a finding filed against reviewed code would silently start describing
//! code nobody reviewed.
//!
//! The cost is that a long-lived PR accumulates a Change per push. That is paid for
//! by [`superseded_fingerprints`]: findings from the previous head that do not recur
//! stop being reported, without being deleted.
//!
//! # Fetching is behind a trait
//!
//! [`PullRequestSource`] exists so discovery can be tested without a network, a
//! token, or a GitHub account. The `gh`-backed implementation is a thin adapter over
//! `gh pr list --json`, and its *parsing* is tested against a captured payload —
//! which is the part that actually breaks when `gh` changes its output.

use std::collections::BTreeSet;

use revlocal_core::{ChangeKind, DiffStat, RepoConfig, Timestamp};

use crate::adapter::DetectedChange;
use crate::skip_rules::{Skip, SkipReason};

/// An open pull request, as much of it as discovery needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    /// The PR number.
    pub number: u64,
    /// Its title.
    pub title: String,
    /// The head commit. Half of the change's identity.
    pub head_sha: String,
    /// The branch it targets.
    pub base_ref: String,
    /// The branch it comes from.
    pub head_ref: String,
    /// Login of whoever opened it.
    pub author: String,
    /// Whether it is a draft (SPEC §13.2 `review_draft_prs`, default false).
    pub is_draft: bool,
    /// When it last changed. §6.3 orders discovery by this.
    pub updated_at: Option<Timestamp>,
    /// Web URL.
    pub url: String,
    /// Every commit the PR contains, for the `covered_by_pr` skip.
    pub commit_shas: Vec<String>,
}

impl PullRequest {
    /// The change identity for this PR at its current head (SPEC §6.3).
    pub fn external_id(&self) -> String {
        format!("{}:{}", self.number, self.head_sha)
    }
}

/// Where open pull requests come from.
///
/// A trait so discovery is testable offline. Implementations are transport-specific
/// (`gh`, the GitHub MCP server, unauthenticated REST) and chosen by `RL-307`'s
/// ladder.
#[async_trait::async_trait]
pub trait PullRequestSource: Send + Sync {
    /// Open PRs targeting any of `base_branches`, newest activity first.
    async fn open_pull_requests(
        &self,
        base_branches: &[String],
    ) -> Result<Vec<PullRequest>, crate::git::GitError>;
}

/// Turn a pull request into a change.
pub fn to_detected_change(pr: &PullRequest) -> DetectedChange {
    DetectedChange {
        kind: ChangeKind::Pr,
        external_id: pr.external_id(),
        title: Some(pr.title.clone()),
        author_name: Some(pr.author.clone()),
        author_email: None,
        authored_at: pr.updated_at,
        branch: Some(pr.head_ref.clone()),
        base_ref: Some(pr.base_ref.clone()),
        parents: Vec::new(),
        // Filled by materialization; PR listing does not carry a file list, and
        // guessing one would make the skip rules act on a fiction.
        paths: Vec::new(),
        head_ref: Some(pr.head_sha.clone()),
        url: Some(pr.url.clone()),
        diff_stat: DiffStat::default(),
        skip_reason: None,
        // §6.3: the PR cursor is `updated_at`, not a SHA — a PR can change without
        // its head moving (a label, a base change), and the cursor has to advance
        // for those too or discovery would re-report them forever.
        cursor_value: pr
            .updated_at
            .map_or_else(|| pr.head_sha.clone(), |t| t.to_rfc3339()),
    }
}

/// Discover open pull requests as changes, applying the draft rule.
///
/// Drafts are skipped rather than omitted, so a user who wonders why their draft
/// has no review can see the reason (SPEC §18).
pub async fn discover(
    source: &dyn PullRequestSource,
    base_branches: &[String],
    config: &RepoConfig,
) -> Result<Vec<DetectedChange>, crate::git::GitError> {
    let mut pull_requests = source.open_pull_requests(base_branches).await?;

    // §6.3 orders discovery by `updated_at`. Ascending, for the same reason commit
    // discovery is oldest-first: reviews publish in the order things happened.
    pull_requests.sort_by(|a, b| {
        a.updated_at
            .cmp(&b.updated_at)
            .then_with(|| a.number.cmp(&b.number))
    });

    Ok(pull_requests
        .iter()
        .map(|pr| {
            let mut change = to_detected_change(pr);
            if pr.is_draft && !config.review_draft_prs {
                change.skip_reason = Some(
                    Skip {
                        reason: SkipReason::DraftPr,
                        detail: format!(
                            "pull request #{} is a draft and review_draft_prs is off",
                            pr.number
                        ),
                    }
                    .to_skip_reason(),
                );
            }
            change
        })
        .collect())
}

/// Mark commits that an open pull request already covers (SPEC §6.3).
///
/// Only relevant when both `review_commits` and `review_prs` are on. Without this,
/// a ten-commit PR would be reviewed eleven times — once per commit and once as a
/// PR — and file the same findings from each.
///
/// Returns how many were marked.
pub fn mark_covered_by_pr(
    commits: &mut [DetectedChange],
    open_pull_requests: &[PullRequest],
) -> usize {
    let covered: BTreeSet<&str> = open_pull_requests
        .iter()
        .flat_map(|pr| pr.commit_shas.iter().map(String::as_str))
        .collect();

    let mut marked = 0;
    for change in commits.iter_mut() {
        // Only commits. A PR change's own external_id is `number:sha` and would
        // never match, but being explicit keeps a future caller from passing a mixed
        // list and getting a confusing result.
        if change.kind != ChangeKind::Commit || change.skip_reason.is_some() {
            continue;
        }
        let Some(pr) = open_pull_requests
            .iter()
            .find(|pr| pr.commit_shas.iter().any(|sha| sha == &change.external_id))
        else {
            continue;
        };
        if covered.contains(change.external_id.as_str()) {
            change.skip_reason = Some(
                Skip {
                    reason: SkipReason::CoveredByPr,
                    detail: format!(
                        "already covered by open pull request #{}, which is reviewed \
                         as a whole",
                        pr.number
                    ),
                }
                .to_skip_reason(),
            );
            marked += 1;
        }
    }
    marked
}

/// Fingerprints from the previous head that did not recur (SPEC §6.3).
///
/// A re-push makes a new Change, so the previous head's findings are attached to a
/// run nobody will look at again. The ones whose fingerprints recur are re-reported
/// naturally; the ones that do not are **superseded, not deleted** — the finding
/// still happened, and an audit trail that quietly loses findings is not one.
///
/// Fingerprints rather than ids because §10.3's fingerprint is deliberately
/// line-number independent: a finding that survived a rebase keeps its fingerprint
/// and should not be re-filed.
pub fn superseded_fingerprints(
    previous: &BTreeSet<String>,
    current: &BTreeSet<String>,
) -> Vec<String> {
    previous.difference(current).cloned().collect()
}

/// The fields `gh pr list --json` is asked for.
///
/// Named once, so the request and the parser cannot drift: asking for a field the
/// parser ignores is harmless, but parsing a field that was never requested yields
/// `null` for every PR and looks like GitHub returning nothing.
pub const GH_PR_FIELDS: &str =
    "number,title,headRefOid,baseRefName,headRefName,author,isDraft,updatedAt,url,commits";

/// Parse `gh pr list --json …` output.
///
/// Separated from fetching because this is the part that breaks when `gh` changes
/// its output, and it is the part that can be tested without a network. Unknown
/// fields are ignored rather than rejected: a newer `gh` adding a key must not stop
/// discovery.
pub fn parse_gh_pr_list(json: &str) -> Result<Vec<PullRequest>, serde_json::Error> {
    let raw: Vec<serde_json::Value> = serde_json::from_str(json)?;

    Ok(raw
        .iter()
        .filter_map(|pr| {
            Some(PullRequest {
                number: pr.get("number")?.as_u64()?,
                title: pr
                    .get("title")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned(),
                head_sha: pr.get("headRefOid")?.as_str()?.to_owned(),
                base_ref: pr
                    .get("baseRefName")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned(),
                head_ref: pr
                    .get("headRefName")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned(),
                // `author` is an object; older `gh` used a bare string. Both are
                // accepted, because a version bump should not stop discovery.
                author: pr
                    .get("author")
                    .and_then(|a| {
                        a.get("login")
                            .and_then(|l| l.as_str())
                            .or_else(|| a.as_str())
                    })
                    .unwrap_or_default()
                    .to_owned(),
                is_draft: pr
                    .get("isDraft")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                updated_at: pr
                    .get("updatedAt")
                    .and_then(|v| v.as_str())
                    .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                    .map(|t| t.with_timezone(&chrono::Utc)),
                url: pr
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned(),
                commit_shas: pr
                    .get("commits")
                    .and_then(|c| c.as_array())
                    .map(|commits| {
                        commits
                            .iter()
                            .filter_map(|c| {
                                c.get("oid")
                                    .and_then(|o| o.as_str())
                                    .or_else(|| c.as_str())
                                    .map(str::to_owned)
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            })
        })
        .collect())
}

/// A [`PullRequestSource`] backed by the `gh` CLI.
#[derive(Debug, Clone)]
pub struct GhCliSource {
    runner: crate::git::GitRunner,
    repo_dir: std::path::PathBuf,
    limit: u32,
}

impl GhCliSource {
    /// Fetch pull requests by running `gh` in `repo_dir`.
    pub fn new(runner: &crate::git::GitRunner, repo_dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            // `gh`, not `git` — but through the same wrapper, for its timeout and
            // its non-interactive environment. `gh` will open a browser and wait
            // forever otherwise, and a daemon has nobody to click it.
            runner: runner.clone().with_program("gh"),
            repo_dir: repo_dir.into(),
            limit: 100,
        }
    }

    /// Cap how many pull requests one pass fetches.
    pub const fn with_limit(mut self, limit: u32) -> Self {
        self.limit = limit;
        self
    }
}

#[async_trait::async_trait]
impl PullRequestSource for GhCliSource {
    async fn open_pull_requests(
        &self,
        base_branches: &[String],
    ) -> Result<Vec<PullRequest>, crate::git::GitError> {
        let limit = self.limit.to_string();
        let output = self
            .runner
            .run(
                &self.repo_dir,
                &[
                    "pr",
                    "list",
                    "--state",
                    "open",
                    "--limit",
                    &limit,
                    "--json",
                    GH_PR_FIELDS,
                ],
            )
            .await?;

        let all = parse_gh_pr_list(&output.stdout).map_err(|e| crate::git::GitError::Failed {
            args: "pr list --json".to_owned(),
            code: -1,
            stderr: format!("could not parse `gh pr list` output: {e}"),
        })?;

        // Filtering here rather than with `--base`: `gh` takes a single base branch,
        // and a repo can watch several (SPEC §13.2's `branches`, which is a glob
        // list). One call plus a filter beats one call per branch.
        Ok(all
            .into_iter()
            .filter(|pr| {
                base_branches.is_empty()
                    || base_branches
                        .iter()
                        .any(|b| branch_matches(b, &pr.base_ref))
            })
            .collect())
    }
}

/// Match a watched-branch pattern against a PR's base branch.
fn branch_matches(pattern: &str, branch: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == branch,
        Some((prefix, suffix)) => {
            branch.len() >= prefix.len() + suffix.len()
                && branch.starts_with(prefix)
                && branch.ends_with(suffix)
        }
    }
}
