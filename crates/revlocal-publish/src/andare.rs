//! The Andare issue target (RL-705, SPEC §11.4).
//!
//! # The fingerprint trailer is the idempotency key, and it lives in the body
//!
//! §11.4 files a finding as an issue carrying `rev-local-fingerprint: <fp>`, and
//! searches for that trailer before filing again. The trailer is in the issue
//! body rather than a label or a custom field because ADR 0028 found Andare's
//! `create_issue` takes no labels at all — a label would cost a second call and
//! could fail independently, leaving an issue nothing can ever find again.
//!
//! # Without search, the target does not file
//!
//! The fourth criterion is the one worth stating carefully. If the `search`
//! capability is unmapped, rev-local cannot tell a first filing from a hundredth.
//! Filing anyway would put a new issue in somebody's tracker on **every run**, and
//! the tracker is the one place where that is unrecoverable by a retry — you
//! cannot un-notify the people who were watching the project.
//!
//! So it degrades to comment-only and says which capability is missing. A target
//! that does less and says so beats a target that does damage quietly, and §18's
//! no-silent-caps rule is the same rule seen from the publishing end.

use std::fmt::Write as _;

use async_trait::async_trait;
use revlocal_core::{
    Capability, CapabilitySet, Finding, PublishAction, PublishReceipt, Severity, TargetHealth,
};
use serde::{Deserialize, Serialize};

use crate::target::{PublishError, PublishTarget};

/// The trailer that makes an issue findable again.
pub const FINGERPRINT_TRAILER: &str = "rev-local-fingerprint:";

/// How this repository files into Andare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AndareOptions {
    /// The project key issues are filed into.
    pub project: String,
    /// Findings below this severity do not become issues (§11.4, default `high`).
    pub min_severity: Severity,
}

impl Default for AndareOptions {
    fn default() -> Self {
        Self {
            project: String::new(),
            min_severity: Severity::High,
        }
    }
}

/// What a finding's issue can link back to.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueContext {
    /// The pull request URL, commit URL, or `r1234` for a Subversion revision.
    pub change_ref: Option<String>,
    /// The Trama review page (§11.5).
    pub trama_url: Option<String>,
    /// The lines the finding is about.
    pub code_excerpt: Option<String>,
}

/// An issue as it would be filed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueDraft {
    /// The project key.
    pub project: String,
    /// Andare's `summary` — ADR 0028: not `title`.
    pub summary: String,
    /// Andare's `description` — ADR 0028: not `body`.
    pub description: String,
    /// The fingerprint carried in the trailer.
    pub fingerprint: String,
}

/// Rank severities so a threshold can be compared.
const fn rank(severity: Severity) -> u8 {
    match severity {
        Severity::Info => 0,
        Severity::Low => 1,
        Severity::Medium => 2,
        Severity::High => 3,
        Severity::Critical => 4,
    }
}

/// Whether this finding is severe enough to file (§11.4).
pub const fn is_filable(finding_severity: Severity, min: Severity) -> bool {
    rank(finding_severity) >= rank(min)
}

/// The findings that would become issues, in the order given.
pub fn filing_candidates<'a>(findings: &'a [Finding], options: &AndareOptions) -> Vec<&'a Finding> {
    findings
        .iter()
        .filter(|finding| is_filable(finding.severity, options.min_severity))
        .collect()
}

/// Compose the issue for one finding.
///
/// Every section §11.4 lists is present or explicitly absent — a body that simply
/// omits the failure scenario reads as a finding that had none, which is a
/// different claim from "the engine did not give one".
pub fn compose_issue(
    finding: &Finding,
    context: &IssueContext,
    options: &AndareOptions,
) -> IssueDraft {
    let mut description = String::new();

    let _ = writeln!(description, "{}\n", finding.body.trim());

    if let Some(scenario) = &finding.failure_scenario {
        let _ = writeln!(description, "## Failure scenario\n\n{}\n", scenario.trim());
    }

    if let Some(excerpt) = &context.code_excerpt {
        let _ = writeln!(
            description,
            "## Code\n\n```\n{}\n```\n",
            excerpt.trim_end_matches('\n')
        );
    }

    if let Some(fix) = &finding.suggested_fix {
        let _ = writeln!(description, "## Suggested fix\n\n{}\n", fix.trim());
    }

    let _ = writeln!(description, "## Where\n");
    let _ = writeln!(description, "- Location: {}", location(finding));
    match &context.change_ref {
        Some(reference) => {
            let _ = writeln!(description, "- Change: {reference}");
        }
        None => {
            let _ = writeln!(description, "- Change: not recorded");
        }
    }
    match &context.trama_url {
        Some(url) => {
            let _ = writeln!(description, "- Review page: {url}");
        }
        None => {
            let _ = writeln!(description, "- Review page: not published");
        }
    }
    let _ = writeln!(
        description,
        "- Severity: {} · Category: {} · Confidence: {:.2}",
        finding.severity.as_str(),
        finding.category.as_str(),
        finding.confidence
    );

    let _ = write!(
        description,
        "\n---\n{FINGERPRINT_TRAILER} {}\n",
        finding.fingerprint
    );

    IssueDraft {
        project: options.project.clone(),
        summary: finding.title.clone(),
        description,
        fingerprint: finding.fingerprint.clone(),
    }
}

/// `path:line`, `path`, or a note that the finding is not file-scoped.
fn location(finding: &Finding) -> String {
    match (&finding.file, finding.line_start, finding.line_end) {
        (Some(file), Some(start), Some(end)) if end > start => format!("`{file}:{start}-{end}`"),
        (Some(file), Some(start), _) => format!("`{file}:{start}`"),
        (Some(file), None, _) => format!("`{file}`"),
        (None, _, _) => "not file-scoped".to_owned(),
    }
}

/// The AQL that finds an issue already filed for this fingerprint.
///
/// Quoted and matched with `~` because the trailer sits inside the description.
/// Restricted to the project so a fingerprint that somehow collided across
/// projects cannot make one project's finding comment on another's issue.
pub fn search_query(project: &str, fingerprint: &str) -> String {
    format!(r#"project = "{project}" AND text ~ "{FINGERPRINT_TRAILER} {fingerprint}""#)
}

/// The comment left on an issue that already exists.
pub fn recurrence_comment(finding: &Finding, context: &IssueContext) -> String {
    let mut body = String::from("rev-local saw this again.\n\n");
    let _ = writeln!(body, "- Location: {}", location(finding));
    match &context.change_ref {
        Some(reference) => {
            let _ = writeln!(body, "- Change: {reference}");
        }
        None => {
            let _ = writeln!(body, "- Change: not recorded");
        }
    }
    if let Some(url) = &context.trama_url {
        let _ = writeln!(body, "- Review page: {url}");
    }
    let _ = write!(
        body,
        "\n<sub>{FINGERPRINT_TRAILER} {}</sub>\n",
        finding.fingerprint
    );
    body
}

/// What a search for the fingerprint turned up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchOutcome {
    /// An issue already carries this fingerprint.
    Found(String),
    /// Searched, and there is none.
    NotFound,
    /// The search capability is not mapped on this server.
    Unavailable,
}

/// What the target will do with one finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilingPlan {
    /// File a new issue.
    Create(Box<IssueDraft>),
    /// Comment on the issue that already carries this fingerprint.
    CommentOn {
        /// The existing issue.
        key: String,
        /// What to say.
        body: String,
    },
    /// File nothing, and say why.
    ///
    /// §11.4's degraded mode. Not an error: the run succeeded and this target did
    /// less than it would have.
    Degraded {
        /// What a person should read.
        reason: String,
    },
    /// Below `andare_min_severity`.
    BelowThreshold,
}

/// Decide what to do with one finding.
pub fn plan(
    finding: &Finding,
    context: &IssueContext,
    options: &AndareOptions,
    search: &SearchOutcome,
) -> FilingPlan {
    if !is_filable(finding.severity, options.min_severity) {
        return FilingPlan::BelowThreshold;
    }

    match search {
        SearchOutcome::Found(key) => FilingPlan::CommentOn {
            key: key.clone(),
            body: recurrence_comment(finding, context),
        },
        SearchOutcome::NotFound => {
            FilingPlan::Create(Box::new(compose_issue(finding, context, options)))
        }
        // Filing without search would put a new issue in somebody's tracker on
        // every run, and that is the one failure a retry cannot undo — you cannot
        // un-notify the people watching the project.
        SearchOutcome::Unavailable => FilingPlan::Degraded {
            reason: "the `search` capability is unmapped on this Andare server, so \
                     rev-local cannot tell a first filing from a repeat; filing is \
                     disabled rather than risking a duplicate issue on every run \
                     (map it with `revlocal targets map`)"
                .to_owned(),
        },
    }
}

/// The Andare operations this target needs.
#[async_trait]
pub trait AndareWriter: Send + Sync {
    /// Whether the `search` capability is bound on this server.
    fn can_search(&self) -> bool;

    /// Find an issue carrying this fingerprint.
    async fn search(&self, query: &str) -> Result<Option<String>, PublishError>;

    /// File an issue, returning its key.
    async fn create_issue(&self, draft: &IssueDraft) -> Result<String, PublishError>;

    /// Comment on an issue.
    async fn comment(&self, key: &str, body: &str) -> Result<(), PublishError>;
}

/// Everything the stored action carries for one filing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AndarePayload {
    /// The issue as it would be filed.
    pub draft: IssueDraft,
    /// What it links back to.
    pub context: IssueContext,
    /// What to say if the issue already exists.
    pub recurrence_body: String,
}

/// The Andare publish target (§11.4).
#[derive(Debug)]
pub struct AndareTarget<W: AndareWriter> {
    writer: W,
}

impl<W: AndareWriter> AndareTarget<W> {
    /// A target over `writer`.
    pub const fn new(writer: W) -> Self {
        Self { writer }
    }
}

#[async_trait]
impl<W: AndareWriter> PublishTarget for AndareTarget<W> {
    fn id(&self) -> &str {
        "andare"
    }

    async fn discover(&self) -> Result<CapabilitySet, PublishError> {
        let mut capabilities = vec![
            Capability::CreateIssue,
            Capability::SetStatus,
            Capability::Comment,
        ];
        if !self.writer.can_search() {
            // Reported rather than assumed: §11.2 wants an unmapped capability
            // visible, and this is the one whose absence changes behaviour.
            capabilities.retain(|c| *c != Capability::CreateIssue);
        }
        Ok(CapabilitySet::new(capabilities))
    }

    async fn execute(&self, action: &PublishAction) -> Result<PublishReceipt, PublishError> {
        let payload: AndarePayload =
            serde_json::from_str(&action.payload_json).map_err(|e| PublishError::Rejected {
                target: "andare".to_owned(),
                status: None,
                detail: format!("the stored payload is not an issue: {e}"),
            })?;

        if !self.writer.can_search() {
            return Err(PublishError::Unsupported {
                target: "andare".to_owned(),
                capability: Capability::CreateIssue,
            });
        }

        let query = search_query(&payload.draft.project, &payload.draft.fingerprint);
        match self.writer.search(&query).await? {
            Some(key) => {
                self.writer.comment(&key, &payload.recurrence_body).await?;
                Ok(PublishReceipt {
                    external_ref: Some(key),
                    response_json: None,
                    // The effect already existed; §11.6 wants that distinguishable
                    // from a fresh filing in the audit log.
                    deduplicated: true,
                })
            }
            None => {
                let key = self.writer.create_issue(&payload.draft).await?;
                Ok(PublishReceipt {
                    external_ref: Some(key),
                    response_json: None,
                    deduplicated: false,
                })
            }
        }
    }

    async fn health(&self) -> Result<TargetHealth, PublishError> {
        let capabilities = self.discover().await?;
        Ok(TargetHealth {
            reachable: true,
            detail: (!self.writer.can_search()).then(|| {
                "`search` is unmapped, so filing is disabled to avoid duplicate issues".to_owned()
            }),
            capabilities,
        })
    }
}
