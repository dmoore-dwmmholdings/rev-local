//! Which engine actually runs a review (RL-1206, SPEC §8.4, decision D3).
//!
//! # The choice had no effect
//!
//! D3 puts the engine on the repository rather than globally, `repo add` writes
//! it and `repo show` reports it — and until this module nothing read it. Every
//! review ran the mock. `revlocal review` said so on stderr, which is what kept
//! it a gap rather than a lie, but `--json` could not see that line and a screen
//! asking somebody to pick an engine before showing them a mock result would be
//! telling them something untrue about what just ran.
//!
//! # A configured template beats the default, and a broken one is an error
//!
//! §8.4's whole point is that invocations are config-driven "because CLI flags
//! drift". So a `[engines.claude]` table in config replaces the shipped default.
//! What it must not do is *silently* replace it with something unusable: a
//! malformed table returns an error naming the key, because the alternative is
//! falling back to a default that works and leaving somebody's deliberate
//! override quietly doing nothing.

use revlocal_core::{EngineKind, GlobalConfig};
use revlocal_engine::{CliEngine, Engine, InvocationTemplate, MockEngine};

/// Why an engine could not be built.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EngineError {
    /// A `[engines.*]` table has a key of the wrong shape.
    #[error("[engines.{engine}] has `{key}` as {found}, which should be {wanted}\n  try: fix it in your config, or delete the key to use the default")]
    BadKey {
        /// Which engine's table.
        engine: String,
        /// Which key.
        key: String,
        /// What was there.
        found: String,
        /// What it should be.
        wanted: String,
    },

    /// The configured template names no binary.
    #[error("[engines.{engine}] sets `bin` to an empty string, so there is nothing to run\n  try: set bin, or delete the key to use §8.4's default")]
    NoBinary {
        /// Which engine's table.
        engine: String,
    },
}

/// Read one engine's invocation template (§8.4).
///
/// Config first, §8.4's shipped default second. `bin` is accepted under its spec
/// name and under `binary`, because §13.1's own example document uses the latter —
/// a config that the spec prints and the parser rejects would be the spec's fault
/// to a user and the parser's fault in fact.
pub fn template_for(
    kind: EngineKind,
    config: &GlobalConfig,
) -> Result<InvocationTemplate, EngineError> {
    let name = kind.as_str();
    let default = InvocationTemplate::default_for(kind).unwrap_or_default();

    let Some(table) = config.engines.get(name).and_then(toml::Value::as_table) else {
        return Ok(default);
    };

    let string = |key: &str| -> Result<Option<String>, EngineError> {
        match table.get(key) {
            None => Ok(None),
            Some(toml::Value::String(text)) => Ok(Some(text.clone())),
            Some(other) => Err(EngineError::BadKey {
                engine: name.to_owned(),
                key: key.to_owned(),
                found: other.type_str().to_owned(),
                wanted: "a string".to_owned(),
            }),
        }
    };

    let strings = |key: &str| -> Result<Option<Vec<String>>, EngineError> {
        match table.get(key) {
            None => Ok(None),
            Some(toml::Value::Array(items)) => items
                .iter()
                .map(|item| {
                    item.as_str().map(str::to_owned).ok_or(EngineError::BadKey {
                        engine: name.to_owned(),
                        key: key.to_owned(),
                        found: item.type_str().to_owned(),
                        wanted: "an array of strings".to_owned(),
                    })
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Some),
            Some(other) => Err(EngineError::BadKey {
                engine: name.to_owned(),
                key: key.to_owned(),
                found: other.type_str().to_owned(),
                wanted: "an array of strings".to_owned(),
            }),
        }
    };

    let boolean = |key: &str| -> Result<Option<bool>, EngineError> {
        match table.get(key) {
            None => Ok(None),
            Some(toml::Value::Boolean(value)) => Ok(Some(*value)),
            Some(other) => Err(EngineError::BadKey {
                engine: name.to_owned(),
                key: key.to_owned(),
                found: other.type_str().to_owned(),
                wanted: "true or false".to_owned(),
            }),
        }
    };

    let bin = string("bin")?.or(string("binary")?).unwrap_or(default.bin);
    if bin.trim().is_empty() {
        return Err(EngineError::NoBinary {
            engine: name.to_owned(),
        });
    }

    Ok(InvocationTemplate {
        bin,
        args: strings("args")?.unwrap_or(default.args),
        version_args: strings("version_args")?.unwrap_or(default.version_args),
        stdin_prompt: boolean("stdin_prompt")?.unwrap_or(default.stdin_prompt),
        pass_env: strings("pass_env")?.unwrap_or(default.pass_env),
    })
}

/// The engine a repository's `engine` column names (D3).
///
/// Boxed because the two implementations are genuinely different types and the
/// caller does not care which — that is the whole point of D3 being a per-repo
/// setting rather than a compile-time one.
pub fn for_kind(kind: EngineKind, config: &GlobalConfig) -> Result<Box<dyn Engine>, EngineError> {
    match kind {
        // The mock is a real choice, not a fallback. §16.1 runs the whole suite
        // against it, and a repository set to `mock` wants the mock.
        EngineKind::Mock => Ok(Box::new(MockEngine::new())),
        other => Ok(Box::new(CliEngine::new(
            other,
            template_for(other, config)?,
        ))),
    }
}

/// Whether a review with this engine spends anything.
///
/// So a caller can say which it used, and mean it. A result that came from the
/// mock and looked like a real one would make the first thing a new user sees a
/// lie about their own machine.
pub const fn is_mock(kind: EngineKind) -> bool {
    matches!(kind, EngineKind::Mock)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(toml_text: &str) -> GlobalConfig {
        GlobalConfig::parse(toml_text).map_or_else(|_| GlobalConfig::default(), |(c, _)| c)
    }

    #[test]
    fn engines_a_repository_set_to_claude_gets_claude() {
        // D3, which had no effect until this existed.
        let engine = for_kind(EngineKind::Claude, &GlobalConfig::default()).expect("built");

        assert_eq!(engine.id(), EngineKind::Claude);
    }

    #[test]
    fn engines_the_shipped_default_is_the_one_in_the_spec() {
        let template = template_for(EngineKind::Claude, &GlobalConfig::default()).expect("built");

        assert_eq!(template.bin, "claude");
        // §8.4's args verbatim, including the placeholder that decides whether the
        // prompt goes in argv or a file.
        assert!(template.args.contains(&"{prompt_file_content}".to_owned()));
    }

    #[test]
    fn engines_config_overrides_the_default_template() {
        // §8.4: "invocations are config-driven templates, not hardcoded, because
        // CLI flags drift". An override that did nothing would defeat the reason
        // the templates are in config at all.
        let config = config(
            r#"
[engines.claude]
bin = "claude-next"
args = ["--review", "{cwd}"]
stdin_prompt = true
"#,
        );

        let template = template_for(EngineKind::Claude, &config).expect("built");

        assert_eq!(template.bin, "claude-next");
        assert_eq!(template.args, vec!["--review", "{cwd}"]);
        assert!(template.stdin_prompt);
        // Keys the override left out keep §8.4's default rather than becoming
        // empty: a partial override is a partial override.
        assert_eq!(template.version_args, vec!["--version"]);
    }

    #[test]
    fn engines_the_spec_document_own_spelling_is_accepted() {
        // §13.1's example document writes `binary`, §8.4 writes `bin`. A config
        // the spec prints and the parser ignores would be the spec's fault to a
        // user and this parser's fault in fact.
        let config = config(
            r#"
[engines.claude]
binary = "claude"
timeout_secs = 600
"#,
        );

        let template = template_for(EngineKind::Claude, &config).expect("built");
        assert_eq!(template.bin, "claude");
    }

    #[test]
    fn engines_a_malformed_table_is_an_error_not_a_silent_default() {
        // The failure worth guarding: falling back to a working default leaves
        // somebody's deliberate override doing nothing, and nothing says so.
        let config = config(
            r#"
[engines.claude]
args = "not an array"
"#,
        );

        let error = template_for(EngineKind::Claude, &config).expect_err("refused");

        let text = error.to_string();
        assert!(text.contains("args"), "{text}");
        assert!(text.contains("engines.claude"), "{text}");
    }

    #[test]
    fn engines_an_empty_binary_is_refused() {
        let config = config(
            r#"
[engines.codex]
bin = ""
"#,
        );

        assert_eq!(
            template_for(EngineKind::Codex, &config),
            Err(EngineError::NoBinary {
                engine: "codex".to_owned()
            })
        );
    }

    #[test]
    fn engines_the_mock_is_a_choice_not_a_fallback() {
        // §16.1 runs the whole suite against it, and a repository set to `mock`
        // wants the mock — so it is reachable by name and identifiable after.
        let engine = for_kind(EngineKind::Mock, &GlobalConfig::default()).expect("built");

        assert_eq!(engine.id(), EngineKind::Mock);
        assert!(is_mock(EngineKind::Mock));
        assert!(!is_mock(EngineKind::Claude));
    }

    #[test]
    fn engines_an_unrelated_engines_table_does_not_affect_this_one() {
        let config = config(
            r#"
[engines.codex]
bin = "codex-next"
"#,
        );

        assert_eq!(
            template_for(EngineKind::Claude, &config)
                .expect("built")
                .bin,
            "claude"
        );
    }
}
