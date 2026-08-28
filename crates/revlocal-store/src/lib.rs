//! SQLite persistence: migrations, entity repositories, audit log and budget
//! ledger (SPEC §5, decision D7).
//!
//! Everything here opens the database through [`open`], which applies the three
//! pragmas the schema depends on. They are set on the *connect options*, not
//! executed after connecting, so every connection in the pool gets them —
//! including ones the pool opens later under load, which is where a
//! connect-then-configure approach silently loses them.

use std::path::Path;
use std::time::Duration;

use sqlx::sqlite::SqliteConnectOptions;

/// The embedded migration set, compiled from `migrations/`.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// How long a writer waits for a lock before giving up.
///
/// WAL allows one writer at a time. The daemon reviews and publishes
/// concurrently (SPEC §4.3), so a brief overlap is normal and a busy timeout is
/// what turns it into a short wait instead of a `database is locked` error.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

mod changes;
mod error;
mod publish;
mod repos;

pub use changes::{ChangeStore, FindingStore, RunStore};
pub use error::{Result, StoreError};
pub use publish::{AuditStore, BudgetLedgerStore, PublishActionStore};
pub use repos::{CursorStore, RepoStore};

/// A pool of connections to one rev-local database.
pub type Pool = sqlx::SqlitePool;

/// Open (creating if absent) the database at `path` and run every migration.
///
/// The pragmas are not optional decoration:
///
/// - `journal_mode = WAL` lets the UI read while a run writes (decision D7).
/// - `foreign_keys = ON` is **off by default in SQLite**, and every
///   `ON DELETE CASCADE` in SPEC §5 is inert without it — deleting a repo would
///   leave its runs and findings behind as orphans.
/// - `busy_timeout` turns normal write contention into a wait rather than an
///   error.
pub async fn open(path: &Path) -> Result<Pool> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .foreign_keys(true)
        .busy_timeout(BUSY_TIMEOUT);

    let pool = Pool::connect_with(options).await?;
    MIGRATOR.run(&pool).await?;
    Ok(pool)
}

/// Open a pool without running migrations.
///
/// For the migration tests and for `revlocal db` subcommands that need to inspect
/// a database before deciding what to do with it.
pub async fn open_unmigrated(path: &Path) -> Result<Pool> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .foreign_keys(true)
        .busy_timeout(BUSY_TIMEOUT);

    Ok(Pool::connect_with(options).await?)
}

/// Apply every migration that has not been applied yet.
///
/// Idempotent: sqlx records applied versions in `_sqlx_migrations` and skips
/// them, so running this on an up-to-date database is a no-op.
pub async fn migrate(pool: &Pool) -> Result<()> {
    Ok(MIGRATOR.run(pool).await?)
}

/// Revert migrations down to and including `target`.
///
/// `target = 0` empties the database of rev-local's own tables. This exists so
/// that a migration can be tested in both directions; it is not something the app
/// does on its own.
pub async fn revert_to(pool: &Pool, target: i64) -> Result<()> {
    Ok(MIGRATOR.undo(pool, target).await?)
}
