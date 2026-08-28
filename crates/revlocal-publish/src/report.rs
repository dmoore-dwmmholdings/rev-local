//! Per-target publish status for one run (RL-710, SPEC §11.6).
//!
//! §11.6's last line: "Partial failure is normal and reported: a run can be
//! `done` with GitHub posted, Andare failed."
//!
//! That sentence has a consequence people miss. If a failed target held the run
//! open, then one unreachable system would leave every run of the day stuck in
//! `publishing`, and the review that GitHub *did* receive would look unfinished.
//! So a failure does not block completion — [`RunPublishReport::blocks_completion`]
//! is about work still outstanding, not about work that went badly. What a failure
//! does is stay visible, and stay replayable.

use std::collections::BTreeMap;

use revlocal_core::{PublishAction, PublishActionStatus, RunId};
use revlocal_store::{Pool, PublishActionStore, StoreError};

/// Where one target stands for one run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetState {
    /// Everything asked of this target was delivered.
    Delivered,
    /// Some delivered, some did not.
    Partial,
    /// Nothing delivered, and nothing left to try.
    Failed,
    /// Work is still outstanding.
    Pending,
    /// Waiting on a person (§12).
    AwaitingApproval,
    /// Dry run: nothing was sent, and nothing was meant to be.
    SkippedDryRun,
}

impl TargetState {
    /// How this reads in the UI and in `revlocal runs show`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Delivered => "delivered",
            Self::Partial => "partial",
            Self::Failed => "failed",
            Self::Pending => "pending",
            Self::AwaitingApproval => "awaiting approval",
            Self::SkippedDryRun => "skipped (dry run)",
        }
    }
}

/// One target's outcome for one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetOutcome {
    /// Which target.
    pub target: String,
    /// Actions delivered.
    pub sent: usize,
    /// Actions still to be attempted.
    pub pending: usize,
    /// Actions waiting on a person.
    pub awaiting_approval: usize,
    /// Actions that will not be attempted again.
    pub failed: usize,
    /// Actions a dry run did not send.
    pub skipped: usize,
    /// Actions a person declined.
    pub rejected: usize,
    /// The most recent failure, where there was one.
    pub last_error: Option<String>,
    /// What was created — issue keys, review ids, page URLs.
    pub external_refs: Vec<String>,
}

impl TargetOutcome {
    /// How many actions this target was asked for.
    pub const fn total(&self) -> usize {
        self.sent
            + self.pending
            + self.awaiting_approval
            + self.failed
            + self.skipped
            + self.rejected
    }

    /// Where this target stands.
    ///
    /// Outstanding work outranks failure: a target with one action failed and one
    /// still queued is `pending`, because the run is not finished with it yet.
    pub const fn state(&self) -> TargetState {
        if self.awaiting_approval > 0 {
            TargetState::AwaitingApproval
        } else if self.pending > 0 {
            TargetState::Pending
        } else if self.failed > 0 {
            if self.sent > 0 {
                TargetState::Partial
            } else {
                TargetState::Failed
            }
        } else if self.skipped > 0 && self.sent == 0 {
            TargetState::SkippedDryRun
        } else {
            TargetState::Delivered
        }
    }

    /// Whether this target is still owed something.
    pub const fn is_outstanding(&self) -> bool {
        self.pending > 0 || self.awaiting_approval > 0
    }

    /// The line a run detail shows for this target.
    pub fn summary_line(&self) -> String {
        let mut line = format!(
            "{}: {} — {} of {} delivered",
            self.target,
            self.state().as_str(),
            self.sent,
            self.total()
        );
        if let Some(error) = &self.last_error {
            line.push_str(&format!(" ({error})"));
        }
        line
    }
}

/// Every target's outcome for one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunPublishReport {
    /// Which run.
    pub run_id: RunId,
    /// One entry per target the run had actions for, in target order.
    pub targets: Vec<TargetOutcome>,
}

impl RunPublishReport {
    /// Read one run's publish status.
    pub async fn load(pool: &Pool, run_id: RunId) -> Result<Self, StoreError> {
        let actions = PublishActionStore::new(pool).list_for_run(run_id).await?;
        Ok(Self::from_actions(run_id, &actions))
    }

    /// Build a report from actions already in hand.
    ///
    /// Separate from [`Self::load`] so the shape of the report can be tested
    /// without a database, and so a caller that already listed the actions does
    /// not list them twice.
    pub fn from_actions(run_id: RunId, actions: &[PublishAction]) -> Self {
        let mut by_target: BTreeMap<&str, TargetOutcome> = BTreeMap::new();

        for action in actions {
            let entry = by_target
                .entry(action.target.as_str())
                .or_insert_with(|| TargetOutcome {
                    target: action.target.clone(),
                    sent: 0,
                    pending: 0,
                    awaiting_approval: 0,
                    failed: 0,
                    skipped: 0,
                    rejected: 0,
                    last_error: None,
                    external_refs: Vec::new(),
                });

            match action.status {
                PublishActionStatus::Sent => {
                    entry.sent += 1;
                    if let Some(reference) = &action.external_ref {
                        entry.external_refs.push(reference.clone());
                    }
                }
                PublishActionStatus::Pending | PublishActionStatus::Approved => {
                    entry.pending += 1;
                }
                PublishActionStatus::AwaitingApproval => entry.awaiting_approval += 1,
                PublishActionStatus::Failed => {
                    entry.failed += 1;
                    // The most recent failure wins: an operator reading this wants
                    // to know why it is failing now, not why it failed first.
                    if action.error.is_some() {
                        entry.last_error.clone_from(&action.error);
                    }
                }
                PublishActionStatus::SkippedDryRun => entry.skipped += 1,
                PublishActionStatus::Rejected => entry.rejected += 1,
            }
        }

        Self {
            run_id,
            targets: by_target.into_values().collect(),
        }
    }

    /// One target's outcome.
    pub fn target(&self, target: &str) -> Option<&TargetOutcome> {
        self.targets.iter().find(|t| t.target == target)
    }

    /// Whether anything is still owed.
    ///
    /// **A failed target does not block a run from finishing.** §11.6 says a run
    /// can be done with one target posted and another failed, and the alternative
    /// is worse than it sounds: one unreachable system would hold every run of the
    /// day open, and the review GitHub *did* receive would read as unfinished.
    pub fn blocks_completion(&self) -> bool {
        self.targets.iter().any(TargetOutcome::is_outstanding)
    }

    /// Whether any target failed.
    pub fn any_failed(&self) -> bool {
        self.targets
            .iter()
            .any(|t| matches!(t.state(), TargetState::Failed | TargetState::Partial))
    }

    /// The targets a `publish replay` would be worth running against.
    pub fn replayable(&self) -> impl Iterator<Item = &TargetOutcome> {
        self.targets.iter().filter(|t| t.failed > 0)
    }

    /// One line per target.
    pub fn summary_lines(&self) -> Vec<String> {
        self.targets
            .iter()
            .map(TargetOutcome::summary_line)
            .collect()
    }
}
