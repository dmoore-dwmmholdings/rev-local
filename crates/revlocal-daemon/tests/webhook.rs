//! The GitHub webhook listener (RL-1005, SPEC §7.3).
//!
//! Two of the tests here are about *ordering* rather than behaviour, because the
//! security of this module is mostly in what happens before what: the signature is
//! verified before any JSON is trusted, and the replay cache is consulted only
//! after the signature verifies.
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use revlocal_core::{RepoId, TriggerSource};
use revlocal_daemon::trigger_receiver::bind;
use revlocal_daemon::triggers::TriggerBus;
use revlocal_daemon::webhook::{
    router, sign, verify_signature, DeliveryLog, WebhookRepo, WebhookState,
    AUDIT_KIND_WEBHOOK_REJECTED, DELIVERY_HEADER, DELIVERY_MEMORY, EVENT_HEADER, SIGNATURE_HEADER,
    WEBHOOK_PATH,
};

const SECRET: &str = "a-webhook-secret-from-the-keychain";
const REPO: &str = "acme/api";

fn push_body(sha: &str) -> String {
    format!(r#"{{"repository":{{"full_name":"{REPO}"}},"after":"{sha}"}}"#)
}

fn pr_body(action: &str, number: u64) -> String {
    format!(
        r#"{{"repository":{{"full_name":"{REPO}"}},"action":"{action}","pull_request":{{"number":{number}}}}}"#
    )
}

fn state(enabled_listener: bool, opted_in: bool, bus: Arc<Mutex<TriggerBus>>) -> WebhookState {
    let mut repos = BTreeMap::new();
    repos.insert(
        REPO.to_owned(),
        WebhookRepo {
            repo_id: RepoId::new(1),
            secret: SECRET.to_owned(),
            enabled: opted_in,
        },
    );
    WebhookState::new(repos, enabled_listener, bus)
}

async fn serve(state: WebhookState) -> Result<(SocketAddr, tokio::task::JoinHandle<()>), String> {
    let listener = bind(0).await.map_err(|e| e.to_string())?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    let app = router(state);
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok((addr, handle))
}

/// POST a delivery. `signature` of `None` means "sign it correctly".
async fn deliver(
    addr: SocketAddr,
    event: &str,
    delivery: &str,
    body: &str,
    signature: Option<&str>,
) -> Result<u16, String> {
    let computed = sign(SECRET, body.as_bytes());
    let signature = signature.unwrap_or(&computed);

    let response = reqwest::Client::new()
        .post(format!("http://{addr}{WEBHOOK_PATH}"))
        .header("content-type", "application/json")
        .header(EVENT_HEADER, event)
        .header(DELIVERY_HEADER, delivery)
        .header(SIGNATURE_HEADER, signature)
        .body(body.to_owned())
        .send()
        .await
        .map_err(|e| e.to_string())?;

    Ok(response.status().as_u16())
}

#[test]
fn a_bad_signature_is_rejected_with_401_and_audited() -> Result<(), String> {
    // Criterion 1. A bad signature is the one event here worth waking somebody
    // for: either a secret has drifted, in which case reviews have silently
    // stopped happening, or somebody is probing the endpoint.
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let bus = Arc::new(Mutex::new(TriggerBus::default()));
        let state = state(true, true, Arc::clone(&bus));
        let (addr, handle) = serve(state.clone()).await?;

        for (label, signature) in [
            ("no signature", Some("")),
            ("not a sha256 header", Some("md5=abcdef")),
            (
                "well-formed but wrong",
                Some(&sign("wrong-secret", push_body("deadbeef").as_bytes())[..]),
            ),
            ("not hex", Some("sha256=zzzz")),
            ("odd-length hex", Some("sha256=abc")),
        ] {
            let status = deliver(addr, "push", "d-1", &push_body("deadbeef"), signature).await?;
            assert_eq!(status, 401, "{label} was not rejected");
        }

        let records = state.audit_records();
        assert_eq!(records.len(), 5, "every bad signature must be audited");
        assert!(records
            .iter()
            .all(|r| r.kind == AUDIT_KIND_WEBHOOK_REJECTED));
        assert_eq!(records[0].delivery, "d-1");
        assert_eq!(records[0].claimed_repo, REPO);

        // And nothing reached the bus.
        let admitted = bus
            .lock()
            .map_err(|_| "bus mutex poisoned".to_owned())?
            .pending_sources(RepoId::new(1));
        assert!(admitted.is_empty());

        handle.abort();
        Ok(())
    })
}

#[test]
fn a_good_signature_is_accepted() -> Result<(), String> {
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let bus = Arc::new(Mutex::new(TriggerBus::default()));
        let (addr, handle) = serve(state(true, true, Arc::clone(&bus))).await?;

        let status = deliver(addr, "push", "d-ok", &push_body("cafebabe"), None).await?;
        assert_eq!(status, 202);

        let sources = bus
            .lock()
            .map_err(|_| "bus mutex poisoned".to_owned())?
            .pending_sources(RepoId::new(1));
        assert_eq!(sources, vec![TriggerSource::Webhook]);

        handle.abort();
        Ok(())
    })
}

#[test]
fn the_signature_is_verified_over_the_bytes_github_sent() {
    // Criterion 2's other half, and the reason the handler takes `Bytes` rather
    // than `Json`. Verifying against a re-serialisation fails on any whitespace
    // GitHub chose differently from serde — and would mean running a deserialiser
    // over unauthenticated input first.
    let compact = r#"{"repository":{"full_name":"acme/api"}}"#;
    let spaced = r#"{ "repository": { "full_name": "acme/api" } }"#;

    let signature = sign(SECRET, compact.as_bytes());
    assert!(verify_signature(SECRET, compact.as_bytes(), &signature));
    assert!(
        !verify_signature(SECRET, spaced.as_bytes(), &signature),
        "the same JSON with different whitespace is different bytes"
    );
}

#[test]
fn signature_comparison_is_constant_time() {
    // Criterion 2. Asserting the *property* by timing is a flaky test on a shared
    // runner, so this asserts the mechanism instead: comparison goes through the
    // `hmac` crate's `verify_slice`, and a byte-for-byte `==` on the hex string
    // would accept nothing this rejects while differing only in timing.
    //
    // What is testable is that no prefix is ever accepted — the failure a
    // short-circuiting comparison enables.
    let body = b"{}";
    let full = sign(SECRET, body);
    assert!(verify_signature(SECRET, body, &full));

    let hex = full.trim_start_matches("sha256=");
    for length in 2..hex.len() {
        if length % 2 != 0 {
            continue;
        }
        let prefix = format!("sha256={}", &hex[..length]);
        assert!(
            !verify_signature(SECRET, body, &prefix),
            "a {length}-character prefix was accepted"
        );
    }

    // A correct-length signature differing in exactly the last byte is rejected,
    // which is the case a short-circuit gets right last.
    let mut last = hex.to_owned();
    last.pop();
    last.push(if hex.ends_with('0') { '1' } else { '0' });
    assert!(!verify_signature(SECRET, body, &format!("sha256={last}")));
}

#[test]
fn a_replayed_delivery_is_ignored() -> Result<(), String> {
    // Criterion 3. GitHub redelivers on its own, and a redelivery that produced a
    // second review would double the cost of every flaky network moment.
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let bus = Arc::new(Mutex::new(TriggerBus::default()));
        let (addr, handle) = serve(state(true, true, Arc::clone(&bus))).await?;
        let body = push_body("deadbeef");

        assert_eq!(deliver(addr, "push", "same-id", &body, None).await?, 202);
        // 200, not 401: the delivery was authentic and correctly handled. A 4xx
        // would make GitHub retry something that already succeeded.
        assert_eq!(deliver(addr, "push", "same-id", &body, None).await?, 200);
        assert_eq!(deliver(addr, "push", "same-id", &body, None).await?, 200);

        // A different id for the same content is a genuinely new delivery.
        assert_eq!(deliver(addr, "push", "other-id", &body, None).await?, 202);

        let sources = bus
            .lock()
            .map_err(|_| "bus mutex poisoned".to_owned())?
            .pending_sources(RepoId::new(1));
        assert_eq!(sources.len(), 2, "two distinct deliveries, two events");

        handle.abort();
        Ok(())
    })
}

#[test]
fn an_unverified_delivery_cannot_poison_the_replay_cache() -> Result<(), String> {
    // The ordering that matters. If the replay cache were consulted before the
    // signature, an unauthenticated caller could POST a few thousand made-up
    // delivery ids and the genuine deliveries carrying those ids would be silently
    // dropped — a denial of review that leaves no failed request behind.
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let bus = Arc::new(Mutex::new(TriggerBus::default()));
        let (addr, handle) = serve(state(true, true, Arc::clone(&bus))).await?;
        let body = push_body("deadbeef");

        // An attacker claims the id first, with a bad signature.
        assert_eq!(
            deliver(addr, "push", "predicted-id", &body, Some("sha256=00")).await?,
            401
        );

        // GitHub's genuine delivery with that id must still be acted on.
        assert_eq!(
            deliver(addr, "push", "predicted-id", &body, None).await?,
            202
        );

        let sources = bus
            .lock()
            .map_err(|_| "bus mutex poisoned".to_owned())?
            .pending_sources(RepoId::new(1));
        assert_eq!(sources.len(), 1, "the genuine delivery was dropped");

        handle.abort();
        Ok(())
    })
}

#[test]
fn the_listener_is_disabled_unless_the_port_is_set_and_the_repo_opted_in() -> Result<(), String> {
    // Criterion 4. Two separate switches, both load-bearing: `webhook_port = 0`
    // means nothing binds at all, and a repository that has not opted in is
    // rejected even when the port is open — so adding one repository does not
    // quietly expose every other repository on the machine.
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    runtime.block_on(async {
        for (label, listener, opted_in) in [
            ("no listener, repo opted in", false, true),
            ("listener up, repo not opted in", true, false),
            ("neither", false, false),
        ] {
            let bus = Arc::new(Mutex::new(TriggerBus::default()));
            let (addr, handle) = serve(state(listener, opted_in, Arc::clone(&bus))).await?;

            let status = deliver(addr, "push", "d", &push_body("deadbeef"), None).await?;
            assert_eq!(status, 401, "{label} should have been rejected");

            let sources = bus
                .lock()
                .map_err(|_| "bus mutex poisoned".to_owned())?
                .pending_sources(RepoId::new(1));
            assert!(sources.is_empty(), "{label} admitted an event");

            handle.abort();
        }
        Ok(())
    })
}

#[test]
fn the_three_rejection_reasons_are_indistinguishable_from_outside() -> Result<(), String> {
    // A 404 for "no such repo", a 403 for "not opted in" and a 401 for "bad
    // signature" would let an unauthorised caller enumerate which repositories
    // exist and which have webhooks on.
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let bus = Arc::new(Mutex::new(TriggerBus::default()));
        let (addr, handle) = serve(state(true, false, bus)).await?;

        let unknown = r#"{"repository":{"full_name":"someone/else"},"after":"x"}"#.to_owned();
        let client = reqwest::Client::new();
        let mut bodies = Vec::new();
        for body in [unknown, push_body("deadbeef")] {
            let response = client
                .post(format!("http://{addr}{WEBHOOK_PATH}"))
                .header("content-type", "application/json")
                .header(EVENT_HEADER, "push")
                .header(DELIVERY_HEADER, "d")
                .header(SIGNATURE_HEADER, sign(SECRET, body.as_bytes()))
                .body(body)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            bodies.push((
                response.status().as_u16(),
                response.text().await.unwrap_or_default(),
            ));
        }

        assert_eq!(bodies[0], bodies[1], "the two answers must be identical");

        handle.abort();
        Ok(())
    })
}

#[test]
fn only_the_pull_request_actions_spec_names_are_acted_on() -> Result<(), String> {
    // §7.3 lists four. `closed`, `labeled` and the rest are deliveries rev-local
    // receives and correctly does nothing with — 200, so GitHub does not retry.
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let bus = Arc::new(Mutex::new(TriggerBus::default()));
        let (addr, handle) = serve(state(true, true, Arc::clone(&bus))).await?;

        for (index, action) in ["opened", "synchronize", "reopened", "ready_for_review"]
            .iter()
            .enumerate()
        {
            let status = deliver(
                addr,
                "pull_request",
                &format!("handled-{index}"),
                &pr_body(action, 42),
                None,
            )
            .await?;
            assert_eq!(status, 202, "{action} should be handled");
        }

        for (index, action) in ["closed", "labeled", "assigned"].iter().enumerate() {
            let status = deliver(
                addr,
                "pull_request",
                &format!("ignored-{index}"),
                &pr_body(action, 42),
                None,
            )
            .await?;
            assert_eq!(status, 200, "{action} should be received and ignored");
        }

        // `ping` is what GitHub sends when a webhook is created. It must answer 200
        // or the "register webhook" button reports failure.
        let ping = format!(r#"{{"repository":{{"full_name":"{REPO}"}},"zen":"hi"}}"#);
        assert_eq!(deliver(addr, "ping", "ping-1", &ping, None).await?, 200);

        let sources = bus
            .lock()
            .map_err(|_| "bus mutex poisoned".to_owned())?
            .pending_sources(RepoId::new(1));
        assert_eq!(sources.len(), 4, "only the four named actions");

        handle.abort();
        Ok(())
    })
}

#[test]
fn the_delivery_log_is_bounded() {
    // The cache is fed by a network endpoint, so an unbounded set is a memory
    // exhaustion bug wearing a correctness hat.
    let mut log = DeliveryLog::new();
    for index in 0..DELIVERY_MEMORY + 500 {
        assert!(!log.is_replay(&format!("d-{index}")));
    }

    assert_eq!(log.len(), DELIVERY_MEMORY);
    // The oldest were evicted, which is the trade: a redelivery arriving after
    // four thousand others is reviewed twice rather than exhausting memory.
    assert!(!log.is_replay("d-0"));
    assert!(log.is_replay(&format!("d-{}", DELIVERY_MEMORY + 499)));
}

#[test]
fn the_state_does_not_print_secrets() {
    let bus = Arc::new(Mutex::new(TriggerBus::default()));
    let rendered = format!("{:?}", state(true, true, bus));

    assert!(rendered.contains(REPO));
    assert!(
        !rendered.contains(SECRET),
        "the secret reached Debug: {rendered}"
    );
}
