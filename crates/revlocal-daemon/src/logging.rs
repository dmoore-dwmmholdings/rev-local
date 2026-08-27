//! Structured logging with secret redaction (SPEC §18).
//!
//! A JSON layer writes to `{data_dir}/logs/`, and **every field passes through
//! [`revlocal_core::redact`] before it reaches a sink**.
//!
//! The redaction is a `tracing` *field visitor*, not a post-processing pass over
//! the finished line. That placement is the design: a value is scrubbed as it is
//! recorded, so it is already redacted before any formatting, buffering or writing
//! happens. Scrubbing serialized output instead would mean the secret existed in
//! memory as part of a formatted line, and every additional sink would need the
//! same treatment or silently leak.
//!
//! Span fields are covered as well as event fields, which matters more than it
//! sounds: a span is recorded once and then attached to every event inside it, so
//! a secret in a span field leaks into *every line* of that span rather than one.
//!
//! This crate emits the JSON itself rather than wrapping
//! `tracing_subscriber::fmt`. That layer's field formatting cannot be intercepted
//! — `RecordFields` is a sealed trait — so wrapping it would have forced redaction
//! back to the serialized-text position this module exists to avoid.

use std::fmt;
use std::io::Write as _;
use std::path::Path;

use revlocal_core::redact_field;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Record};
use tracing::{Event, Id, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt as _};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{EnvFilter, Layer};

/// Anything that can go wrong setting logging up.
#[derive(Debug, thiserror::Error)]
pub enum LoggingError {
    /// The log directory could not be created.
    #[error("could not create the log directory {path}: {source}")]
    LogDir {
        /// The directory that could not be created.
        path: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },

    /// A global subscriber was already installed.
    #[error("logging was already initialised: {0}")]
    AlreadyInitialised(String),
}

/// Collects a set of `tracing` fields into JSON, redacting as it goes.
///
/// Implements the string and `Debug` halves of the field API separately, because
/// most values reach a subscriber through `Debug` and a secret formatted through
/// `Debug` is still a secret. Numbers and booleans pass through as JSON numbers
/// and booleans — they cannot carry a credential, and stringifying them would make
/// the log harder to query for no benefit.
#[derive(Debug, Default)]
pub struct RedactingVisitor {
    fields: serde_json::Map<String, serde_json::Value>,
}

impl RedactingVisitor {
    /// A visitor with nothing recorded yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// The fields recorded so far, already redacted.
    pub fn into_fields(self) -> serde_json::Map<String, serde_json::Value> {
        self.fields
    }

    /// Merge another visitor's fields into this one.
    fn extend_from(&mut self, other: &serde_json::Map<String, serde_json::Value>) {
        for (key, value) in other {
            self.fields.insert(key.clone(), value.clone());
        }
    }

    fn insert_redacted(&mut self, field: &Field, value: &str) {
        let cleaned = redact_field(field.name(), value);
        self.fields.insert(
            field.name().to_owned(),
            serde_json::Value::String(cleaned.into_owned()),
        );
    }
}

impl Visit for RedactingVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.insert_redacted(field, value);
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.insert_redacted(field, &format!("{value:?}"));
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        // An error's Display very often quotes the request that failed, headers
        // included. This is a realistic leak, not a theoretical one.
        self.insert_redacted(field, &value.to_string());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields.insert(field.name().to_owned(), value.into());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields.insert(field.name().to_owned(), value.into());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields.insert(field.name().to_owned(), value.into());
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.fields.insert(field.name().to_owned(), value.into());
    }
}

/// A span's redacted fields, stored so every event inside the span can carry them.
#[derive(Debug)]
struct SpanFields(serde_json::Map<String, serde_json::Value>);

/// A JSON logging layer that redacts every field as it is recorded.
pub struct RedactingJsonLayer<W> {
    writer: W,
}

impl<W> RedactingJsonLayer<W> {
    /// Build a layer writing to `writer`.
    pub const fn new(writer: W) -> Self {
        Self { writer }
    }
}

impl<S, W> Layer<S> for RedactingJsonLayer<W>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    W: for<'a> tracing_subscriber::fmt::MakeWriter<'a> + 'static,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut visitor = RedactingVisitor::new();
        attrs.record(&mut visitor);
        if let Some(span) = ctx.span(id) {
            span.extensions_mut()
                .insert(SpanFields(visitor.into_fields()));
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        // `Span::record` after creation must be redacted too, or a field set later
        // — which is the normal way a run_id or a token is attached — would bypass
        // everything above.
        let mut visitor = RedactingVisitor::new();
        values.record(&mut visitor);
        if let Some(span) = ctx.span(id) {
            let mut extensions = span.extensions_mut();
            if let Some(existing) = extensions.get_mut::<SpanFields>() {
                for (key, value) in visitor.into_fields() {
                    existing.0.insert(key, value);
                }
            } else {
                extensions.insert(SpanFields(visitor.into_fields()));
            }
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let mut visitor = RedactingVisitor::new();
        event.record(&mut visitor);

        let mut spans = Vec::new();
        if let Some(scope) = ctx.event_scope(event) {
            // Outermost first, so the JSON reads the way the nesting does.
            for span in scope.from_root() {
                let mut entry = serde_json::Map::new();
                entry.insert("name".to_owned(), span.name().into());
                if let Some(fields) = span.extensions().get::<SpanFields>() {
                    let mut redacted = RedactingVisitor::new();
                    redacted.extend_from(&fields.0);
                    entry.insert(
                        "fields".to_owned(),
                        serde_json::Value::Object(redacted.into_fields()),
                    );
                }
                spans.push(serde_json::Value::Object(entry));
            }
        }

        let metadata = event.metadata();
        let record = serde_json::json!({
            "timestamp": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "level": metadata.level().as_str(),
            "target": metadata.target(),
            "fields": serde_json::Value::Object(visitor.into_fields()),
            "spans": spans,
        });

        // A logging failure must never take the process down, and must not recurse
        // into tracing to report itself.
        if let Ok(mut line) = serde_json::to_vec(&record) {
            line.push(b'\n');
            let mut writer = self.writer.make_writer();
            let _ = writer.write_all(&line);
            let _ = writer.flush();
        }
    }
}

/// A handle that keeps the non-blocking writer alive.
///
/// Dropping it stops the background flush, so callers must hold it for as long as
/// they want logs. Returned rather than leaked so a test can flush deterministically.
#[must_use = "dropping this stops the log writer flushing"]
pub struct LoggingHandle {
    _guard: tracing_appender::non_blocking::WorkerGuard,
}

/// Initialise logging into `{data_dir}/logs/`.
///
/// The filter comes from `RUST_LOG`, defaulting to `info`.
pub fn init(data_dir: &Path) -> Result<LoggingHandle, LoggingError> {
    let log_dir = data_dir.join("logs");
    std::fs::create_dir_all(&log_dir).map_err(|source| LoggingError::LogDir {
        path: log_dir.display().to_string(),
        source,
    })?;

    let appender = tracing_appender::rolling::daily(&log_dir, "revlocal.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(RedactingJsonLayer::new(writer))
        .try_init()
        .map_err(|e| LoggingError::AlreadyInitialised(e.to_string()))?;

    Ok(LoggingHandle { _guard: guard })
}
