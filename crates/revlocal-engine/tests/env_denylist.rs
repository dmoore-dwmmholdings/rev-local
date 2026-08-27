//! Acceptance tests for `RL-406` — the engine environment denylist (SPEC §8.5).
//!
//! §8.5's reasoning is worth quoting, because it is the whole justification:
//! *"the review engine has no business acting on remotes; only rev-local's publish
//! layer does."*
//!
//! A review engine holding a `GITHUB_TOKEN` can push. That is not a hypothetical
//! risk with an AI agent that has been asked to find problems and may decide to fix
//! one. The denylist is the boundary that makes it structurally unable to.
//!
//! # These tests are in a file named for the gate
//!
//! `RL-406`'s gate is `cargo test -p revlocal-engine env_denylist`. When this work
//! first landed inside `RL-405`, the tests were named `supervision_*` — and the gate
//! selected **zero tests and exited 0**. A filter that matches nothing is the
//! quietest way for a gate to pass while testing nothing, and it is worth being
//! deliberate about names for that reason alone.

mod env_denylist {
    use revlocal_engine::supervise::{filtered_env, is_denied, supervise};
    use revlocal_engine::template::Invocation;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
    }

    /// Run `env` under supervision with `env_vars` and return what the child saw.
    ///
    /// Returns `Result`; helpers are not `#[test]` fns (ADR 0003).
    async fn child_environment(env_vars: &BTreeMap<String, String>) -> Result<Vec<String>, String> {
        let invocation = Invocation {
            program: "env".to_owned(),
            args: Vec::new(),
            stdin: None,
        };

        let result = supervise(
            revlocal_core::EngineKind::Mock,
            &invocation,
            &workspace_root(),
            env_vars,
            Duration::from_secs(10),
            &CancellationToken::new(),
        )
        .await
        .map_err(|e| format!("supervise: {e}"))?;

        Ok(result.stdout.lines().map(str::to_owned).collect())
    }

    /// A parent environment carrying a realistic mix of secrets and necessities.
    fn parent_env() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("PATH".to_owned(), std::env::var("PATH").unwrap_or_default()),
            ("HOME".to_owned(), std::env::var("HOME").unwrap_or_default()),
            ("LANG".to_owned(), "C.UTF-8".to_owned()),
            (
                "GITHUB_TOKEN".to_owned(),
                "ghp_must_not_reach_the_engine".to_owned(),
            ),
            (
                "GH_TOKEN".to_owned(),
                "gho_must_not_reach_the_engine".to_owned(),
            ),
            (
                "OPENAI_API_KEY".to_owned(),
                "sk-must_not_reach_the_engine".to_owned(),
            ),
            (
                "TRAMA_SECRET".to_owned(),
                "must_not_reach_the_engine".to_owned(),
            ),
            (
                "DB_PASSWORD".to_owned(),
                "must_not_reach_the_engine".to_owned(),
            ),
        ])
    }

    // --- the child's actual environment ---------------------------------------

    #[tokio::test]
    async fn env_denylist_a_github_token_is_absent_from_the_child() {
        // Acceptance criteria 1 and 3 together. This runs `env` under the real
        // supervisor and reads what the child printed — the builder's intent is not
        // the same thing as the child's environment, and only one of them is what an
        // engine can read.
        let filtered = filtered_env(
            parent_env().iter().map(|(k, v)| (k.as_str(), v.as_str())),
            &[],
        );
        let seen = child_environment(&filtered)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        let joined = seen.join("\n");
        assert!(
            !joined.contains("must_not_reach_the_engine"),
            "a secret reached the child:\n{joined}"
        );
        assert!(
            !seen.iter().any(|line| line.starts_with("GITHUB_TOKEN=")),
            "GITHUB_TOKEN is present in the child's environment"
        );
    }

    #[tokio::test]
    async fn env_denylist_the_variables_a_cli_needs_still_arrive() {
        // The other half, and the one that breaks everything if it fails: D9 says
        // engines authenticate via the user's existing CLI logins, which need HOME
        // to find their credential files and PATH to run at all. A denylist that
        // took those would leave every engine unusable and the cause would look like
        // an engine bug.
        let filtered = filtered_env(
            parent_env().iter().map(|(k, v)| (k.as_str(), v.as_str())),
            &[],
        );
        let seen = child_environment(&filtered)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        for required in ["PATH=", "HOME=", "LANG="] {
            assert!(
                seen.iter().any(|line| line.starts_with(required)),
                "{required} did not reach the child: {seen:?}"
            );
        }
    }

    #[tokio::test]
    async fn env_denylist_pass_env_lets_a_named_variable_through_to_the_child() {
        // Acceptance criterion 2, asserted at the child rather than at the filter —
        // an escape hatch that worked in the builder and not in the process would be
        // the most frustrating possible bug.
        let filtered = filtered_env(
            parent_env().iter().map(|(k, v)| (k.as_str(), v.as_str())),
            &["GITHUB_TOKEN".to_owned()],
        );
        let seen = child_environment(&filtered)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        assert!(
            seen.iter()
                .any(|line| line == "GITHUB_TOKEN=ghp_must_not_reach_the_engine"),
            "pass_env did not let the named variable through: {seen:?}"
        );
        assert!(
            !seen.iter().any(|line| line.starts_with("GH_TOKEN=")),
            "allowing one variable must not allow the others"
        );
    }

    #[tokio::test]
    async fn env_denylist_the_environment_is_cleared_not_merely_overridden() {
        // The difference is invisible until something reads it. If the child
        // inherited the parent's environment and the filtered map were layered on
        // top, a withheld variable would still be there — just not in the map.
        //
        // This test process genuinely has variables the map below does not, so if
        // anything leaks through, it shows up here.
        let minimal = BTreeMap::from([
            ("PATH".to_owned(), std::env::var("PATH").unwrap_or_default()),
            ("ONLY_THIS".to_owned(), "yes".to_owned()),
        ]);
        let seen = child_environment(&minimal)
            .await
            .unwrap_or_else(|e| panic!("{e}"));

        let names: Vec<&str> = seen
            .iter()
            .filter_map(|line| line.split('=').next())
            .collect();

        assert!(names.contains(&"ONLY_THIS"));
        assert!(
            !names.contains(&"CARGO_PKG_NAME"),
            "the child inherited the parent's environment rather than being given \
             one: {names:?}"
        );
    }

    // --- the rule itself --------------------------------------------------------

    #[test]
    fn env_denylist_covers_everything_spec_8_5_names() {
        // A guard on the rule rather than on one call: emptying the denylist would
        // otherwise leave every test above passing, because they assert on specific
        // variables and would simply see them absent for a different reason.
        for named in ["GITHUB_TOKEN", "GH_TOKEN"] {
            assert!(is_denied(named, &[]), "§8.5 names {named} explicitly");
        }
        for pattern in ["ANYTHING_API_KEY", "ANYTHING_SECRET", "ANYTHING_PASSWORD"] {
            assert!(
                is_denied(pattern, &[]),
                "§8.5's suffix rules must catch {pattern}"
            );
        }
    }

    #[test]
    fn env_denylist_uses_suffixes_rather_than_a_fixed_list() {
        // The interesting names follow whatever service a user happens to use, so a
        // fixed list would always be one service behind.
        for invented in [
            "SOME_STARTUP_NOBODY_HAS_HEARD_OF_API_KEY",
            "INTERNAL_TOOL_SECRET",
            "LEGACY_SYSTEM_PASSWORD",
        ] {
            assert!(
                is_denied(invented, &[]),
                "{invented} should be caught by a suffix"
            );
        }
    }

    #[test]
    fn env_denylist_is_case_insensitive() {
        // Environment names are conventionally uppercase but not required to be, and
        // a lowercase `github_token` is the same secret.
        assert!(is_denied("github_token", &[]));
        assert!(is_denied("My_Api_Key", &[]));
        assert!(is_denied("db_password", &[]));
    }

    #[test]
    fn env_denylist_does_not_catch_ordinary_variables() {
        // Over-blocking has a cost too: an engine missing PATH or a proxy setting
        // fails in a way that looks like the engine's fault.
        for ordinary in [
            "PATH",
            "HOME",
            "LANG",
            "TERM",
            "SHELL",
            "TMPDIR",
            "RUST_LOG",
            "HTTP_PROXY",
            "NO_PROXY",
            "TZ",
            "USER",
        ] {
            assert!(
                !is_denied(ordinary, &[]),
                "{ordinary} must reach the engine"
            );
        }
    }

    #[test]
    fn env_denylist_pass_env_is_per_variable_not_a_master_switch() {
        assert!(!is_denied("GITHUB_TOKEN", &["GITHUB_TOKEN".to_owned()]));
        assert!(is_denied("OPENAI_API_KEY", &["GITHUB_TOKEN".to_owned()]));
        assert!(
            is_denied("GITHUB_TOKEN", &["github_token".to_owned()]),
            "pass_env matches exactly; a near-miss must not silently allow a secret"
        );
    }

    #[test]
    fn env_denylist_a_withheld_credential_is_reported_rather_than_silently_dropped() {
        // The sharp edge of §8.5. D9 says engines authenticate via existing CLI
        // logins, and the common case is a credential file — which needs HOME, and
        // HOME is passed. But a user who authenticates with an API key will find the
        // engine unauthenticated, with NOTHING connecting that to rev-local
        // withholding a variable they set themselves.
        //
        // Withholding is correct. Doing it silently turns a two-word fix into an
        // afternoon of confusion.
        use revlocal_engine::supervise::{withheld_auth_remediation, withheld_auth_variables};

        let source = [
            ("PATH", "/usr/bin"),
            ("ANTHROPIC_API_KEY", "sk-ant-something"),
            ("SOME_OTHER_SECRET", "irrelevant"),
        ];

        let withheld = withheld_auth_variables(source, &[]);
        assert_eq!(
            withheld,
            ["ANTHROPIC_API_KEY"],
            "an unrelated secret is not an engine credential and should not be \
             reported as one"
        );

        let advice = withheld_auth_remediation("ANTHROPIC_API_KEY", "claude");
        assert!(advice.contains("pass_env"), "{advice}");
        assert!(
            advice.contains("engines.claude"),
            "the advice must name the key to edit"
        );
        assert!(
            advice.contains("§8.5"),
            "and say why it is withheld, so it does not read as a bug: {advice}"
        );
    }

    #[test]
    fn env_denylist_a_credential_already_allowed_is_not_reported_as_withheld() {
        use revlocal_engine::supervise::withheld_auth_variables;

        let source = [("ANTHROPIC_API_KEY", "sk-ant-something")];
        assert!(
            withheld_auth_variables(source, &["ANTHROPIC_API_KEY".to_owned()]).is_empty(),
            "once pass_env allows it, there is nothing to warn about"
        );
    }

    #[test]
    fn env_denylist_widens_spec_8_5_with_a_token_suffix() {
        // A deliberate widening, recorded rather than assumed. §8.5 names
        // GITHUB_TOKEN and GH_TOKEN explicitly and gives suffixes for API_KEY,
        // SECRET and PASSWORD — but not TOKEN, so `NPM_TOKEN` or `TRAMA_TOKEN` would
        // pass through. Widening errs towards withholding, and pass_env is the way
        // back for anyone who needs one.
        assert!(is_denied("NPM_TOKEN", &[]));
        assert!(is_denied("TRAMA_TOKEN", &[]));
        assert!(!is_denied("NPM_TOKEN", &["NPM_TOKEN".to_owned()]));
    }
}
