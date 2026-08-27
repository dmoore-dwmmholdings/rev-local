//! Typed repositories over the `repo` and `cursor` tables (SPEC §5).
//!
//! Queries use sqlx's `query!` macros, so the SQL is checked against the real
//! schema at compile time and a column renamed in a migration breaks the build
//! rather than the daemon (ADR 0009).

use crate::{Pool, Result, StoreError};
use revlocal_core::{AutonomyMode, Cursor, EngineKind, Repo, RepoId, RepoKind, Timestamp};
use std::str::FromStr;

/// Parse a domain enum out of a `TEXT` column, naming the column if it fails.
///
/// A `CHECK` constraint keeps out values the schema never allowed, but it cannot
/// keep out a value this build does not know — a row written by a newer rev-local
/// is exactly that. Failing with [`StoreError::Corrupt`] says "disk disagrees with
/// this build", which is a different problem from the database being unreachable.
pub(crate) fn parse_enum<T: FromStr>(column: &'static str, raw: &str) -> Result<T>
where
    T::Err: std::fmt::Display,
{
    T::from_str(raw).map_err(|e| StoreError::Corrupt {
        column,
        detail: e.to_string(),
    })
}

/// Parse an RFC 3339 timestamp out of a `TEXT` column.
pub(crate) fn parse_time(column: &'static str, raw: &str) -> Result<Timestamp> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|t| t.with_timezone(&chrono::Utc))
        .map_err(|e| StoreError::Corrupt {
            column,
            detail: e.to_string(),
        })
}

/// Format a timestamp for a `TEXT` column.
///
/// RFC 3339 with a `Z` offset, which sorts lexicographically — `idx_audit_at`
/// orders by a text column, so the encoding has to be sortable to be useful.
pub(crate) fn format_time(at: Timestamp) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

/// CRUD over the `repo` table.
#[derive(Debug, Clone)]
pub struct RepoStore<'a> {
    pool: &'a Pool,
}

impl<'a> RepoStore<'a> {
    /// Open the repository over `pool`.
    pub const fn new(pool: &'a Pool) -> Self {
        Self { pool }
    }

    /// Insert a repo, returning it with the id the database assigned.
    ///
    /// `repo.id` on the input is ignored: the database owns identity. Taking the
    /// whole [`Repo`] anyway keeps one shape for the caller rather than a
    /// parallel "new repo" struct that would drift from it.
    pub async fn insert(&self, repo: &Repo) -> Result<Repo> {
        let kind = repo.kind.as_str();
        let engine = repo.engine.as_str();
        let autonomy = repo.autonomy.as_str();
        let created = format_time(repo.created_at);
        let updated = format_time(repo.updated_at);

        let id = sqlx::query!(
            "INSERT INTO repo
               (name, kind, local_path, remote_url, default_branch, engine, autonomy,
                enabled, config_json, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             RETURNING id",
            repo.name,
            kind,
            repo.local_path,
            repo.remote_url,
            repo.default_branch,
            engine,
            autonomy,
            repo.enabled,
            repo.config_json,
            created,
            updated,
        )
        .fetch_one(self.pool)
        .await
        .map_err(|e| StoreError::from_sqlx("repo", format!("name={}", repo.name), e))?
        .id;

        Ok(Repo {
            id: RepoId::new(id),
            ..repo.clone()
        })
    }

    /// Fetch one repo by id.
    pub async fn get(&self, id: RepoId) -> Result<Repo> {
        let raw = id.get();
        let row = sqlx::query!(
            "SELECT id, name, kind, local_path, remote_url, default_branch, engine,
                    autonomy, enabled, config_json, created_at, updated_at
             FROM repo WHERE id = ?",
            raw
        )
        .fetch_optional(self.pool)
        .await?
        .ok_or_else(|| StoreError::NotFound {
            entity: "repo",
            key: format!("id={raw}"),
        })?;

        Ok(Repo {
            id: RepoId::new(row.id),
            name: row.name,
            kind: parse_enum::<RepoKind>("repo.kind", &row.kind)?,
            local_path: row.local_path,
            remote_url: row.remote_url,
            default_branch: row.default_branch,
            engine: parse_enum::<EngineKind>("repo.engine", &row.engine)?,
            autonomy: parse_enum::<AutonomyMode>("repo.autonomy", &row.autonomy)?,
            enabled: row.enabled != 0,
            config_json: row.config_json,
            created_at: parse_time("repo.created_at", &row.created_at)?,
            updated_at: parse_time("repo.updated_at", &row.updated_at)?,
        })
    }

    /// Every repo, oldest first.
    pub async fn list(&self) -> Result<Vec<Repo>> {
        let ids = sqlx::query!("SELECT id FROM repo ORDER BY id")
            .fetch_all(self.pool)
            .await?;

        let mut repos = Vec::with_capacity(ids.len());
        for row in ids {
            repos.push(self.get(RepoId::new(row.id)).await?);
        }
        Ok(repos)
    }

    /// Record which transport reaches GitHub for this repo (SPEC §6.3).
    ///
    /// Separate from [`update`](Self::update) because it is a *discovered* fact, not
    /// a user setting: it is written by the probe, not by anyone editing config, and
    /// folding it into the general update would let a stale in-memory `Repo` clobber
    /// a fresher probe result.
    pub async fn set_github_transport(&self, id: RepoId, transport: Option<&str>) -> Result<()> {
        let raw = id.get();
        let affected = sqlx::query!(
            "UPDATE repo SET github_transport = ? WHERE id = ?",
            transport,
            raw
        )
        .execute(self.pool)
        .await?
        .rows_affected();

        if affected == 0 {
            return Err(StoreError::NotFound {
                entity: "repo",
                key: format!("id={raw}"),
            });
        }
        Ok(())
    }

    /// The transport last probed for this repo, if any.
    ///
    /// `None` means "not probed yet", which is deliberately distinguishable from
    /// `Some("unauthenticated")`: one means nobody has looked, the other means we
    /// looked and this is as good as it gets.
    pub async fn github_transport(&self, id: RepoId) -> Result<Option<String>> {
        let raw = id.get();
        let row = sqlx::query!("SELECT github_transport FROM repo WHERE id = ?", raw)
            .fetch_optional(self.pool)
            .await?
            .ok_or_else(|| StoreError::NotFound {
                entity: "repo",
                key: format!("id={raw}"),
            })?;
        Ok(row.github_transport)
    }

    /// Update the mutable fields of a repo.
    ///
    /// `name` and `kind` are not updatable here: a repo that changes either is a
    /// different repo, and renaming one silently would break every finding
    /// fingerprint, which hashes `repo.name` (SPEC §10.3).
    pub async fn update(&self, repo: &Repo) -> Result<()> {
        let id = repo.id.get();
        let engine = repo.engine.as_str();
        let autonomy = repo.autonomy.as_str();
        let updated = format_time(repo.updated_at);

        let affected = sqlx::query!(
            "UPDATE repo
             SET local_path = ?, remote_url = ?, default_branch = ?, engine = ?,
                 autonomy = ?, enabled = ?, config_json = ?, updated_at = ?
             WHERE id = ?",
            repo.local_path,
            repo.remote_url,
            repo.default_branch,
            engine,
            autonomy,
            repo.enabled,
            repo.config_json,
            updated,
            id,
        )
        .execute(self.pool)
        .await?
        .rows_affected();

        if affected == 0 {
            return Err(StoreError::NotFound {
                entity: "repo",
                key: format!("id={id}"),
            });
        }
        Ok(())
    }

    /// Delete a repo and, by cascade, everything belonging to it.
    pub async fn delete(&self, id: RepoId) -> Result<()> {
        let raw = id.get();
        let affected = sqlx::query!("DELETE FROM repo WHERE id = ?", raw)
            .execute(self.pool)
            .await?
            .rows_affected();

        if affected == 0 {
            return Err(StoreError::NotFound {
                entity: "repo",
                key: format!("id={raw}"),
            });
        }
        Ok(())
    }
}

/// Read and advance discovery cursors (SPEC §5, §7.1).
#[derive(Debug, Clone)]
pub struct CursorStore<'a> {
    pool: &'a Pool,
}

impl<'a> CursorStore<'a> {
    /// Open the repository over `pool`.
    pub const fn new(pool: &'a Pool) -> Self {
        Self { pool }
    }

    /// The cursor for one `(repo, scope)`, if it has ever advanced.
    ///
    /// `None` means "never looked", which a caller must not confuse with "looked
    /// and found nothing" — the first is a full backfill, the second is a no-op.
    pub async fn get(&self, repo_id: RepoId, scope: &str) -> Result<Option<Cursor>> {
        let raw = repo_id.get();
        let row = sqlx::query!(
            "SELECT repo_id, scope, value, updated_at FROM cursor
             WHERE repo_id = ? AND scope = ?",
            raw,
            scope
        )
        .fetch_optional(self.pool)
        .await?;

        row.map(|row| {
            Ok(Cursor {
                repo_id: RepoId::new(row.repo_id),
                scope: row.scope,
                value: row.value,
                updated_at: parse_time("cursor.updated_at", &row.updated_at)?,
            })
        })
        .transpose()
    }

    /// Advance (or create) a cursor, atomically.
    ///
    /// One `INSERT ... ON CONFLICT DO UPDATE` rather than a read followed by a
    /// write: a poll and a webhook can discover the same branch at once, and a
    /// read-then-write would let one overwrite the other's advance, silently
    /// re-reviewing or skipping changes.
    pub async fn advance(
        &self,
        repo_id: RepoId,
        scope: &str,
        value: &str,
        at: Timestamp,
    ) -> Result<()> {
        let raw = repo_id.get();
        let updated = format_time(at);

        sqlx::query!(
            "INSERT INTO cursor (repo_id, scope, value, updated_at)
             VALUES (?, ?, ?, ?)
             ON CONFLICT (repo_id, scope) DO UPDATE
               SET value = excluded.value, updated_at = excluded.updated_at",
            raw,
            scope,
            value,
            updated,
        )
        .execute(self.pool)
        .await
        .map_err(|e| StoreError::from_sqlx("cursor", format!("repo_id={raw}, scope={scope}"), e))?;

        Ok(())
    }

    /// Every cursor for one repo, by scope.
    pub async fn list_for_repo(&self, repo_id: RepoId) -> Result<Vec<Cursor>> {
        let raw = repo_id.get();
        let rows = sqlx::query!(
            "SELECT repo_id, scope, value, updated_at FROM cursor
             WHERE repo_id = ? ORDER BY scope",
            raw
        )
        .fetch_all(self.pool)
        .await?;

        rows.into_iter()
            .map(|row| {
                Ok(Cursor {
                    repo_id: RepoId::new(row.repo_id),
                    scope: row.scope,
                    value: row.value,
                    updated_at: parse_time("cursor.updated_at", &row.updated_at)?,
                })
            })
            .collect()
    }
}
