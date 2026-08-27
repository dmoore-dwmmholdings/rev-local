//! `HttpClient` against a local HTTP mock (RL-602, SPEC §11.2).
//!
//! The mock is a real socket speaking real HTTP, hand-rolled rather than pulled from
//! a crate, for one reason: three of the four criteria are about **exact bytes** —
//! which content type came back, which status, and which headers went out. A
//! framework that normalises any of those would test the framework.
//!
//! Nothing here leaves the loopback interface.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use revlocal_core::SecretRef;
use revlocal_mcp::{HttpClient, HttpEndpoint, HttpError, SecretResolver};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// The token used throughout. Distinctive so a leak is unmistakable in any output.
const TOKEN: &str = "revlocal-secret-token-8f3a91c4-DO-NOT-LOG";

// --- the mock ------------------------------------------------------------

/// What the mock should answer with.
#[derive(Debug, Clone)]
enum Reply {
    /// `200 application/json` with this body.
    Json(String),
    /// `200 text/event-stream` with these `data:` payloads, in order.
    EventStream(Vec<String>),
    /// This status, with this body and content type.
    Status(u16, String),
    /// Close the connection without answering.
    Hangup,
}

/// A mock MCP-over-HTTP server on loopback.
struct MockServer {
    url: String,
    /// Every request line + headers the server saw, in order.
    seen: Arc<Mutex<Vec<String>>>,
    handle: tokio::task::JoinHandle<()>,
}

impl MockServer {
    /// Start a server that answers each request with the next scripted reply,
    /// repeating the last one once the script runs out.
    async fn start(script: Vec<Reply>) -> Result<Self, String> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| format!("bind: {e}"))?;
        let port = listener
            .local_addr()
            .map_err(|e| format!("local_addr: {e}"))?
            .port();

        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_for_task = Arc::clone(&seen);

        let handle = tokio::spawn(async move {
            let mut index = 0_usize;
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };

                let mut buffer = vec![0_u8; 65536];
                let Ok(read) = socket.read(&mut buffer).await else {
                    continue;
                };
                let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
                if let Ok(mut seen) = seen_for_task.lock() {
                    seen.push(request);
                }

                let reply = script
                    .get(index)
                    .or_else(|| script.last())
                    .cloned()
                    .unwrap_or(Reply::Json("{}".to_owned()));
                index += 1;

                let response = match reply {
                    Reply::Json(body) => http_response(200, "application/json", &body, &[]),
                    Reply::EventStream(events) => {
                        let body = events
                            .iter()
                            .map(|e| format!("event: message\ndata: {e}\n\n"))
                            .collect::<String>();
                        http_response(200, "text/event-stream", &body, &[])
                    }
                    Reply::Status(code, body) => {
                        let extra: &[(&str, &str)] = if code == 429 {
                            &[("Retry-After", "7")]
                        } else {
                            &[]
                        };
                        http_response(code, "application/json", &body, extra)
                    }
                    Reply::Hangup => {
                        drop(socket);
                        continue;
                    }
                };

                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.flush().await;
                let _ = socket.shutdown().await;
            }
        });

        Ok(Self {
            url: format!("http://127.0.0.1:{port}/mcp"),
            seen,
            handle,
        })
    }

    /// Every request the server saw, whole.
    fn requests(&self) -> Vec<String> {
        self.seen.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

fn http_response(status: u16, content_type: &str, body: &str, extra: &[(&str, &str)]) -> String {
    let mut head = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in extra {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    head.push_str(body);
    head
}

/// A JSON-RPC success body for a given id.
fn rpc_result(id: u64, result: serde_json::Value) -> String {
    serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

fn initialize_result() -> serde_json::Value {
    serde_json::json!({
        "protocolVersion": revlocal_mcp::PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "mock-http", "version": "1.0.0" },
    })
}

fn tools_result() -> serde_json::Value {
    serde_json::json!({
        "tools": [{
            "name": "create_page",
            "description": "make a page",
            "inputSchema": { "type": "object", "properties": { "title": { "type": "string" } } },
        }],
    })
}

// --- resolvers -----------------------------------------------------------

/// A keychain stand-in. Never touches the developer's real keychain, which a test
/// suite has no business reading.
struct FakeKeychain(BTreeMap<String, String>);

impl SecretResolver for FakeKeychain {
    fn resolve(&self, name: &str) -> Result<String, String> {
        self.0
            .get(name)
            .cloned()
            .ok_or_else(|| format!("no entry named `{name}`"))
    }
}

fn keychain() -> FakeKeychain {
    let mut entries = BTreeMap::new();
    entries.insert("trama-token".to_owned(), TOKEN.to_owned());
    FakeKeychain(entries)
}

fn authed_endpoint(url: &str) -> HttpEndpoint {
    HttpEndpoint::new("trama", url).with_header(
        "authorization",
        SecretRef::parse("{{keychain:trama-token}}"),
    )
}

// --- criterion 1: initialize + tools/list against a local mock ----------

#[tokio::test]
async fn http_initialize_and_list_tools_succeed() {
    let server = MockServer::start(vec![
        Reply::Json(rpc_result(1, initialize_result())),
        Reply::Json("{}".to_owned()), // the initialized notification
        Reply::Json(rpc_result(2, tools_result())),
    ])
    .await
    .unwrap_or_else(|e| panic!("{e}"));

    let mut client =
        HttpClient::new(authed_endpoint(&server.url)).unwrap_or_else(|e| panic!("{e}"));
    let tools = client
        .list_tools(&keychain())
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "create_page");
    // §11.2 caches schemas; a tool list without them makes every later validation
    // vacuous.
    assert!(tools[0].input_schema.get("properties").is_some());

    let handshake = client.handshake().expect("connected");
    assert_eq!(
        handshake.protocol_version.as_deref(),
        Some(revlocal_mcp::PROTOCOL_VERSION),
        "camelCase deserialization regressed"
    );
    assert_eq!(client.connect_count(), 1);
}

/// §13.1: resolved **at connect time**, not at load time. A config resolved at load
/// time holds every token for the process's lifetime.
#[tokio::test]
async fn http_no_secret_is_read_until_the_first_call() {
    /// Records whether anything asked it for a secret.
    struct Counting(Arc<Mutex<usize>>);
    impl SecretResolver for Counting {
        fn resolve(&self, _name: &str) -> Result<String, String> {
            if let Ok(mut count) = self.0.lock() {
                *count += 1;
            }
            Ok(TOKEN.to_owned())
        }
    }

    let calls = Arc::new(Mutex::new(0_usize));
    let resolver = Counting(Arc::clone(&calls));

    let client = HttpClient::new(authed_endpoint("http://127.0.0.1:1/mcp"))
        .unwrap_or_else(|e| panic!("{e}"));

    assert!(!client.is_connected());
    assert_eq!(*calls.lock().unwrap_or_else(|e| panic!("{e}")), 0);

    // And it *is* read once something asks — otherwise the assertion above would
    // pass against a client that never reads secrets at all.
    let mut client = client;
    let _ = client.list_tools(&resolver).await;
    assert_eq!(*calls.lock().unwrap_or_else(|e| panic!("{e}")), 1);
}

/// The session is established once and reused. Re-initializing per call would
/// re-run the handshake every time, re-read the keychain every time, and break any
/// server that keys state to `Mcp-Session-Id`.
///
/// Written after a negative probe that made `call` always reconnect changed no test —
/// the claim was in the design and nowhere in the suite.
#[tokio::test]
async fn http_the_session_is_established_once_and_reused() {
    let server = MockServer::start(vec![
        Reply::Json(rpc_result(1, initialize_result())),
        Reply::Json("{}".to_owned()),
        Reply::Json(rpc_result(2, tools_result())),
        Reply::Json(rpc_result(3, tools_result())),
    ])
    .await
    .unwrap_or_else(|e| panic!("{e}"));

    let mut client =
        HttpClient::new(authed_endpoint(&server.url)).unwrap_or_else(|e| panic!("{e}"));

    client
        .list_tools(&keychain())
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    client
        .list_tools(&keychain())
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(
        client.connect_count(),
        1,
        "the client re-initialized instead of reusing its session"
    );

    let initializes = server
        .requests()
        .iter()
        .filter(|r| r.contains("\"initialize\""))
        .count();
    assert_eq!(initializes, 1, "`initialize` was sent more than once");
}

#[tokio::test]
async fn http_the_resolved_header_is_actually_sent() {
    let server = MockServer::start(vec![
        Reply::Json(rpc_result(1, initialize_result())),
        Reply::Json("{}".to_owned()),
        Reply::Json(rpc_result(2, tools_result())),
    ])
    .await
    .unwrap_or_else(|e| panic!("{e}"));

    let mut client =
        HttpClient::new(authed_endpoint(&server.url)).unwrap_or_else(|e| panic!("{e}"));
    client
        .list_tools(&keychain())
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    let requests = server.requests();
    assert!(!requests.is_empty());
    assert!(
        requests[0].to_lowercase().contains("authorization:"),
        "the resolved header never reached the wire — every redaction test below \
         would then pass against a client that simply never sends it:\n{}",
        requests[0]
    );
    assert!(requests[0].contains(TOKEN), "the header was sent empty");
    // Both content types are advertised, or a server that offers SSE would never
    // exercise the path this item is required to handle.
    assert!(requests[0].to_lowercase().contains("text/event-stream"));
}

// --- criterion 2: the auth value never appears in logs or errors --------

/// The whole point of §13.1's deferred resolution. Every error path is walked with a
/// distinctive token in play, and the token must appear in none of them.
///
/// Note the companion test above: `http_the_resolved_header_is_actually_sent` proves
/// the token is real and does reach the wire. Without it, all of this would pass
/// against a client that simply never resolved anything.
#[tokio::test]
async fn http_the_token_never_appears_in_an_error() {
    let cases: Vec<(&str, Vec<Reply>)> = vec![
        (
            "401",
            vec![Reply::Status(401, r#"{"error":"bad token"}"#.to_owned())],
        ),
        ("403", vec![Reply::Status(403, "{}".to_owned())]),
        ("404", vec![Reply::Status(404, "not found".to_owned())]),
        ("429", vec![Reply::Status(429, "slow down".to_owned())]),
        ("500", vec![Reply::Status(500, "boom".to_owned())]),
        ("hangup", vec![Reply::Hangup]),
        ("garbage", vec![Reply::Json("not json at all".to_owned())]),
    ];

    for (name, script) in cases {
        let server = MockServer::start(script)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let mut client =
            HttpClient::new(authed_endpoint(&server.url)).unwrap_or_else(|e| panic!("{e}"));

        let error = client
            .list_tools(&keychain())
            .await
            .expect_err("the mock was scripted to fail");

        let display = error.to_string();
        let debug = format!("{error:?}");
        let client_debug = format!("{client:?}");

        for (what, text) in [
            ("Display", &display),
            ("Debug", &debug),
            ("the client's Debug", &client_debug),
        ] {
            assert!(
                !text.contains(TOKEN),
                "case {name}: the token leaked through {what}:\n{text}"
            );
        }
    }
}

/// A `{{keychain:...}}` that cannot be resolved names the **entry**, not a value,
/// and says what to do.
#[tokio::test]
async fn http_an_unresolvable_secret_names_the_entry_and_the_remedy() {
    let mut client = HttpClient::new(
        HttpEndpoint::new("trama", "http://127.0.0.1:1/mcp")
            .with_header("authorization", SecretRef::parse("{{keychain:missing}}")),
    )
    .unwrap_or_else(|e| panic!("{e}"));

    let error = client
        .list_tools(&keychain())
        .await
        .expect_err("the entry does not exist");

    assert!(matches!(error, HttpError::Secret { .. }), "{error:?}");
    let message = error.to_string();
    assert!(message.contains("missing"), "{message}");
    assert!(message.contains("authorization"), "{message}");
    // §18: a user-visible error says what to do about it.
    assert!(message.contains("try:"), "{message}");
    assert_eq!(error.retryable(), Some(false));
}

/// A pasted token with a trailing newline is the common cause of this, and the
/// message deliberately does **not** quote the value to explain the newline.
#[tokio::test]
async fn http_a_header_value_with_a_newline_is_rejected_without_printing_it() {
    let mut client = HttpClient::new(
        HttpEndpoint::new("trama", "http://127.0.0.1:1/mcp").with_header(
            "authorization",
            SecretRef::Literal(format!("Bearer {TOKEN}\n")),
        ),
    )
    .unwrap_or_else(|e| panic!("{e}"));

    let error = client
        .list_tools(&revlocal_mcp::NoSecrets)
        .await
        .expect_err("a header with a newline is invalid");

    assert!(matches!(error, HttpError::BadHeader { .. }), "{error:?}");
    assert!(!error.to_string().contains(TOKEN));
    assert!(!format!("{error:?}").contains(TOKEN));
    assert!(error.to_string().contains("whitespace or newlines"));
}

/// The endpoint itself gets logged — it is in every diagnostic dump — so its `Debug`
/// must not reach a value either. `SecretRef`'s `Debug` redacts; this asserts the
/// composition does too.
#[tokio::test]
async fn http_the_endpoint_debug_does_not_print_a_literal_token() {
    let endpoint = HttpEndpoint::new("trama", "https://example.invalid/mcp")
        .with_header("authorization", SecretRef::Literal(TOKEN.to_owned()));

    let debug = format!("{endpoint:?}");
    assert!(!debug.contains(TOKEN), "{debug}");
    assert!(debug.contains("redacted"), "{debug}");
}

/// Tracing output is where a secret most plausibly escapes, so the whole
/// connect-and-fail path runs under a subscriber that captures every event, and the
/// captured bytes are searched.
#[tokio::test]
async fn http_the_token_never_reaches_a_tracing_event() {
    use std::io::Write;

    /// A writer that appends everything to a shared buffer.
    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if let Ok(mut sink) = self.0.lock() {
                sink.extend_from_slice(buf);
            }
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
        type Writer = Self;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    let sink = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(Capture(Arc::clone(&sink)))
        .with_max_level(tracing::Level::TRACE)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    // A server that answers a *different* protocol version, so the warning path runs
    // too rather than only the happy one.
    let server = MockServer::start(vec![
        Reply::Json(rpc_result(
            1,
            serde_json::json!({
                "protocolVersion": "1999-01-01",
                "serverInfo": { "name": "old", "version": "0.1" },
            }),
        )),
        Reply::Json("{}".to_owned()),
        Reply::Status(500, "boom".to_owned()),
    ])
    .await
    .unwrap_or_else(|e| panic!("{e}"));

    let mut client =
        HttpClient::new(authed_endpoint(&server.url)).unwrap_or_else(|e| panic!("{e}"));
    let _ = client.list_tools(&keychain()).await;
    tracing::info!(client = ?client, "a diagnostic dump of the client");

    let captured = String::from_utf8_lossy(&sink.lock().unwrap_or_else(|e| panic!("{e}")).clone())
        .into_owned();

    assert!(
        !captured.is_empty(),
        "nothing was captured; the search proves nothing"
    );
    assert!(
        captured.contains("version differs"),
        "the version-skew warning did not fire, so this test is not walking the path \
         it claims to:\n{captured}"
    );
    assert!(
        !captured.contains(TOKEN),
        "the token reached a log line:\n{captured}"
    );
}

// --- criterion 3: non-2xx maps to typed, actionable errors --------------

#[tokio::test]
async fn http_401_and_403_are_separate_because_the_remedies_differ() {
    for (status, expected) in [(401_u16, "expired, or wrong"), (403, "scopes")] {
        let server = MockServer::start(vec![Reply::Status(status, "{}".to_owned())])
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let mut client =
            HttpClient::new(authed_endpoint(&server.url)).unwrap_or_else(|e| panic!("{e}"));

        let error = client
            .list_tools(&keychain())
            .await
            .expect_err("scripted to fail");

        assert!(matches!(error, HttpError::Unauthorized { .. }), "{error:?}");
        assert!(
            error.to_string().contains(expected),
            "status {status} did not give its own remedy — telling someone to \
             re-issue a token that already works wastes their afternoon:\n{error}"
        );
        assert_eq!(error.retryable(), Some(false));
    }
}

#[tokio::test]
async fn http_404_says_the_url_is_wrong_and_is_not_retried() {
    let server = MockServer::start(vec![Reply::Status(404, "no such endpoint".to_owned())])
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let mut client =
        HttpClient::new(authed_endpoint(&server.url)).unwrap_or_else(|e| panic!("{e}"));

    let error = client
        .list_tools(&keychain())
        .await
        .expect_err("scripted to fail");

    let message = error.to_string();
    assert!(message.contains("404"), "{message}");
    assert!(message.contains("URL"), "{message}");
    assert!(message.contains("try:"), "{message}");
    assert_eq!(
        error.retryable(),
        Some(false),
        "retrying a 404 loops forever"
    );
}

#[tokio::test]
async fn http_429_is_retryable_and_honours_retry_after() {
    let server = MockServer::start(vec![Reply::Status(429, "slow down".to_owned())])
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let mut client =
        HttpClient::new(authed_endpoint(&server.url)).unwrap_or_else(|e| panic!("{e}"));

    let error = client
        .list_tools(&keychain())
        .await
        .expect_err("scripted to fail");

    assert_eq!(error.retryable(), Some(true));
    // §11.6: honour what the server asked for rather than guessing a backoff.
    assert_eq!(error.retry_after_ms(), Some(7_000));
}

#[tokio::test]
async fn http_5xx_is_retryable_and_4xx_is_not() {
    for (status, retryable) in [(500_u16, true), (503, true), (400, false), (422, false)] {
        let server = MockServer::start(vec![Reply::Status(status, "x".to_owned())])
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let mut client =
            HttpClient::new(authed_endpoint(&server.url)).unwrap_or_else(|e| panic!("{e}"));

        let error = client
            .list_tools(&keychain())
            .await
            .expect_err("scripted to fail");

        assert_eq!(
            error.retryable(),
            Some(retryable),
            "status {status} was classified wrongly: {error}"
        );
    }
}

/// An HTML error page is 40 KB of nothing. The excerpt keeps enough to recognise the
/// problem and not enough to make the log unreadable.
#[tokio::test]
async fn http_a_huge_error_body_is_excerpted_not_pasted() {
    let body = "x".repeat(50_000);
    let server = MockServer::start(vec![Reply::Status(502, body)])
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let mut client =
        HttpClient::new(authed_endpoint(&server.url)).unwrap_or_else(|e| panic!("{e}"));

    let error = client
        .list_tools(&keychain())
        .await
        .expect_err("scripted to fail");

    let message = error.to_string();
    assert!(
        message.len() < 1_000,
        "the whole body was pasted: {} bytes",
        message.len()
    );
    assert!(
        message.contains('…'),
        "the truncation was not marked: {message}"
    );
}

#[tokio::test]
async fn http_a_dropped_connection_is_a_transport_error_and_drops_the_session() {
    let server = MockServer::start(vec![
        Reply::Json(rpc_result(1, initialize_result())),
        Reply::Json("{}".to_owned()),
        Reply::Hangup,
    ])
    .await
    .unwrap_or_else(|e| panic!("{e}"));

    let mut client =
        HttpClient::new(authed_endpoint(&server.url)).unwrap_or_else(|e| panic!("{e}"));

    let error = client
        .list_tools(&keychain())
        .await
        .expect_err("the server hung up");

    assert!(error.is_transport(), "{error:?}");
    assert_eq!(error.retryable(), Some(true));
    assert!(!client.is_connected(), "the dead session was kept");
}

// --- criterion 4: both content types ------------------------------------

#[tokio::test]
async fn http_reads_a_text_event_stream_reply() {
    let server = MockServer::start(vec![
        Reply::EventStream(vec![rpc_result(1, initialize_result())]),
        Reply::Json("{}".to_owned()),
        Reply::EventStream(vec![rpc_result(2, tools_result())]),
    ])
    .await
    .unwrap_or_else(|e| panic!("{e}"));

    let mut client =
        HttpClient::new(authed_endpoint(&server.url)).unwrap_or_else(|e| panic!("{e}"));
    let tools = client
        .list_tools(&keychain())
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "create_page");
}

/// A stream may carry progress events before the answer. Taking the **first** would
/// return a progress notification as the result — the same reasoning as §8.2 taking
/// the *last* fenced block of an engine's output.
#[tokio::test]
async fn http_takes_the_last_data_event_not_the_first() {
    let progress = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/progress",
        "params": { "progress": 1 },
    })
    .to_string();

    let server = MockServer::start(vec![
        Reply::EventStream(vec![progress.clone(), rpc_result(1, initialize_result())]),
        Reply::Json("{}".to_owned()),
        Reply::EventStream(vec![progress, rpc_result(2, tools_result())]),
    ])
    .await
    .unwrap_or_else(|e| panic!("{e}"));

    let mut client =
        HttpClient::new(authed_endpoint(&server.url)).unwrap_or_else(|e| panic!("{e}"));
    let tools = client
        .list_tools(&keychain())
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(tools.len(), 1, "a progress event was taken as the result");
}

#[tokio::test]
async fn http_sse_parsing_handles_the_shapes_a_server_may_send() {
    use revlocal_mcp::parse_sse;

    assert_eq!(
        parse_sse("data: {\"a\":1}\n\n").as_deref(),
        Some(r#"{"a":1}"#)
    );
    // No space after the colon is legal.
    assert_eq!(
        parse_sse("data:{\"a\":1}\n\n").as_deref(),
        Some(r#"{"a":1}"#)
    );
    // CRLF line endings.
    assert_eq!(
        parse_sse("event: message\r\ndata: {\"a\":1}\r\n\r\n").as_deref(),
        Some(r#"{"a":1}"#)
    );
    // A multi-line data field is joined with newlines, per the SSE spec.
    assert_eq!(
        parse_sse("data: {\ndata: \"a\":1}\n\n").as_deref(),
        Some("{\n\"a\":1}")
    );
    // No trailing blank line: the last event still counts.
    assert_eq!(parse_sse("data: {\"a\":1}").as_deref(), Some(r#"{"a":1}"#));
    // Nothing usable.
    assert!(parse_sse("event: ping\n\n").is_none());
    assert!(parse_sse("").is_none());
}

#[tokio::test]
async fn http_an_event_stream_with_no_data_is_a_readable_error() {
    let server = MockServer::start(vec![Reply::EventStream(Vec::new())])
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let mut client =
        HttpClient::new(authed_endpoint(&server.url)).unwrap_or_else(|e| panic!("{e}"));

    let error = client
        .list_tools(&keychain())
        .await
        .expect_err("an empty stream is not a reply");

    assert!(matches!(error, HttpError::Malformed { .. }), "{error:?}");
    assert!(error.to_string().contains("event stream"));
}
