//! Internal macros that generate the repetitive half of the domain model.

/// Define a domain enum whose wire form is a fixed string.
///
/// Every domain enum in SPEC §5 appears in three places — a Rust variant, a SQLite
/// `CHECK` constraint, and JSON on the engine and MCP boundaries — and all three
/// have to agree. The macro takes the wire spelling as an explicit literal rather
/// than deriving it from the variant name, so the SQL `CHECK` list can be read off
/// the Rust source and a rename cannot silently change the stored value.
///
/// It generates: the enum (with `Serialize`/`Deserialize` renamed to the literal),
/// `ALL` in declaration order, `as_str`, `Display`, and `FromStr` returning
/// [`ParseEnumError`](crate::ParseEnumError).
///
/// `Ord` is derived, so **declaration order is the ordering**. Where an enum has a
/// meaningful order (`AutonomyMode`, `Severity`) the variants are declared so that
/// the derived comparison is the one call sites want; see ADR 0004.
macro_rules! string_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $(
                $(#[$vmeta:meta])*
                $variant:ident => $wire:literal
            ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash,
            ::serde::Serialize, ::serde::Deserialize,
        )]
        $vis enum $name {
            $(
                $(#[$vmeta])*
                #[serde(rename = $wire)]
                $variant,
            )+
        }

        impl $name {
            /// Every variant, in declaration order.
            ///
            /// This is what the serde round-trip test enumerates, so a new variant
            /// is covered by that test the moment it is added here.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            /// Every accepted wire spelling, in declaration order.
            ///
            /// Matches the SQLite `CHECK (... IN (...))` list for this column.
            pub const WIRE_NAMES: &'static [&'static str] = &[$($wire),+];

            /// This variant's wire spelling.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $wire),+
                }
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl ::std::str::FromStr for $name {
            type Err = $crate::ParseEnumError;

            fn from_str(s: &str) -> ::std::result::Result<Self, Self::Err> {
                match s {
                    $($wire => Ok(Self::$variant),)+
                    other => Err($crate::ParseEnumError::new(
                        stringify!($name),
                        other,
                        Self::WIRE_NAMES,
                    )),
                }
            }
        }
    };
}

/// Define a newtype over a SQLite `INTEGER PRIMARY KEY`.
///
/// The point is that `RepoId` and `RunId` are different types. `From<i64>` is
/// deliberately **not** implemented: an id has to be built with `new`, so a bare
/// integer cannot drift across an API boundary through an inferred `.into()`.
macro_rules! id_newtype {
    ($(#[$meta:meta])* $vis:vis struct $name:ident;) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash,
            ::serde::Serialize, ::serde::Deserialize,
        )]
        #[serde(transparent)]
        $vis struct $name(i64);

        impl $name {
            #[doc = concat!("Wrap a raw row id as a `", stringify!($name), "`.")]
            ///
            /// Explicit by design — see the module documentation.
            pub const fn new(raw: i64) -> Self {
                Self(raw)
            }

            /// The underlying row id, for the persistence layer only.
            pub const fn get(self) -> i64 {
                self.0
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                ::std::fmt::Display::fmt(&self.0, f)
            }
        }
    };
}
