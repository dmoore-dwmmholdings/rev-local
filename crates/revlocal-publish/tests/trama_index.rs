//! The rolling review index and publish gating (RL-708 and RL-709, SPEC §11.5).
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use revlocal_core::{ActionIntent, RiskClass};
use revlocal_publish::{
    index_backlink, index_page_title, merge_body, render_index, review_page_section,
    review_page_title, IndexEntry, PagePayload, PublishError, TramaTarget, TramaWriter,
    DEFAULT_INDEX_LIMIT,
};

fn entry(n: usize) -> IndexEntry {
    IndexEntry {
        page_title: review_page_title("rev-local", &format!("sha{n}"), &format!("change {n}")),
        change_ref: Some(format!("https://github.com/acme/widgets/pull/{n}")),
        verdict: "comment".to_owned(),
        findings: n % 3,
        reviewed_at: format!("2026-08-28 12:{n:02}"),
    }
}

fn entries(count: usize) -> Vec<IndexEntry> {
    (0..count).map(entry).collect()
}

// --- criterion 2: the cap is applied and stated ---------------------------

#[test]
fn trama_index_applies_the_cap_and_says_so_on_the_page() {
    let body = render_index("rev-local", &entries(120), DEFAULT_INDEX_LIMIT);

    let rows = body
        .lines()
        .filter(|line| line.contains("[[Review:"))
        .count();
    assert_eq!(rows, DEFAULT_INDEX_LIMIT, "the cap is applied");

    assert!(
        body.contains("Showing the 50 most recent reviews (50 of 120 recorded)"),
        "§18: an index silently showing 50 of 120 reads as `there have been 50 \
         reviews`:\n{body}"
    );
}

#[test]
fn trama_index_below_the_cap_reports_the_real_count() {
    let body = render_index("rev-local", &entries(3), DEFAULT_INDEX_LIMIT);
    assert!(body.contains("(3 of 3 recorded)"), "{body}");
    assert_eq!(body.lines().filter(|l| l.contains("[[Review:")).count(), 3);
}

#[test]
fn trama_index_with_no_reviews_says_so_rather_than_rendering_an_empty_table() {
    let body = render_index("rev-local", &[], DEFAULT_INDEX_LIMIT);
    assert!(body.contains("No reviews yet."), "{body}");
    assert!(
        !body.contains("|---|"),
        "a header with no rows reads as a broken page:\n{body}"
    );
}

// --- criterion 1: the index is a projection, so it restores completely ----

#[test]
fn trama_index_is_regenerated_whole_so_a_deleted_page_comes_back_complete() {
    let rows = entries(10);
    let fresh = render_index("rev-local", &rows, DEFAULT_INDEX_LIMIT);

    // The page was deleted: there is nothing to merge into.
    let after_deletion = merge_body(None, &fresh);

    // The page still holds last run's index, which listed a review that is no
    // longer in the most-recent set.
    let stale_row = IndexEntry {
        page_title: "Review: rev-local ancient — long ago".to_owned(),
        ..entry(0)
    };
    let previous = merge_body(
        None,
        &render_index(
            "rev-local",
            std::slice::from_ref(&stale_row),
            DEFAULT_INDEX_LIMIT,
        ),
    );
    let after_rerun = merge_body(Some(&previous), &fresh);

    for body in [&after_deletion, &after_rerun] {
        for row in &rows {
            assert!(
                body.contains(&format!("[[{}]]", row.page_title)),
                "every recorded review must be present whatever the page contained \
                 before: {} missing",
                row.page_title
            );
        }
    }

    assert!(
        !after_rerun.contains(&stale_row.page_title),
        "a review the database no longer lists must be gone from the projection — \
         the index is regenerated, not appended to:\n{after_rerun}"
    );
}

#[test]
fn trama_index_does_not_depend_on_what_was_on_the_page() {
    let rows = entries(5);
    let section = render_index("rev-local", &rows, DEFAULT_INDEX_LIMIT);

    let from_nothing = merge_body(None, &section);
    let from_stale = merge_body(
        Some(&merge_body(
            None,
            &render_index("rev-local", &entries(99), 50),
        )),
        &section,
    );

    assert_eq!(
        from_nothing, from_stale,
        "SQLite is the source of truth and Trama is a projection of it, so the \
         result cannot depend on what the projection happened to hold"
    );
}

// --- criterion 3: the links go both ways ----------------------------------

#[test]
fn trama_index_links_to_every_review_and_reviews_link_back() {
    let rows = entries(3);
    let index = render_index("rev-local", &rows, DEFAULT_INDEX_LIMIT);
    for row in &rows {
        assert!(index.contains(&format!("[[{}]]", row.page_title)));
    }

    let review = review_page_section("rev-local", "## Findings\n\nNone.");
    assert!(
        review.contains(&index_backlink("rev-local")),
        "§11.5: each review page links [[{{repo}} Review Index]]:\n{review}"
    );
    assert_eq!(index_backlink("rev-local"), "[[rev-local Review Index]]");
    assert_eq!(index_page_title("rev-local"), "rev-local Review Index");
}

// --- RL-709: publish gating and cross-linking -----------------------------

#[test]
fn trama_publish_gate_a_draft_is_low_risk_and_publishing_is_high_risk() {
    assert_eq!(
        ActionIntent::UpsertDoc { published: false }.baseline_risk(),
        RiskClass::Low,
        "§11.5: an unpublished draft is a low-risk action"
    );
    assert_eq!(
        ActionIntent::UpsertDoc { published: true }.baseline_risk(),
        RiskClass::High,
        "and publishing is high-risk — it is the step that makes a page something \
         other people are expected to rely on"
    );
}

#[derive(Default)]
struct FakeTrama {
    calls: Arc<Mutex<Vec<String>>>,
    fail_link: bool,
}

#[async_trait]
impl TramaWriter for FakeTrama {
    async fn get_page(&self, _space: &str, _title: &str) -> Result<Option<String>, PublishError> {
        self.record("get")?;
        Ok(None)
    }

    async fn create_page(
        &self,
        _space: &str,
        title: &str,
        _parent: Option<&str>,
        _markdown: &str,
    ) -> Result<String, PublishError> {
        self.record("create")?;
        Ok(format!("https://trama.example.com/eng/{title}"))
    }

    async fn update_page(
        &self,
        _space: &str,
        title: &str,
        _markdown: &str,
    ) -> Result<String, PublishError> {
        self.record("update")?;
        Ok(format!("https://trama.example.com/eng/{title}"))
    }

    async fn publish_page(&self, _space: &str, _title: &str) -> Result<(), PublishError> {
        self.record("publish")?;
        Ok(())
    }

    async fn link_to_issue(
        &self,
        _space: &str,
        _title: &str,
        issue_key: &str,
    ) -> Result<(), PublishError> {
        self.record(&format!("link:{issue_key}"))?;
        if self.fail_link {
            return Err(PublishError::Transport {
                target: "trama".to_owned(),
                detail: "the link service is down".to_owned(),
            });
        }
        Ok(())
    }
}

impl FakeTrama {
    fn record(&self, what: &str) -> Result<(), PublishError> {
        self.calls
            .lock()
            .map_err(|e| PublishError::Transport {
                target: "trama".to_owned(),
                detail: e.to_string(),
            })?
            .push(what.to_owned());
        Ok(())
    }
}

fn a_page(publish: bool, issue_key: Option<&str>) -> PagePayload {
    PagePayload {
        space: "ENG".to_owned(),
        title: "Review: rev-local abc123 — a change".to_owned(),
        parent: None,
        section: "## Findings".to_owned(),
        publish,
        issue_key: issue_key.map(str::to_owned),
    }
}

#[tokio::test]
async fn trama_publish_gate_links_only_when_an_issue_was_actually_filed() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let target = TramaTarget::new(FakeTrama {
        calls: Arc::clone(&calls),
        fail_link: false,
    });

    target
        .upsert(&a_page(false, None))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(
        !calls
            .lock()
            .unwrap_or_else(|e| panic!("{e}"))
            .iter()
            .any(|c| c.starts_with("link:")),
        "no issue was filed, so there is no key — and a guessed key links the \
         review to somebody else's ticket with no way for a reader to tell"
    );

    target
        .upsert(&a_page(false, Some("REVL-42")))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(
        calls
            .lock()
            .unwrap_or_else(|e| panic!("{e}"))
            .contains(&"link:REVL-42".to_owned()),
        "the key comes from Andare's receipt, verbatim"
    );
}

#[tokio::test]
async fn trama_publish_gate_a_failed_cross_link_does_not_fail_the_run() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let target = TramaTarget::new(FakeTrama {
        calls: Arc::clone(&calls),
        fail_link: true,
    });

    target
        .upsert(&a_page(true, Some("REVL-42")))
        .await
        .unwrap_or_else(|e| {
            panic!(
                "the page exists and the issue exists; a link that did not attach \
                 costs a click, and failing here throws away work that succeeded: {e}"
            )
        });

    let log = calls.lock().unwrap_or_else(|e| panic!("{e}")).clone();
    assert!(log.contains(&"publish".to_owned()), "{log:?}");
    assert!(log.contains(&"link:REVL-42".to_owned()), "{log:?}");
}

#[tokio::test]
async fn trama_publish_gate_publishing_happens_after_the_page_exists() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let target = TramaTarget::new(FakeTrama {
        calls: Arc::clone(&calls),
        fail_link: false,
    });

    target
        .upsert(&a_page(true, None))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    let log = calls.lock().unwrap_or_else(|e| panic!("{e}")).clone();
    assert_eq!(
        log,
        vec!["get".to_owned(), "create".to_owned(), "publish".to_owned()],
        "publishing a page that does not exist yet is not a thing"
    );
}
