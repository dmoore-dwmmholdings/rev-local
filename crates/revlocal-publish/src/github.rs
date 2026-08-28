//! The GitHub review target (RL-703, SPEC §11.3).
//!
//! §11.3 asks for one review per run: a body carrying the summary and a findings
//! table, plus inline comments anchored to `(file, line_start..line_end)` on the
//! PR's head SHA. Two of its rules are the ones worth reading the code for.
//!
//! # A comment that will not anchor is demoted, never dropped
//!
//! GitHub refuses an inline comment on a line the diff does not contain. That
//! happens constantly and for ordinary reasons: a finding about a function that
//! the change *calls* rather than edits, a range that spills past the hunk, a
//! whole-file finding with no line at all.
//!
//! §18's no-silent-caps rule decides what to do about it. A dropped comment makes
//! the review look like it found less than it did, which is the failure mode where
//! somebody merges on a review that was quietly incomplete. So an unanchorable
//! finding moves into the body under "Findings outside the diff", and the review
//! still carries everything the engine said.
//!
//! # APPROVE is opt-in, and the default is deliberate
//!
//! §10.2 has rev-local post `COMMENT` even when its verdict is `approve`, unless
//! the repository has explicitly set `allow_approve`. An approving review is a
//! signal other people act on — in some repositories it satisfies a branch
//! protection rule — and a tool that grants that by default has made a decision
//! about somebody's merge process that nobody asked it to make.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use revlocal_core::{Finding, Severity, Verdict};

/// The review action to submit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewEvent {
    /// A review with no verdict attached.
    Comment,
    /// An approving review. Only ever produced when `allow_approve` is set.
    Approve,
    /// A blocking review.
    RequestChanges,
}

impl ReviewEvent {
    /// GitHub's own name for it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Comment => "COMMENT",
            Self::Approve => "APPROVE",
            Self::RequestChanges => "REQUEST_CHANGES",
        }
    }
}

/// What the repository allows this review to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReviewOptions {
    /// Whether an `approve` verdict may be submitted as an approving review.
    ///
    /// Defaults to false, and §10.2 wants it that way: an approving review can
    /// satisfy a branch protection rule, and granting that by default decides
    /// something about somebody's merge process that they did not ask for.
    pub allow_approve: bool,
    /// Whether a blocking verdict submits `REQUEST_CHANGES` rather than a comment.
    pub block_on_findings: bool,
}

/// The lines an inline comment can be attached to.
///
/// Built from the PR diff: GitHub will only accept a comment on a line the diff
/// actually contains. Held per file as inclusive ranges because that is the shape
/// a diff hunk has, and checking containment is the only question asked of it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiffAnchors {
    by_file: BTreeMap<String, Vec<(u32, u32)>>,
}

impl DiffAnchors {
    /// No lines are anchorable. Every finding is demoted.
    pub fn none() -> Self {
        Self::default()
    }

    /// Record a hunk: `file` has lines `start..=end` in the diff.
    pub fn add(&mut self, file: &str, start: u32, end: u32) {
        self.by_file
            .entry(file.to_owned())
            .or_default()
            .push((start.min(end), start.max(end)));
    }

    /// Whether a comment on this line would be accepted.
    pub fn contains(&self, file: &str, line: u32) -> bool {
        self.by_file
            .get(file)
            .is_some_and(|ranges| ranges.iter().any(|(lo, hi)| line >= *lo && line <= *hi))
    }
}

/// One inline comment, in the shape GitHub's review API wants.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InlineComment {
    /// Path relative to the repository root.
    pub path: String,
    /// The last line of the range — GitHub's `line`.
    pub line: u32,
    /// The first line, when the comment spans more than one.
    pub start_line: Option<u32>,
    /// Markdown.
    pub body: String,
}

/// A review ready to submit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewDraft {
    /// The review body: summary, findings table, and anything demoted.
    pub body: String,
    /// Comments that will anchor.
    pub comments: Vec<InlineComment>,
    /// What to submit it as.
    pub event: ReviewEvent,
}

impl ReviewDraft {
    /// How many findings were demoted into the body.
    pub fn demoted_count(&self) -> usize {
        self.body
            .lines()
            .skip_while(|line| !line.starts_with("## Findings outside the diff"))
            .filter(|line| line.starts_with("- **"))
            .count()
    }
}

/// SPEC §11.3's idempotency key.
///
/// Keyed on the head SHA, so a re-run against the same commit finds the review it
/// already posted and edits it. A new push changes the SHA and therefore earns a
/// new review, which is the behaviour a reviewer expects: the old one stays
/// attached to the commit it was about.
pub fn idempotency_key(repo: &str, pr: u64, head_sha: &str, run_kind: &str) -> String {
    format!("gh:{repo}:{pr}:{head_sha}:{run_kind}")
}

/// Which review event a verdict becomes, given what the repository allows.
pub const fn event_for(verdict: Verdict, options: ReviewOptions) -> ReviewEvent {
    match verdict {
        // The gate. Without `allow_approve` an approving verdict is still only a
        // comment — rev-local reports, it does not vote.
        Verdict::Approve if options.allow_approve => ReviewEvent::Approve,
        Verdict::RequestChanges if options.block_on_findings => ReviewEvent::RequestChanges,
        _ => ReviewEvent::Comment,
    }
}

/// Compose one review from a run's findings.
pub fn compose(
    summary: Option<&str>,
    findings: &[Finding],
    anchors: &DiffAnchors,
    verdict: Verdict,
    options: ReviewOptions,
) -> ReviewDraft {
    let mut comments = Vec::new();
    let mut demoted = Vec::new();

    for finding in findings {
        match anchor_for(finding, anchors) {
            Some(comment) => comments.push(comment),
            None => demoted.push(finding),
        }
    }

    let mut body = String::new();
    body.push_str("## rev-local review\n\n");

    match summary {
        Some(text) if !text.trim().is_empty() => {
            body.push_str(text.trim());
            body.push_str("\n\n");
        }
        _ => {}
    }

    if findings.is_empty() {
        // The zero-findings body is a real case, not an edge case: most reviews of
        // a small change find nothing, and a review that renders as an empty
        // heading reads like a broken tool rather than a clean result.
        body.push_str("No findings.\n");
    } else {
        body.push_str(&findings_table(findings));
    }

    if !demoted.is_empty() {
        body.push_str("\n## Findings outside the diff\n\n");
        body.push_str(
            "These could not be anchored to a line in this diff, and are reported here \
             rather than dropped.\n\n",
        );
        for finding in &demoted {
            let _ = writeln!(
                body,
                "- **{}** — {} ({}, {})\n\n{}\n",
                escape_pipes(&finding.title),
                location(finding),
                finding.severity.as_str(),
                finding.category.as_str(),
                indent(&finding.body)
            );
        }
    }

    ReviewDraft {
        body,
        comments,
        event: event_for(verdict, options),
    }
}

/// The inline comment for a finding, when it can have one.
fn anchor_for(finding: &Finding, anchors: &DiffAnchors) -> Option<InlineComment> {
    let file = finding.file.as_deref()?;
    let end = finding.line_end.or(finding.line_start)?;
    let start = finding.line_start.unwrap_or(end);

    // GitHub anchors on the last line of a range and takes `start_line` for the
    // rest, so the end is the line that must be in the diff.
    if !anchors.contains(file, end) {
        return None;
    }

    // A multi-line comment whose start is outside the diff is rejected as a whole,
    // so it narrows to the anchorable end rather than being demoted entirely —
    // half the context beats none of the comment.
    let start_line = (start < end && anchors.contains(file, start)).then_some(start);

    Some(InlineComment {
        path: file.to_owned(),
        line: end,
        start_line,
        body: comment_body(finding),
    })
}

/// The markdown of one inline comment.
fn comment_body(finding: &Finding) -> String {
    let mut body = format!(
        "**{}** — {} · {}\n\n{}\n",
        finding.title,
        finding.severity.as_str(),
        finding.category.as_str(),
        finding.body.trim()
    );

    if let Some(scenario) = &finding.failure_scenario {
        let _ = write!(body, "\n**Failure scenario:** {}\n", scenario.trim());
    }
    if let Some(fix) = &finding.suggested_fix {
        let _ = write!(body, "\n**Suggested fix:**\n\n{}\n", fix.trim());
    }

    let _ = write!(body, "\n<sub>rev-local · {}</sub>\n", finding.fingerprint);
    body
}

/// The summary table.
fn findings_table(findings: &[Finding]) -> String {
    let mut table = String::from("| Severity | Category | Location | Finding |\n");
    table.push_str("|---|---|---|---|\n");

    // Ordered worst-first so the table reads as a priority list. Findings arrive
    // already sorted by the pipeline (ADR 0024), and this sort is stable, so equal
    // severities keep that order rather than shuffling between runs.
    let mut ordered: Vec<&Finding> = findings.iter().collect();
    ordered.sort_by_key(|f| std::cmp::Reverse(severity_rank(f.severity)));

    for finding in ordered {
        let _ = writeln!(
            table,
            "| {} | {} | {} | {} |",
            finding.severity.as_str(),
            finding.category.as_str(),
            location(finding),
            escape_pipes(&finding.title)
        );
    }
    table
}

const fn severity_rank(severity: Severity) -> u8 {
    match severity {
        Severity::Critical => 4,
        Severity::High => 3,
        Severity::Medium => 2,
        Severity::Low => 1,
        Severity::Info => 0,
    }
}

/// `path:line` or `path`, or `—` for a finding with no file at all.
fn location(finding: &Finding) -> String {
    match (&finding.file, finding.line_start) {
        (Some(file), Some(line)) => format!("`{file}:{line}`"),
        (Some(file), None) => format!("`{file}`"),
        (None, _) => "—".to_owned(),
    }
}

/// A pipe in a title would end the table cell early.
fn escape_pipes(text: &str) -> String {
    text.replace('|', "\\|")
}

/// Indent a body so it sits under its bullet.
fn indent(text: &str) -> String {
    text.trim()
        .lines()
        .map(|line| format!("  {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

// --- delivering the review -------------------------------------------------

use async_trait::async_trait;
use revlocal_core::{CapabilitySet, PublishAction, PublishReceipt, TargetHealth};

use crate::target::{PublishError, PublishTarget};

/// Exactly what would be sent, as it is stored in `publish_action.payload_json`.
///
/// Composed when the action is *created*, not when it is delivered. §5 says the
/// approvals inbox renders this payload verbatim, which only means anything if the
/// payload is the real thing — composing at delivery time would let an approver
/// see one review and a reviewer receive another.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReviewPayload {
    /// `owner/name`.
    pub repo: String,
    /// The pull request number.
    pub pr: u64,
    /// The commit the review is about.
    pub head_sha: String,
    /// The review body.
    pub body: String,
    /// Inline comments that will anchor.
    pub comments: Vec<InlineComment>,
    /// `COMMENT`, `APPROVE` or `REQUEST_CHANGES`.
    pub event: String,
}

impl ReviewPayload {
    /// Build a payload from a composed draft.
    pub fn new(repo: &str, pr: u64, head_sha: &str, draft: &ReviewDraft) -> Self {
        Self {
            repo: repo.to_owned(),
            pr,
            head_sha: head_sha.to_owned(),
            body: draft.body.clone(),
            comments: draft.comments.clone(),
            event: draft.event.as_str().to_owned(),
        }
    }
}

/// What a review already posted for this run looks like.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingReview {
    /// GitHub's review id.
    pub id: u64,
    /// Its URL, for the receipt.
    pub url: Option<String>,
}

/// The GitHub operations this target needs.
///
/// A port, so the target is testable without a network and so the `gh` CLI and
/// the GitHub MCP server (§6.3's transport ladder) are two implementations of one
/// thing rather than two code paths through the target.
#[async_trait]
pub trait GitHubWriter: Send + Sync {
    /// The review rev-local already posted for this head SHA, if any.
    async fn find_review(
        &self,
        repo: &str,
        pr: u64,
        head_sha: &str,
    ) -> Result<Option<ExistingReview>, PublishError>;

    /// Post a new review.
    async fn create_review(&self, payload: &ReviewPayload) -> Result<ExistingReview, PublishError>;

    /// Replace an existing review's body.
    async fn update_review(
        &self,
        repo: &str,
        review_id: u64,
        body: &str,
    ) -> Result<ExistingReview, PublishError>;
}

/// The GitHub publish target (§11.3).
#[derive(Debug)]
pub struct GitHubTarget<W: GitHubWriter> {
    writer: W,
}

impl<W: GitHubWriter> GitHubTarget<W> {
    /// A target over `writer`.
    pub const fn new(writer: W) -> Self {
        Self { writer }
    }
}

#[async_trait]
impl<W: GitHubWriter> PublishTarget for GitHubTarget<W> {
    fn id(&self) -> &str {
        "github"
    }

    async fn discover(&self) -> Result<CapabilitySet, PublishError> {
        Ok(CapabilitySet::new([
            revlocal_core::Capability::PostReview,
            revlocal_core::Capability::Comment,
            revlocal_core::Capability::SetCheck,
        ]))
    }

    async fn execute(&self, action: &PublishAction) -> Result<PublishReceipt, PublishError> {
        let payload: ReviewPayload =
            serde_json::from_str(&action.payload_json).map_err(|e| PublishError::Rejected {
                target: "github".to_owned(),
                status: None,
                detail: format!("the stored payload is not a review: {e}"),
            })?;

        // §11.3: on a re-run for the same head SHA, edit rather than post a second
        // review. Checked against GitHub rather than against our own row, because
        // the row can be behind — a crash between the post and the record leaves a
        // review GitHub has and rev-local does not know about, and posting again
        // would be the duplicate this rule exists to prevent.
        let existing = self
            .writer
            .find_review(&payload.repo, payload.pr, &payload.head_sha)
            .await?;

        let review = match existing {
            Some(found) => {
                self.writer
                    .update_review(&payload.repo, found.id, &payload.body)
                    .await?
            }
            None => self.writer.create_review(&payload).await?,
        };

        Ok(PublishReceipt {
            external_ref: review.url.clone().or_else(|| Some(review.id.to_string())),
            response_json: Some(
                serde_json::json!({ "review_id": review.id, "url": review.url }).to_string(),
            ),
            deduplicated: false,
        })
    }

    async fn health(&self) -> Result<TargetHealth, PublishError> {
        Ok(TargetHealth {
            reachable: true,
            capabilities: self.discover().await?,
            detail: None,
        })
    }
}

// --- the `gh` CLI request shapes -------------------------------------------

/// One `gh api` invocation, as arguments and an optional JSON body.
///
/// Built as data rather than executed inline so the request shape is testable
/// without a network, a repository, or a GitHub account. ADR 0023's rule applies
/// in reverse here: rather than guessing what `gh` prints, this pins what
/// rev-local *sends*, which is the half it controls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhRequest {
    /// Arguments after the program name.
    pub args: Vec<String>,
    /// JSON to write to stdin, for requests that carry a body.
    ///
    /// Passed with `--input -` rather than as repeated `-f key=value` flags: a
    /// review body is markdown with newlines and quotes in it, and inline comments
    /// are an array of objects, neither of which survives flag encoding intact.
    pub stdin: Option<String>,
}

/// The marker rev-local puts in every review body so it can find its own again.
///
/// A review is identified by (repo, pr, head_sha) plus this marker: GitHub does
/// not let a client attach arbitrary metadata to a review, and matching on the
/// author alone would pick up a human using the same token.
pub const REVIEW_MARKER: &str = "<!-- rev-local:review -->";

/// List the reviews on a pull request.
pub fn gh_list_reviews(repo: &str, pr: u64) -> GhRequest {
    GhRequest {
        args: vec![
            "api".to_owned(),
            "--paginate".to_owned(),
            format!("repos/{repo}/pulls/{pr}/reviews"),
        ],
        stdin: None,
    }
}

/// Post a review.
pub fn gh_create_review(payload: &ReviewPayload) -> Result<GhRequest, serde_json::Error> {
    let comments: Vec<serde_json::Value> = payload
        .comments
        .iter()
        .map(|comment| {
            let mut value = serde_json::json!({
                "path": comment.path,
                "line": comment.line,
                "body": comment.body,
                "side": "RIGHT",
            });
            if let Some(start) = comment.start_line {
                value["start_line"] = serde_json::json!(start);
                value["start_side"] = serde_json::json!("RIGHT");
            }
            value
        })
        .collect();

    let body = serde_json::json!({
        "commit_id": payload.head_sha,
        "body": with_marker(&payload.body),
        "event": payload.event,
        "comments": comments,
    });

    Ok(GhRequest {
        args: vec![
            "api".to_owned(),
            "--method".to_owned(),
            "POST".to_owned(),
            format!("repos/{}/pulls/{}/reviews", payload.repo, payload.pr),
            "--input".to_owned(),
            "-".to_owned(),
        ],
        stdin: Some(serde_json::to_string(&body)?),
    })
}

/// Replace an existing review's body.
///
/// Only the body. GitHub's update endpoint does not take comments or an event, so
/// a re-run's new inline comments cannot be added to an existing review — the body
/// carries them instead, which is the same demotion path §11.3 already uses for a
/// comment that will not anchor.
pub fn gh_update_review(
    repo: &str,
    pr: u64,
    review_id: u64,
    body: &str,
) -> Result<GhRequest, serde_json::Error> {
    let payload = serde_json::json!({ "body": with_marker(body) });
    Ok(GhRequest {
        args: vec![
            "api".to_owned(),
            "--method".to_owned(),
            "PUT".to_owned(),
            format!("repos/{repo}/pulls/{pr}/reviews/{review_id}"),
            "--input".to_owned(),
            "-".to_owned(),
        ],
        stdin: Some(serde_json::to_string(&payload)?),
    })
}

/// Add the marker if it is not already there.
fn with_marker(body: &str) -> String {
    if body.contains(REVIEW_MARKER) {
        body.to_owned()
    } else {
        format!("{REVIEW_MARKER}\n{body}")
    }
}

/// Pick rev-local's own review for a head SHA out of a `reviews` listing.
pub fn find_own_review(listing_json: &str, head_sha: &str) -> Option<ExistingReview> {
    let reviews: Vec<serde_json::Value> = serde_json::from_str(listing_json).ok()?;
    reviews.iter().rev().find_map(|review| {
        let body = review.get("body").and_then(serde_json::Value::as_str)?;
        let commit = review
            .get("commit_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        // Both conditions: the marker says it is ours, the SHA says it is about
        // this commit. Matching on either alone would edit the wrong review — the
        // marker alone picks up our review of an earlier push.
        if !body.contains(REVIEW_MARKER) || commit != head_sha {
            return None;
        }
        Some(ExistingReview {
            id: review.get("id").and_then(serde_json::Value::as_u64)?,
            url: review
                .get("html_url")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
        })
    })
}
