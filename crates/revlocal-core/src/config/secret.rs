//! Deferred secret references (SPEC §13.1).
//!
//! Config files may carry `{{keychain:name}}` placeholders. They are resolved at
//! connect time from the OS keychain, never at load time, and never logged.
//!
//! [`SecretRef`] is the type that makes that structural rather than a rule people
//! have to remember. A resolved secret has no `Debug` or `Display` that prints it,
//! so a config struct can be logged wholesale — which is exactly what happens in a
//! diagnostic dump — without leaking anything.

use serde::{Deserialize, Serialize};
use std::fmt;

/// The placeholder prefix, per SPEC §13.1.
const KEYCHAIN_PREFIX: &str = "{{keychain:";
/// The placeholder suffix.
const KEYCHAIN_SUFFIX: &str = "}}";

/// A configured string that may be a literal or a deferred keychain lookup.
///
/// Deserializes from a plain TOML/JSON string, so config files stay readable.
/// Whether that string was a placeholder is decided once, at parse time.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum SecretRef {
    /// A literal value written directly in the config.
    ///
    /// Still redacted when formatted: a value in a field that *could* hold a
    /// secret is treated as one, because the alternative is deciding per field and
    /// getting it wrong once.
    Literal(String),
    /// A `{{keychain:name}}` placeholder, unresolved.
    Keychain {
        /// The keychain entry to look up at connect time.
        name: String,
    },
}

impl SecretRef {
    /// Parse a configured string, recognising the `{{keychain:name}}` form.
    pub fn parse(raw: &str) -> Self {
        let trimmed = raw.trim();
        if let Some(inner) = trimmed
            .strip_prefix(KEYCHAIN_PREFIX)
            .and_then(|rest| rest.strip_suffix(KEYCHAIN_SUFFIX))
        {
            let name = inner.trim();
            if !name.is_empty() {
                return Self::Keychain {
                    name: name.to_owned(),
                };
            }
        }
        Self::Literal(raw.to_owned())
    }

    /// Whether this reference still needs a keychain lookup.
    pub const fn is_deferred(&self) -> bool {
        matches!(self, Self::Keychain { .. })
    }

    /// The keychain entry name, when this is a deferred reference.
    pub fn keychain_name(&self) -> Option<&str> {
        match self {
            Self::Keychain { name } => Some(name),
            Self::Literal(_) => None,
        }
    }

    /// The literal value, for the one caller that legitimately needs it.
    ///
    /// Named `expose` rather than `get` or `as_str` so that reading a secret is
    /// visible at the call site and greppable in review.
    pub fn expose(&self) -> Option<&str> {
        match self {
            Self::Literal(value) => Some(value),
            Self::Keychain { .. } => None,
        }
    }

    /// The original config spelling, for round-tripping a document unchanged.
    pub fn to_config_string(&self) -> String {
        match self {
            Self::Literal(value) => value.clone(),
            Self::Keychain { name } => format!("{KEYCHAIN_PREFIX}{name}{KEYCHAIN_SUFFIX}"),
        }
    }
}

impl From<String> for SecretRef {
    fn from(raw: String) -> Self {
        Self::parse(&raw)
    }
}

impl From<SecretRef> for String {
    fn from(value: SecretRef) -> Self {
        value.to_config_string()
    }
}

/// What a redacted secret renders as.
const REDACTED: &str = "<redacted>";

// Both formatters redact. `Debug` especially: a config struct reaching a log line
// through `{:?}` is the most likely way a secret escapes, and deriving Debug on the
// enclosing struct is the natural thing to do.
impl fmt::Debug for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Literal(_) => write!(f, "SecretRef::Literal({REDACTED})"),
            Self::Keychain { name } => write!(f, "SecretRef::Keychain({name})"),
        }
    }
}

impl fmt::Display for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // The keychain *name* is not secret and is useful in an error message
            // ("no keychain entry `github-token`"). The value never is.
            Self::Keychain { name } => write!(f, "{KEYCHAIN_PREFIX}{name}{KEYCHAIN_SUFFIX}"),
            Self::Literal(_) => f.write_str(REDACTED),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_keychain_placeholder_is_recognised_and_left_unresolved() {
        let secret = SecretRef::parse("{{keychain:github-token}}");
        assert!(secret.is_deferred());
        assert_eq!(secret.keychain_name(), Some("github-token"));
        assert_eq!(
            secret.expose(),
            None,
            "a deferred ref has nothing to expose yet"
        );
    }

    #[test]
    fn a_plain_string_is_a_literal() {
        let secret = SecretRef::parse("ghp_example");
        assert!(!secret.is_deferred());
        assert_eq!(secret.expose(), Some("ghp_example"));
    }

    #[test]
    fn a_malformed_placeholder_is_not_silently_treated_as_a_lookup() {
        // An empty name would look up nothing; treating it as a literal keeps the
        // failure visible at connect time instead of resolving to an empty secret.
        for malformed in [
            "{{keychain:}}",
            "{{keychain:",
            "keychain:name",
            "{{other:name}}",
        ] {
            assert!(
                !SecretRef::parse(malformed).is_deferred(),
                "{malformed:?} must not be taken as a keychain reference"
            );
        }
    }

    #[test]
    fn debug_never_prints_a_literal_secret() {
        // This is the formatter that leaks: a config struct derives Debug and ends
        // up in a log line.
        let rendered = format!("{:?}", SecretRef::parse("ghp_supersecret"));
        assert!(!rendered.contains("ghp_supersecret"), "{rendered}");
        assert!(rendered.contains(REDACTED), "{rendered}");
    }

    #[test]
    fn display_never_prints_a_literal_secret() {
        let rendered = SecretRef::parse("ghp_supersecret").to_string();
        assert!(!rendered.contains("ghp_supersecret"), "{rendered}");
    }

    #[test]
    fn a_keychain_name_stays_visible_because_it_is_not_the_secret() {
        // "no keychain entry `github-token`" is a useful error; redacting the name
        // would make a misconfiguration undiagnosable.
        let secret = SecretRef::parse("{{keychain:github-token}}");
        assert!(secret.to_string().contains("github-token"));
        assert!(format!("{secret:?}").contains("github-token"));
    }

    #[test]
    fn a_secret_round_trips_through_config_without_being_resolved() {
        let original = "{{keychain:trama-token}}";
        let secret = SecretRef::parse(original);
        assert_eq!(secret.to_config_string(), original);

        let json = serde_json::to_string(&secret).unwrap_or_default();
        assert_eq!(json, format!("\"{original}\""));
        let back: SecretRef = serde_json::from_str(&json).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(back, secret);
        assert!(back.is_deferred(), "a round-trip must not eagerly resolve");
    }
}
