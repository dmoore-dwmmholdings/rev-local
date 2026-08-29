//! The commands that change something a human was asked about (RL-1201, §12.4, §14).
//!
//! `approvals approve` · `approvals reject` · `findings suppress` · `budget reset`
//! · `publish retry`. They are together because they share one property that the
//! read-only commands do not have: each is somebody deciding, and §12.4 requires a
//! decision to be distinguishable afterwards from a timeout, a retry, or nothing
//! having happened at all.
//!
//! # Every one reports what it changed, not that it ran
//!
//! `approve --all` on an empty inbox and `approve --all` on three actions both
//! "succeed". Reporting only success makes those identical, and the second is the
//! one somebody needs to see. So each command returns counts and ids, and says so
//! plainly when the answer is zero.
//!
//! # Approval carries a digest, and that is the whole point
//!
//! §12.4's rule is that an edit after approval is impossible. The queue re-computes
//! the payload digest at dispatch and refuses an action whose payload moved since a
//! human looked at it, so approving means recording *what* was approved rather than
//! *that* it was. Approving without the digest would leave that as an intention.

use revlocal_core::{
    payload_digest, PublishActionId, RepoId, Suppression, SuppressionId, Timestamp,
};
use revlocal_store::{BudgetLedgerStore, Pool, PublishActionStore, RepoStore, SuppressionStore};
use serde::{Deserialize, Serialize};

/// Why a decision could not be recorded.
#[derive(Debug, thiserror::Error)]
pub enum DecideError {
    /// The database could not be read or written.
    #[error("could not reach the local database: {source}\n  try: revlocal db migrate")]
    Store {
        /// Why.
        #[source]
        source: Box<revlocal_store::StoreError>,
    },

    /// The action is not waiting for a human.
    ///
    /// Separate from a missing row, because the remedies differ: one is a typo,
    /// the other is somebody having already decided.
    #[error(
        "action #{id} is not waiting for approval\n  \
         try: revlocal approvals list — it may already have been approved, rejected or sent"
    )]
    NotWaiting {
        /// Which action.
        id: i64,
    },

    /// No such repository.
    #[error("no repository named {name:?}\n  try: revlocal repo list")]
    NoSuchRepo {
        /// What was asked for.
        name: String,
    },

    /// Nothing in that state to retry.
    #[error(
        "publish action #{id} is not in a failed state\n  \
         try: revlocal publish status --run <ID> — only a failed action can be retried"
    )]
    NotFailed {
        /// Which action.
        id: i64,
    },

    /// The report could not be serialised.
    #[error("could not render the report: {source}")]
    Unrenderable {
        /// Why.
        #[source]
        source: serde_json::Error,
    },
}

fn boxed(source: revlocal_store::StoreError) -> DecideError {
    DecideError::Store {
        source: Box::new(source),
    }
}

/// What an approve or reject did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionReport {
    /// `approve` or `reject`.
    pub action: String,
    /// The ids that were decided, in the order they were decided.
    pub decided: Vec<i64>,
    /// Suppressions created alongside a rejection, by id.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub suppressed: Vec<i64>,
    /// A sentence for a person.
    pub detail: String,
}

impl DecisionReport {
    /// The line the human path prints.
    pub fn render_human(&self) -> String {
        let mut out = self.detail.clone();
        out.push('\n');
        for id in &self.decided {
            out.push_str(&format!("  #{id}\n"));
        }
        out
    }
}

/// Which actions an approve applies to (§14's `<id|--run R|--all>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Exactly one action.
    One(i64),
    /// Every waiting action for one run.
    Run(i64),
    /// Every waiting action.
    All,
}

/// `revlocal approvals approve <id|--run R|--all>` (SPEC §12.4).
pub async fn approve(pool: &Pool, scope: Scope) -> Result<DecisionReport, DecideError> {
    let store = PublishActionStore::new(pool);
    let waiting = store.list_awaiting_approval().await.map_err(boxed)?;

    let chosen: Vec<_> = waiting
        .iter()
        .filter(|action| match scope {
            Scope::One(id) => action.id.get() == id,
            Scope::Run(run) => action.run_id.get() == run,
            Scope::All => true,
        })
        .collect();

    // A named id that is not waiting is an error, because the caller believed
    // something specific about it. An empty `--all` is not: "nothing was waiting"
    // is a true and useful answer to "approve everything".
    if let Scope::One(id) = scope {
        if chosen.is_empty() {
            return Err(DecideError::NotWaiting { id });
        }
    }

    let mut decided = Vec::new();
    for action in &chosen {
        // The digest is computed here, from the payload as it stands now. That is
        // the point: it pins what was approved, so a later edit cannot ride along
        // on this decision.
        let digest = payload_digest(&action.payload_json);
        store.approve(action.id, &digest).await.map_err(boxed)?;
        decided.push(action.id.get());
    }

    let detail = match decided.len() {
        0 => "Nothing was waiting for approval.".to_owned(),
        1 => "Approved 1 action.".to_owned(),
        n => format!("Approved {n} actions."),
    };
    Ok(DecisionReport {
        action: "approve".to_owned(),
        decided,
        suppressed: Vec::new(),
        detail,
    })
}

/// `revlocal approvals reject <id> [--suppress]` (SPEC §12.4).
///
/// `--suppress` also records the finding's fingerprint so the same thing is not
/// proposed again. Rejecting without it means saying no to this action; rejecting
/// with it means saying no to the finding — and being asked the same question every
/// run is how people stop reading the questions.
pub async fn reject(
    pool: &Pool,
    id: i64,
    suppress: bool,
    at: Timestamp,
) -> Result<DecisionReport, DecideError> {
    let store = PublishActionStore::new(pool);
    let waiting = store.list_awaiting_approval().await.map_err(boxed)?;
    let action = waiting
        .iter()
        .find(|a| a.id.get() == id)
        .ok_or(DecideError::NotWaiting { id })?;

    // §12.4 names `expired` for a timeout and requires it to stay distinguishable
    // from a person saying no: one is a decision, the other is that nobody looked.
    store
        .reject(PublishActionId::new(id), "rejected by operator")
        .await
        .map_err(boxed)?;

    let mut suppressed = Vec::new();
    if suppress {
        // Only a finding has a fingerprint. An action with none — a run-level
        // summary, say — is rejected without a suppression rather than with an
        // inert one, and the report says which happened.
        if let Some(fingerprint) = fingerprint_of(pool, action.finding_id).await? {
            let row = SuppressionStore::new(pool)
                .insert(&Suppression {
                    id: SuppressionId::new(0),
                    repo_id: None,
                    fingerprint: Some(fingerprint),
                    glob: None,
                    reason: Some("rejected with --suppress".to_owned()),
                    created_at: at,
                })
                .await
                .map_err(boxed)?;
            suppressed.push(row.id.get());
        }
    }

    let detail = if suppress && suppressed.is_empty() {
        format!("Rejected action #{id}. Nothing to suppress — it carries no finding.")
    } else if suppress {
        format!("Rejected action #{id} and suppressed its finding.")
    } else {
        format!("Rejected action #{id}.")
    };
    Ok(DecisionReport {
        action: "reject".to_owned(),
        decided: vec![id],
        suppressed,
        detail,
    })
}

/// The fingerprint of the finding an action carries, if it carries one.
async fn fingerprint_of(
    pool: &Pool,
    finding_id: Option<revlocal_core::FindingId>,
) -> Result<Option<String>, DecideError> {
    let Some(finding_id) = finding_id else {
        return Ok(None);
    };
    let finding = revlocal_store::FindingStore::new(pool)
        .get(finding_id)
        .await
        .map_err(boxed)?;
    Ok(Some(finding.fingerprint))
}

/// What a suppression created.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuppressReport {
    /// The new suppression's id.
    pub id: i64,
    /// What it suppresses.
    pub fingerprint: String,
    /// The repository it is scoped to, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// A sentence for a person.
    pub detail: String,
}

impl SuppressReport {
    /// The line the human path prints.
    pub fn render_human(&self) -> String {
        format!("{}\n", self.detail)
    }
}

/// `revlocal findings suppress <fingerprint>` (SPEC §14).
///
/// Scoped to one repository when `repo` is given and globally otherwise. Global is
/// not the safer default — it is the wider one — so it has to be what was asked
/// for rather than what was left out, and the report always says which it did.
pub async fn suppress(
    pool: &Pool,
    fingerprint: &str,
    repo: Option<&str>,
    at: Timestamp,
) -> Result<SuppressReport, DecideError> {
    let repo_id = match repo {
        None => None,
        Some(name) => Some(named_repo(pool, name).await?),
    };

    let row = SuppressionStore::new(pool)
        .insert(&Suppression {
            id: SuppressionId::new(0),
            repo_id,
            fingerprint: Some(fingerprint.to_owned()),
            glob: None,
            reason: Some("suppressed from the command line".to_owned()),
            created_at: at,
        })
        .await
        .map_err(boxed)?;

    let detail = match repo {
        Some(name) => format!("Suppressed {fingerprint} in {name}."),
        None => format!("Suppressed {fingerprint} everywhere."),
    };
    Ok(SuppressReport {
        id: row.id.get(),
        fingerprint: fingerprint.to_owned(),
        repo: repo.map(str::to_owned),
        detail,
    })
}

/// What a budget reset cleared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResetReport {
    /// Which repository.
    pub repo: String,
    /// The day that was cleared, `YYYY-MM-DD`.
    pub day: String,
    /// Whether there was anything to clear.
    pub cleared: bool,
    /// A sentence for a person.
    pub detail: String,
}

impl ResetReport {
    /// The line the human path prints.
    pub fn render_human(&self) -> String {
        format!("{}\n", self.detail)
    }
}

/// `revlocal budget reset --repo N` (SPEC §13.1, §14).
///
/// Clears today's allowance accounting so work can resume before midnight. It does
/// not erase that the work happened: runs, findings and the audit log are
/// untouched, so the spend is still explainable afterwards.
pub async fn reset_budget(
    pool: &Pool,
    repo: &str,
    at: Timestamp,
) -> Result<ResetReport, DecideError> {
    let repo_id = named_repo(pool, repo).await?;
    // The ledger's key is a local calendar day, because a budget is a human-facing
    // daily allowance and rolls over on the user's midnight rather than UTC's.
    let day = at
        .with_timezone(&chrono::Local)
        .format("%Y-%m-%d")
        .to_string();

    let cleared = BudgetLedgerStore::new(pool)
        .reset(repo_id, &day)
        .await
        .map_err(boxed)?;

    let detail = if cleared {
        format!("Cleared {repo}'s spend for {day}. Runs and findings are untouched.")
    } else {
        // Said rather than silently succeeding: an operator resetting a budget
        // that was never spent should learn that, not wonder whether it worked.
        format!("{repo} had spent nothing on {day}; nothing to clear.")
    };
    Ok(ResetReport {
        repo: repo.to_owned(),
        day,
        cleared,
        detail,
    })
}

/// What a retry re-queued.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryReport {
    /// The action that was re-queued.
    pub action_id: i64,
    /// A sentence for a person.
    pub detail: String,
}

impl RetryReport {
    /// The line the human path prints.
    pub fn render_human(&self) -> String {
        format!("{}\n", self.detail)
    }
}

/// `revlocal publish retry <action_id>` (SPEC §14).
///
/// One action, not one target — which is the whole difference from `publish
/// replay --run R --target T`. When a run produced eight comments and one was
/// rejected for a bad path, replaying the target re-posts the seven that landed.
pub async fn retry_action(pool: &Pool, id: i64) -> Result<RetryReport, DecideError> {
    let affected = PublishActionStore::new(pool)
        .reset_one_for_retry(PublishActionId::new(id))
        .await
        .map_err(boxed)?;

    if affected == 0 {
        return Err(DecideError::NotFailed { id });
    }
    Ok(RetryReport {
        action_id: id,
        detail: format!("Action #{id} is queued for another attempt."),
    })
}

/// Resolve a repository by name.
async fn named_repo(pool: &Pool, name: &str) -> Result<RepoId, DecideError> {
    RepoStore::new(pool)
        .list()
        .await
        .map_err(boxed)?
        .into_iter()
        .find(|repo| repo.name == name)
        .map(|repo| repo.id)
        .ok_or_else(|| DecideError::NoSuchRepo {
            name: name.to_owned(),
        })
}

/// Render a report for a person or a script.
pub fn render<T: Serialize>(report: &T, human: String, json: bool) -> Result<String, DecideError> {
    if json {
        serde_json::to_string_pretty(report).map_err(|source| DecideError::Unrenderable { source })
    } else {
        Ok(human)
    }
}
