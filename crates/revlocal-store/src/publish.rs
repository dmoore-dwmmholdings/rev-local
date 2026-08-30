//! Repositories over `publish_action`, `audit` and `budget_ledger` (SPEC §5).

use crate::repos::{format_time, parse_enum, parse_time};
use crate::{Pool, Result, StoreError};
use revlocal_core::{
    AuditEntry, AuditId, BudgetLedgerEntry, Capability, FindingId, PublishAction, PublishActionId,
    PublishActionStatus, RepoId, RiskClass, RunId, Suppression, SuppressionId, Timestamp, Usage,
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

    /// Every action waiting on a human, oldest first (§12.4).
    pub async fn list_awaiting_approval(&self) -> Result<Vec<PublishAction>> {
        let status = PublishActionStatus::AwaitingApproval.as_str();
        let rows = sqlx::query!(
            "SELECT id FROM publish_action WHERE status = ? ORDER BY id",
            status
        )
        .fetch_all(self.pool)
        .await?;

        let mut actions = Vec::with_capacity(rows.len());
        for row in rows {
            actions.push(self.get(PublishActionId::new(row.id)).await?);
        }
        Ok(actions)
    }

    /// Record an approval, together with a digest of the payload that was shown.
    ///
    /// The digest is what makes "an edit after approval is impossible" checkable:
    /// the queue re-computes it at dispatch and refuses an action whose payload has
    /// moved since a human looked at it. Storing the approval without it would
    /// leave the criterion as an intention.
    ///
    /// Only an `awaiting_approval` row can be approved — approving something
    /// already sent, failed or rejected is a caller bug, and the row count says so
    /// rather than silently rewriting a settled action.
    pub async fn approve(&self, id: PublishActionId, digest: &str) -> Result<()> {
        let raw = id.get();
        let approved = PublishActionStatus::Approved.as_str();
        let awaiting = PublishActionStatus::AwaitingApproval.as_str();

        let affected = sqlx::query!(
            "UPDATE publish_action
             SET status = ?, approved_payload_digest = ?
             WHERE id = ? AND status = ?",
            approved,
            digest,
            raw,
            awaiting
        )
        .execute(self.pool)
        .await?
        .rows_affected();

        if affected == 0 {
            return Err(StoreError::NotFound {
                entity: "publish_action awaiting approval",
                key: format!("id={raw}"),
            });
        }
        Ok(())
    }

    /// Replace an action's payload before it is approved (SPEC §12.4).
    ///
    /// §12.4 lists "edit body then approve" among the five actions, and the edit
    /// has to happen *before* the approval rather than alongside it. The digest
    /// recorded at approval is computed over the payload as it then stands, and
    /// the queue re-computes it at dispatch — so editing after approving would
    /// invalidate the approval, which is exactly the protection working.
    ///
    /// Only an `awaiting_approval` row can be edited. Editing something already
    /// approved would slip past the digest that was recorded for it; editing
    /// something sent would rewrite history. The row count says so rather than
    /// silently changing a settled action.
    pub async fn edit_payload(&self, id: PublishActionId, payload_json: &str) -> Result<()> {
        let raw = id.get();
        let awaiting = PublishActionStatus::AwaitingApproval.as_str();

        let affected = sqlx::query!(
            "UPDATE publish_action SET payload_json = ? WHERE id = ? AND status = ?",
            payload_json,
            raw,
            awaiting
        )
        .execute(self.pool)
        .await?
        .rows_affected();

        if affected == 0 {
            return Err(StoreError::NotFound {
                entity: "publish_action awaiting approval",
                key: format!("id={raw}"),
            });
        }
        Ok(())
    }

    /// Record a rejection and why.
    ///
    /// `reason` carries `expired` for a timeout, which §12.4 names explicitly and
    /// which must stay distinguishable from a person saying no: one is a decision,
    /// the other is that nobody looked.
    pub async fn reject(&self, id: PublishActionId, reason: &str) -> Result<()> {
        let raw = id.get();
        let rejected = PublishActionStatus::Rejected.as_str();
        let awaiting = PublishActionStatus::AwaitingApproval.as_str();

        let affected = sqlx::query!(
            "UPDATE publish_action
             SET status = ?, decision_reason = ?
             WHERE id = ? AND status = ?",
            rejected,
            reason,
            raw,
            awaiting
        )
        .execute(self.pool)
        .await?
        .rows_affected();

        if affected == 0 {
            return Err(StoreError::NotFound {
                entity: "publish_action awaiting approval",
                key: format!("id={raw}"),
            });
        }
        Ok(())
    }

    /// The digest recorded when this action was approved, if it was.
    pub async fn approved_digest(&self, id: PublishActionId) -> Result<Option<String>> {
        let raw = id.get();
        let row = sqlx::query!(
            "SELECT approved_payload_digest FROM publish_action WHERE id = ?",
            raw
        )
        .fetch_optional(self.pool)
        .await?
        .ok_or_else(|| StoreError::NotFound {
            entity: "publish_action",
            key: format!("id={raw}"),
        })?;
        Ok(row.approved_payload_digest)
    }

    /// Why an action was rejected, if it was.
    pub async fn decision_reason(&self, id: PublishActionId) -> Result<Option<String>> {
        let raw = id.get();
        let row = sqlx::query!(
            "SELECT decision_reason FROM publish_action WHERE id = ?",
            raw
        )
        .fetch_optional(self.pool)
        .await?
        .ok_or_else(|| StoreError::NotFound {
            entity: "publish_action",
            key: format!("id={raw}"),
        })?;
        Ok(row.decision_reason)
    }

    /// Put one target's failed actions for one run back in the queue.    /// Put one target's failed actions for one run back in the queue.
    ///
    /// `attempts` is reset to zero, and this is deliberate. A replay is a person
    /// saying "try again", and leaving the count at five would mean the retry
    /// policy refuses on the first pass — the request would be honoured in form
    /// and denied in substance. `attempts` therefore means attempts in the
    /// current delivery cycle, not attempts ever; the audit log is where the
    /// history lives.
    ///
    /// Only `failed` rows are touched. A pending action is already going to be
    /// tried, and resetting it would discard a backoff that is doing its job.
    pub async fn reset_for_retry(&self, run_id: RunId, target: &str) -> Result<u64> {
        let raw = run_id.get();
        let pending = PublishActionStatus::Pending.as_str();
        let failed = PublishActionStatus::Failed.as_str();

        let affected = sqlx::query!(
            "UPDATE publish_action
             SET status = ?, attempts = 0, next_attempt_at = NULL, error = NULL
             WHERE run_id = ? AND target = ? AND status = ?",
            pending,
            raw,
            target,
            failed
        )
        .execute(self.pool)
        .await?
        .rows_affected();

        Ok(affected)
    }

    /// Put one failed action back in the queue.
    ///
    /// §14 names this and `replay` separately, and the difference is the unit:
    /// `replay --run R --target T` re-queues every failed action for a target,
    /// while this re-queues exactly one. When a run produced eight comments and
    /// one was rejected for a bad path, replaying the target re-posts the seven
    /// that already landed.
    ///
    /// Only a `failed` action is retryable. Returning the count rather than `()`
    /// lets the caller tell "retried it" from "there was nothing in that state",
    /// which is the difference between a working command and one that quietly did
    /// nothing.
    pub async fn reset_one_for_retry(&self, id: PublishActionId) -> Result<u64> {
        let raw = id.get();
        let pending = PublishActionStatus::Pending.as_str();
        let failed = PublishActionStatus::Failed.as_str();

        let affected = sqlx::query!(
            "UPDATE publish_action
             SET status = ?, attempts = 0, next_attempt_at = NULL, error = NULL
             WHERE id = ? AND status = ?",
            pending,
            raw,
            failed
        )
        .execute(self.pool)
        .await?
        .rows_affected();

        Ok(affected)
    }

    /// Whether a `(target, capability)` pair has ever been delivered successfully.    /// Whether a `(target, capability)` pair has ever been delivered successfully.
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

/// Operator state that is not configuration (SPEC §12.1).
///
/// The kill switch lives here rather than in `config.toml` for ADR 0015's reason:
/// config is what the user wrote, this is what rev-local was told to do at
/// runtime, and a report has to be able to say which it is looking at.
#[derive(Debug, Clone)]
pub struct SettingStore<'a> {
    pool: &'a Pool,
}

/// The key the kill switch is stored under.
pub const SETTING_PAUSED: &str = "paused";

impl<'a> SettingStore<'a> {
    /// Open the repository over `pool`.
    pub const fn new(pool: &'a Pool) -> Self {
        Self { pool }
    }

    /// Read a setting.
    pub async fn get(&self, key: &str) -> Result<Option<String>> {
        let row = sqlx::query!("SELECT value FROM setting WHERE key = ?", key)
            .fetch_optional(self.pool)
            .await?;
        Ok(row.map(|r| r.value))
    }

    /// Write a setting.
    pub async fn set(&self, key: &str, value: &str, at: Timestamp) -> Result<()> {
        let when = format_time(at);
        sqlx::query!(
            "INSERT INTO setting (key, value, updated_at) VALUES (?, ?, ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value,
                                            updated_at = excluded.updated_at",
            key,
            value,
            when
        )
        .execute(self.pool)
        .await?;
        Ok(())
    }

    /// Whether the kill switch is engaged.
    ///
    /// Absent means running. A fresh install is not paused, and defaulting the
    /// other way would make a first start look like somebody had stopped it.
    pub async fn is_paused(&self) -> Result<bool> {
        Ok(self.get(SETTING_PAUSED).await?.as_deref() == Some("true"))
    }

    /// Engage or release the kill switch.
    pub async fn set_paused(&self, paused: bool, at: Timestamp) -> Result<()> {
        self.set(SETTING_PAUSED, if paused { "true" } else { "false" }, at)
            .await
    }
}

/// The `suppression` table (SPEC §5, §12.4's reject-and-suppress).
#[derive(Debug, Clone)]
pub struct SuppressionStore<'a> {
    pool: &'a Pool,
}

impl<'a> SuppressionStore<'a> {
    /// Open the repository over `pool`.
    pub const fn new(pool: &'a Pool) -> Self {
        Self { pool }
    }

    /// Record a suppression.
    pub async fn insert(&self, suppression: &Suppression) -> Result<Suppression> {
        let repo_id = suppression.repo_id.map(RepoId::get);
        let created = format_time(suppression.created_at);

        let id = sqlx::query!(
            "INSERT INTO suppression (repo_id, fingerprint, glob, reason, created_at)
             VALUES (?, ?, ?, ?, ?)
             RETURNING id",
            repo_id,
            suppression.fingerprint,
            suppression.glob,
            suppression.reason,
            created
        )
        .fetch_one(self.pool)
        .await?
        .id;

        Ok(Suppression {
            id: SuppressionId::new(id),
            ..suppression.clone()
        })
    }

    /// Every suppression that applies to `repo_id`, including global ones.
    ///
    /// Global suppressions (`repo_id IS NULL`) are included deliberately: a user
    /// who said "never tell me this again" about a finding did not mean "in this
    /// repository only" unless they scoped it.
    pub async fn list_for_repo(&self, repo_id: RepoId) -> Result<Vec<Suppression>> {
        let raw = repo_id.get();
        let rows = sqlx::query!(
            "SELECT id, repo_id, fingerprint, glob, reason, created_at
             FROM suppression
             WHERE repo_id IS NULL OR repo_id = ?
             ORDER BY id",
            raw
        )
        .fetch_all(self.pool)
        .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(Suppression {
                id: SuppressionId::new(row.id),
                repo_id: row.repo_id.map(RepoId::new),
                fingerprint: row.fingerprint,
                glob: row.glob,
                reason: row.reason,
                created_at: parse_time("suppression.created_at", &row.created_at)?,
            });
        }
        Ok(out)
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
    /// Since RL-409 an unmeasured *token* count does the same to `tokens_complete`
    /// — the day's total becomes a lower bound rather than a total, and one
    /// unmeasured run is enough to say so.
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
        let tokens_complete = i64::from(usage.tokens_are_known());

        sqlx::query!(
            "INSERT INTO budget_ledger
               (repo_id, day, runs, tokens_in, tokens_out, cost_usd, cost_complete,
                tokens_complete)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT (repo_id, day) DO UPDATE SET
               runs = budget_ledger.runs + excluded.runs,
               tokens_in = budget_ledger.tokens_in + excluded.tokens_in,
               tokens_out = budget_ledger.tokens_out + excluded.tokens_out,
               cost_usd = budget_ledger.cost_usd + excluded.cost_usd,
               cost_complete = MIN(budget_ledger.cost_complete, excluded.cost_complete),
               -- MIN, so one unmeasured run makes the whole day incomplete and no
               -- later measured run can quietly restore the claim.
               tokens_complete = MIN(budget_ledger.tokens_complete, excluded.tokens_complete)",
            raw,
            day,
            runs,
            tokens_in,
            tokens_out,
            cost,
            complete,
            tokens_complete,
        )
        .execute(self.pool)
        .await
        .map_err(|e| {
            StoreError::from_sqlx("budget_ledger", format!("repo_id={raw}, day={day}"), e)
        })?;

        Ok(())
    }

    /// Clear one repo's spend for one day (§14's `budget reset --repo N`).
    ///
    /// Returns whether a row was there. An operator resetting a budget that was
    /// never spent should be told so, not left wondering whether it worked.
    ///
    /// This deletes the *allowance* accounting, not the record that the work
    /// happened: runs, findings and the audit log are untouched, so the spend is
    /// still explainable afterwards. A reset that erased the history would make a
    /// budget question unanswerable the moment somebody used the escape hatch.
    pub async fn reset(&self, repo_id: RepoId, day: &str) -> Result<bool> {
        let raw = repo_id.get();
        let affected = sqlx::query!(
            "DELETE FROM budget_ledger WHERE repo_id = ? AND day = ?",
            raw,
            day
        )
        .execute(self.pool)
        .await?
        .rows_affected();
        Ok(affected > 0)
    }

    /// One day's spend, if anything was spent.
    pub async fn get(&self, repo_id: RepoId, day: &str) -> Result<Option<BudgetLedgerEntry>> {
        let raw = repo_id.get();
        let row = sqlx::query!(
            "SELECT repo_id, day, runs, tokens_in, tokens_out, cost_usd, cost_complete,
                    tokens_complete
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
                    // Same rule as cost: a day containing one unmeasured run is a
                    // day whose token total is a lower bound, not a total.
                    tokens_known: row.tokens_complete != 0,
                    cost_usd: complete.then_some(known_cost_usd),
                },
                known_cost_usd,
            }
        }))
    }
}
