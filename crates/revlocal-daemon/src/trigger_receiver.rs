//! The loopback trigger receiver (RL-1003, SPEC §7.2).
//!
//! # This is the one component a hostile process can talk to
//!
//! Everything else in rev-local is reached by rev-local. This listens, which makes
//! it the only place where somebody else chooses when it runs. Three properties
//! follow, and they are the three acceptance criteria.
//!
//! **Loopback only.** Bound to `127.0.0.1`, never `0.0.0.0`. The difference is one
//! character and the consequence is whether a laptop on café wifi is running a
//! service anyone on that network can POST to. It is asserted by a test that tries
//! the machine's own non-loopback address and expects to be refused, rather than
//! by reading the bind string back — a `SocketAddr` that says `127.0.0.1` proves
//! what the code intended, not what the kernel did.
//!
//! **A shared secret, compared in constant time.** A git hook is a shell script on
//! the developer's own machine, so the secret is not protecting against them. It
//! is protecting against every *other* process on the machine — a browser tab
//! cannot POST here without it, and a compromised dependency in some unrelated
//! project cannot make rev-local start reviewing on demand.
//!
//! **An unknown repository and a wrong secret get the same answer.** Distinguishing
//! them turns this endpoint into an oracle for which repositories a developer is
//! working on, which is not information it should hand out to anything that can
//! open a socket.
//!
//! # Why it answers before it does anything
//!
//! §7.2 requires hooks be non-blocking with a 2-second timeout, because *a
//! developer's commit must never fail because rev-local is down* (RL-1004's
//! acceptance test). The receiver's half of that bargain is to admit the event to
//! the bus and return, never to run discovery on the request thread. A handler
//! that did real work would make a slow discovery pass into a slow `git commit`.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use revlocal_core::{RepoId, Timestamp, TriggerSource};
use serde::{Deserialize, Serialize};

use crate::triggers::{TriggerBus, TriggerEvent};

/// SPEC §13.1's default loopback port.
pub const DEFAULT_TRIGGER_PORT: u16 = 41791;

/// The header a hook puts its shared secret in.
pub const SECRET_HEADER: &str = "x-revlocal-secret";

/// The route §7.2 names.
pub const TRIGGER_PATH: &str = "/trigger";

/// What a hook POSTs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerRequest {
    /// The repository's configured name.
    pub repo: String,
    /// A sha, PR number or revision, if the hook knew one.
    ///
    /// Advisory. See [`TriggerEvent::hint`] — nothing looks it up.
    #[serde(default)]
    pub hint: Option<String>,
}

/// What the receiver answers with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerResponse {
    /// Whether the event was admitted to the bus.
    pub accepted: bool,
    /// What happened, for a human reading `curl -v` output.
    pub detail: String,
}

/// One repository the receiver will accept triggers for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoSecret {
    /// The repository's id, for the event.
    pub repo_id: RepoId,
    /// Its shared secret.
    pub secret: String,
}

/// Everything the handler needs.
#[derive(Clone)]
pub struct ReceiverState {
    /// Name to id and secret.
    repos: Arc<BTreeMap<String, RepoSecret>>,
    /// The bus events are admitted to.
    bus: Arc<Mutex<TriggerBus>>,
    /// Injected so tests do not depend on wall-clock time (ADR 0024).
    now: Arc<dyn Fn() -> Timestamp + Send + Sync>,
}

impl std::fmt::Debug for ReceiverState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The secrets are deliberately not printed. A Debug impl that renders them
        // puts them in every log line that ever formats this struct.
        f.debug_struct("ReceiverState")
            .field("repos", &self.repos.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl ReceiverState {
    /// State for the given repositories, admitting to `bus`.
    pub fn new(repos: BTreeMap<String, RepoSecret>, bus: Arc<Mutex<TriggerBus>>) -> Self {
        Self {
            repos: Arc::new(repos),
            bus,
            now: Arc::new(chrono::Utc::now),
        }
    }

    /// Replace the clock. For tests.
    #[must_use]
    pub fn with_clock(mut self, now: Arc<dyn Fn() -> Timestamp + Send + Sync>) -> Self {
        self.now = now;
        self
    }
}

/// The router §7.2 describes: one route, one method.
pub fn router(state: ReceiverState) -> Router {
    Router::new()
        .route(TRIGGER_PATH, post(handle_trigger))
        .with_state(state)
}

/// Handle `POST /trigger`.
async fn handle_trigger(
    State(state): State<ReceiverState>,
    headers: HeaderMap,
    Json(request): Json<TriggerRequest>,
) -> (StatusCode, Json<TriggerResponse>) {
    let presented = headers
        .get(SECRET_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();

    // An unknown repository and a wrong secret get the same answer, and the
    // comparison runs either way. Returning early on an unknown name would leak
    // which repositories exist through response timing as well as through the
    // status code.
    let known = state.repos.get(&request.repo);
    let expected = known.map(|repo| repo.secret.as_str()).unwrap_or("");
    let secret_ok = constant_time_eq(presented.as_bytes(), expected.as_bytes());

    let Some(repo) = known.filter(|_| secret_ok && !expected.is_empty()) else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(TriggerResponse {
                accepted: false,
                detail: "unknown repository or incorrect secret".to_owned(),
            }),
        );
    };

    let mut event = TriggerEvent::new(repo.repo_id, TriggerSource::Hook, (state.now)());
    if let Some(hint) = &request.hint {
        event = event.with_hint(hint);
    }

    // Admit and return. Discovery never runs on the request thread: §7.2 gives
    // hooks a 2-second timeout, and a handler that did real work would turn a slow
    // discovery pass into a slow `git commit`.
    let detail = match state.bus.lock() {
        Ok(mut bus) => format!("{:?}", bus.admit(&event)),
        // A poisoned mutex means another thread panicked while holding it. The
        // honest answer is that the trigger was not admitted, not a 200 that
        // implies it was.
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(TriggerResponse {
                    accepted: false,
                    detail: "the trigger bus is unavailable".to_owned(),
                }),
            )
        }
    };

    (
        StatusCode::ACCEPTED,
        Json(TriggerResponse {
            accepted: true,
            detail,
        }),
    )
}

/// Compare two byte strings without leaking their contents through timing.
///
/// Length is compared first and *is* leaked, which is fine — the secret's length
/// is not the secret. What must not leak is where the first differing byte is,
/// because that turns guessing a 32-byte secret from 256^32 attempts into 32×256.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut difference = 0_u8;
    for (left, right) in a.iter().zip(b.iter()) {
        difference |= left ^ right;
    }
    difference == 0
}

/// Why the receiver could not bind.
#[derive(Debug, thiserror::Error)]
pub enum BindError {
    /// Something else is already on the port.
    ///
    /// §18, and the remediation is the point: "address in use" tells a user what
    /// happened, not what to do about it. A port that is actually free is a better
    /// answer than a suggestion to go and find one.
    #[error(
        "port {port} is already in use, so the git-hook trigger receiver cannot \
         start\n  try: set global.trigger_port = {suggestion} in your config, or \
         stop whatever is on {port} (lsof -i :{port})"
    )]
    PortInUse {
        /// The port that was asked for.
        port: u16,
        /// A port that was free when this error was built.
        suggestion: u16,
    },

    /// Anything else the OS said.
    #[error("could not bind 127.0.0.1:{port}: {source}")]
    Failed {
        /// The port that was asked for.
        port: u16,
        /// Why.
        #[source]
        source: std::io::Error,
    },
}

/// Bind the receiver to loopback on `port`.
///
/// `127.0.0.1` is hard-coded rather than configurable. A configurable bind address
/// is a configurable mistake: there is no correct value other than loopback, and
/// the one wrong value exposes a trigger endpoint to the network.
pub async fn bind(port: u16) -> Result<tokio::net::TcpListener, BindError> {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);

    match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => Ok(listener),
        Err(source) if source.kind() == std::io::ErrorKind::AddrInUse => {
            Err(BindError::PortInUse {
                port,
                suggestion: suggest_port().await.unwrap_or(port.saturating_add(1)),
            })
        }
        Err(source) => Err(BindError::Failed { port, source }),
    }
}

/// A loopback port that was free a moment ago.
///
/// Asking the OS for port 0 and reading back what it gave is the only way to name
/// a port that is actually free. It can be taken again before the user acts on the
/// suggestion — which is why the error says "try", and why it also tells them how
/// to find what is on the port they wanted.
async fn suggest_port() -> Option<u16> {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
    let listener = tokio::net::TcpListener::bind(addr).await.ok()?;
    let port = listener.local_addr().ok()?.port();
    drop(listener);
    Some(port)
}
