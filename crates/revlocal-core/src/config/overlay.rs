//! The in-repo `.rev-local.toml` overlay and the authority rule (SPEC §13.2).
//!
//! SPEC §13.2: *"Optional in-repo override: `.rev-local.toml` at the repo root,
//! merged over the stored config (repo-local wins for scope/ignores, never for
//! autonomy or targets — a repository must not be able to grant itself more
//! authority)."*
//!
//! The parenthetical is the rule; the clause before it is an example of it. This
//! module implements the rule, which is stricter than the example — see
//! [`PERMITTED_KEYS`] and ADR 0007.
//!
//! # Why an allowlist
//!
//! The obvious implementation refuses `autonomy` and `targets` and permits the
//! rest. That is a denylist, and it fails in the direction that costs something:
//! a field added to [`RepoConfig`] next year is *granted* to every repository by
//! default, silently, by nobody's decision.
//!
//! An allowlist fails the other way. A new field is refused until somebody looks
//! at it and decides it is safe, and the refusal is visible — the user is told
//! which key was ignored and why.

use super::{ConfigWarning, RepoConfig};
use crate::{AutonomyMode, Category};
use serde::{Deserialize, Serialize};

/// The only keys an in-repo `.rev-local.toml` may set.
///
/// Everything else is refused, including keys that look harmless. These five are
/// what SPEC §13.2's "scope/ignores" names, and each of them can only make
/// rev-local do *less* or look *harder* — never grant the repository authority it
/// was not given.
pub const PERMITTED_KEYS: &[&str] = &[
    "scope",
    "ignore_globs",
    "ignore_authors",
    "sensitive_globs",
    "convention_files",
];

/// Why an in-repo key was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// The key exists in [`RepoConfig`] but a repository may not set it.
    #[error(
        "`{key}` may not be set in .rev-local.toml: a repository cannot grant itself \
         more authority than it was given (SPEC §13.2). Set it in the repo's stored \
         configuration instead."
    )]
    FieldNotPermitted {
        /// The refused key.
        key: String,
    },

    /// A permitted key held a value of the wrong shape.
    #[error("`{key}` in .rev-local.toml is malformed: {detail}")]
    MalformedValue {
        /// The key whose value could not be read.
        key: String,
        /// What went wrong.
        detail: String,
    },
}

impl ConfigError {
    /// The key this error concerns.
    pub fn key(&self) -> &str {
        match self {
            Self::FieldNotPermitted { key } | Self::MalformedValue { key, .. } => key,
        }
    }
}

/// The result of merging an in-repo overlay over a stored per-repo config.
///
/// Refusals do not fail the merge. One forbidden key must not discard a file whose
/// other keys were legitimate — that would punish the wrong thing and tempt people
/// to delete the file entirely. Every refusal is reported so the user learns which
/// key was ignored.
#[derive(Debug, Clone, PartialEq)]
pub struct MergeOutcome {
    /// The effective configuration.
    pub config: RepoConfig,
    /// Keys refused by the authority rule, or malformed.
    pub refusals: Vec<ConfigError>,
    /// Keys this version does not recognise at all.
    pub warnings: Vec<ConfigWarning>,
}

impl MergeOutcome {
    /// Whether the overlay was applied with nothing refused or ignored.
    pub fn is_clean(&self) -> bool {
        self.refusals.is_empty() && self.warnings.is_empty()
    }
}

/// The parsed contents of an in-repo `.rev-local.toml`.
///
/// Held as a raw table rather than a struct so that the authority rule is applied
/// key by key. A struct with only the permitted fields would silently drop a
/// refused key instead of reporting it, and the user would never learn that their
/// `autonomy = "auto"` did nothing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InRepoConfig {
    /// Every key the file set, unvalidated.
    pub table: toml::Table,
}

impl InRepoConfig {
    /// Parse an in-repo overlay. Syntax errors fail; policy is applied at merge.
    pub fn parse(toml_text: &str) -> Result<Self, toml::de::Error> {
        Ok(Self {
            table: toml::from_str(toml_text)?,
        })
    }
}

/// Read a permitted key's value, or record why it could not be read.
fn read<T: serde::de::DeserializeOwned>(
    key: &str,
    value: &toml::Value,
    refusals: &mut Vec<ConfigError>,
) -> Option<T> {
    match T::deserialize(value.clone()) {
        Ok(parsed) => Some(parsed),
        Err(e) => {
            refusals.push(ConfigError::MalformedValue {
                key: key.to_owned(),
                detail: e.to_string(),
            });
            None
        }
    }
}

/// Add `additions` to `base` without duplicating, preserving `base`'s order.
///
/// Union rather than replacement: an operator's entry cannot be removed by the
/// repository. Un-ignoring a path the operator ignored would make rev-local review
/// and file issues on code it was told to leave alone, which is a widening however
/// the field is labelled.
fn union(base: &mut Vec<String>, additions: Vec<String>) {
    for item in additions {
        if !base.contains(&item) {
            base.push(item);
        }
    }
}

/// Merge an in-repo overlay over a stored per-repo config (SPEC §13.2).
///
/// Field semantics, all chosen so the result can only be equal to or narrower than
/// what the operator configured:
///
/// - `scope` — **intersection**. A repo may drop a review dimension, not add one.
/// - `ignore_globs`, `ignore_authors` — **union**. A repo may add ignores, not
///   remove the operator's.
/// - `sensitive_globs` — **union**. Adding forces deeper review (SPEC §9.3);
///   removing would make review shallower, so it is not possible.
/// - `convention_files` — **union**. Reading more of a repo's own conventions
///   grants nothing.
///
/// Every other key in [`RepoConfig`] is refused. Keys in no version of
/// `RepoConfig` are reported as unknown, exactly as in the other two documents.
pub fn merge_in_repo(stored: &RepoConfig, overlay: &InRepoConfig) -> MergeOutcome {
    let mut config = stored.clone();
    let mut refusals = Vec::new();
    let mut warnings = Vec::new();

    // Field names of RepoConfig, derived from the type rather than hardcoded, so
    // adding a field cannot leave this list stale.
    let known: Vec<String> = serde_json::to_value(RepoConfig::default())
        .ok()
        .and_then(|v| v.as_object().map(|o| o.keys().cloned().collect()))
        .unwrap_or_default();

    for (key, value) in &overlay.table {
        match key.as_str() {
            "scope" => {
                if let Some(requested) = read::<Vec<Category>>(key, value, &mut refusals) {
                    // Intersection: keep only dimensions the operator also enabled.
                    config.scope.retain(|c| requested.contains(c));
                }
            }
            "ignore_globs" => {
                if let Some(more) = read::<Vec<String>>(key, value, &mut refusals) {
                    union(&mut config.ignore_globs, more);
                }
            }
            "ignore_authors" => {
                if let Some(more) = read::<Vec<String>>(key, value, &mut refusals) {
                    union(&mut config.ignore_authors, more);
                }
            }
            "sensitive_globs" => {
                if let Some(more) = read::<Vec<String>>(key, value, &mut refusals) {
                    union(&mut config.sensitive_globs, more);
                }
            }
            "convention_files" => {
                if let Some(more) = read::<Vec<String>>(key, value, &mut refusals) {
                    union(&mut config.convention_files, more);
                }
            }
            other if known.iter().any(|k| k == other) => {
                refusals.push(ConfigError::FieldNotPermitted {
                    key: other.to_owned(),
                });
            }
            other => {
                warnings.push(ConfigWarning::UnknownKey {
                    path: other.to_owned(),
                });
            }
        }
    }

    MergeOutcome {
        config,
        refusals,
        warnings,
    }
}

/// The autonomy a repository actually gets (SPEC §12.2).
///
/// A separate function from the merge because the ceiling applies whether or not
/// an overlay exists: `.rev-local.toml` is not the only way a repo could end up
/// asking for more than the app allows.
pub fn effective_autonomy(global: AutonomyMode, repo: &RepoConfig) -> AutonomyMode {
    AutonomyMode::effective(global, repo.autonomy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EngineKind;

    fn merge(toml_text: &str) -> MergeOutcome {
        let overlay = InRepoConfig::parse(toml_text).unwrap_or_else(|e| panic!("{e}"));
        merge_in_repo(&RepoConfig::default(), &overlay)
    }

    // --- The rule: authority cannot be widened -----------------------------

    #[test]
    fn an_in_repo_config_cannot_raise_its_own_autonomy() {
        let outcome = merge(r#"autonomy = "auto""#);

        assert_eq!(
            outcome.config.autonomy,
            RepoConfig::default().autonomy,
            "the refused value must not have been applied"
        );
        assert_eq!(
            outcome.refusals,
            vec![ConfigError::FieldNotPermitted {
                key: "autonomy".to_owned()
            }]
        );
        let message = outcome.refusals[0].to_string();
        assert!(message.contains("autonomy"), "{message}");
        assert!(message.contains("more authority"), "{message}");
    }

    #[test]
    fn an_in_repo_config_cannot_lower_its_autonomy_either() {
        // Even the safe direction is refused. Autonomy is the operator's setting,
        // and a repo silently reviewing nothing because someone committed
        // `autonomy = "off"` is a failure the operator would have to debug.
        let outcome = merge(r#"autonomy = "off""#);
        assert_eq!(outcome.config.autonomy, RepoConfig::default().autonomy);
        assert_eq!(outcome.refusals.len(), 1);
    }

    #[test]
    fn an_in_repo_config_cannot_add_a_publish_target() {
        let stored = RepoConfig {
            targets: vec!["github".to_owned()],
            ..RepoConfig::default()
        };
        let overlay = InRepoConfig::parse(r#"targets = ["github", "andare", "trama"]"#)
            .unwrap_or_else(|e| panic!("{e}"));
        let outcome = merge_in_repo(&stored, &overlay);

        assert_eq!(
            outcome.config.targets,
            ["github"],
            "targets must be untouched"
        );
        assert_eq!(
            outcome.refusals,
            vec![ConfigError::FieldNotPermitted {
                key: "targets".to_owned()
            }]
        );
    }

    #[test]
    fn the_fields_that_change_risk_class_are_refused_even_though_13_2_does_not_name_them() {
        // trama_publish turns a low-risk draft into a high-risk publish, and
        // allow_approve gates whether the app will ever submit a GitHub APPROVE
        // (§12.3, §10.2). Both are authority in effect. See ADR 0007.
        for key in ["trama_publish", "allow_approve"] {
            let outcome = merge(&format!("{key} = true"));
            assert_eq!(
                outcome.refusals,
                vec![ConfigError::FieldNotPermitted {
                    key: key.to_owned()
                }],
                "{key} must be refused"
            );
        }
        assert!(!merge("trama_publish = true").config.trama_publish);
        assert!(!merge("allow_approve = true").config.allow_approve);
    }

    #[test]
    fn every_repo_config_field_outside_the_allowlist_is_refused() {
        // The allowlist's whole point: a field added to RepoConfig later is refused
        // by default rather than granted to every repository by nobody's decision.
        let defaults = serde_json::to_value(RepoConfig::default()).unwrap_or_default();
        let fields: Vec<String> = defaults
            .as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default();
        assert!(
            fields.len() > PERMITTED_KEYS.len(),
            "sanity: there are non-permitted fields"
        );

        for field in fields {
            if PERMITTED_KEYS.contains(&field.as_str()) || field == "extra" {
                continue;
            }
            // The value's type does not matter: refusal happens before parsing.
            let outcome = merge(&format!("{field} = \"whatever\""));
            assert_eq!(
                outcome.refusals,
                vec![ConfigError::FieldNotPermitted { key: field.clone() }],
                "{field} is outside the allowlist and must be refused"
            );
        }
    }

    // --- The rule: scope and ignores may narrow ----------------------------

    #[test]
    fn a_repo_may_narrow_its_review_scope() {
        let outcome = merge(r#"scope = ["security"]"#);
        assert!(outcome.is_clean(), "{outcome:?}");
        assert_eq!(outcome.config.scope, [Category::Security]);
    }

    #[test]
    fn a_repo_cannot_widen_its_review_scope_by_asking_for_more() {
        // `perf` is not in the D8 default scope. Requesting it must not add it —
        // scope is an intersection, not a replacement.
        let outcome = merge(r#"scope = ["security", "perf"]"#);
        assert_eq!(outcome.config.scope, [Category::Security]);
        assert!(!outcome.config.covers(Category::Perf));
    }

    #[test]
    fn a_repo_may_add_ignores_but_not_remove_the_operators() {
        let stored = RepoConfig {
            ignore_globs: vec!["**/vendor/**".to_owned()],
            ..RepoConfig::default()
        };
        let overlay = InRepoConfig::parse(r#"ignore_globs = ["generated/**"]"#)
            .unwrap_or_else(|e| panic!("{e}"));
        let outcome = merge_in_repo(&stored, &overlay);

        assert!(outcome.is_clean());
        assert_eq!(
            outcome.config.ignore_globs,
            ["**/vendor/**", "generated/**"],
            "the operator's ignore must survive; the repo's is appended"
        );
    }

    #[test]
    fn omitting_an_operators_ignore_does_not_remove_it() {
        // The realistic mistake: someone writes the file listing only their own
        // additions. Under replacement semantics that would silently un-ignore
        // vendor code and start filing issues on it.
        let stored = RepoConfig {
            ignore_globs: vec!["**/node_modules/**".to_owned(), "**/vendor/**".to_owned()],
            ..RepoConfig::default()
        };
        let overlay =
            InRepoConfig::parse(r#"ignore_globs = ["docs/**"]"#).unwrap_or_else(|e| panic!("{e}"));
        let outcome = merge_in_repo(&stored, &overlay);

        assert!(outcome
            .config
            .ignore_globs
            .contains(&"**/vendor/**".to_owned()));
        assert!(outcome
            .config
            .ignore_globs
            .contains(&"**/node_modules/**".to_owned()));
    }

    #[test]
    fn a_repo_may_mark_more_paths_sensitive_but_not_fewer() {
        // Adding forces deeper review (§9.3); removing would make review shallower.
        let stored = RepoConfig {
            sensitive_globs: vec!["**/auth/**".to_owned()],
            ..RepoConfig::default()
        };
        let overlay = InRepoConfig::parse(r#"sensitive_globs = ["billing/**"]"#)
            .unwrap_or_else(|e| panic!("{e}"));
        let outcome = merge_in_repo(&stored, &overlay);

        assert_eq!(outcome.config.sensitive_globs, ["**/auth/**", "billing/**"]);
    }

    #[test]
    fn adding_an_ignore_twice_does_not_duplicate_it() {
        let outcome = merge(r#"ignore_globs = ["**/vendor/**", "**/vendor/**"]"#);
        let vendor_count = outcome
            .config
            .ignore_globs
            .iter()
            .filter(|g| g.as_str() == "**/vendor/**")
            .count();
        assert_eq!(vendor_count, 1);
    }

    // --- Precedence and composition ----------------------------------------

    #[test]
    fn merge_precedence_by_field_class() {
        let stored = RepoConfig {
            autonomy: AutonomyMode::DryRun,
            targets: vec!["github".to_owned()],
            scope: vec![Category::Correctness, Category::Security],
            ignore_globs: vec!["**/vendor/**".to_owned()],
            poll_interval_secs: 300,
            ..RepoConfig::default()
        };
        let overlay = InRepoConfig::parse(
            r#"
autonomy = "auto"
targets = ["github", "andare"]
scope = ["security"]
ignore_globs = ["generated/**"]
poll_interval_secs = 5
"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let outcome = merge_in_repo(&stored, &overlay);

        // Authority: stored wins, always.
        assert_eq!(outcome.config.autonomy, AutonomyMode::DryRun);
        assert_eq!(outcome.config.targets, ["github"]);
        // Preference outside the allowlist: stored wins.
        assert_eq!(outcome.config.poll_interval_secs, 300);
        // Scope: narrowed.
        assert_eq!(outcome.config.scope, [Category::Security]);
        // Ignores: unioned.
        assert_eq!(
            outcome.config.ignore_globs,
            ["**/vendor/**", "generated/**"]
        );

        let refused: Vec<&str> = outcome.refusals.iter().map(ConfigError::key).collect();
        assert_eq!(refused, ["autonomy", "poll_interval_secs", "targets"]);
    }

    #[test]
    fn one_refused_key_does_not_discard_the_rest_of_the_file() {
        // Otherwise a single bad line costs the user every legitimate setting, and
        // the sane response becomes deleting the file.
        let outcome = merge(
            r#"
autonomy = "auto"
ignore_globs = ["generated/**"]
scope = ["security"]
"#,
        );
        assert_eq!(outcome.refusals.len(), 1);
        assert!(outcome
            .config
            .ignore_globs
            .contains(&"generated/**".to_owned()));
        assert_eq!(outcome.config.scope, [Category::Security]);
    }

    #[test]
    fn a_permitted_key_with_the_wrong_type_is_reported_not_silently_dropped() {
        let outcome = merge(r#"ignore_globs = "generated/**""#);
        assert_eq!(outcome.refusals.len(), 1);
        assert!(matches!(
            outcome.refusals[0],
            ConfigError::MalformedValue { .. }
        ));
        assert_eq!(outcome.refusals[0].key(), "ignore_globs");
        assert_eq!(
            outcome.config.ignore_globs,
            RepoConfig::default().ignore_globs,
            "a malformed value must leave the stored value alone"
        );
    }

    #[test]
    fn an_unknown_key_warns_rather_than_being_refused() {
        // A key in no version of RepoConfig is a typo or a future feature, not an
        // attempt to gain authority. It gets the same treatment as in §13.1/§13.2.
        let outcome = merge("future_feature = true");
        assert!(outcome.refusals.is_empty(), "{:?}", outcome.refusals);
        assert_eq!(outcome.warnings.len(), 1);
        assert_eq!(outcome.warnings[0].path(), "future_feature");
    }

    #[test]
    fn an_absent_overlay_changes_nothing() {
        let outcome = merge("");
        assert!(outcome.is_clean());
        assert_eq!(outcome.config, RepoConfig::default());
    }

    #[test]
    fn a_malformed_overlay_file_fails_to_parse() {
        assert!(InRepoConfig::parse("scope = [").is_err());
    }

    // --- The global ceiling still applies -----------------------------------

    #[test]
    fn the_global_mode_caps_the_merged_result() {
        // SPEC §12.2. Even a legitimately-stored `auto` repo is capped by the app.
        let repo = RepoConfig {
            autonomy: AutonomyMode::Auto,
            ..RepoConfig::default()
        };
        assert_eq!(
            effective_autonomy(AutonomyMode::DryRun, &repo),
            AutonomyMode::DryRun
        );
        assert_eq!(
            effective_autonomy(AutonomyMode::Auto, &repo),
            AutonomyMode::Auto
        );
    }

    #[test]
    fn an_overlay_cannot_escape_the_global_ceiling_through_any_path() {
        // Belt and braces: the overlay is refused, and even if it were not, the
        // ceiling would still hold.
        let stored = RepoConfig {
            autonomy: AutonomyMode::DryRun,
            ..RepoConfig::default()
        };
        let overlay = InRepoConfig::parse(r#"autonomy = "auto""#).unwrap_or_else(|e| panic!("{e}"));
        let merged = merge_in_repo(&stored, &overlay).config;

        assert_eq!(merged.autonomy, AutonomyMode::DryRun);
        assert_eq!(
            effective_autonomy(AutonomyMode::AutoLowAskHigh, &merged),
            AutonomyMode::DryRun
        );
    }

    #[test]
    fn a_repo_cannot_switch_its_own_engine() {
        // Engine choice is per-repo by decision D3, but it is the operator's
        // choice: a different engine spends a different budget and has different
        // sandbox behaviour.
        let outcome = merge(r#"engine = "codex""#);
        assert_eq!(outcome.config.engine, EngineKind::Claude);
        assert_eq!(outcome.refusals.len(), 1);
    }
}
