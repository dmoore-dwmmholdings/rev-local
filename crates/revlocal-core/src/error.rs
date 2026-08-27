//! Error types for the domain layer.

use std::fmt;

/// A string that does not name any variant of a domain enum.
///
/// Produced by every `FromStr` implementation in this crate. It carries the
/// enum's name and the accepted spellings so that a bad value read out of SQLite
/// or off an engine's JSON says what it should have been.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{value}` is not a valid {type_name}; expected one of: {}", .expected.join(", "))]
pub struct ParseEnumError {
    /// The name of the enum that rejected the value, e.g. `Severity`.
    pub type_name: &'static str,
    /// The value that was rejected.
    pub value: String,
    /// Every wire spelling the enum accepts, in declaration order.
    pub expected: &'static [&'static str],
}

impl ParseEnumError {
    /// Build a rejection for `value` against `type_name`'s `expected` spellings.
    pub fn new(type_name: &'static str, value: &str, expected: &'static [&'static str]) -> Self {
        Self {
            type_name,
            value: value.to_owned(),
            expected,
        }
    }
}

/// Errors the domain layer can raise without performing any I/O.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DomainError {
    /// A string did not name a variant of a domain enum.
    #[error(transparent)]
    ParseEnum(#[from] ParseEnumError),
}

/// The domain layer's result alias.
pub type Result<T, E = DomainError> = std::result::Result<T, E>;

// Keep `fmt` used even as this module grows; `Display` comes from thiserror.
const _: fn() = || {
    fn assert_display<T: fmt::Display>() {}
    assert_display::<ParseEnumError>();
};
