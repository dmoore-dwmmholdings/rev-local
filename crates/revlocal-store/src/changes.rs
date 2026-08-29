//! Typed repositories over the `change`, `run` and `finding` tables (SPEC §5).

use crate::repos::{format_time, parse_enum, parse_time};
use crate::{Pool, Result, StoreError};
use revlocal_core::{
    Category, Change, ChangeId, ChangeKind, Depth, DiffStat, EngineKind, Finding, FindingId,
    FindingState, RepoId, Run, RunId, RunStatus, Severity, Timestamp, TriggerSource, Usage,
};

/// Unwrap the id an `INSERT ... RETURNING id` produced.
///
/// SQLite declares `INTEGER PRIMARY KEY` as nullable — it is a rowid alias — so
/// sqlx types the returned column as `Option<i64>`. A row that inserted without a
/// rowid is impossible, but "impossible" is not the same as "may unwrap": if it
/// ever happens the database is not what this build thinks it is, which is what
/// [`StoreError::Corrupt`] means.
fn assigned_id(entity: &'static str, column: &'static str, id: Option<i64>) -> Result<i64> {
    id.ok_or(StoreError::Corrupt {
        column,
        detail: format!("inserting a {entity} returned no row id"),
    })
}

/// Upsert and read changes (SPEC §5).
#[derive(Debug, Clone)]
pub struct ChangeStore<'a> {
    pool: &'a Pool,
}

impl<'a> ChangeStore<'a> {
    /// Open the repository over `pool`.
    pub const fn new(pool: &'a Pool) -> Self {
        Self { pool }
    }

    /// Insert a change, or update the one already recorded for
    /// `(repo_id, kind, external_id)`.
    ///
    /// Upsert rather than insert because rediscovery is the normal case, not the
    /// exception: the poll trigger re-reads the same commits every interval
    /// (SPEC §7.1). An insert would raise a constraint error on every poll, and a
    /// naive "insert if absent" would never notice a PR whose title or head SHA
    /// moved.
    ///
    /// `detected_at` is deliberately **not** updated on conflict. It records when
    /// rev-local first saw the change, and refreshing it on every poll would erase
    /// that.
    pub async fn upsert(&self, change: &Change) -> Result<Change> {
        let repo_id = change.repo_id.get();
        let kind = change.kind.as_str();
        let diff_stat =
            serde_json::to_string(&change.diff_stat).map_err(|e| StoreError::Corrupt {
                column: "change.diff_stat_json",
                detail: e.to_string(),
            })?;
        let authored = change.authored_at.map(format_time);
        let detected = format_time(change.detected_at);

        let id = sqlx::query!(
            "INSERT INTO change
               (repo_id, kind, external_id, title, author_name, author_email, authored_at,
                branch, base_ref, head_ref, url, diff_stat_json, detected_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (repo_id, kind, external_id) DO UPDATE SET
               title = excluded.title,
               author_name = excluded.author_name,
               author_email = excluded.author_email,
               authored_at = excluded.authored_at,
               branch = excluded.branch,
               base_ref = excluded.base_ref,
               head_ref = excluded.head_ref,
               url = excluded.url,
               diff_stat_json = excluded.diff_stat_json
             RETURNING id",
            repo_id,
            kind,
            change.external_id,
            change.title,
            change.author_name,
            change.author_email,
            authored,
            change.branch,
            change.base_ref,
            change.head_ref,
            change.url,
            diff_stat,
            detected,
        )
        .fetch_one(self.pool)
        .await
        .map_err(|e| {
            StoreError::from_sqlx(
                "change",
                format!(
                    "repo_id={repo_id}, kind={kind}, external_id={}",
                    change.external_id
                ),
                e,
            )
        })?
        .id;

        self.get(ChangeId::new(id)).await
    }

    /// Fetch one change by id.
    pub async fn get(&self, id: ChangeId) -> Result<Change> {
        let raw = id.get();
        let row = sqlx::query!(
            "SELECT id, repo_id, kind, external_id, title, author_name, author_email,
                    authored_at, branch, base_ref, head_ref, url, diff_stat_json, detected_at
             FROM change WHERE id = ?",
            raw
        )
        .fetch_optional(self.pool)
        .await?
        .ok_or_else(|| StoreError::NotFound {
            entity: "change",
            key: format!("id={raw}"),
        })?;

        let diff_stat: DiffStat =
            serde_json::from_str(&row.diff_stat_json).map_err(|e| StoreError::Corrupt {
                column: "change.diff_stat_json",
                detail: e.to_string(),
            })?;

        Ok(Change {
            id: ChangeId::new(row.id),
            repo_id: RepoId::new(row.repo_id),
            kind: parse_enum::<ChangeKind>("change.kind", &row.kind)?,
            external_id: row.external_id,
            title: row.title,
            author_name: row.author_name,
            author_email: row.author_email,
            authored_at: row
                .authored_at
                .map(|t| parse_time("change.authored_at", &t))
                .transpose()?,
            branch: row.branch,
            base_ref: row.base_ref,
            head_ref: row.head_ref,
            url: row.url,
            diff_stat,
            detected_at: parse_time("change.detected_at", &row.detected_at)?,
        })
    }

    /// Look up a change by the identity it has in its own system.
    pub async fn find(
        &self,
        repo_id: RepoId,
        kind: ChangeKind,
        external_id: &str,
    ) -> Result<Option<Change>> {
        let raw = repo_id.get();
        let kind_str = kind.as_str();
        let row = sqlx::query!(
            "SELECT id FROM change WHERE repo_id = ? AND kind = ? AND external_id = ?",
            raw,
            kind_str,
            external_id
        )
        .fetch_optional(self.pool)
        .await?;

        match row {
            Some(row) => Ok(Some(self.get(ChangeId::new(row.id)).await?)),
            None => Ok(None),
        }
    }
}

/// Insert runs and move them through their lifecycle (SPEC §5, §9.1).
#[derive(Debug, Clone)]
pub struct RunStore<'a> {
    pool: &'a Pool,
}

impl<'a> RunStore<'a> {
    /// Open the repository over `pool`.
    pub const fn new(pool: &'a Pool) -> Self {
        Self { pool }
    }

    /// Insert a run, returning it with the id the database assigned.
    ///
    /// Refuses an inconsistent run: SPEC §18 forbids silent caps, so a `skipped`
    /// run must carry a reason and a `failed` run must carry its error. Checking
    /// here rather than trusting callers means the invariant holds for whatever
    /// writes next, not just for the caller that exists today.
    pub async fn insert(&self, run: &Run) -> Result<Run> {
        Self::check_consistent(run)?;

        let change_id = run.change_id.get();
        let attempt = i64::from(run.attempt);
        let status = run.status.as_str();
        let engine = run.engine.as_str();
        let depth = run.depth.as_str();
        let trigger = run.trigger.as_str();
        let tokens_in = i64::try_from(run.usage.tokens_in).unwrap_or(i64::MAX);
        let tokens_out = i64::try_from(run.usage.tokens_out).unwrap_or(i64::MAX);
        let tokens_known = i64::from(run.usage.tokens_are_known());
        let started = run.started_at.map(format_time);
        let finished = run.finished_at.map(format_time);
        let created = format_time(run.created_at);
        let truncated = i64::from(run.truncated);
        let omitted =
            serde_json::to_string(&run.omitted_files).map_err(|e| StoreError::Corrupt {
                column: "run.omitted_files_json",
                detail: e.to_string(),
            })?;
        let verdict = run.verdict.map(|v| v.as_str());

        let id = sqlx::query!(
            "INSERT INTO run
               (change_id, attempt, status, engine, depth, trigger, skip_reason, error,
                degraded, tokens_in, tokens_out, tokens_known, cost_usd, started_at, finished_at,
                transcript_path, created_at, truncated, omitted_files_json, verdict,
                summary)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             RETURNING id",
            change_id,
            attempt,
            status,
            engine,
            depth,
            trigger,
            run.skip_reason,
            run.error,
            run.degraded,
            tokens_in,
            tokens_out,
            tokens_known,
            run.usage.cost_usd,
            started,
            finished,
            run.transcript_path,
            created,
            truncated,
            omitted,
            verdict,
            run.summary,
        )
        .fetch_one(self.pool)
        .await
        .map_err(|e| {
            StoreError::from_sqlx(
                "run",
                format!("change_id={change_id}, attempt={}", run.attempt),
                e,
            )
        })?
        .id;

        Ok(Run {
            id: RunId::new(assigned_id("run", "run.id", id)?),
            ..run.clone()
        })
    }

    /// Fetch one run by id.
    pub async fn get(&self, id: RunId) -> Result<Run> {
        let raw = id.get();
        let row = sqlx::query!(
            "SELECT id, change_id, attempt, status, engine, depth, trigger, skip_reason,
                    error, degraded, tokens_in, tokens_out, tokens_known, cost_usd, started_at,
                    finished_at, transcript_path, created_at, truncated,
                    omitted_files_json, verdict, summary
             FROM run WHERE id = ?",
            raw
        )
        .fetch_optional(self.pool)
        .await?
        .ok_or_else(|| StoreError::NotFound {
            entity: "run",
            key: format!("id={raw}"),
        })?;

        Ok(Run {
            id: RunId::new(row.id),
            change_id: ChangeId::new(row.change_id),
            attempt: u32::try_from(row.attempt).unwrap_or(u32::MAX),
            status: parse_enum::<RunStatus>("run.status", &row.status)?,
            engine: parse_enum::<EngineKind>("run.engine", &row.engine)?,
            depth: parse_enum::<Depth>("run.depth", &row.depth)?,
            trigger: parse_enum::<TriggerSource>("run.trigger", &row.trigger)?,
            skip_reason: row.skip_reason,
            error: row.error,
            degraded: row.degraded,
            usage: Usage {
                tokens_in: u64::try_from(row.tokens_in).unwrap_or_default(),
                tokens_out: u64::try_from(row.tokens_out).unwrap_or_default(),
                tokens_known: row.tokens_known != 0,
                cost_usd: row.cost_usd,
            },
            started_at: row
                .started_at
                .map(|t| parse_time("run.started_at", &t))
                .transpose()?,
            finished_at: row
                .finished_at
                .map(|t| parse_time("run.finished_at", &t))
                .transpose()?,
            transcript_path: row.transcript_path,
            truncated: row.truncated != 0,
            omitted_files: row
                .omitted_files_json
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .map_err(|e| StoreError::Corrupt {
                    column: "run.omitted_files_json",
                    detail: e.to_string(),
                })?
                .unwrap_or_default(),
            verdict: row
                .verdict
                .as_deref()
                .map(|v| parse_enum::<revlocal_core::Verdict>("run.verdict", v))
                .transpose()?,
            summary: row.summary,
            created_at: parse_time("run.created_at", &row.created_at)?,
        })
    }

    /// Move a run to `next`, atomically.
    ///
    /// The `UPDATE ... WHERE status = ?` is a compare-and-swap: it succeeds only
    /// if the run is still in the status the caller checked against. A
    /// read-modify-write would let two callers both see `publishing` and both
    /// move the run, and the second move would be from a status that no longer
    /// held.
    ///
    /// Zero rows affected means the run moved underneath us or does not exist, so
    /// the current status is re-read to say which.
    pub async fn transition(&self, id: RunId, from: RunStatus, next: RunStatus) -> Result<()> {
        from.check_transition(next)?;

        let raw = id.get();
        let from_str = from.as_str();
        let next_str = next.as_str();

        let affected = sqlx::query!(
            "UPDATE run SET status = ? WHERE id = ? AND status = ?",
            next_str,
            raw,
            from_str
        )
        .execute(self.pool)
        .await?
        .rows_affected();

        if affected == 1 {
            return Ok(());
        }

        // Report what actually happened rather than a bare "no rows".
        let current = self.get(id).await?.status;
        Err(StoreError::from(
            current
                .check_transition(next)
                .err()
                .unwrap_or(revlocal_core::IllegalTransition {
                    from: current,
                    to: next,
                }),
        ))
    }

    /// Runs stuck in a non-terminal stage since before `now - stale_after`.
    ///
    /// "Stuck" is judged on the most recent timestamp the run has — `started_at` if
    /// it ever started, `created_at` otherwise. A run that has been queued for an
    /// hour is as abandoned as one that has been reviewing for an hour; both mean
    /// nothing is going to move them.
    ///
    /// Terminal runs are excluded by status rather than by age, because a run that
    /// finished last year is not stale, it is done.
    pub async fn list_stale(
        &self,
        now: Timestamp,
        stale_after: chrono::Duration,
    ) -> Result<Vec<Run>> {
        let cutoff = format_time(now - stale_after);

        let rows = sqlx::query!(
            "SELECT id FROM run
             WHERE status NOT IN ('done','failed','skipped','cancelled')
               AND COALESCE(started_at, created_at) < ?
             ORDER BY id",
            cutoff
        )
        .fetch_all(self.pool)
        .await?;

        let mut runs = Vec::with_capacity(rows.len());
        for row in rows {
            runs.push(self.get(RunId::new(row.id)).await?);
        }
        Ok(runs)
    }

    /// Fail a run that a previous process abandoned.
    ///
    /// Not routed through `transition`: the lifecycle allows `queued -> failed` and
    /// so on, but recovery must work from *whatever* stage the run was stuck in,
    /// including ones a caller cannot know in advance. The compare-and-swap that
    /// protects ordinary transitions is not what protects this — the run being
    /// non-terminal is.
    ///
    /// SPEC §18: the error is recorded, so an interrupted run is distinguishable
    /// from one that failed on its own merits.
    pub async fn mark_interrupted(&self, id: RunId, error: &str) -> Result<()> {
        let raw = id.get();
        let failed = RunStatus::Failed.as_str();

        let affected = sqlx::query!(
            "UPDATE run SET status = ?, error = ?
             WHERE id = ? AND status NOT IN ('done','failed','skipped','cancelled')",
            failed,
            error,
            raw
        )
        .execute(self.pool)
        .await?
        .rows_affected();

        if affected == 0 {
            // Already terminal. Recovery racing with a run that finished on its own
            // is normal, and finishing wins — the run really did complete.
            return Ok(());
        }
        Ok(())
    }

    /// Every run for one change, oldest attempt first.
    /// Record the engine process this run spawned (SPEC §12.1).
    ///
    /// Written while the process is alive and cleared when it finishes, so a
    /// non-NULL pid on a run that is no longer active is exactly the orphan
    /// `kill --hard` looks for. Stored rather than held in memory because a crash
    /// is how orphans happen, and an in-memory list dies with the daemon that
    /// would have needed it.
    pub async fn set_engine_pid(&self, id: RunId, pid: Option<u32>) -> Result<()> {
        let raw = id.get();
        let pid = pid.map(i64::from);
        let affected = sqlx::query!("UPDATE run SET engine_pid = ? WHERE id = ?", pid, raw)
            .execute(self.pool)
            .await?
            .rows_affected();

        if affected == 0 {
            return Err(StoreError::NotFound {
                entity: "run",
                key: format!("id={raw}"),
            });
        }
        Ok(())
    }

    /// Pids recorded against runs that are no longer active.
    ///
    /// The orphan candidates: a process rev-local started, on a run that has since
    /// finished, failed or been cancelled. Whether they are still alive is the
    /// caller's question — this only says which pids to ask about.
    pub async fn orphan_pids(&self) -> Result<Vec<(RunId, u32)>> {
        let rows = sqlx::query!(
            "SELECT id, engine_pid FROM run
             WHERE engine_pid IS NOT NULL
               AND status IN ('done','failed','cancelled','skipped')
             ORDER BY id"
        )
        .fetch_all(self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .filter_map(|row| {
                row.engine_pid
                    .and_then(|pid| u32::try_from(pid).ok())
                    .map(|pid| (RunId::new(row.id), pid))
            })
            .collect())
    }

    /// Recent runs, newest first, optionally narrowed (§14's `runs list`).
    ///
    /// Joined through `change` because a run does not carry its repository — the
    /// change does. Filtering in SQL rather than in Rust: `runs list` on a
    /// long-lived install would otherwise read every run ever recorded in order to
    /// show twenty.
    ///
    /// `limit` is applied by the database and reported by the caller. §18: a list
    /// that silently shows the first twenty of nine hundred reads as nine hundred
    /// being twenty.
    pub async fn list_recent(
        &self,
        repo_id: Option<RepoId>,
        status: Option<RunStatus>,
        limit: u32,
    ) -> Result<Vec<Run>> {
        let repo = repo_id.map(RepoId::get);
        let status = status.map(|s| s.as_str().to_owned());
        let limit = i64::from(limit);

        // Both filters are optional and SQLite has no dynamic query builder here,
        // so each is expressed as "unset, or matching" — which keeps this one
        // prepared statement rather than four.
        let rows = sqlx::query!(
            "SELECT run.id AS id
               FROM run
               JOIN change ON change.id = run.change_id
              WHERE (?1 IS NULL OR change.repo_id = ?1)
                AND (?2 IS NULL OR run.status = ?2)
              ORDER BY run.id DESC
              LIMIT ?3",
            repo,
            status,
            limit
        )
        .fetch_all(self.pool)
        .await?;

        let mut runs = Vec::with_capacity(rows.len());
        for row in rows {
            runs.push(self.get(RunId::new(row.id)).await?);
        }
        Ok(runs)
    }

    /// How many runs match, ignoring `limit`.
    ///
    /// So the caller can say "20 of 900" rather than "20".
    pub async fn count_matching(
        &self,
        repo_id: Option<RepoId>,
        status: Option<RunStatus>,
    ) -> Result<u32> {
        let repo = repo_id.map(RepoId::get);
        let status = status.map(|s| s.as_str().to_owned());

        let row = sqlx::query!(
            "SELECT COUNT(*) AS total
               FROM run
               JOIN change ON change.id = run.change_id
              WHERE (?1 IS NULL OR change.repo_id = ?1)
                AND (?2 IS NULL OR run.status = ?2)",
            repo,
            status
        )
        .fetch_one(self.pool)
        .await?;

        Ok(u32::try_from(row.total).unwrap_or(u32::MAX))
    }

    /// Delete runs finished before `before`, and say what went (SPEC §5.1, §14).
    ///
    /// §5.1: run and finding rows are never auto-deleted in v1, and this is the
    /// manual escape hatch. Findings and publish actions go with their runs by
    /// `ON DELETE CASCADE`.
    ///
    /// The transcript paths are returned rather than just counted, because the
    /// row is the only thing that knows where the file is. Deleting the row and
    /// leaving the file would leak disk space permanently and silently — the
    /// opposite of what somebody reclaiming space asked for.
    ///
    /// Only runs that have **finished** are eligible. A run with no `finished_at`
    /// is either in flight or was interrupted, and deleting it mid-flight would
    /// leave the daemon writing to a row that is gone.
    pub async fn delete_finished_before(&self, before: Timestamp) -> Result<(u64, Vec<String>)> {
        let cutoff = format_time(before);

        // Read the paths first: after the delete there is nothing left to ask.
        let transcripts = sqlx::query!(
            "SELECT transcript_path FROM run
             WHERE finished_at IS NOT NULL AND finished_at < ?",
            cutoff
        )
        .fetch_all(self.pool)
        .await?
        .into_iter()
        .filter_map(|row| row.transcript_path)
        .collect();

        let deleted = sqlx::query!(
            "DELETE FROM run WHERE finished_at IS NOT NULL AND finished_at < ?",
            cutoff
        )
        .execute(self.pool)
        .await?
        .rows_affected();

        Ok((deleted, transcripts))
    }

    /// Every run belonging to one change, in creation order.
    pub async fn list_for_change(&self, change_id: ChangeId) -> Result<Vec<Run>> {
        let raw = change_id.get();
        let rows = sqlx::query!(
            "SELECT id FROM run WHERE change_id = ? ORDER BY attempt",
            raw
        )
        .fetch_all(self.pool)
        .await?;

        let mut runs = Vec::with_capacity(rows.len());
        for row in rows {
            runs.push(self.get(RunId::new(row.id)).await?);
        }
        Ok(runs)
    }

    /// Reject a run whose state contradicts itself (SPEC §18).
    fn check_consistent(run: &Run) -> Result<()> {
        if run.is_consistent() {
            return Ok(());
        }
        Err(StoreError::Corrupt {
            column: "run.status",
            detail: format!(
                "status `{}` with skip_reason={:?} and error={:?}: a skipped run must say \
                 why and a failed run must carry its error (SPEC §18)",
                run.status,
                run.skip_reason.as_deref(),
                run.error.as_deref()
            ),
        })
    }
}

/// Insert findings and look them up by fingerprint (SPEC §5, §10.3).
#[derive(Debug, Clone)]
pub struct FindingStore<'a> {
    pool: &'a Pool,
}

impl<'a> FindingStore<'a> {
    /// Open the repository over `pool`.
    pub const fn new(pool: &'a Pool) -> Self {
        Self { pool }
    }

    /// Insert a finding, returning it with the id the database assigned.
    pub async fn insert(&self, finding: &Finding) -> Result<Finding> {
        let run_id = finding.run_id.get();
        let severity = finding.severity.as_str();
        let category = finding.category.as_str();
        let state = finding.state.as_str();
        let line_start = finding.line_start.map(i64::from);
        let line_end = finding.line_end.map(i64::from);
        let created = format_time(finding.created_at);

        let id = sqlx::query!(
            "INSERT INTO finding
               (run_id, fingerprint, severity, category, confidence, file, line_start,
                line_end, title, body, failure_scenario, suggested_fix, state, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             RETURNING id",
            run_id,
            finding.fingerprint,
            severity,
            category,
            finding.confidence,
            finding.file,
            line_start,
            line_end,
            finding.title,
            finding.body,
            finding.failure_scenario,
            finding.suggested_fix,
            state,
            created,
        )
        .fetch_one(self.pool)
        .await
        .map_err(|e| {
            StoreError::from_sqlx("finding", format!("fingerprint={}", finding.fingerprint), e)
        })?
        .id;

        Ok(Finding {
            id: FindingId::new(assigned_id("finding", "finding.id", id)?),
            ..finding.clone()
        })
    }

    /// Every finding carrying `fingerprint`, newest first.
    ///
    /// Crosses runs on purpose: that is what dedupe is. The same defect found
    /// again after a rebase has the same fingerprint and a different run
    /// (SPEC §10.3), and this is the query that notices.
    pub async fn by_fingerprint(&self, fingerprint: &str) -> Result<Vec<Finding>> {
        let rows = sqlx::query!(
            "SELECT id FROM finding WHERE fingerprint = ? ORDER BY id DESC",
            fingerprint
        )
        .fetch_all(self.pool)
        .await?;

        let mut findings = Vec::with_capacity(rows.len());
        for row in rows {
            findings.push(self.get(FindingId::new(row.id)).await?);
        }
        Ok(findings)
    }

    /// Fetch one finding by id.
    pub async fn get(&self, id: FindingId) -> Result<Finding> {
        let raw = id.get();
        let row = sqlx::query!(
            "SELECT id, run_id, fingerprint, severity, category, confidence, file,
                    line_start, line_end, title, body, failure_scenario, suggested_fix,
                    state, created_at
             FROM finding WHERE id = ?",
            raw
        )
        .fetch_optional(self.pool)
        .await?
        .ok_or_else(|| StoreError::NotFound {
            entity: "finding",
            key: format!("id={raw}"),
        })?;

        Ok(Finding {
            id: FindingId::new(row.id),
            run_id: RunId::new(row.run_id),
            fingerprint: row.fingerprint,
            severity: parse_enum::<Severity>("finding.severity", &row.severity)?,
            category: parse_enum::<Category>("finding.category", &row.category)?,
            confidence: row.confidence,
            file: row.file,
            line_start: row.line_start.map(|n| u32::try_from(n).unwrap_or_default()),
            line_end: row.line_end.map(|n| u32::try_from(n).unwrap_or_default()),
            title: row.title,
            body: row.body,
            failure_scenario: row.failure_scenario,
            suggested_fix: row.suggested_fix,
            state: parse_enum::<FindingState>("finding.state", &row.state)?,
            created_at: parse_time("finding.created_at", &row.created_at)?,
        })
    }

    /// Every finding produced by one run, in insertion order.
    pub async fn list_for_run(&self, run_id: RunId) -> Result<Vec<Finding>> {
        let raw = run_id.get();
        let rows = sqlx::query!("SELECT id FROM finding WHERE run_id = ? ORDER BY id", raw)
            .fetch_all(self.pool)
            .await?;

        let mut findings = Vec::with_capacity(rows.len());
        for row in rows {
            findings.push(self.get(FindingId::new(row.id)).await?);
        }
        Ok(findings)
    }

    /// Move a finding to a new state (SPEC §5).
    pub async fn set_state(&self, id: FindingId, state: FindingState) -> Result<()> {
        let raw = id.get();
        let state_str = state.as_str();
        let affected = sqlx::query!("UPDATE finding SET state = ? WHERE id = ?", state_str, raw)
            .execute(self.pool)
            .await?
            .rows_affected();

        if affected == 0 {
            return Err(StoreError::NotFound {
                entity: "finding",
                key: format!("id={raw}"),
            });
        }
        Ok(())
    }
}
