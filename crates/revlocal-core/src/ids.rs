//! Newtype identifiers for every persisted entity.
//!
//! SPEC §5 gives each table an `INTEGER PRIMARY KEY`. Those ids never cross an API
//! boundary as a bare `i64`: a function that wants a `RunId` will not accept a
//! `RepoId`, and neither will accept `42`.
//!
//! There is no `From<i64>`. Constructing an id is always the explicit
//! [`new`](RepoId::new), which keeps `.into()` from quietly inferring the wrong
//! id type at a call site — the exact mistake the newtypes exist to prevent.
//! `compile_fail` doctests below, and the `trybuild` suite in
//! `tests/ids_are_not_interchangeable.rs`, hold that line.
//!
//! ```
//! use revlocal_core::{RepoId, RunId};
//!
//! fn cancel(run: RunId) -> i64 { run.get() }
//! assert_eq!(cancel(RunId::new(7)), 7);
//! ```
//!
//! A `RepoId` is not a `RunId`:
//!
//! ```compile_fail
//! use revlocal_core::{RepoId, RunId};
//!
//! fn cancel(run: RunId) -> i64 { run.get() }
//! cancel(RepoId::new(7));
//! ```
//!
//! And a bare integer is not an id:
//!
//! ```compile_fail
//! use revlocal_core::RunId;
//!
//! fn cancel(run: RunId) -> i64 { run.get() }
//! cancel(7);
//! ```

id_newtype! {
    /// Identifies a row in `repo` (SPEC §5).
    pub struct RepoId;
}

id_newtype! {
    /// Identifies a row in `change` (SPEC §5).
    pub struct ChangeId;
}

id_newtype! {
    /// Identifies a row in `run` (SPEC §5).
    pub struct RunId;
}

id_newtype! {
    /// Identifies a row in `finding` (SPEC §5).
    pub struct FindingId;
}

id_newtype! {
    /// Identifies a row in `publish_action` (SPEC §5).
    pub struct PublishActionId;
}

id_newtype! {
    /// Identifies a row in `suppression` (SPEC §5).
    pub struct SuppressionId;
}

id_newtype! {
    /// Identifies a row in `audit` (SPEC §5).
    pub struct AuditId;
}
