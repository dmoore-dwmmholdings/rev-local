//! Secret redaction for logs and transcripts (SPEC §18).
//!
//! Two mechanisms, and it matters which is which:
//!
//! 1. **Field names are the primary defence.** A field called `token`, `secret` or
//!    `password` has its value replaced wholesale, whatever the value looks like.
//!    This is reliable because it does not have to recognise anything.
//! 2. **Patterns are a safety net** for secrets that arrive inside free text — an
//!    error message quoting a request header, an engine transcript echoing a
//!    command line. Pattern matching cannot be complete, so it is defence in depth
//!    and never the thing being relied on.
//!
//! The functions are pure and allocate only when something was found, so the
//!    logging layer can run them on every field of every event without cost on the
//! overwhelmingly common path where there is nothing to redact.

use std::borrow::Cow;

/// What a redacted value is replaced with.
pub const REDACTED: &str = "[redacted]";

/// Substrings that make a field name sensitive, matched case-insensitively.
///
/// Deliberately broad. A false positive costs one unreadable log field; a false
/// negative writes a credential to disk.
const SENSITIVE_FIELD_MARKERS: &[&str] = &[
    "token",
    "secret",
    "password",
    "passwd",
    "apikey",
    "api_key",
    "credential",
    "authorization",
    "auth_header",
    "private_key",
    "session_id",
];

/// A prefix that introduces a credential, and what must follow for it to count.
struct SecretPattern {
    /// The literal prefix, e.g. `ghp_`.
    prefix: &'static str,
    /// Minimum length of the value after the prefix.
    min_len: usize,
    /// Whether the value must contain a digit.
    ///
    /// This is what keeps `andare_min_severity` — a real config field name — from
    /// being mistaken for `andare_a1b2c3...`. Without it the generic prefixes
    /// would redact ordinary identifiers and make logs useless.
    needs_digit: bool,
}

/// Credential shapes recognised in free text.
const PATTERNS: &[SecretPattern] = &[
    // GitHub personal access, OAuth, user-to-server, server-to-server, refresh.
    SecretPattern {
        prefix: "ghp_",
        min_len: 20,
        needs_digit: false,
    },
    SecretPattern {
        prefix: "gho_",
        min_len: 20,
        needs_digit: false,
    },
    SecretPattern {
        prefix: "ghu_",
        min_len: 20,
        needs_digit: false,
    },
    SecretPattern {
        prefix: "ghs_",
        min_len: 20,
        needs_digit: false,
    },
    SecretPattern {
        prefix: "ghr_",
        min_len: 20,
        needs_digit: false,
    },
    SecretPattern {
        prefix: "github_pat_",
        min_len: 20,
        needs_digit: false,
    },
    // The suite's own services (SPEC §13.1 references these by name).
    SecretPattern {
        prefix: "trama_",
        min_len: 16,
        needs_digit: true,
    },
    SecretPattern {
        prefix: "andare_",
        min_len: 16,
        needs_digit: true,
    },
    // Common third parties an engine's transcript might echo.
    SecretPattern {
        prefix: "sk-",
        min_len: 20,
        needs_digit: false,
    },
    SecretPattern {
        prefix: "xoxb-",
        min_len: 10,
        needs_digit: true,
    },
    SecretPattern {
        prefix: "xoxp-",
        min_len: 10,
        needs_digit: true,
    },
    SecretPattern {
        prefix: "AKIA",
        min_len: 16,
        needs_digit: true,
    },
];

/// The scheme that precedes a bearer credential in an `Authorization` header.
const BEARER: &str = "Bearer ";

/// Shortest thing after `Bearer ` that is worth redacting.
const BEARER_MIN_LEN: usize = 8;

/// Whether a field with this name should have its value redacted outright.
///
/// Matched case-insensitively on substrings, so `github_token`, `X-Auth-Token`
/// and `TRAMA_SECRET` are all caught.
pub fn is_sensitive_field(name: &str) -> bool {
    let lowered = name.to_lowercase();
    SENSITIVE_FIELD_MARKERS
        .iter()
        .any(|marker| lowered.contains(marker))
}

/// Characters that can appear inside a credential body.
fn is_secret_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// Scrub credential-shaped substrings out of free text.
///
/// Returns [`Cow::Borrowed`] unchanged when nothing matched, which is the case for
/// almost every log line.
pub fn redact(text: &str) -> Cow<'_, str> {
    if !might_contain_secret(text) {
        return Cow::Borrowed(text);
    }

    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut index = 0;

    while index < text.len() {
        if !text.is_char_boundary(index) {
            index += 1;
            continue;
        }
        let rest = &text[index..];

        if let Some(after) = rest.strip_prefix(BEARER) {
            let value_len = after.chars().take_while(|c| !c.is_whitespace()).count();
            if value_len >= BEARER_MIN_LEN {
                out.push_str(BEARER);
                out.push_str(REDACTED);
                index += BEARER.len() + after[..value_len].len();
                continue;
            }
        }

        if let Some((matched_len, replacement)) = match_pattern(rest) {
            out.push_str(&replacement);
            index += matched_len;
            continue;
        }

        // Not a match: copy one character and move on.
        let char_len = rest.chars().next().map_or(1, char::len_utf8);
        out.push_str(&rest[..char_len]);
        index += char_len;
        let _ = bytes;
    }

    Cow::Owned(out)
}

/// Cheap pre-check so the common case never allocates or scans twice.
fn might_contain_secret(text: &str) -> bool {
    text.contains(BEARER) || PATTERNS.iter().any(|p| text.contains(p.prefix))
}

/// If `rest` starts with a credential, return how much of it to consume and what
/// to put in its place.
fn match_pattern(rest: &str) -> Option<(usize, String)> {
    for pattern in PATTERNS {
        let Some(after) = rest.strip_prefix(pattern.prefix) else {
            continue;
        };
        let body: String = after.chars().take_while(|c| is_secret_char(*c)).collect();

        if body.len() < pattern.min_len {
            continue;
        }
        if pattern.needs_digit && !body.chars().any(|c| c.is_ascii_digit()) {
            continue;
        }

        // Keep the prefix: "ghp_[redacted]" says what kind of credential leaked,
        // which is what an operator needs in order to rotate the right one.
        return Some((
            pattern.prefix.len() + body.len(),
            format!("{}{REDACTED}", pattern.prefix),
        ));
    }
    None
}

/// Redact one structured field, by name and then by pattern.
///
/// The name check comes first and is total: a field called `token` is replaced
/// entirely rather than pattern-scrubbed, because a credential this code does not
/// recognise is exactly the one worth protecting.
pub fn redact_field<'a>(name: &str, value: &'a str) -> Cow<'a, str> {
    if is_sensitive_field(name) {
        Cow::Borrowed(REDACTED)
    } else {
        redact(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assert `input` is scrubbed: the secret is gone and the marker is present.
    fn assert_scrubbed(input: &str, secret: &str) {
        let out = redact(input);
        assert!(
            !out.contains(secret),
            "{secret:?} survived redaction of {input:?}: got {out}"
        );
        assert!(
            out.contains(REDACTED),
            "nothing was marked as redacted in {out}"
        );
    }

    #[test]
    fn redact_bearer_tokens() {
        assert_scrubbed(
            "Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.payload.sig",
            "eyJhbGciOiJIUzI1NiJ9.payload.sig",
        );
        // The scheme survives, so the log still says what kind of header it was.
        assert!(redact("Bearer abcdefghijklmnop").starts_with("Bearer "));
    }

    #[test]
    fn redact_github_token_families() {
        for prefix in ["ghp_", "gho_", "ghu_", "ghs_", "ghr_"] {
            let token = format!("{prefix}A1b2C3d4E5f6G7h8I9j0K1l2");
            assert_scrubbed(&format!("cloning with {token} failed"), &token);
        }
        let fine_grained = "github_pat_11ABCDEFG0aBcDeFgHiJkLmNoPqRsTuVwXyZ";
        assert_scrubbed(&format!("token={fine_grained}"), fine_grained);
    }

    #[test]
    fn redact_keeps_the_prefix_so_the_right_credential_gets_rotated() {
        // "something leaked" is not actionable; "a ghp_ token leaked" is.
        let out = redact("ghp_A1b2C3d4E5f6G7h8I9j0K1l2");
        assert_eq!(out, format!("ghp_{REDACTED}"));
    }

    #[test]
    fn redact_suite_prefixed_keys() {
        for key in ["trama_a1b2c3d4e5f6g7h8", "andare_9z8y7x6w5v4u3t2s"] {
            assert_scrubbed(&format!("using {key} to connect"), key);
        }
    }

    #[test]
    fn redact_does_not_eat_ordinary_config_field_names() {
        // The reason the suite prefixes require a digit. `andare_min_severity` and
        // `trama_space` are real RepoConfig fields (SPEC §13.2); redacting them
        // would make the config log useless and hide nothing.
        for ordinary in [
            "andare_min_severity",
            "andare_key_regex",
            "andare_project",
            "trama_space",
            "trama_publish",
        ] {
            let text = format!("setting {ordinary} = high");
            assert_eq!(
                redact(&text),
                text,
                "{ordinary} must not be treated as a secret"
            );
        }
    }

    #[test]
    fn redact_leaves_ordinary_prose_untouched_and_unallocated() {
        let prose = "reviewing commit deadbeef on branch main: 3 findings, 0 blocking";
        assert!(
            matches!(redact(prose), Cow::Borrowed(_)),
            "the common path must not allocate"
        );
        assert_eq!(redact(prose), prose);
    }

    #[test]
    fn redact_a_keychain_placeholder_is_not_a_secret() {
        // SPEC §13.1: the placeholder is a *reference*. Redacting the name would
        // make "no keychain entry `github-token`" undiagnosable.
        let text = "resolving {{keychain:github-token}} at connect time";
        assert_eq!(redact(text), text);
    }

    #[test]
    fn redact_handles_several_secrets_in_one_line() {
        let text = "primary ghp_A1b2C3d4E5f6G7h8I9j0K1l2 fallback trama_a1b2c3d4e5f6g7h8 done";
        let out = redact(text);
        assert!(!out.contains("A1b2C3d4E5f6G7h8I9j0K1l2"), "{out}");
        assert!(!out.contains("a1b2c3d4e5f6g7h8"), "{out}");
        assert!(out.contains("done"), "surrounding text must survive: {out}");
        assert_eq!(out.matches(REDACTED).count(), 2);
    }

    #[test]
    fn redact_is_not_confused_by_multibyte_text() {
        // Byte-indexing a UTF-8 string is the classic way this kind of scanner
        // panics. The secret sits between two multi-byte runs on purpose.
        let text = "clé — ghp_A1b2C3d4E5f6G7h8I9j0K1l2 — 完了";
        let out = redact(text);
        assert!(!out.contains("A1b2C3d4E5f6G7h8I9j0K1l2"), "{out}");
        assert!(out.contains("clé"), "{out}");
        assert!(out.contains("完了"), "{out}");
    }

    #[test]
    fn redact_short_lookalikes_are_left_alone() {
        // `sk-` and friends appear in ordinary words. A length floor keeps the
        // scrubber from mangling prose.
        for harmless in ["sk-1", "Bearer x", "trama_ok"] {
            assert_eq!(redact(harmless), harmless, "{harmless} must survive");
        }
    }

    #[test]
    fn redact_field_names_are_matched_case_insensitively_and_by_substring() {
        for name in [
            "token",
            "github_token",
            "X-Auth-Token",
            "SECRET",
            "password",
            "api_key",
            "apiKey",
            "authorization",
            "trama_credential",
        ] {
            assert!(
                is_sensitive_field(name),
                "{name} must be treated as sensitive"
            );
        }
        for name in ["repo", "run_id", "change", "depth", "verdict", "file"] {
            assert!(!is_sensitive_field(name), "{name} must not be");
        }
    }

    #[test]
    fn redact_field_replaces_a_sensitive_value_whatever_shape_it_has() {
        // The point of name-based redaction: a credential this code does not
        // recognise is exactly the one worth protecting.
        assert_eq!(redact_field("token", "not-a-recognised-shape"), REDACTED);
        assert_eq!(redact_field("password", "hunter2"), REDACTED);
        assert_eq!(redact_field("api_key", ""), REDACTED);
    }

    #[test]
    fn redact_field_still_scrubs_patterns_in_an_innocuous_field() {
        // An error message quoting a request is the realistic leak.
        let out = redact_field(
            "error",
            "GET /repos failed: header was Bearer eyJhbGciOiJIUzI1NiJ9",
        );
        assert!(!out.contains("eyJhbGciOiJIUzI1NiJ9"), "{out}");
        assert!(out.contains("GET /repos failed"), "{out}");
    }

    #[test]
    fn redact_field_leaves_an_ordinary_field_borrowed() {
        assert!(matches!(
            redact_field("repo", "rev-local"),
            Cow::Borrowed(_)
        ));
    }
}
