//! The Trama documentation target (RL-707, SPEC §11.5).
//!
//! # `update_page` replaces the body, so every update is a read-modify-write
//!
//! This is the server's own guidance and §11.5 repeats it as a critical
//! constraint. Sending a fragment does not append it — it deletes everything else
//! on the page. So the target reads the page, merges its section into what is
//! there, and sends the whole document back.
//!
//! The failure this prevents is not subtle and not recoverable: somebody's
//! hand-written runbook replaced by a findings table, discovered whenever they
//! next look for it.
//!
//! # Markers, so a person and a machine can share a page
//!
//! rev-local owns the text between [`MARKER_BEGIN`] and [`MARKER_END`] and nothing
//! else. Everything outside survives an update untouched, which is what makes it
//! reasonable to let a person edit a page rev-local also writes to — they can add
//! context above it, notes below it, and keep them.
//!
//! A page with no markers is not assumed to be rev-local's to overwrite. The
//! section is appended and the existing body kept, because the alternative — treat
//! an unmarked page as ours — is exactly the clobber this design exists to avoid.

use async_trait::async_trait;
use revlocal_core::{Capability, CapabilitySet, PublishAction, PublishReceipt, TargetHealth};
use serde::{Deserialize, Serialize};

use crate::target::{PublishError, PublishTarget};

/// Start of the region rev-local owns.
pub const MARKER_BEGIN: &str = "<!-- rev-local:begin -->";

/// End of the region rev-local owns.
pub const MARKER_END: &str = "<!-- rev-local:end -->";

/// How long a change title may be in a page title before it is truncated.
pub const TITLE_BUDGET: usize = 60;

/// §11.5's page title: `Review: {repo} {short_id} — {truncated change title}`.
///
/// Truncated on a character boundary and marked with an ellipsis, because a page
/// title is also a wikilink target — a title that changed length between runs
/// would leave the old link pointing at nothing.
pub fn review_page_title(repo: &str, short_id: &str, change_title: &str) -> String {
    let trimmed = change_title.trim();
    let title = if trimmed.chars().count() > TITLE_BUDGET {
        let kept: String = trimmed.chars().take(TITLE_BUDGET - 1).collect();
        format!("{kept}…")
    } else {
        trimmed.to_owned()
    };

    if title.is_empty() {
        format!("Review: {repo} {short_id}")
    } else {
        format!("Review: {repo} {short_id} — {title}")
    }
}

/// §11.5's parent page for a repository's reviews.
pub fn parent_page_title(repo: &str) -> String {
    format!("Code Reviews / {repo}")
}

/// §11.5's per-repo rolling index page.
pub fn index_page_title(repo: &str) -> String {
    format!("{repo} Review Index")
}

/// Wrap rev-local's content in its markers.
pub fn marked_section(body: &str) -> String {
    format!("{MARKER_BEGIN}\n{}\n{MARKER_END}", body.trim_end())
}

/// Merge rev-local's section into whatever is already on the page.
///
/// Returns the **whole document**. Callers must never send anything else:
/// `update_page` replaces the body.
pub fn merge_body(existing: Option<&str>, section: &str) -> String {
    let marked = marked_section(section);

    let Some(existing) = existing else {
        return marked;
    };

    let Some(start) = existing.find(MARKER_BEGIN) else {
        // No markers: this page was written by a person, or by a version of
        // rev-local that did not mark its work. Append rather than replace — the
        // whole point of reading first is not to destroy what we did not write.
        let separator = if existing.trim_end().is_empty() {
            ""
        } else {
            "\n\n"
        };
        return format!("{}{separator}{marked}", existing.trim_end());
    };

    // `find` from the marker's start so an END that appears before a BEGIN — a
    // page somebody edited badly — does not produce a reversed range.
    let after_start = start + MARKER_BEGIN.len();
    let end = existing[after_start..]
        .find(MARKER_END)
        .map_or(existing.len(), |offset| {
            after_start + offset + MARKER_END.len()
        });

    format!("{}{marked}{}", &existing[..start], &existing[end..])
}

/// What a person wrote on the page, with rev-local's section removed.
///
/// Exposed so a test can assert survival directly rather than by string search.
pub fn human_content(body: &str) -> String {
    let Some(start) = body.find(MARKER_BEGIN) else {
        return body.to_owned();
    };
    let after_start = start + MARKER_BEGIN.len();
    let end = body[after_start..]
        .find(MARKER_END)
        .map_or(body.len(), |offset| after_start + offset + MARKER_END.len());

    format!("{}{}", &body[..start], &body[end..])
}

/// Which page an upsert is about, and what goes in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PagePayload {
    /// The Trama space key.
    pub space: String,
    /// The page title.
    pub title: String,
    /// The parent page, for placement when the page is created.
    pub parent: Option<String>,
    /// rev-local's section, unmarked — the target adds the markers.
    pub section: String,
    /// Whether to publish it (§11.5: high risk, so opt-in).
    pub publish: bool,
}

/// The Trama operations this target needs.
///
/// Tool names are resolved by RL-604's mapper; this port is the shape those tools
/// take, not their names.
#[async_trait]
pub trait TramaWriter: Send + Sync {
    /// The page's current markdown, or `None` if there is no such page.
    async fn get_page(&self, space: &str, title: &str) -> Result<Option<String>, PublishError>;

    /// Create a page. Returns its URL or id.
    async fn create_page(
        &self,
        space: &str,
        title: &str,
        parent: Option<&str>,
        markdown: &str,
    ) -> Result<String, PublishError>;

    /// Replace a page's body with `markdown`.
    async fn update_page(
        &self,
        space: &str,
        title: &str,
        markdown: &str,
    ) -> Result<String, PublishError>;

    /// Publish a page (§11.5: only when `trama_publish` is set).
    async fn publish_page(&self, space: &str, title: &str) -> Result<(), PublishError>;
}

/// The Trama publish target.
#[derive(Debug)]
pub struct TramaTarget<W: TramaWriter> {
    writer: W,
}

impl<W: TramaWriter> TramaTarget<W> {
    /// A target over `writer`.
    pub const fn new(writer: W) -> Self {
        Self { writer }
    }

    /// Upsert one page, reading before writing.
    pub async fn upsert(&self, payload: &PagePayload) -> Result<String, PublishError> {
        // The read is not an optimisation and must not be skipped when the page is
        // "probably new": `update_page` replaces the body, and being wrong about
        // that costs somebody their page.
        let existing = self.writer.get_page(&payload.space, &payload.title).await?;

        let reference = match existing {
            Some(body) => {
                let merged = merge_body(Some(&body), &payload.section);
                self.writer
                    .update_page(&payload.space, &payload.title, &merged)
                    .await?
            }
            None => {
                let merged = merge_body(None, &payload.section);
                match self
                    .writer
                    .create_page(
                        &payload.space,
                        &payload.title,
                        payload.parent.as_deref(),
                        &merged,
                    )
                    .await
                {
                    Ok(reference) => reference,
                    // A title collision means the page appeared between the read
                    // and the create — another run, or a person. Reading again and
                    // updating is correct; retrying the create would fail forever.
                    Err(PublishError::Rejected { detail, .. }) if is_collision(&detail) => {
                        let body = self.writer.get_page(&payload.space, &payload.title).await?;
                        let merged = merge_body(body.as_deref(), &payload.section);
                        self.writer
                            .update_page(&payload.space, &payload.title, &merged)
                            .await?
                    }
                    Err(other) => return Err(other),
                }
            }
        };

        if payload.publish {
            self.writer
                .publish_page(&payload.space, &payload.title)
                .await?;
        }

        Ok(reference)
    }
}

/// Whether a rejection means the page already exists.
fn is_collision(detail: &str) -> bool {
    let lower = detail.to_lowercase();
    lower.contains("already exists") || lower.contains("duplicate title")
}

#[async_trait]
impl<W: TramaWriter> PublishTarget for TramaTarget<W> {
    fn id(&self) -> &str {
        "trama"
    }

    async fn discover(&self) -> Result<CapabilitySet, PublishError> {
        Ok(CapabilitySet::new([
            Capability::UpsertDoc,
            Capability::Comment,
            Capability::LinkDocToIssue,
        ]))
    }

    async fn execute(&self, action: &PublishAction) -> Result<PublishReceipt, PublishError> {
        let payload: PagePayload =
            serde_json::from_str(&action.payload_json).map_err(|e| PublishError::Rejected {
                target: "trama".to_owned(),
                status: None,
                detail: format!("the stored payload is not a page: {e}"),
            })?;

        let reference = self.upsert(&payload).await?;

        Ok(PublishReceipt {
            external_ref: Some(reference),
            response_json: None,
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

// --- speaking to a real Trama over MCP -------------------------------------

use std::sync::Arc;

use revlocal_mcp::{McpClient, SecretResolver};
use tokio::sync::Mutex;

/// The tool names this target calls.
///
/// Defaults are §11.5's list, which is what the server exposes today. They are a
/// struct rather than constants because RL-604's mapper resolves the real names
/// from `tools/list` — a server that calls the operation something else is exactly
/// the case §11.2 exists for, and hardcoding would undo that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TramaToolNames {
    /// Read a page.
    pub get_page: String,
    /// Create a page.
    pub create_page: String,
    /// Replace a page's body.
    pub update_page: String,
    /// Publish a page.
    pub publish_page: String,
}

impl Default for TramaToolNames {
    fn default() -> Self {
        Self {
            get_page: "get_page".to_owned(),
            create_page: "create_page".to_owned(),
            update_page: "update_page".to_owned(),
            publish_page: "publish_page".to_owned(),
        }
    }
}

/// A [`TramaWriter`] backed by an MCP server.
pub struct McpTramaWriter {
    client: Mutex<McpClient>,
    resolver: Arc<dyn SecretResolver>,
    tools: TramaToolNames,
}

impl std::fmt::Debug for McpTramaWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpTramaWriter")
            .field("tools", &self.tools)
            .finish_non_exhaustive()
    }
}

impl McpTramaWriter {
    /// A writer over `client`.
    pub fn new(
        client: McpClient,
        resolver: Arc<dyn SecretResolver>,
        tools: TramaToolNames,
    ) -> Self {
        Self {
            client: Mutex::new(client),
            resolver,
            tools,
        }
    }

    /// Call one tool and return its text.
    async fn call(
        &self,
        tool: &str,
        args: serde_json::Value,
    ) -> Result<Option<String>, PublishError> {
        let mut client = self.client.lock().await;
        let result = client
            .call_tool(tool, args, self.resolver.as_ref())
            .await
            .map_err(|error| PublishError::Transport {
                target: "trama".to_owned(),
                detail: error.to_string(),
            })?;

        if result.is_error {
            // A tool that ran and refused is the server's answer, not a transport
            // problem — §11.5's read-before-write refusal is exactly this, and
            // retrying it without reading first would refuse again.
            return Err(PublishError::Rejected {
                target: "trama".to_owned(),
                status: None,
                detail: result.text(),
            });
        }

        Ok(Some(result.text()))
    }
}

#[async_trait]
impl TramaWriter for McpTramaWriter {
    async fn get_page(&self, space: &str, title: &str) -> Result<Option<String>, PublishError> {
        match self
            .call(
                &self.tools.get_page,
                serde_json::json!({ "space": space, "title": title }),
            )
            .await
        {
            Ok(body) => Ok(body),
            // A page that is not there is an answer, not a failure. Anything else
            // propagates.
            Err(PublishError::Rejected { detail, .. }) if is_missing_page(&detail) => Ok(None),
            Err(other) => Err(other),
        }
    }

    async fn create_page(
        &self,
        space: &str,
        title: &str,
        parent: Option<&str>,
        markdown: &str,
    ) -> Result<String, PublishError> {
        let mut args = serde_json::json!({
            "space": space,
            "title": title,
            "markdown": markdown,
        });
        if let Some(parent) = parent {
            args["parent"] = serde_json::json!(parent);
        }

        self.call(&self.tools.create_page, args)
            .await
            .map(|text| text.unwrap_or_else(|| title.to_owned()))
    }

    async fn update_page(
        &self,
        space: &str,
        title: &str,
        markdown: &str,
    ) -> Result<String, PublishError> {
        self.call(
            &self.tools.update_page,
            serde_json::json!({ "space": space, "title": title, "markdown": markdown }),
        )
        .await
        .map(|text| text.unwrap_or_else(|| title.to_owned()))
    }

    async fn publish_page(&self, space: &str, title: &str) -> Result<(), PublishError> {
        self.call(
            &self.tools.publish_page,
            serde_json::json!({ "space": space, "title": title }),
        )
        .await
        .map(|_| ())
    }
}

/// Whether a refusal means the page does not exist.
fn is_missing_page(detail: &str) -> bool {
    let lower = detail.to_lowercase();
    lower.contains("no such page") || lower.contains("not found")
}
