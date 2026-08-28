//! The loopback trigger receiver (RL-1003, SPEC §7.2).
//!
//! This is the only component of rev-local that something else can talk to, which
//! is why all three acceptance criteria are about refusing things.
//!
//! Criterion 1 is asserted by *trying* the machine's own non-loopback address and
//! expecting to be refused, not by reading the bind string back. A `SocketAddr`
//! that says `127.0.0.1` proves what the code intended; only a refused connection
//! proves what the kernel did.
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use revlocal_core::RepoId;
use revlocal_daemon::trigger_receiver::{
    bind, router, BindError, ReceiverState, RepoSecret, SECRET_HEADER, TRIGGER_PATH,
};
use revlocal_daemon::triggers::TriggerBus;

const SECRET: &str = "a-shared-secret-from-the-keychain";

fn state_with_one_repo(bus: Arc<Mutex<TriggerBus>>) -> ReceiverState {
    let mut repos = BTreeMap::new();
    repos.insert(
        "acme-api".to_owned(),
        RepoSecret {
            repo_id: RepoId::new(1),
            secret: SECRET.to_owned(),
        },
    );
    ReceiverState::new(repos, bus)
}

/// Serve the router on a bound loopback listener until the test drops the handle.
async fn serve(
    bus: Arc<Mutex<TriggerBus>>,
) -> Result<(SocketAddr, tokio::task::JoinHandle<()>), String> {
    let listener = bind(0).await.map_err(|e| e.to_string())?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    let app = router(state_with_one_repo(bus));

    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    Ok((addr, handle))
}

/// POST a body with an optional secret header, returning (status, body).
async fn post(addr: SocketAddr, secret: Option<&str>, body: &str) -> Result<(u16, String), String> {
    let client = reqwest::Client::new();
    let mut request = client
        .post(format!("http://{addr}{TRIGGER_PATH}"))
        .header("content-type", "application/json")
        .body(body.to_owned());
    if let Some(secret) = secret {
        request = request.header(SECRET_HEADER, secret);
    }

    let response = request.send().await.map_err(|e| e.to_string())?;
    let status = response.status().as_u16();
    let text = response.text().await.map_err(|e| e.to_string())?;
    Ok((status, text))
}

/// A non-loopback address this machine actually has, if any.
///
/// The UDP trick asks the routing table which local address would be used to
/// reach a public one. It sends nothing — `connect` on a UDP socket only sets the
/// default peer.
fn a_non_loopback_address() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    let addr = socket.local_addr().ok()?.ip();
    (!addr.is_loopback() && !addr.is_unspecified()).then_some(addr)
}

#[test]
fn it_binds_loopback_only() -> Result<(), String> {
    // Criterion 1. `0.0.0.0` instead of `127.0.0.1` is one character, and the
    // consequence is whether a laptop on café wifi is running a service anyone on
    // that network can POST to.
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let bus = Arc::new(Mutex::new(TriggerBus::default()));
        let (addr, handle) = serve(bus).await?;

        assert!(
            addr.ip().is_loopback(),
            "the receiver bound {addr}, which is not loopback"
        );

        // It answers on loopback...
        let (status, _) = post(addr, Some(SECRET), r#"{"repo":"acme-api"}"#).await?;
        assert_eq!(status, 202);

        // ...and the kernel refuses the same port on this machine's own LAN
        // address. If the box has no non-loopback address there is nothing to
        // prove here, and the test says so rather than passing quietly.
        match a_non_loopback_address() {
            Some(ip) => {
                let external = SocketAddr::new(ip, addr.port());
                let refused =
                    std::net::TcpStream::connect_timeout(&external, Duration::from_millis(750));
                assert!(
                    refused.is_err(),
                    "the receiver answered on {external}; it must be loopback-only"
                );
            }
            None => println!(
                "NOTE: this machine has no non-loopback address, so only the bind \
                 address was checked"
            ),
        }

        handle.abort();
        Ok(())
    })
}

#[test]
fn a_request_without_the_correct_secret_is_rejected() -> Result<(), String> {
    // Criterion 2. The secret is not protecting against the developer — a git hook
    // is a shell script on their own machine. It protects against every *other*
    // process on that machine: a browser tab cannot POST here, and a compromised
    // dependency in an unrelated project cannot make rev-local review on demand.
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let bus = Arc::new(Mutex::new(TriggerBus::default()));
        let (addr, handle) = serve(Arc::clone(&bus)).await?;

        for (label, secret) in [
            ("no header at all", None),
            ("an empty secret", Some("")),
            ("the wrong secret", Some("not-the-secret")),
            ("a prefix of the right one", Some("a-shared-secret")),
        ] {
            let (status, body) = post(addr, secret, r#"{"repo":"acme-api"}"#).await?;
            assert_eq!(status, 401, "{label} was accepted: {body}");
        }

        // And nothing reached the bus.
        let admitted = bus
            .lock()
            .map_err(|_| "bus mutex poisoned".to_owned())?
            .is_pass_running(RepoId::new(1));
        assert!(!admitted, "a rejected request must not admit an event");

        handle.abort();
        Ok(())
    })
}

#[test]
fn an_unknown_repo_and_a_wrong_secret_are_indistinguishable() -> Result<(), String> {
    // Distinguishing them turns this endpoint into an oracle for which
    // repositories a developer is working on, which is not something it should
    // hand out to anything that can open a socket.
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let bus = Arc::new(Mutex::new(TriggerBus::default()));
        let (addr, handle) = serve(bus).await?;

        let (unknown_status, unknown_body) =
            post(addr, Some(SECRET), r#"{"repo":"does-not-exist"}"#).await?;
        let (wrong_status, wrong_body) =
            post(addr, Some("wrong"), r#"{"repo":"acme-api"}"#).await?;

        assert_eq!(unknown_status, wrong_status);
        assert_eq!(
            unknown_body, wrong_body,
            "the two answers must be byte-identical, or the difference is the oracle"
        );

        handle.abort();
        Ok(())
    })
}

#[test]
fn a_valid_trigger_reaches_the_bus() -> Result<(), String> {
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let bus = Arc::new(Mutex::new(TriggerBus::default()));
        let (addr, handle) = serve(Arc::clone(&bus)).await?;

        let (status, _) = post(
            addr,
            Some(SECRET),
            r#"{"repo":"acme-api","hint":"deadbeef"}"#,
        )
        .await?;
        assert_eq!(status, 202);

        // Two more within the window fold into the same pass rather than becoming
        // three — the receiver's job is to admit, and the bus's is to coalesce.
        for _ in 0..2 {
            let (status, _) = post(addr, Some(SECRET), r#"{"repo":"acme-api"}"#).await?;
            assert_eq!(status, 202);
        }

        let sources = bus
            .lock()
            .map_err(|_| "bus mutex poisoned".to_owned())?
            .pending_sources(RepoId::new(1));
        assert_eq!(sources.len(), 3, "all three admitted into one window");

        handle.abort();
        Ok(())
    })
}

#[test]
fn a_port_conflict_says_what_to_do_about_it() -> Result<(), String> {
    // Criterion 3. "address in use" tells a user what happened, not what to do —
    // and a port that is actually free is a better answer than a suggestion to go
    // and find one.
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let held = bind(0).await.map_err(|e| e.to_string())?;
        let port = held.local_addr().map_err(|e| e.to_string())?.port();

        let conflict = bind(port).await;
        let Err(error) = conflict else {
            return Err(format!("binding {port} twice should have failed"));
        };

        let BindError::PortInUse {
            port: named,
            suggestion,
        } = &error
        else {
            return Err(format!("expected a port conflict, got: {error}"));
        };
        assert_eq!(*named, port);
        assert_ne!(
            *suggestion, port,
            "the suggestion must not be the busy port"
        );

        // The suggestion is a port the OS said was free, so it must be bindable.
        let proof = bind(*suggestion).await;
        assert!(
            proof.is_ok(),
            "the suggested port {suggestion} could not be bound"
        );

        let text = error.to_string();
        assert!(
            text.contains(&port.to_string()),
            "must name the port: {text}"
        );
        assert!(text.contains("try:"), "must say what to do: {text}");
        assert!(
            text.contains("trigger_port"),
            "must name the setting to change: {text}"
        );

        drop(held);
        Ok(())
    })
}

#[test]
fn the_state_does_not_print_secrets() {
    // A Debug impl that renders the secret puts it in every log line that ever
    // formats this struct, which is the sort of leak that survives review because
    // nobody writes the log line and the impl in the same week.
    let bus = Arc::new(Mutex::new(TriggerBus::default()));
    let rendered = format!("{:?}", state_with_one_repo(bus));

    assert!(
        rendered.contains("acme-api"),
        "the repo name is not a secret"
    );
    assert!(
        !rendered.contains(SECRET),
        "the secret reached a Debug rendering: {rendered}"
    );
}
