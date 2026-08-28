//! Acceptance tests for `RL-111` — a fake token pushed through `tracing` must not
//! reach the log file.
//!
//! These drive the real layer over a real file rather than asserting on the
//! redactor in isolation. SPEC §18 asks for exactly that: "a unit test that feeds
//! a fake token through the logger and asserts it does not appear in the output".
//! Testing `redact()` alone would pass even if the layer forgot to call it.

use revlocal_daemon::RedactingJsonLayer;
use std::sync::{Arc, Mutex};
use tracing::subscriber::with_default;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::Registry;

/// An in-memory sink standing in for the log file.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn text(&self) -> String {
        self.0
            .lock()
            .map(|buf| String::from_utf8_lossy(&buf).into_owned())
            .unwrap_or_default()
    }
}

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Ok(mut inner) = self.0.lock() {
            inner.extend_from_slice(buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Run `body` with the redacting layer installed, and return what was logged.
///
/// Returns the captured text rather than unwrapping inside; helpers are not
/// `#[test]` fns (ADR 0003).
fn capture(body: impl FnOnce()) -> String {
    let sink = Captured::default();
    let subscriber = Registry::default().with(RedactingJsonLayer::new(sink.clone()));
    with_default(subscriber, body);
    sink.text()
}

/// A token that is obviously fake but structurally real.
const FAKE_TOKEN: &str = "ghp_A1b2C3d4E5f6G7h8I9j0K1l2";

#[test]
fn a_fake_token_in_an_event_field_never_reaches_the_sink() {
    let logged = capture(|| {
        tracing::info!(github_token = FAKE_TOKEN, "authenticating");
    });

    assert!(
        !logged.contains(FAKE_TOKEN),
        "the token reached the log: {logged}"
    );
    assert!(
        logged.contains("authenticating"),
        "the message must survive: {logged}"
    );
    assert!(logged.contains("[redacted]"), "{logged}");
}

#[test]
fn a_fake_token_in_the_message_itself_never_reaches_the_sink() {
    // The realistic leak is not a field called `token`; it is a token interpolated
    // into a message or an error string.
    let logged = capture(|| {
        tracing::error!("clone failed: remote rejected {FAKE_TOKEN}");
    });

    assert!(!logged.contains("A1b2C3d4E5f6G7h8I9j0K1l2"), "{logged}");
    assert!(logged.contains("clone failed"), "{logged}");
    assert!(
        logged.contains("ghp_"),
        "the prefix survives so the right key is rotated: {logged}"
    );
}

#[test]
fn span_fields_are_redacted_as_well_as_event_fields() {
    // This matters more than event fields: a span's fields are attached to EVERY
    // event inside it, so one leaked span field leaks on every line of the span.
    let logged = capture(|| {
        let span = tracing::info_span!("publish", repo = "rev-local", api_token = FAKE_TOKEN);
        let _entered = span.enter();
        tracing::info!("posting review");
        tracing::info!("posting comment");
    });

    assert!(
        !logged.contains(FAKE_TOKEN),
        "a span field leaked: {logged}"
    );
    assert_eq!(
        logged.lines().count(),
        2,
        "both events should have been logged: {logged}"
    );
    assert!(
        logged.contains("rev-local"),
        "the span's non-sensitive fields must survive: {logged}"
    );
    for line in logged.lines() {
        assert!(
            line.contains("publish"),
            "each line must carry its span: {line}"
        );
    }
}

#[test]
fn a_field_recorded_after_the_span_was_created_is_still_redacted() {
    // `Span::record` is the normal way a value learned later gets attached, and it
    // takes a different code path from span creation.
    let logged = capture(|| {
        let span = tracing::info_span!("connect", secret = tracing::field::Empty);
        span.record("secret", FAKE_TOKEN);
        let _entered = span.enter();
        tracing::info!("connected");
    });

    assert!(
        !logged.contains(FAKE_TOKEN),
        "a late-recorded field leaked: {logged}"
    );
}

#[test]
fn a_token_reaching_the_log_through_debug_formatting_is_redacted() {
    // Most values reach a subscriber through Debug, and a secret formatted through
    // Debug is still a secret.
    #[derive(Debug)]
    #[allow(dead_code)]
    struct Header {
        authorization: String,
    }

    let logged = capture(|| {
        let header = Header {
            authorization: format!("Bearer {FAKE_TOKEN}"),
        };
        tracing::warn!(?header, "request failed");
    });

    assert!(!logged.contains(FAKE_TOKEN), "{logged}");
    assert!(logged.contains("request failed"), "{logged}");
}

#[test]
fn an_error_field_is_redacted_because_errors_quote_the_request_that_failed() {
    let logged = capture(|| {
        let failure: Box<dyn std::error::Error> =
            format!("401 for header Bearer {FAKE_TOKEN}").into();
        tracing::error!(error = failure.as_ref(), "publish failed");
    });

    assert!(!logged.contains(FAKE_TOKEN), "{logged}");
    assert!(logged.contains("publish failed"), "{logged}");
}

#[test]
fn ordinary_logging_is_unharmed_and_still_valid_json() {
    let logged = capture(|| {
        let span = tracing::info_span!("run", run_id = 42_i64, repo = "rev-local");
        let _entered = span.enter();
        tracing::info!(findings = 3_i64, blocking = false, "review complete");
    });

    let line = logged.lines().next().unwrap_or_default();
    let parsed: serde_json::Value =
        serde_json::from_str(line).unwrap_or_else(|e| panic!("not JSON: {e}\n{line}"));

    assert_eq!(parsed["level"], "INFO");
    assert_eq!(parsed["fields"]["message"], "review complete");
    assert_eq!(
        parsed["fields"]["findings"], 3,
        "a numeric field must stay a JSON number, not become a string"
    );
    assert_eq!(parsed["fields"]["blocking"], false);
    assert_eq!(parsed["spans"][0]["name"], "run");
    assert_eq!(parsed["spans"][0]["fields"]["run_id"], 42);
    assert!(parsed["timestamp"].is_string());
}

#[test]
fn nested_spans_are_all_carried_outermost_first() {
    let logged = capture(|| {
        let outer = tracing::info_span!("run", run_id = 1_i64);
        let _outer = outer.enter();
        let inner = tracing::info_span!("publish", target = "andare");
        let _inner = inner.enter();
        tracing::info!("sending");
    });

    let line = logged.lines().next().unwrap_or_default();
    let parsed: serde_json::Value =
        serde_json::from_str(line).unwrap_or_else(|e| panic!("not JSON: {e}\n{line}"));
    assert_eq!(parsed["spans"][0]["name"], "run");
    assert_eq!(parsed["spans"][1]["name"], "publish");
}

#[test]
fn the_log_file_on_disk_contains_no_token() {
    // The layer tests above use an in-memory sink. This one is the criterion as
    // written: through the real initialiser, to a real file, read back off disk.
    let dir = tempfile::TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));

    // init() installs a *global* subscriber, which can only happen once per
    // process, so this drives the same layer over a real file writer instead of
    // fighting the other tests for that slot.
    let log_dir = dir.path().join("logs");
    std::fs::create_dir_all(&log_dir).unwrap_or_else(|e| panic!("mkdir: {e}"));
    let appender = tracing_appender::rolling::never(&log_dir, "revlocal.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);

    let subscriber = Registry::default().with(RedactingJsonLayer::new(writer));
    with_default(subscriber, || {
        let span = tracing::info_span!("publish", token = FAKE_TOKEN);
        let _entered = span.enter();
        tracing::info!("posting to {FAKE_TOKEN}");
    });
    drop(guard); // flush the non-blocking writer

    let path = log_dir.join("revlocal.log");
    let contents = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));

    assert!(
        !contents.is_empty(),
        "nothing was written to {}",
        path.display()
    );
    assert!(
        !contents.contains("A1b2C3d4E5f6G7h8I9j0K1l2"),
        "the token reached disk at {}:\n{contents}",
        path.display()
    );
    assert!(contents.contains("[redacted]"), "{contents}");
}
