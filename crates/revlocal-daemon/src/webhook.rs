//! The GitHub webhook listener (RL-1005, SPEC §7.3).
//!
//! # Two orderings that are the whole security of this module
//!
//! **Verify before parsing.** The body is taken as bytes, the signature is checked
//! against those exact bytes, and only then is any JSON parsed. Parsing first
//! would run a deserialiser over unauthenticated attacker-controlled input, and
//! would also mean the signature was verified against a re-serialisation rather
//! than what GitHub actually signed — which fails on any whitespace GitHub chose
//! differently from serde.
//!
//! **Check for replays after verifying, never before.** A replay cache keyed on
//! `X-GitHub-Delivery` is a cache an unauthenticated caller could otherwise
//! poison: POST a few thousand made-up delivery ids, and the genuine deliveries
//! carrying those ids are silently dropped. Only a delivery that has proved it
//! came from GitHub is allowed to consume an entry.
//!
//! # Off by default, twice
//!
//! §7.3: the listener is off by default and requires explicit opt-in **per repo**.
//! Those are two separate switches and both are load-bearing. `webhook_port = 0`
//! means nothing binds at all — a machine that never configured webhooks is not
//! running a listener. A repository that has not opted in is rejected even when
//! the port is open, so adding one repository does not quietly expose every other
//! repository on the machine to whoever can reach the tunnel.
//!
//! # A rejected delivery is audited
//!
//! A bad signature is the one event here worth waking somebody for. Either a
//! secret has drifted — in which case reviews have silently stopped happening —
//! or somebody is probing the endpoint. Both are things an operator should be able
//! to find in the audit log rather than infer from an absence of reviews.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use hmac::{Hmac, Mac};
use revlocal_core::{RepoId, Timestamp, TriggerSource};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::triggers::{TriggerBus, TriggerEvent};

/// The route §7.3 describes.
pub const WEBHOOK_PATH: &str = "/webhook";

/// GitHub's signature header.
pub const SIGNATURE_HEADER: &str = "x-hub-signature-256";

/// GitHub's event-type header.
pub const EVENT_HEADER: &str = "x-github-event";

/// GitHub's per-delivery id header.
pub const DELIVERY_HEADER: &str = "x-github-delivery";

/// How many delivery ids are remembered for replay detection.
///
/// Bounded because the cache is fed by a network endpoint: an unbounded set is a
/// memory exhaustion bug wearing a correctness hat. A few thousand covers any
/// plausible redelivery window — GitHub retries within minutes, not days.
pub const DELIVERY_MEMORY: usize = 4_096;

/// The audit event for a rejected delivery.
pub const AUDIT_KIND_WEBHOOK_REJECTED: &str = "webhook_rejected";

/// Which `pull_request` actions §7.3 handles.
pub const HANDLED_PR_ACTIONS: &[&str] = &["opened", "synchronize", "reopened", "ready_for_review"];

/// Why a delivery was not turned into a trigger.
///
/// Named rather than collapsed to a status code: §18, and "we received a webhook
/// and did nothing" has four very different meanings for an operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// No listener is configured at all.
    ListenerDisabled,
    /// The repository has not opted in.
    RepoNotOptedIn,
    /// The signature was absent, malformed, or wrong.
    BadSignature,
    /// This delivery id has already been handled.
    Replay,
    /// An event type or action rev-local does not act on.
    NotHandled,
}

impl Rejection {
    /// What to record and, for the safe ones, what to say.
    pub const fn detail(self) -> &'static str {
        match self {
            Self::ListenerDisabled => "the webhook listener is not enabled",
            Self::RepoNotOptedIn => "this repository has not opted in to webhooks",
            Self::BadSignature => "the delivery signature did not verify",
            Self::Replay => "this delivery has already been handled",
            Self::NotHandled => "this event type is not one rev-local acts on",
        }
    }

    /// Whether this is worth an audit row.
    ///
    /// A bad signature is: either a secret has drifted, in which case reviews have
    /// silently stopped, or somebody is probing. A redelivery is routine and a
    /// `watch` event is noise.
    pub const fn is_auditable(self) -> bool {
        matches!(self, Self::BadSignature)
    }

    /// The status code to answer with.
    ///
    /// Everything a caller could use to enumerate configuration answers 401. A 404
    /// for "no such repo" and a 401 for "bad signature" would tell an unauthorised
    /// caller which repositories exist.
    pub const fn status(self) -> StatusCode {
        match self {
            Self::ListenerDisabled | Self::RepoNotOptedIn | Self::BadSignature => {
                StatusCode::UNAUTHORIZED
            }
            // Handled correctly, deliberately not acted on. GitHub should not retry.
            Self::Replay | Self::NotHandled => StatusCode::OK,
        }
    }
}

/// One repository's webhook settings (§7.3, §13.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookRepo {
    /// The repository's id.
    pub repo_id: RepoId,
    /// The shared secret, resolved from the keychain reference in config.
    pub secret: String,
    /// §13.2's `webhook_enabled`. Off by default.
    pub enabled: bool,
}

/// Remembers delivery ids so a redelivery is not reviewed twice.
#[derive(Debug, Default)]
pub struct DeliveryLog {
    seen: VecDeque<String>,
}

impl DeliveryLog {
    /// An empty log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether this delivery has been seen, recording it if not.
    ///
    /// One call rather than a `contains` plus an `insert`, because the pair has a
    /// window between them and two concurrent redeliveries of the same id is
    /// exactly when that window is open.
    pub fn is_replay(&mut self, delivery: &str) -> bool {
        if self.seen.iter().any(|seen| seen == delivery) {
            return true;
        }
        if self.seen.len() >= DELIVERY_MEMORY {
            self.seen.pop_front();
        }
        self.seen.push_back(delivery.to_owned());
        false
    }

    /// How many ids are remembered.
    pub fn len(&self) -> usize {
        self.seen.len()
    }

    /// Whether nothing has been recorded.
    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

/// Everything the handler needs.
#[derive(Clone)]
pub struct WebhookState {
    /// Repository full name (`owner/repo`) to its settings.
    repos: Arc<BTreeMap<String, WebhookRepo>>,
    /// Whether a listener is configured at all (§13.1's `webhook_port` != 0).
    listener_enabled: bool,
    /// Delivery ids already handled.
    deliveries: Arc<Mutex<DeliveryLog>>,
    /// The bus events are admitted to.
    bus: Arc<Mutex<TriggerBus>>,
    /// Rejections worth auditing, in order.
    audit: Arc<Mutex<Vec<AuditRecord>>>,
    /// Injected so tests do not depend on wall-clock time.
    now: Arc<dyn Fn() -> Timestamp + Send + Sync>,
}

impl std::fmt::Debug for WebhookState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Secrets are deliberately absent. A Debug impl that renders them puts
        // them in every log line that ever formats this struct.
        f.debug_struct("WebhookState")
            .field("repos", &self.repos.keys().collect::<Vec<_>>())
            .field("listener_enabled", &self.listener_enabled)
            .finish_non_exhaustive()
    }
}

/// One audited rejection (§7.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditRecord {
    /// Always [`AUDIT_KIND_WEBHOOK_REJECTED`].
    pub kind: String,
    /// The repository the delivery claimed to be for.
    ///
    /// Claimed, not verified — an unverified delivery is by definition not
    /// trustworthy about its own contents, and the field is named to say so.
    pub claimed_repo: String,
    /// GitHub's delivery id, for correlating with the other end.
    pub delivery: String,
    /// Why it was rejected.
    pub reason: String,
    /// When.
    pub at: Timestamp,
}

impl WebhookState {
    /// State for the given repositories.
    pub fn new(
        repos: BTreeMap<String, WebhookRepo>,
        listener_enabled: bool,
        bus: Arc<Mutex<TriggerBus>>,
    ) -> Self {
        Self {
            repos: Arc::new(repos),
            listener_enabled,
            deliveries: Arc::new(Mutex::new(DeliveryLog::new())),
            bus,
            audit: Arc::new(Mutex::new(Vec::new())),
            now: Arc::new(chrono::Utc::now),
        }
    }

    /// Replace the clock. For tests.
    #[must_use]
    pub fn with_clock(mut self, now: Arc<dyn Fn() -> Timestamp + Send + Sync>) -> Self {
        self.now = now;
        self
    }

    /// The audit rows recorded so far.
    pub fn audit_records(&self) -> Vec<AuditRecord> {
        self.audit
            .lock()
            .map(|records| records.clone())
            .unwrap_or_default()
    }

    fn record(&self, claimed_repo: &str, delivery: &str, reason: Rejection) {
        if !reason.is_auditable() {
            return;
        }
        if let Ok(mut records) = self.audit.lock() {
            records.push(AuditRecord {
                kind: AUDIT_KIND_WEBHOOK_REJECTED.to_owned(),
                claimed_repo: claimed_repo.to_owned(),
                delivery: delivery.to_owned(),
                reason: reason.detail().to_owned(),
                at: (self.now)(),
            });
        }
    }
}

/// What the listener answers with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebhookResponse {
    /// Whether a trigger was admitted.
    pub accepted: bool,
    /// What happened.
    pub detail: String,
}

/// Just enough of GitHub's payload to route it. Parsed only after verification.
#[derive(Debug, Deserialize)]
struct Payload {
    #[serde(default)]
    repository: Option<Repository>,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    after: Option<String>,
    #[serde(default)]
    pull_request: Option<PullRequest>,
}

#[derive(Debug, Deserialize)]
struct Repository {
    full_name: String,
}

#[derive(Debug, Deserialize)]
struct PullRequest {
    number: u64,
}

/// The router §7.3 describes.
pub fn router(state: WebhookState) -> Router {
    Router::new()
        .route(WEBHOOK_PATH, post(handle_delivery))
        .with_state(state)
}

/// Verify GitHub's `X-Hub-Signature-256` over `body`.
///
/// The header is `sha256=<hex>`. Comparison is constant-time over the raw bytes
/// rather than the hex strings, so neither the value nor the position of the first
/// differing byte leaks through timing.
pub fn verify_signature(secret: &str, body: &[u8], header: &str) -> bool {
    let Some(hex) = header.strip_prefix("sha256=") else {
        return false;
    };
    let Some(presented) = decode_hex(hex) else {
        return false;
    };

    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);

    // `verify_slice` is the constant-time comparison from the `hmac` crate; it is
    // what makes this constant-time rather than the decode above.
    mac.verify_slice(&presented).is_ok()
}

/// Compute the header GitHub would send. For tests and for `hooks register`.
pub fn sign(secret: &str, body: &[u8]) -> String {
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return String::new();
    };
    mac.update(body);
    let bytes = mac.finalize().into_bytes();

    let mut out = String::with_capacity(7 + bytes.len() * 2);
    out.push_str("sha256=");
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Decode lowercase or uppercase hex. `None` on anything that is not hex.
fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    if hex.len() % 2 != 0 {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(hex.get(index..index + 2)?, 16).ok())
        .collect()
}

/// Handle `POST /webhook`.
///
/// Takes `Bytes`, not `Json`. The signature is over the bytes GitHub sent, and
/// verifying it against a re-serialisation would fail on any whitespace GitHub
/// chose differently from serde — quite apart from running a deserialiser over
/// unauthenticated input.
async fn handle_delivery(
    State(state): State<WebhookState>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, Json<WebhookResponse>) {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    };
    let delivery = header(DELIVERY_HEADER);
    let event = header(EVENT_HEADER);
    let signature = header(SIGNATURE_HEADER);

    if !state.listener_enabled {
        return reject(&state, "", &delivery, Rejection::ListenerDisabled);
    }

    // The repository name is read from the body *before* verification only to
    // choose which secret to check against — it is not trusted for anything else,
    // and nothing happens on this path if verification then fails.
    let claimed = serde_json::from_slice::<Payload>(&body)
        .ok()
        .and_then(|payload| payload.repository.map(|repo| repo.full_name))
        .unwrap_or_default();

    let Some(repo) = state.repos.get(&claimed) else {
        return reject(&state, &claimed, &delivery, Rejection::BadSignature);
    };
    if !repo.enabled {
        return reject(&state, &claimed, &delivery, Rejection::RepoNotOptedIn);
    }

    if !verify_signature(&repo.secret, &body, &signature) {
        return reject(&state, &claimed, &delivery, Rejection::BadSignature);
    }

    // Only now. A replay cache checked before verification is a cache an
    // unauthenticated caller can poison: post a few thousand made-up ids and the
    // genuine deliveries carrying them are silently dropped.
    if delivery.is_empty() {
        return reject(&state, &claimed, &delivery, Rejection::BadSignature);
    }
    let replayed = state
        .deliveries
        .lock()
        .map(|mut log| log.is_replay(&delivery))
        .unwrap_or(false);
    if replayed {
        return reject(&state, &claimed, &delivery, Rejection::Replay);
    }

    let Some(hint) = routable_hint(&event, &body) else {
        return reject(&state, &claimed, &delivery, Rejection::NotHandled);
    };

    let mut trigger = TriggerEvent::new(repo.repo_id, TriggerSource::Webhook, (state.now)());
    if let Some(hint) = hint {
        trigger = trigger.with_hint(&hint);
    }

    let detail = match state.bus.lock() {
        Ok(mut bus) => format!("{:?}", bus.admit(&trigger)),
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(WebhookResponse {
                    accepted: false,
                    detail: "the trigger bus is unavailable".to_owned(),
                }),
            )
        }
    };

    (
        StatusCode::ACCEPTED,
        Json(WebhookResponse {
            accepted: true,
            detail,
        }),
    )
}

/// Whether §7.3 acts on this event, and what hint it carries.
///
/// `Some(None)` means "handled, no hint" and `None` means "not handled" — a
/// distinction a bare `Option<String>` would lose.
fn routable_hint(event: &str, body: &[u8]) -> Option<Option<String>> {
    let payload = serde_json::from_slice::<Payload>(body).ok()?;

    match event {
        "push" => Some(payload.after),
        "pull_request" => {
            let action = payload.action.unwrap_or_default();
            if !HANDLED_PR_ACTIONS.contains(&action.as_str()) {
                return None;
            }
            Some(payload.pull_request.map(|pr| format!("pr:{}", pr.number)))
        }
        // `ping` is what GitHub sends when a webhook is created. Answering 200
        // without acting is what makes the "register webhook" button report
        // success, and it is not a review-worthy event.
        _ => None,
    }
}

/// Record and answer.
fn reject(
    state: &WebhookState,
    claimed_repo: &str,
    delivery: &str,
    reason: Rejection,
) -> (StatusCode, Json<WebhookResponse>) {
    state.record(claimed_repo, delivery, reason);
    (
        reason.status(),
        Json(WebhookResponse {
            accepted: false,
            // Deliberately uniform for the three 401 cases: a caller that cannot
            // authenticate learns nothing about which repositories exist or which
            // opted in.
            detail: if reason.status() == StatusCode::UNAUTHORIZED {
                "the delivery could not be authenticated".to_owned()
            } else {
                reason.detail().to_owned()
            },
        }),
    )
}
