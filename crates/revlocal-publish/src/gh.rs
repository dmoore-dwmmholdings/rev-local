//! Running `gh` (RL-1511, SPEC §11.3, §6.3).
//!
//! # The request builders had nobody to run them
//!
//! `gh_list_reviews`, `gh_create_review`, `gh_update_review` and `gh_set_check`
//! have existed since M7 and produced a [`GhRequest`] each. Nothing in the
//! workspace ever executed one: `grep "impl GitHubWriter"` found a single hit, and
//! it was the fake in the review tests. So GitHub publishing compiled, was
//! covered by tests, and could not post anything.
//!
//! # The program is a field, not a constant
//!
//! [`GhCli::with_program`] exists so the tests can point at a fixture script and
//! assert the error mapping without a network or a token — the ground rule that
//! inner-loop tests never touch either. It is not a configuration knob and is not
//! read from anywhere a repository under review could set it.
//!
//! # What `gh` says when it fails, and why the mapping matters
//!
//! `gh api` writes `gh: Not Found (HTTP 404)` to stderr and exits non-zero.
//! §11.6 makes "should this be tried again?" structural rather than a matter of
//! pattern-matching error text later, so the status is parsed once, here, and
//! turned into the [`PublishError`] variant that carries the answer. A 404 that
//! retried would hammer a repository that does not exist; a 502 that did not
//! would drop a review because GitHub hiccuped.

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::github::{
    find_own_review, gh_create_review, gh_list_reviews, gh_update_review, ExistingReview,
    GhRequest, GitHubWriter, ReviewPayload,
};
use crate::target::PublishError;

/// The program run when nothing else is named.
pub const DEFAULT_PROGRAM: &str = "gh";

/// The target id every error from this module is attributed to.
const TARGET: &str = "github";

/// Runs [`GhRequest`]s with the GitHub CLI.
#[derive(Debug, Clone)]
pub struct GhCli {
    program: PathBuf,
}

impl Default for GhCli {
    fn default() -> Self {
        Self::new()
    }
}

impl GhCli {
    /// `gh`, found on `PATH`.
    pub fn new() -> Self {
        Self {
            program: PathBuf::from(DEFAULT_PROGRAM),
        }
    }

    /// A specific program. For tests; see the module docs.
    pub fn with_program(program: impl AsRef<Path>) -> Self {
        Self {
            program: program.as_ref().to_path_buf(),
        }
    }

    /// Run one request and return its stdout.
    pub async fn run(&self, request: &GhRequest) -> Result<String, PublishError> {
        use tokio::io::AsyncWriteExt as _;

        let mut command = tokio::process::Command::new(&self.program);
        command
            .args(&request.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let mut child = command.spawn().map_err(|error| {
            // The one failure worth its own sentence: a missing `gh` is a setup
            // problem with a fix, and reporting it as a transport error with the
            // raw OS message sends somebody to check their network.
            let detail = if error.kind() == std::io::ErrorKind::NotFound {
                format!(
                    "`{}` is not installed\n  try: install the GitHub CLI and run `gh auth login`",
                    self.program.display()
                )
            } else {
                format!("could not run `{}`: {error}", self.program.display())
            };
            PublishError::Transport {
                target: TARGET.to_owned(),
                detail,
            }
        })?;

        // Closed either way. A request with no body still has to see EOF, or `gh`
        // waits on a pipe nobody will ever write to and the pass hangs rather
        // than fails — the failure mode this project has been bitten by most.
        if let Some(mut stdin) = child.stdin.take() {
            if let Some(body) = &request.stdin {
                stdin.write_all(body.as_bytes()).await.map_err(|error| {
                    PublishError::Transport {
                        target: TARGET.to_owned(),
                        detail: format!("could not send the request body: {error}"),
                    }
                })?;
            }
            drop(stdin);
        }

        let output = child
            .wait_with_output()
            .await
            .map_err(|error| PublishError::Transport {
                target: TARGET.to_owned(),
                detail: format!("`{}` did not finish: {error}", self.program.display()),
            })?;

        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }

        Err(classify(&String::from_utf8_lossy(&output.stderr)))
    }
}

/// Turn `gh`'s stderr into the error variant that answers "retry?".
///
/// Exposed for the tests, which assert the classification rather than the
/// wording: §11.6's retry rules are the contract, and the wording is not.
pub fn classify(stderr: &str) -> PublishError {
    let detail = first_useful_line(stderr);

    let Some(status) = http_status(stderr) else {
        // No status at all means `gh` failed before it reached GitHub — not
        // logged in, no network, a bad flag. Retryable, because the two most
        // common causes are transient and the third is fixed by a human without
        // this queue needing to know.
        return PublishError::Transport {
            target: TARGET.to_owned(),
            detail,
        };
    };

    // Checked before the 4xx arm below, because a rate limit *is* a 403 and
    // treating it as terminal would drop the work GitHub only asked us to defer.
    if is_rate_limited(stderr, status) {
        return PublishError::RateLimited {
            target: TARGET.to_owned(),
            // `gh` does not surface `Retry-After`, and inventing a number would
            // be worse than the retry policy's own backoff.
            retry_after_secs: None,
        };
    }

    if status >= 500 {
        return PublishError::Server {
            target: TARGET.to_owned(),
            status: Some(status),
            detail,
        };
    }

    PublishError::Rejected {
        target: TARGET.to_owned(),
        status: Some(status),
        detail,
    }
}

/// Pull `nnn` out of `gh`'s `(HTTP nnn)` suffix.
fn http_status(stderr: &str) -> Option<u16> {
    let start = stderr.find("(HTTP ")? + "(HTTP ".len();
    let rest = stderr.get(start..)?;
    let end = rest.find(')')?;
    rest.get(..end)?.trim().parse().ok()
}

/// Whether GitHub is asking for less traffic rather than refusing outright.
fn is_rate_limited(stderr: &str, status: u16) -> bool {
    if status == 429 {
        return true;
    }
    // GitHub answers a secondary rate limit with 403 and says so in the body,
    // which is the only thing distinguishing it from a permissions refusal.
    let lowered = stderr.to_ascii_lowercase();
    status == 403 && (lowered.contains("rate limit") || lowered.contains("abuse detection"))
}

/// The first line of stderr worth showing a person.
///
/// `gh` prefixes its own diagnostics with `gh: `; blank lines and the usage
/// footer it prints after a bad flag are noise in an error message.
fn first_useful_line(stderr: &str) -> String {
    stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map_or_else(
            || "`gh` failed and said nothing".to_owned(),
            |line| line.strip_prefix("gh: ").unwrap_or(line).to_owned(),
        )
}

/// [`GitHubWriter`] over the `gh` CLI.
#[derive(Debug, Clone, Default)]
pub struct GhWriter {
    cli: GhCli,
}

impl GhWriter {
    /// A writer over `gh` on `PATH`.
    pub fn new() -> Self {
        Self { cli: GhCli::new() }
    }

    /// A writer over a specific program. For tests; see the module docs.
    pub fn with_program(program: impl AsRef<Path>) -> Self {
        Self {
            cli: GhCli::with_program(program),
        }
    }

    fn unencodable(error: serde_json::Error) -> PublishError {
        PublishError::Rejected {
            target: TARGET.to_owned(),
            status: None,
            detail: format!("the request could not be encoded: {error}"),
        }
    }
}

/// Read the review GitHub echoes back after a create or an update.
///
/// A missing id is a rejection rather than a retry: the call succeeded and
/// returned something this code does not understand, and repeating it would post
/// a second review.
fn review_from(json: &str) -> Result<ExistingReview, PublishError> {
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|error| PublishError::Rejected {
            target: TARGET.to_owned(),
            status: None,
            detail: format!("`gh` returned output that is not JSON: {error}"),
        })?;

    let id = value
        .get("id")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| PublishError::Rejected {
            target: TARGET.to_owned(),
            status: None,
            detail: "the review GitHub returned has no id".to_owned(),
        })?;

    Ok(ExistingReview {
        id,
        url: value
            .get("html_url")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
    })
}

#[async_trait]
impl GitHubWriter for GhWriter {
    async fn find_review(
        &self,
        repo: &str,
        pr: u64,
        head_sha: &str,
    ) -> Result<Option<ExistingReview>, PublishError> {
        let listing = self.cli.run(&gh_list_reviews(repo, pr)).await?;
        Ok(find_own_review(&listing, head_sha))
    }

    async fn create_review(&self, payload: &ReviewPayload) -> Result<ExistingReview, PublishError> {
        let request = gh_create_review(payload).map_err(Self::unencodable)?;
        review_from(&self.cli.run(&request).await?)
    }

    async fn update_review(
        &self,
        repo: &str,
        pr: u64,
        review_id: u64,
        body: &str,
    ) -> Result<ExistingReview, PublishError> {
        let request = gh_update_review(repo, pr, review_id, body).map_err(Self::unencodable)?;
        review_from(&self.cli.run(&request).await?)
    }
}
