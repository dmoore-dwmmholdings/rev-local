# 8. Database pragmas belong on the connect options

Date: 2026-08-27
Status: accepted
Item: RL-108

## Context

The schema depends on three SQLite pragmas: `journal_mode = WAL` (decision D7, so
the UI can read while a run writes), `foreign_keys = ON`, and a `busy_timeout`.

`foreign_keys` is the one that matters most and is easiest to lose. SQLite
defaults it to **OFF**, and it is a per-connection setting. Every `ON DELETE
CASCADE` in SPEC §5 is inert without it — deleting a repo silently leaves its
changes, runs and findings behind as orphans, and nothing fails.

The natural implementation is to connect and then execute the pragmas. Against a
pool that is wrong in a way that does not show up in testing: the pool opens more
connections later, under load, and those never run the setup statements. The
database then behaves correctly on a quiet machine and loses referential integrity
on a busy one.

## Decision

Pragmas are set on `SqliteConnectOptions`, never executed after connecting. sqlx
applies connect options to every connection the pool opens, including ones created
later.

`revlocal_store::open` is the only way the application gets a pool, and it applies
the options and runs migrations. There is a second constructor,
`open_unmigrated`, used by the migration tests and by `revlocal db` subcommands
that need to look at a database before deciding what to do with it.

## Testing consequences

Two things about these tests are deliberate:

- **Tests use a file-backed database in a temp dir, never `:memory:`.** An
  in-memory SQLite database reports `journal_mode = memory` whatever is requested,
  so a WAL assertion against one passes vacuously.
- **`foreign_keys` is asserted across several pooled connections**, and separately
  a cascade is exercised end to end. The pragma being readable is the mechanism;
  the orphaned row not existing is the behaviour worth having.
- `an_unmigrated_database_has_no_spec_tables` exists so
  `creates_the_schema_from_empty` cannot pass vacuously. If `open_unmigrated` also
  produced the schema, the positive test would prove nothing.

## Down migrations

`0001_init.down.sql` drops children before parents, so the drops hold with
`foreign_keys = ON` — which is how the pool always opens, so a down migration
written parent-first would fail only in the configuration that actually ships.

The test reverts and then **re-applies**. A down migration that cannot be followed
by an up migration is a dead end, not a path.

## Note for RL-109

This crate does not use sqlx's compile-time-checked `query!` macros yet, because
they need a `DATABASE_URL` at build time and that would make the workspace's build
depend on a prepared database. `RL-109` should decide between committing
`.sqlx/` offline metadata (`cargo sqlx prepare`) and staying with runtime-checked
`query()`. Offline metadata is the better answer if it can be kept current in CI;
the risk is a stale `.sqlx/` silently diverging from the migrations.
