//! Repositories over `publish_action`, `audit` and `budget_ledger` (SPEC §5).

use crate::repos::{format_time, parse_enum, parse_time};
use crate::{Pool, Result, StoreError};
use revlocal_core::{
    AuditEntry, AuditId, BudgetLedgerEntry, Capability, FindingId, PublishAction, PublishActionId,
    PublishActionStatus, RepoId, RiskClass, RunId, Timestamp, Usage,
};

/// Insert and dispatch publish actions (SPEC §5, §11.6).
#[derive(Debug, Clone)]
pub struct PublishActionStore<'a> {
    pool: &'a Pool,
}

impl<'a> PublishActionStore<'a> {
    /// Open the repository over `pool`.
    pub const fn new(pool: &'a Pool) -> Self {
        Self { pool }
    }

    /// Insert an action.
    ///
    /// A duplicate `(target, idempotency_key)` comes back as
    /// [`StoreError::AlreadyExists`], which the publish queue treats as a
    /// **success**: SPEC §11.6 wants at-least-once delivery with exactly-once
    /// effect, so a redelivery landing on an action already recorded means the
    /// effect happened, not that anything failed.
    pub async fn insert(&self, action: &PublishAction) -> Result<PublishAction> {
        let run_id = action.run_id.get();
        let finding_id = action.finding_id.map(FindingId::get);
        let capability = action.capability.as_str();
        let risk = action.risk.as_str();
        let status = action.status.as_str();
        let attempts = i64::from(action.attempts);
        let created = format_time(action.created_at);
        let sent = action.sent_at.map(format_time);

        let id = sqlx::query!(
            "INSERT INTO publish_action
               (run_id, finding_id, target, capability, risk, idempotency_key,
                payload_json, status, attempts, response_json, external_ref, error,
                created_at, sent_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             RETURNING id",
            run_id,
            finding_id,
            action.target,
            capability,
            risk,
            action.idempotency_key,
            action.payload_json,
            status,
            attempts,
            action.response_json,
            action.external_ref,
            action.error,
            created,
            sent,
        )
        .fetch_one(self.pool)
        .await
        .map_err(|e| {
            StoreError::from_sqlx(
                "publish_action",
                format!(
                    "target={}, idempotency_key={}",
                    action.target, action.idempotency_key
                ),
                e,
            )
        })?
        .id;

        Ok(PublishAction {
            id: PublishActionId::new(id.ok_or(StoreError::Corrupt {
                column: "publish_action.id",
                detail: "inserting a publish_action returned no row id".to_owned(),
            })?),
            ..action.clone()
        })
    }

    /// Fetch one action by id.
    pub async fn get(&self, id: PublishActionId) -> Result<PublishAction> {
        let raw = id.get();
        let row = sqlx::query!(
            "SELECT id, run_id, finding_id, target, capability, risk, idempotency_key,
                    payload_json, status, attempts, response_json, external_ref, error,
                    created_at, sent_at
             FROM publish_action WHERE id = ?",
            raw
        )
        .fetch_optional(self.pool)
        .await?
        .ok_or_else(|| StoreError::NotFound {
            entity: "publish_action",
            key: format!("id={raw}"),
        })?;

        Ok(PublishAction {
            id: PublishActionId::new(row.id),
            run_id: RunId::new(row.run_id),
            finding_id: row.finding_id.map(FindingId::new),
            target: row.target,
            capability: parse_enum::<Capability>("publish_action.capability", &row.capability)?,
            risk: parse_enum::<RiskClass>("publish_action.risk", &row.risk)?,
            idempotency_key: row.idempotency_key,
            payload_json: row.payload_json,
            status: parse_enum::<PublishActionStatus>("publish_action.status", &row.status)?,
            attempts: u32::try_from(row.attempts).unwrap_or_default(),
            response_json: row.response_json,
            external_ref: row.external_ref,
            error: row.error,
            created_at: parse_time("publish_action.created_at", &row.created_at)?,
            sent_at: row
                .sent_at
                .map(|t| parse_time("publish_action.sent_at", &t))
                .transpose()?,
        })
    }

    /// Find the action already recorded for `(target, idempotency_key)`.
    ///
    /// The other half of idempotency: on a collision the caller needs the existing
    /// action's `external_ref` — the issue key or review id that was already
    /// created — rather than just the knowledge that one exists.
    pub async fn find_by_idempotency_key(
        &self,
        target: &str,
        idempotency_key: &str,
    ) -> Result<Option<PublishAction>> {
        let row = sqlx::query!(
            "SELECT id FROM publish_action WHERE target = ? AND idempotency_key = ?",
            target,
            idempotency_key
        )
        .fetch_optional(self.pool)
        .await?;

        match row {
            Some(row) => Ok(Some(self.get(PublishActionId::new(row.id)).await?)),
            None => Ok(None),
        }
    }

    /// Record the outcome of a delivery attempt.
    pub async fn record_outcome(
        &self,
        id: PublishActionId,
        status: PublishActionStatus,
        external_ref: Option<&str>,
        response_json: Option<&str>,
        error: Option<&str>,
        at: Timestamp,
    ) -> Result<()> {
        let raw = id.get();
        let status_str = status.as_str();
        let sent = (status == PublishActionStatus::Sent).then(|| format_time(at));

        let affected = sqlx::query!(
            "UPDATE publish_action
             SET status = ?, external_ref = ?, response_json = ?, error = ?,
                 attempts = attempts + 1, sent_at = COALESCE(?, sent_at)
             WHERE id = ?",
            status_str,
            external_ref,
            response_json,
            error,
            sent,
            raw
        )
        .execute(self.pool)
        .await?
        .rows_affected();

        if affected == 0 {
            return Err(StoreError::NotFound {
                entity: "publish_action",
                key: format!("id={raw}"),
            });
        }
        Ok(())
    }

    /// Record when this action may next be attempted (SPEC §11.6).
    ///
    /// Stored rather than held in memory so a restart does not make every pending
    /// action immediately due — which would defeat backoff at exactly the moment it
    /// matters, since a restart often follows the burst of failures that caused it.
    pub async fn schedule_retry(&self, id: PublishActionId, at: Timestamp) -> Result<()> {
        let raw = id.get();
        let when = format_time(at);
        let affected = sqlx::query!(
            "UPDATE publish_action SET next_attempt_at = ? WHERE id = ?",
            when,
            raw
        )
        .execute(self.pool)
        .await?
        .rows_affected();

        if affected == 0 {
            return Err(StoreError::NotFound {
                entity: "publish_action",
                key: format!("id={raw}"),
            });
        }
        Ok(())
    }

    /// When this action may next be attempted, if a retry is scheduled.
    pub async fn next_attempt_at(&self, id: PublishActionId) -> Result<Option<Timestamp>> {
        let raw = id.get();
        let row = sqlx::query!(
            "SELECT next_attempt_at FROM publish_action WHERE id = ?",
            raw
        )
        .fetch_optional(self.pool)
        .await?
        .ok_or_else(|| StoreError::NotFound {
            entity: "publish_action",
            key: format!("id={raw}"),
        })?;

        row.next_attempt_at
            .map(|t| parse_time("publish_action.next_attempt_at", &t))
            .transpose()
    }

    /// Every action that is due to be attempted, oldest first.
    ///
    /// "Due" is `pending` with no scheduled retry, or a scheduled retry that has
    /// come round. `awaiting_approval` is deliberately excluded: §12 makes that a
    /// human's decision, and a queue that delivered those would route around the
    /// approval gate.
    ///
    /// Oldest first so a backlog drains in the order it was created rather than
    /// letting new findings starve old ones.
    pub async fn list_pending(&self, now: Timestamp) -> Result<Vec<PublishAction>> {
        let cutoff = format_time(now);
        let rows = sqlx::query!(
            "SELECT id FROM publish_action
             WHERE status = 'pending'
               AND (next_attempt_at IS NULL OR next_attempt_at <= ?)
             ORDER BY id",
            cutoff
        )
        .fetch_all(self.pool)
        .await?;

        let mut actions = Vec::with_capacity(rows.len());
        for row in rows {
            actions.push(self.get(PublishActionId::new(row.id)).await?);
        }
        Ok(actions)
    }

    /// Every action belonging to one run, in creation order.    /// Every action belonging to one run, in creation order.
    pub async fn list_for_run(&self, run_id: RunId) -> Result<Vec<PublishAction>> {
        let raw = run_id.get();
        let rows = sqlx::query!(
            "SELECT id FROM publish_action WHERE run_id = ? ORDER BY id",
            raw
        )
        .fetch_all(self.pool)
        .await?;

        let mut actions = Vec::with_capacity(rows.len());
        for row in rows {
            actions.push(self.get(PublishActionId::new(row.id)).await?);
        }
        Ok(actions)
    }

    /// Whether a `(target, capability)` pair has ever been delivered successfully.
    ///
    /// Feeds the decision of record that first use of a pair is always high risk
    /// (SPEC §12.3). Only `sent` counts: an action that failed, was rejected, or
    /// was skipped in dry-run has **not** established that rev-local can safely
    /// write to that system, which is the whole point of the rule.
    pub async fn pair_has_succeeded(&self, target: &str, capability: Capability) -> Result<bool> {
        let capability_str = capability.as_str();
        let sent = PublishActionStatus::Sent.as_str();

        let count = sqlx::query_scalar!(
            "SELECT COUNT(*) FROM publish_action
             WHERE target = ? AND capability = ? AND status = ?",
            target,
            capability_str,
            sent
        )
        .fetch_one(self.pool)
        .await?;

        Ok(count > 0)
    }

    /// How many actions this repo has delivered since `since`.
    ///
    /// Feeds the burst-threshold escalation (SPEC §12.3). Takes an instant rather
    /// than reading the clock, so the store stays testable and the caller owns
    /// what "the last hour" means.
    pub async fn actions_sent_since(&self, repo_id: RepoId, since: Timestamp) -> Result<u32> {
        let raw = repo_id.get();
        let cutoff = format_time(since);
        let sent = PublishActionStatus::Sent.as_str();

        // publish_action reaches a repo through run -> change. The join is why this
        // lives here rather than being assembled by the caller.
        let count = sqlx::query_scalar!(
            "SELECT COUNT(*) FROM publish_action pa
             JOIN run r ON r.id = pa.run_id
             JOIN change c ON c.id = r.change_id
             WHERE c.repo_id = ? AND pa.status = ? AND pa.sent_at >= ?",
            raw,
            sent,
            cutoff
        )
        .fetch_one(self.pool)
        .await?;

        Ok(u32::try_from(count).unwrap_or(u32::MAX))
    }
}

/// Append to the audit log (SPEC §5, decision D7).
///
/// There is deliberately no update and no delete. The audit log is the record of
/// what rev-local did on a user's behalf; a log that can be rewritten is not one.
#[derive(Debug, Clone)]
pub struct AuditStore<'a> {
    pool: &'a Pool,
}

impl<'a> AuditStore<'a> {
    /// Open the repository over `pool`.
    pub const fn new(pool: &'a Pool) -> Self {
        Self { pool }
    }

    /// Append an entry.
    pub async fn append(&self, entry: &AuditEntry) -> Result<AuditEntry> {
        let at = format_time(entry.at);
        let repo_id = entry.repo_id.map(RepoId::get);
        let run_id = entry.run_id.map(RunId::get);

        let id = sqlx::query!(
            "INSERT INTO audit (at, actor, kind, repo_id, run_id, detail_json)
             VALUES (?, ?, ?, ?, ?, ?)
             RETURNING id",
            at,
            entry.actor,
            entry.kind,
            repo_id,
            run_id,
            entry.detail_json,
        )
        .fetch_one(self.pool)
        .await?
        .id;

        Ok(AuditEntry {
            id: AuditId::new(id),
            ..entry.clone()
        })
    }

    /// Every entry for one run, oldest first.
    pub async fn list_for_run(&self, run_id: RunId) -> Result<Vec<AuditEntry>> {
        let raw = run_id.get();
        let rows = sqlx::query!(
            "SELECT id, at, actor, kind, repo_id, run_id, detail_json
             FROM audit WHERE run_id = ? ORDER BY id",
            raw
        )
        .fetch_all(self.pool)
        .await?;

        rows.into_iter()
            .map(|row| {
                Ok(AuditEntry {
                    id: AuditId::new(row.id),
                    at: parse_time("audit.at", &row.at)?,
                    actor: row.actor,
                    kind: row.kind,
                    repo_id: row.repo_id.map(RepoId::new),
                    run_id: row.run_id.map(RunId::new),
                    detail_json: row.detail_json,
                })
            })
            .collect()
    }

    /// The most recent `limit` entries, newest first.
    pub async fn recent(&self, limit: u32) -> Result<Vec<AuditEntry>> {
        let limit = i64::from(limit);
        let rows = sqlx::query!(
            "SELECT id, at, actor, kind, repo_id, run_id, detail_json
             FROM audit ORDER BY at DESC, id DESC LIMIT ?",
            limit
        )
        .fetch_all(self.pool)
        .await?;

        rows.into_iter()
            .map(|row| {
                Ok(AuditEntry {
                    id: AuditId::new(row.id),
                    at: parse_time("audit.at", &row.at)?,
                    actor: row.actor,
                    kind: row.kind,
                    repo_id: row.repo_id.map(RepoId::new),
                    run_id: row.run_id.map(RunId::new),
                    detail_json: row.detail_json,
                })
            })
            .collect()
    }
}

/// Accumulate daily spend per repo (SPEC §5, decision D10).
#[derive(Debug, Clone)]
pub struct BudgetLedgerStore<'a> {
    pool: &'a Pool,
}

impl<'a> BudgetLedgerStore<'a> {
    /// Open the repository over `pool`.
    pub const fn new(pool: &'a Pool) -> Self {
        Self { pool }
    }

    /// Add one run's spend to `(repo_id, day)`, atomically.
    ///
    /// A single `INSERT ... ON CONFLICT DO UPDATE` that adds to the stored values
    /// rather than reading and writing them back. Under SPEC §4.3's concurrency
    /// two runs finish at once routinely, and read-then-write would lose one of
    /// their increments — a budget that under-counts is a budget that does not
    /// hold.
    ///
    /// An unreported cost does not add zero and move on: it clears
    /// `cost_complete`, so the day is marked as not fully measured (ADR 0010).
    pub async fn add_run(
        &self,
        repo_id: RepoId,
        day: &str,
        runs: u32,
        usage: &Usage,
    ) -> Result<()> {
        let raw = repo_id.get();
        let runs = i64::from(runs);
        let tokens_in = i64::try_from(usage.tokens_in).unwrap_or(i64::MAX);
        let tokens_out = i64::try_from(usage.tokens_out).unwrap_or(i64::MAX);
        let cost = usage.cost_usd.unwrap_or(0.0);
        let complete = i64::from(usage.cost_is_complete());

        sqlx::query!(
            "INSERT INTO budget_ledger
               (repo_id, day, runs, tokens_in, tokens_out, cost_usd, cost_complete)
             VALUES (?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (repo_id, day) DO UPDATE SET
               runs = budget_ledger.runs + excluded.runs,
               tokens_in = budget_ledger.tokens_in + excluded.tokens_in,
               tokens_out = budget_ledger.tokens_out + excluded.tokens_out,
               cost_usd = budget_ledger.cost_usd + excluded.cost_usd,
               cost_complete = MIN(budget_ledger.cost_complete, excluded.cost_complete)",
            raw,
            day,
            runs,
            tokens_in,
            tokens_out,
            cost,
            complete,
        )
        .execute(self.pool)
        .await
        .map_err(|e| {
            StoreError::from_sqlx("budget_ledger", format!("repo_id={raw}, day={day}"), e)
        })?;

        Ok(())
    }

    /// One day's spend, if anything was spent.
    pub async fn get(&self, repo_id: RepoId, day: &str) -> Result<Option<BudgetLedgerEntry>> {
        let raw = repo_id.get();
        let row = sqlx::query!(
            "SELECT repo_id, day, runs, tokens_in, tokens_out, cost_usd, cost_complete
             FROM budget_ledger WHERE repo_id = ? AND day = ?",
            raw,
            day
        )
        .fetch_optional(self.pool)
        .await?;

        Ok(row.map(|row| {
            let known_cost_usd = row.cost_usd;
            let complete = row.cost_complete != 0;
            BudgetLedgerEntry {
                repo_id: RepoId::new(row.repo_id),
                day: row.day,
                runs: u32::try_from(row.runs).unwrap_or(u32::MAX),
                usage: Usage {
                    tokens_in: u64::try_from(row.tokens_in).unwrap_or_default(),
                    tokens_out: u64::try_from(row.tokens_out).unwrap_or_default(),
                    // Some only when the day is fully measured, so a cost budget
                    // cannot read an unmeasured day as a cheap one.
                    cost_usd: complete.then_some(known_cost_usd),
                },
                known_cost_usd,
            }
        }))
    }
}
