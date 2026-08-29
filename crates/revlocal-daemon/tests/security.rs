//! The standing security review (RL-1304, SPEC §8.5, §13.1, §18).
//!
//! §18's rule for this project is that a claim is only worth what verifies it, so
//! this file exists to keep five security properties from being things somebody
//! once checked. It is a review that runs, not a review that happened.
//!
//! Two of the five already have dedicated suites and are not re-tested here:
//! `env_denylist.rs` spawns a real child and asserts a token in the parent's
//! environment does not reach it, and `redaction.rs` drives the real tracing layer
//! over a real sink. What those cannot say is whether the *pieces fit* — a secret
//! can be redacted in the logger and still reach a transcript, or be lazy in
//! `SecretRef` and eager in the code that reads config. That is what this adds.
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

mod security {
    use std::sync::{Arc, Mutex};

    use revlocal_core::{redact, redact_field, SecretRef, REDACTED};
    use revlocal_daemon::RedactingJsonLayer;
    use tracing::subscriber::with_default;
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::Registry;

    /// Structurally real, obviously fake. The shape matters: a redactor that only
    /// matched the literal string "secret" would pass a weaker fixture.
    const PLANTED: &str = "ghp_ZZ9y8X7w6V5u4T3s2R1q0P9o8N7m6L5k4J3i";
    const PLANTED_WEBHOOK: &str = "whsec_aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789";

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

    fn capture(body: impl FnOnce()) -> String {
        let sink = Captured::default();
        let subscriber = Registry::default().with(RedactingJsonLayer::new(sink.clone()));
        with_default(subscriber, body);
        sink.text()
    }

    // --- 1. a planted secret reaches no log -------------------------------

    #[test]
    fn security_a_planted_config_secret_reaches_no_log_line() {
        // Criterion 3, and the reason it is worth testing separately from
        // `redaction.rs`: that suite proves the *layer* redacts. This plants a
        // secret the way a user would — in a config value — and pushes it through
        // the shapes real code logs it in: as a field, inside a message, and
        // inside a serialised structure.
        let config_value = SecretRef::parse(PLANTED);

        let logged = capture(|| {
            // As a named field.
            tracing::info!(webhook_secret = PLANTED, "configuring repo");
            // Interpolated into a message, which is how a hurried error path does it.
            tracing::warn!("could not authenticate with {PLANTED}");
            // Inside a Debug rendering of the config type that holds it.
            tracing::info!(config = ?config_value, "loaded");
            // And in a field whose name gives no hint that it is sensitive.
            tracing::info!(detail = PLANTED, "context");
        });

        assert!(
            !logged.contains(PLANTED),
            "the planted secret reached the log:\n{logged}"
        );
        assert!(
            !logged.is_empty(),
            "nothing was logged; the test proves nothing"
        );
    }

    #[test]
    fn security_a_secret_is_redacted_by_shape_not_only_by_field_name() {
        // A denylist of field names is the version of this that looks right and
        // fails the first time somebody logs a secret as `detail` or `arg`. Both
        // paths are checked, because either alone would let the other rot.
        // The marker itself is the crate's constant, not a literal — a test that
        // hard-codes it fails on a cosmetic change and passes on a real one.
        assert_eq!(redact_field("github_token", PLANTED), REDACTED);
        assert!(
            !redact(&format!("bearer {PLANTED}")).contains(PLANTED),
            "a token with a known shape must be redacted in free text"
        );

        // And the other half: a secret with *no* recognisable shape is caught by
        // the field it is logged in.
        assert_eq!(redact_field("webhook_secret", PLANTED_WEBHOOK), REDACTED);
    }

    #[test]
    fn security_a_shapeless_secret_in_an_innocent_field_is_a_known_limit() {
        // A finding of this review, recorded rather than papered over.
        //
        // Shape-matching recognises credentials with a known prefix — GitHub's
        // `ghp_`, `ghs_` and the rest. A **user-chosen** secret, which is what a
        // GitHub webhook secret is, has no prefix to match. If one is logged in a
        // field whose name carries no marker, neither mechanism catches it:
        assert!(
            redact(PLANTED_WEBHOOK).contains(PLANTED_WEBHOOK),
            "if this starts failing, shape coverage grew and this test should say so"
        );
        assert_eq!(redact_field("detail", PLANTED_WEBHOOK), PLANTED_WEBHOOK);

        // The mitigation is that SENSITIVE_FIELD_MARKERS is deliberately broad —
        // `secret`, `token`, `credential`, `password` and more as substrings — so
        // any field a person would plausibly name for a secret is covered.
        for name in [
            "webhook_secret",
            "repo_secret_ref",
            "GITHUB_TOKEN",
            "api_key",
            "authorization",
        ] {
            assert_eq!(
                redact_field(name, PLANTED_WEBHOOK),
                REDACTED,
                "{name} should be treated as sensitive"
            );
        }

        // The residual risk is a shapeless secret logged under a name nobody would
        // call sensitive. That is a discipline the field-name list cannot enforce,
        // and it is why `security_a_planted_config_secret_reaches_no_log_line`
        // exercises the paths real code actually logs secrets through.
    }

    #[test]
    fn security_redaction_leaves_ordinary_text_alone() {
        // A redactor that eats everything is one people turn off. This is what
        // makes the assertions above mean something.
        let ordinary = "reviewing commit deadbeef on branch main (3 files)";
        assert_eq!(redact(ordinary), ordinary);
    }

    // --- 2. keychain resolution is lazy -----------------------------------

    #[test]
    fn security_a_keychain_reference_is_not_resolved_by_reading_config() {
        // §13.1: config carries `{{keychain:name}}` placeholders, resolved at
        // connect time. Eager resolution would put the secret in memory for every
        // configured target whether or not it is ever used — and, worse, would
        // make merely *loading* config prompt for keychain access.
        let deferred = SecretRef::parse("{{keychain:github-token}}");

        assert!(
            deferred.is_deferred(),
            "the placeholder must stay unresolved"
        );
        assert_eq!(deferred.keychain_name(), Some("github-token"));

        // The *name* is not the secret, and hiding it would make a missing
        // keychain entry undiagnosable.
        let rendered = format!("{deferred:?}");
        assert!(
            rendered.contains("github-token"),
            "the entry name should stay visible: {rendered}"
        );
    }

    #[test]
    fn security_a_literal_secret_in_config_is_still_treated_as_one() {
        // A user who pastes a token directly instead of using the keychain has
        // made a mistake, not an exemption. It is still redacted when formatted.
        let literal = SecretRef::parse(PLANTED);

        assert!(!literal.is_deferred());
        assert!(
            !format!("{literal:?}").contains(PLANTED),
            "a literal secret leaked through Debug"
        );
    }

    // --- 4. signature verification is constant-time ------------------------

    #[test]
    fn security_no_prefix_of_a_valid_signature_is_accepted() {
        // §7.3. Timing the property on a shared runner is a flaky test, so this
        // asserts what a short-circuiting comparison would break: a prefix must
        // never verify, at any length.
        use revlocal_daemon::webhook::{sign, verify_signature};

        let body = br#"{"repository":{"full_name":"acme/api"}}"#;
        let valid = sign(PLANTED_WEBHOOK, body);
        assert!(verify_signature(PLANTED_WEBHOOK, body, &valid));

        let hex = valid.trim_start_matches("sha256=");
        for length in (2..hex.len()).step_by(2) {
            assert!(
                !verify_signature(PLANTED_WEBHOOK, body, &format!("sha256={}", &hex[..length])),
                "a {length}-character prefix verified"
            );
        }

        // And the secret itself is not recoverable from a rejection.
        let wrong = sign("not-the-secret", body);
        assert!(!verify_signature(PLANTED_WEBHOOK, body, &wrong));
    }

    // --- 5. no shell interpolation of untrusted strings --------------------

    /// Shapes that mean a string is being handed to a shell to parse.
    const SHELL_SPAWNS: &[&str] = &[
        r#"Command::new("sh")"#,
        r#"Command::new("bash")"#,
        r#"Command::new("cmd")"#,
        r#"Command::new("powershell")"#,
        r#"Command::new("/bin/sh")"#,
    ];

    #[test]
    fn security_nothing_hands_a_string_to_a_shell_to_parse() {
        // Every external tool in this project is invoked as a program with an
        // argument *vector*, so a branch called `--upload-pack=…` or a repo path
        // with a space in it is one argument rather than several commands.
        //
        // This scans production source only. Test harnesses legitimately run
        // `bash fixtures/build.sh`, and that is a path this project wrote, not an
        // interpolated untrusted string.
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");

        let mut offenders = Vec::new();
        let mut scanned = 0_usize;

        let Ok(crates) = std::fs::read_dir(root.join("crates")) else {
            panic!("the crates directory must be readable");
        };
        let mut roots: Vec<std::path::PathBuf> = crates
            .flatten()
            .map(|entry| entry.path().join("src"))
            .filter(|path| path.is_dir())
            .collect();
        roots.sort();

        for dir in roots {
            walk(&dir, &mut scanned, &mut offenders);
        }

        assert!(
            scanned > 30,
            "the scanner found only {scanned} source files; it has stopped walking"
        );
        assert!(
            offenders.is_empty(),
            "external tools are invoked with an argument vector, never through a \
             shell — a branch name or path is data, not syntax:\n  {}",
            offenders.join("\n  ")
        );
    }

    fn walk(dir: &std::path::Path, scanned: &mut usize, offenders: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, scanned, offenders);
                continue;
            }
            if path.extension().is_none_or(|ext| ext != "rs") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            *scanned += 1;
            for (number, line) in text.lines().enumerate() {
                // Prose about the rule is not a violation of it.
                let code = line.split("//").next().unwrap_or(line);
                if SHELL_SPAWNS.iter().any(|shape| code.contains(shape)) {
                    offenders.push(format!("{}:{}", path.display(), number + 1));
                }
            }
        }
    }

    #[test]
    fn security_the_shell_scanner_still_detects() {
        // A scanner that has quietly stopped matching passes forever while
        // checking nothing — twice today that was a real bug in a guard I wrote.
        let sample =
            r#"    let out = Command::new("bash").arg("-c").arg(format!("git log {rev}"));"#;
        assert!(SHELL_SPAWNS.iter().any(|shape| sample.contains(shape)));

        let ordinary = r#"    let out = Command::new("git").args(["log", rev]);"#;
        assert!(!SHELL_SPAWNS.iter().any(|shape| ordinary.contains(shape)));
    }
}
