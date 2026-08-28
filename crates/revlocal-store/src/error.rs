//! The store's error taxonomy.
//!
//! Callers act on these, so a constraint violation must arrive as a named
//! variant rather than as an opaque `sqlx::Error` they would have to
//! string-match. The publish queue in particular *expects* duplicate
//! idempotency keys — a redelivery landing on one is a success, not a failure
//! (SPEC §11.6) — and it cannot tell that from a database being unreachable
//! unless the two are different variants.

use revlocal_core::IllegalTransition;

/// Anything that can go wrong reaching the database.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// A uniqueness constraint refused the write.
    ///
    /// Carries what collided so the caller can decide whether that was expected.
    #[error("{entity} already exists with {key}")]
    AlreadyExists {
        /// The table or logical entity, e.g. `publish_action`.
        entity: &'static str,
        /// The colliding key, e.g. `target=andare, idempotency_key=...`.
        key: String,
    },

    /// The row a caller named does not exist.
    #[error("no {entity} with {key}")]
    NotFound {
        /// The table or logical entity.
        entity: &'static str,
        /// How it was addressed.
        key: String,
    },

    /// A run was asked to move to a status its lifecycle does not allow.
    #[error(transparent)]
    IllegalTransition(#[from] IllegalTransition),

    /// A stored value does not parse as the domain type for its column.
    ///
    /// Distinct from a database error because it means data on disk disagrees
    /// with this build — a failed migration or a hand-edited row — rather than
    /// the database being unavailable.
    #[error("stored {column} value is not valid: {detail}")]
    Corrupt {
        /// Which column held the value.
        column: &'static str,
        /// What was wrong with it.
        detail: String,
    },

    /// The database rejected a statement or could not be reached.
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    /// A migration could not be applied or reverted.
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
}

impl StoreError {
    /// Whether this is a uniqueness collision.
    ///
    /// The publish queue's redelivery path asks exactly this question.
    pub const fn is_already_exists(&self) -> bool {
        matches!(self, Self::AlreadyExists { .. })
    }

    /// Reclassify a `sqlx` error as [`StoreError::AlreadyExists`] when it is a
    /// uniqueness violation, leaving every other error alone.
    ///
    /// SQLite reports both `UNIQUE` and `PRIMARY KEY` collisions with code 2067
    /// or 1555; matching on the code rather than the message keeps this working
    /// when SQLite rewords itself.
    pub fn from_sqlx(entity: &'static str, key: impl Into<String>, error: sqlx::Error) -> Self {
        let is_unique_violation = error
            .as_database_error()
            .and_then(|e| e.code())
            .is_some_and(|code| code == "2067" || code == "1555");

        if is_unique_violation {
            Self::AlreadyExists {
                entity,
                key: key.into(),
            }
        } else {
            Self::Database(error)
        }
    }
}

/// The store's result alias.
pub type Result<T, E = StoreError> = std::result::Result<T, E>;
