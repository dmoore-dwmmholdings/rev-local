//! Read-before-write page upsert (RL-707, SPEC §11.5).
//!
//! Criterion 1 is about the ORDER of two calls, which no return value can show, so
//! it runs against the real mock MCP server and reads its request journal. That is
//! what RL-204 built the journal for.
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use revlocal_mcp::{NoSecrets, ServerCommand, StdioClient};
use revlocal_publish::{
    human_content, index_page_title, merge_body, parent_page_title, review_page_title,
    McpTramaWriter, PagePayload, PublishError, TramaTarget, TramaToolNames, TramaWriter,
    MARKER_BEGIN, MARKER_END,
};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn node_is_installed() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn mock_server(journal: &std::path::Path) -> ServerCommand {
    let script = workspace_root().join("fixtures/mock-mcp/server.js");
    let mut server = ServerCommand::new("trama", "node", &[&script.display().to_string()]);
    server
        .env
        .insert("MOCK_MCP_JOURNAL".to_owned(), journal.display().to_string());
    server
}

/// The journal, one entry per line, in order.
fn journal_kinds(path: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .map(|entry| {
            let kind = entry
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned();
            match entry.get("tool").and_then(serde_json::Value::as_str) {
                Some(tool) => format!("{kind}:{tool}"),
                None => kind,
            }
        })
        .collect()
}

// --- criterion 1 and 2: read before write, and send the whole body --------

#[tokio::test]
async fn trama_upsert_every_update_is_immediately_preceded_by_a_get_for_the_same_page() {
    if !node_is_installed() {
        println!("SKIPPED (node not installed, nothing verified): trama_upsert_every_update...");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let journal = dir.path().join("journal.jsonl");

    let client = StdioClient::new(mock_server(&journal));
    let writer = McpTramaWriter::new(
        client.into(),
        Arc::new(NoSecrets),
        TramaToolNames::default(),
    );
    let target = TramaTarget::new(writer);

    target
        .upsert(&PagePayload {
            space: "ENG".to_owned(),
            title: "Review: rev-local abc123 — a change".to_owned(),
            parent: Some(parent_page_title("rev-local")),
            section: "## Findings\n\nNone.".to_owned(),
            publish: false,
        })
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    let kinds = journal_kinds(&journal);
    let tool_calls: Vec<&String> = kinds
        .iter()
        .filter(|kind| kind.starts_with("tools/call:"))
        .collect();

    assert_eq!(
        tool_calls,
        vec![
            &"tools/call:get_page".to_owned(),
            &"tools/call:update_page".to_owned()
        ],
        "§11.5: update_page REPLACES the body, so the read is not optional and not \
         an optimisation. Journal was: {kinds:?}"
    );

    assert!(
        !kinds
            .iter()
            .any(|k| k.contains("read_before_write_violation")),
        "the mock refuses a blind write, and it did not have to: {kinds:?}"
    );
}

// --- criterion 4: a hand-edited page survives ----------------------------

#[test]
fn trama_upsert_human_authored_sections_survive_an_update() {
    let hand_written = format!(
        "# Runbook\n\nCall the on-call engineer first.\n\n{MARKER_BEGIN}\nold findings\n{MARKER_END}\n\n## Notes\n\nDo not delete this page.\n"
    );

    let merged = merge_body(Some(&hand_written), "## Findings\n\nOne new finding.");

    assert!(
        merged.contains("Call the on-call engineer first."),
        "the section above rev-local's markers must survive:\n{merged}"
    );
    assert!(
        merged.contains("Do not delete this page."),
        "and so must the section below:\n{merged}"
    );
    assert!(merged.contains("One new finding."));
    assert!(
        !merged.contains("old findings"),
        "rev-local's own section is replaced, not appended to:\n{merged}"
    );

    // Stated as an invariant rather than by string search: what a person wrote is
    // byte-identical before and after.
    assert_eq!(
        human_content(&merged),
        human_content(&hand_written),
        "everything outside the markers is untouched"
    );
}

#[test]
fn trama_upsert_an_unmarked_page_is_appended_to_not_replaced() {
    let theirs = "# Someone else's page\n\nImportant content.";
    let merged = merge_body(Some(theirs), "## Findings");

    assert!(
        merged.contains("Important content."),
        "an unmarked page is not assumed to be ours to overwrite — treating it as \
         ours is exactly the clobber this design prevents:\n{merged}"
    );
    assert!(merged.starts_with("# Someone else's page"));
    assert!(merged.contains(MARKER_BEGIN));
}

#[test]
fn trama_upsert_a_new_page_is_just_the_marked_section() {
    let merged = merge_body(None, "## Findings");
    assert!(merged.starts_with(MARKER_BEGIN));
    assert!(merged.trim_end().ends_with(MARKER_END));
}

#[test]
fn trama_upsert_a_page_with_a_begin_and_no_end_does_not_produce_a_reversed_range() {
    let broken = format!("intro\n\n{MARKER_BEGIN}\nsomebody deleted the end marker");
    let merged = merge_body(Some(&broken), "## Findings");

    assert!(merged.contains("intro"));
    assert!(merged.contains("## Findings"));
    assert!(!merged.contains("somebody deleted the end marker"));
}

// --- criterion 3: create only when the page is genuinely absent -----------

/// A Trama that starts empty and records every call.
#[derive(Default)]
struct FakeTrama {
    pages: Mutex<Vec<(String, String)>>,
    /// Shared out before the writer is moved into the target, so a test can read
    /// the call log afterwards.
    calls: Arc<Mutex<Vec<String>>>,
    /// Make the first create fail as a title collision, as if another run won.
    collide_once: Mutex<bool>,
}

#[async_trait]
impl TramaWriter for FakeTrama {
    async fn get_page(&self, _space: &str, title: &str) -> Result<Option<String>, PublishError> {
        self.calls
            .lock()
            .map_err(|e| transport(&e.to_string()))?
            .push(format!("get:{title}"));
        Ok(self
            .pages
            .lock()
            .map_err(|e| transport(&e.to_string()))?
            .iter()
            .find(|(t, _)| t == title)
            .map(|(_, body)| body.clone()))
    }

    async fn create_page(
        &self,
        _space: &str,
        title: &str,
        _parent: Option<&str>,
        markdown: &str,
    ) -> Result<String, PublishError> {
        self.calls
            .lock()
            .map_err(|e| transport(&e.to_string()))?
            .push(format!("create:{title}"));

        let mut collide = self
            .collide_once
            .lock()
            .map_err(|e| transport(&e.to_string()))?;
        if *collide {
            *collide = false;
            // Somebody created it between our read and our create.
            self.pages
                .lock()
                .map_err(|e| transport(&e.to_string()))?
                .push((title.to_owned(), "# theirs\n\nwritten first".to_owned()));
            return Err(PublishError::Rejected {
                target: "trama".to_owned(),
                status: Some(409),
                detail: "a page with that title already exists".to_owned(),
            });
        }

        self.pages
            .lock()
            .map_err(|e| transport(&e.to_string()))?
            .push((title.to_owned(), markdown.to_owned()));
        Ok(format!("https://trama.example.com/eng/{title}"))
    }

    async fn update_page(
        &self,
        _space: &str,
        title: &str,
        markdown: &str,
    ) -> Result<String, PublishError> {
        self.calls
            .lock()
            .map_err(|e| transport(&e.to_string()))?
            .push(format!("update:{title}"));
        let mut pages = self.pages.lock().map_err(|e| transport(&e.to_string()))?;
        if let Some(entry) = pages.iter_mut().find(|(t, _)| t == title) {
            entry.1 = markdown.to_owned();
        }
        Ok(format!("https://trama.example.com/eng/{title}"))
    }

    async fn publish_page(&self, _space: &str, title: &str) -> Result<(), PublishError> {
        self.calls
            .lock()
            .map_err(|e| transport(&e.to_string()))?
            .push(format!("publish:{title}"));
        Ok(())
    }
}

fn transport(detail: &str) -> PublishError {
    PublishError::Transport {
        target: "trama".to_owned(),
        detail: detail.to_owned(),
    }
}

fn a_page(publish: bool) -> PagePayload {
    PagePayload {
        space: "ENG".to_owned(),
        title: "Review: rev-local abc123 — a change".to_owned(),
        parent: Some(parent_page_title("rev-local")),
        section: "## Findings\n\nOne.".to_owned(),
        publish,
    }
}

#[tokio::test]
async fn trama_upsert_creates_only_when_the_page_is_absent() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let target = TramaTarget::new(FakeTrama {
        calls: Arc::clone(&calls),
        ..FakeTrama::default()
    });

    target
        .upsert(&a_page(false))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    target
        .upsert(&a_page(false))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    let log = calls.lock().unwrap_or_else(|e| panic!("{e}")).clone();
    let creates = log.iter().filter(|c| c.starts_with("create:")).count();
    let updates = log.iter().filter(|c| c.starts_with("update:")).count();

    assert_eq!(
        creates, 1,
        "the second upsert must not create again: {log:?}"
    );
    assert_eq!(updates, 1, "it updates instead: {log:?}");
    assert!(
        log.first().is_some_and(|c| c.starts_with("get:")),
        "and it reads first, both times: {log:?}"
    );
}

#[tokio::test]
async fn trama_upsert_a_title_collision_falls_back_to_read_and_update() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let target = TramaTarget::new(FakeTrama {
        calls: Arc::clone(&calls),
        collide_once: Mutex::new(true),
        ..FakeTrama::default()
    });

    target.upsert(&a_page(false)).await.unwrap_or_else(|e| {
        panic!("a page appearing between the read and the create is ordinary, not fatal: {e}")
    });

    let log = calls.lock().unwrap_or_else(|e| panic!("{e}")).clone();
    assert_eq!(
        log,
        vec![
            "get:Review: rev-local abc123 — a change".to_owned(),
            "create:Review: rev-local abc123 — a change".to_owned(),
            "get:Review: rev-local abc123 — a change".to_owned(),
            "update:Review: rev-local abc123 — a change".to_owned(),
        ],
        "the collision is answered by reading again and updating — retrying the \
         create would fail forever, and updating without re-reading would clobber \
         whatever the other writer just put there"
    );
}

#[tokio::test]
async fn trama_upsert_publishing_is_opt_in() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let target = TramaTarget::new(FakeTrama {
        calls: Arc::clone(&calls),
        ..FakeTrama::default()
    });

    target
        .upsert(&a_page(false))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(
        !calls
            .lock()
            .unwrap_or_else(|e| panic!("{e}"))
            .iter()
            .any(|c| c.starts_with("publish:")),
        "§11.5: an unpublished draft is low risk and publishing is high risk, so \
         publishing must not happen unless asked"
    );

    target
        .upsert(&a_page(true))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(
        calls
            .lock()
            .unwrap_or_else(|e| panic!("{e}"))
            .iter()
            .any(|c| c.starts_with("publish:")),
        "and it must happen when it is"
    );
}

// --- page identity ---------------------------------------------------------

#[test]
fn trama_upsert_the_page_title_follows_the_spec() {
    assert_eq!(
        review_page_title("rev-local", "abc123", "Fix the bounds check"),
        "Review: rev-local abc123 — Fix the bounds check"
    );
    assert_eq!(parent_page_title("rev-local"), "Code Reviews / rev-local");
    assert_eq!(index_page_title("rev-local"), "rev-local Review Index");
}

#[test]
fn trama_upsert_a_long_change_title_is_truncated_on_a_character_boundary() {
    let long = "é".repeat(200);
    let title = review_page_title("rev-local", "abc123", &long);

    assert!(title.ends_with('…'));
    assert!(
        title.chars().count() < 120,
        "a page title is a wikilink target, so it must be bounded: {}",
        title.chars().count()
    );
}

#[test]
fn trama_upsert_an_empty_change_title_does_not_leave_a_dangling_dash() {
    assert_eq!(
        review_page_title("rev-local", "abc123", "   "),
        "Review: rev-local abc123"
    );
}
