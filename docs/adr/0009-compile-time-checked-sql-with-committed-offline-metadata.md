# 9. Compile-time-checked SQL, with committed offline metadata

Date: 2026-08-27
Status: accepted
Item: RL-109a
Supersedes the open question at the end of ADR 0008.

## Context

`RL-109` asks for "compile-time-checked queries". sqlx's `query!` macros verify SQL
against a real schema at compile time, which catches a renamed column, a wrong
type, and a mis-numbered bind parameter at build time rather than on the first
production write.

They need the schema at build time, one of two ways: a live database named by
`DATABASE_URL`, or metadata prepared ahead of time by `cargo sqlx prepare` and
committed to the repository.

A live `DATABASE_URL` was rejected outright: it makes `cargo build` depend on a
running database, which breaks a clean checkout, breaks CI, and makes the workspace
unbuildable by anyone who has not run a setup script first.

## Decision

Use `query!` with **committed offline metadata**:

- `crates/revlocal-store/.sqlx/` holds one JSON file per distinct query, generated
  by `cargo sqlx prepare`.
- `.cargo/config.toml` sets `SQLX_OFFLINE = "true"` for the whole workspace, so
  the macros always read that metadata and never look for a database.
- CI asserts `.sqlx/` exists, so a missing directory fails with a message naming
  the fix rather than a confusing "set DATABASE_URL".

Verified by deleting the database used to prepare the metadata and rebuilding
successfully — a clean checkout with no SQLite anywhere still compiles.

### The drift risk, and why it is acceptable

Committed metadata can go stale: change a migration, forget to re-run
`cargo sqlx prepare`, and a query keeps compiling against the old schema and fails
at runtime instead. sqlx catches a *new or edited* query (no metadata entry, so the
build fails) but not a query whose SQL is unchanged while the schema moved under it.

Two things cover that gap:

1. **Every query is exercised by a round-trip test against a freshly migrated
   database.** The CRUD tests do not mock the store; they run the real SQL over the
   real schema. A stale entry fails there.
2. Re-running `cargo sqlx prepare` after a migration is a one-line step, and the
   failure it prevents is loud rather than silent.

`cargo sqlx prepare --check` in CI would close the gap directly, but it needs
`sqlx-cli` installed on every runner — a multi-minute build on all three
platforms. The round-trip tests give the same signal for a fraction of the cost.
Revisit if CI ever caches the tool.

## Related: sqlx 0.9

Aligning with `sqlx-cli` 0.9 (the metadata format is not compatible across majors)
also brought a guard that rejects dynamic SQL strings unless explicitly wrapped in
`AssertSqlSafe`. That is worth keeping. The one place it fires is a test helper
interpolating a table name into `PRAGMA table_info(...)`, which cannot take a bind
parameter; the value is a `&'static str` constant from the test file, and the
wrapper documents that audit at the call site.

## Consequences

- **After changing a migration, run `cargo sqlx prepare` from
  `crates/revlocal-store/` and commit the result.** A CONTRIBUTING note belongs in
  `RL-1206`.
- `revlocal-store` reads enum columns through `FromStr` and maps failures to
  `StoreError::Corrupt`, which is deliberately distinct from `Database`: a value
  this build does not recognise means the disk disagrees with the binary — a failed
  migration or a row written by a newer rev-local — not an unavailable database.
