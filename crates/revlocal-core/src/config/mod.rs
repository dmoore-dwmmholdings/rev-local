//! Configuration documents, defaults and validation (SPEC §13).
//!
//! Three documents feed the effective configuration:
//!
//! 1. the global TOML at `{config_dir}/rev-local/config.toml` (§13.1);
//! 2. the per-repo JSON in `repo.config_json` (§13.2);
//! 3. an optional in-repo `.rev-local.toml` at the repository root.
//!
//! This module owns all three, their defaults, the handling of keys a given version
//! does not recognise, and the rule that a repository must not be able to grant
//! itself more authority than it was given. See [`overlay`] for that rule.
//!
//! **Unknown keys warn, they do not fail.** A config written for a newer rev-local
//! has to start an older one, and a user's typo should be reported rather than
//! refuse to boot. The warning is what keeps that from being a silent cap (§18):
//! the key is named, so an ignored setting is visible instead of merely inert.

mod global;
mod overlay;
mod repo;
mod secret;

pub use global::{BudgetSettings, GlobalConfig, GlobalSettings, McpServerSettings, OnExhausted};
pub use overlay::{
    effective_autonomy, merge_in_repo, ConfigError, InRepoConfig, MergeOutcome, PERMITTED_KEYS,
};
pub use repo::RepoConfig;
pub use secret::SecretRef;

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Keys captured from a document that this version of rev-local does not define.
///
/// Held as `serde_json::Value` for both TOML and JSON documents so one type covers
/// both; the values are only ever counted and named, never interpreted.
pub type Extra = BTreeMap<String, serde_json::Value>;

/// Something worth telling the user about a config document that still loaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConfigWarning {
    /// A key that this version does not recognise.
    UnknownKey {
        /// Dotted path to the key, e.g. `global.max_concurent_runs`.
        path: String,
    },
}

impl ConfigWarning {
    /// A one-line message for a log or the UI.
    pub fn message(&self) -> String {
        match self {
            Self::UnknownKey { path } => {
                format!("unknown config key `{path}`; it was ignored")
            }
        }
    }

    /// The dotted path this warning concerns.
    pub fn path(&self) -> &str {
        match self {
            Self::UnknownKey { path } => path,
        }
    }
}

/// Turn a table of unrecognised keys into warnings, prefixed with `section`.
fn collect_unknown_keys(extra: &Extra, section: &str, warnings: &mut Vec<ConfigWarning>) {
    for key in extra.keys() {
        let path = if section.is_empty() {
            key.clone()
        } else {
            format!("{section}.{key}")
        };
        warnings.push(ConfigWarning::UnknownKey { path });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AutonomyMode, Category, EngineKind, Severity};

    /// The `[global]` and `[budgets]` document exactly as SPEC §13.1 prints it.
    const SPEC_13_1: &str = r#"
[global]
mode = "auto_low_ask_high"
max_concurrent_runs = 2
coalesce_window_ms = 1500
trigger_port = 41791
webhook_port = 0
transcript_retention_days = 30
keep_scratch_on_failure = true
stale_run_minutes = 10
max_attempts = 3
approval_ttl_hours = 72
burst_threshold = 10

[budgets]
daily_tokens_per_repo = 2000000
daily_runs_per_repo = 200
daily_cost_usd_per_repo = 0
on_exhausted = "pause"

[mcpServers.github]
type = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]

[mcpServers.trama]
type = "http"
url = "https://trama.example.com/mcp"
"#;

    /// The per-repo document exactly as SPEC §13.2 prints it, comments removed.
    const SPEC_13_2: &str = r#"{
  "branches": ["main", "release/*"],
  "review_prs": true,
  "review_commits": false,
  "review_draft_prs": false,
  "review_merge_commits": false,
  "watch_branches": true,
  "poll_interval_secs": 120,
  "scope": ["correctness", "security", "convention", "tests"],
  "engine": "claude",
  "autonomy": "auto_low_ask_high",
  "ignore_globs": ["**/node_modules/**", "**/vendor/**", "**/*.lock",
                   "**/dist/**", "**/*.min.*", "**/target/**"],
  "ignore_authors": ["dependabot[bot]", "renovate[bot]"],
  "sensitive_globs": ["**/auth/**", "**/crypto/**", "**/*.sql", ".github/workflows/**"],
  "deep_file_limit": 150,
  "deep_labels": [],
  "convention_files": ["CLAUDE.md", "AGENTS.md", "CONTRIBUTING.md"],
  "targets": ["github", "andare", "trama"],
  "andare_min_severity": "high",
  "andare_key_regex": "[A-Z][A-Z0-9]+-\\d+",
  "trama_publish": false,
  "max_convention_bytes": 24576,
  "max_file_diff_bytes": 65536,
  "max_total_diff_bytes": 524288,
  "webhook_enabled": false,
  "webhook_secret_ref": null,
  "block_on_findings": false,
  "allow_approve": false,
  "merge_detect_regex": "(?i)\\b(merge|reintegrat\\w+)\\b.*\\b(branches?/[\\w./-]+)"
}"#;

    #[test]
    fn the_global_defaults_are_the_spec_13_1_document() {
        // Parsing the spec's own document must produce exactly the defaults. If
        // they diverge, a user who deletes their config gets different behaviour
        // from one who copies the document out of the spec.
        let (parsed, warnings) = GlobalConfig::parse(SPEC_13_1).unwrap_or_else(|e| panic!("{e}"));
        assert!(warnings.is_empty(), "the spec document warns: {warnings:?}");

        let defaults = GlobalSettings::default();
        assert_eq!(
            parsed.global, defaults,
            "[global] defaults must match SPEC §13.1"
        );
        assert_eq!(
            parsed.budgets,
            BudgetSettings::default(),
            "[budgets] defaults must match SPEC §13.1"
        );
    }

    #[test]
    fn an_empty_global_document_is_the_default_document() {
        let (empty, warnings) = GlobalConfig::parse("").unwrap_or_else(|e| panic!("{e}"));
        assert!(warnings.is_empty());
        assert_eq!(empty.global, GlobalSettings::default());
        assert_eq!(empty.budgets, BudgetSettings::default());
    }

    #[test]
    fn global_default_values_are_the_ones_the_spec_prints() {
        // Spot-checked individually so a wrong value fails with a readable name
        // rather than as one opaque struct comparison.
        let g = GlobalSettings::default();
        assert_eq!(g.mode, AutonomyMode::AutoLowAskHigh);
        assert_eq!(g.max_concurrent_runs, 2);
        assert_eq!(g.coalesce_window_ms, 1500);
        assert_eq!(g.trigger_port, 41791);
        assert_eq!(g.webhook_port, 0, "0 disables the webhook listener");
        assert_eq!(g.transcript_retention_days, 30);
        assert!(g.keep_scratch_on_failure);
        assert_eq!(g.stale_run_minutes, 10);
        assert_eq!(g.max_attempts, 3);
        assert_eq!(g.approval_ttl_hours, 72);
        assert_eq!(g.burst_threshold, crate::DEFAULT_BURST_THRESHOLD);

        let b = BudgetSettings::default();
        assert_eq!(b.daily_tokens_per_repo, 2_000_000);
        assert_eq!(b.daily_runs_per_repo, 200);
        assert_eq!(b.on_exhausted, OnExhausted::Pause);
        assert!(b.cost_is_unlimited(), "SPEC §13.1: 0 means unlimited");
    }

    #[test]
    fn budget_exhaustion_cannot_be_configured_to_drop_silently() {
        // Decision D10: exhaustion pauses, queues or skips — it never silently
        // drops. The type has no variant for it, so a config cannot ask for one.
        assert_eq!(OnExhausted::ALL.len(), 3);
        assert!(!OnExhausted::WIRE_NAMES.contains(&"drop"));
        assert!(toml::from_str::<BudgetSettings>(r#"on_exhausted = "drop""#).is_err());
    }

    #[test]
    fn the_repo_defaults_are_the_spec_13_2_document() {
        let (parsed, warnings) =
            RepoConfig::parse_json(SPEC_13_2).unwrap_or_else(|e| panic!("{e}"));
        assert!(warnings.is_empty(), "the spec document warns: {warnings:?}");
        assert_eq!(
            parsed,
            RepoConfig::default(),
            "per-repo defaults must match the SPEC §13.2 document"
        );
    }

    #[test]
    fn an_empty_repo_document_is_the_default_document() {
        let (empty, warnings) = RepoConfig::parse_json("{}").unwrap_or_else(|e| panic!("{e}"));
        assert!(warnings.is_empty());
        assert_eq!(empty, RepoConfig::default());
    }

    #[test]
    fn repo_default_values_are_the_ones_the_spec_prints() {
        let c = RepoConfig::default();
        assert_eq!(c.branches, ["main", "release/*"]);
        assert!(c.review_prs && !c.review_commits);
        assert!(!c.review_draft_prs && !c.review_merge_commits);
        assert!(c.watch_branches);
        assert_eq!(c.poll_interval_secs, 120);
        assert_eq!(c.engine, EngineKind::Claude);
        assert_eq!(c.autonomy, AutonomyMode::AutoLowAskHigh);
        assert_eq!(c.andare_min_severity, Severity::High);
        assert!(
            !c.trama_publish,
            "a Trama draft is low risk; publishing is not"
        );
        assert!(!c.block_on_findings);
        assert!(
            !c.allow_approve,
            "SPEC §10.2: the app never submits an APPROVE review by default"
        );
        assert_eq!(c.scope.len(), 4, "decision D8's four dimensions");
        for category in [
            Category::Correctness,
            Category::Security,
            Category::Convention,
            Category::Tests,
        ] {
            assert!(c.covers(category), "D8 scope must include {category}");
        }
        assert!(
            !c.covers(Category::Perf),
            "perf is not in the D8 default scope"
        );
        for target in ["github", "andare", "trama"] {
            assert!(c.targets_include(target));
        }
    }

    #[test]
    fn an_unknown_global_key_warns_and_the_document_still_loads() {
        let (config, warnings) = GlobalConfig::parse(
            r#"
[global]
max_concurent_runs = 4
"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));

        assert_eq!(
            config.global.max_concurrent_runs, 2,
            "the misspelled key must not have been applied"
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert_eq!(warnings[0].path(), "global.max_concurent_runs");
        assert!(
            warnings[0].message().contains("max_concurent_runs"),
            "the message must name the key so a typo is findable: {}",
            warnings[0].message()
        );
    }

    #[test]
    fn unknown_keys_are_reported_from_every_section() {
        let (_, warnings) = GlobalConfig::parse(
            r#"
future_feature = true

[global]
unknown_a = 1

[budgets]
unknown_b = 2

[mcpServers.github]
type = "stdio"
unknown_c = 3
"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));

        let paths: Vec<&str> = warnings.iter().map(ConfigWarning::path).collect();
        for expected in [
            "future_feature",
            "global.unknown_a",
            "budgets.unknown_b",
            "mcpServers.github.unknown_c",
        ] {
            assert!(paths.contains(&expected), "missing {expected} in {paths:?}");
        }
    }

    #[test]
    fn an_unknown_repo_key_warns_and_the_document_still_loads() {
        let (config, warnings) =
            RepoConfig::parse_json(r#"{"review_commits": true, "reviw_prs": false}"#)
                .unwrap_or_else(|e| panic!("{e}"));

        assert!(config.review_commits, "the known key must have applied");
        assert!(
            config.review_prs,
            "the misspelled key must not have applied"
        );
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].path(), "reviw_prs");
    }

    #[test]
    fn a_malformed_document_is_an_error_not_a_warning() {
        // Unknown keys are tolerated; syntax and type errors are not. Silently
        // defaulting a field whose value was the wrong type would apply a setting
        // the user did not ask for.
        assert!(GlobalConfig::parse("[global\nmode =").is_err());
        assert!(
            GlobalConfig::parse(
                r#"[global]
max_concurrent_runs = "two""#
            )
            .is_err(),
            "a known key with the wrong type must fail loudly"
        );
        assert!(RepoConfig::parse_json("{").is_err());
        assert!(RepoConfig::parse_json(r#"{"review_prs": "yes"}"#).is_err());
    }

    #[test]
    fn an_invalid_enum_value_names_the_alternatives() {
        let error = GlobalConfig::parse(
            r#"[global]
mode = "yolo""#,
        )
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
        assert!(error.contains("yolo"), "{error}");
        assert!(error.contains("auto_low_ask_high"), "{error}");
    }

    #[test]
    fn mcp_server_headers_hold_deferred_secrets() {
        let (config, _) = GlobalConfig::parse(
            r#"
[mcpServers.trama]
type = "http"
url = "https://trama.example.com/mcp"

[mcpServers.trama.headers]
Authorization = "{{keychain:trama-token}}"
"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));

        let server = config
            .mcp_servers
            .get("trama")
            .unwrap_or_else(|| panic!("no trama server"));
        let auth = server
            .headers
            .get("Authorization")
            .unwrap_or_else(|| panic!("no Authorization header"));

        assert!(
            auth.is_deferred(),
            "the placeholder must not resolve at load time"
        );
        assert_eq!(auth.keychain_name(), Some("trama-token"));
    }

    #[test]
    fn a_debug_dump_of_the_whole_config_leaks_no_secret() {
        // The realistic leak: a diagnostic logs the config struct with `{:?}`.
        let (config, _) = GlobalConfig::parse(
            r#"
[mcpServers.github]
type = "stdio"

[mcpServers.github.headers]
Authorization = "ghp_supersecret"
"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));

        let dumped = format!("{config:?}");
        assert!(!dumped.contains("ghp_supersecret"), "{dumped}");
    }

    #[test]
    fn engine_and_target_tables_survive_a_round_trip_unchanged() {
        // Their shapes belong to §8.4 and §11.3-11.5. Holding them opaquely means
        // this module cannot drift from those sections, but it must not silently
        // discard what a user wrote either.
        let source = r#"
[engines.claude]
binary = "claude"
timeout_secs = 600

[targets.github]
transport = "gh-cli"
"#;
        let (config, _) = GlobalConfig::parse(source).unwrap_or_else(|e| panic!("{e}"));
        let claude = config
            .engines
            .get("claude")
            .unwrap_or_else(|| panic!("no claude engine"));
        assert_eq!(
            claude.get("timeout_secs").and_then(toml::Value::as_integer),
            Some(600)
        );

        let round_tripped = toml::to_string(&config).unwrap_or_default();
        let (again, _) = GlobalConfig::parse(&round_tripped).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            again.engines, config.engines,
            "engine tables must not be lost"
        );
        assert_eq!(
            again.targets, config.targets,
            "target tables must not be lost"
        );
    }
}
