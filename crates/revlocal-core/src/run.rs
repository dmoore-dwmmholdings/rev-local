//! The `run` table and token accounting (SPEC §5, §8).

use crate::{ChangeId, Depth, EngineKind, RunId, RunStatus, Timestamp, TriggerSource, Verdict};
use serde::{Deserialize, Serialize};

/// One execution of the pipeline against one change (`run`, SPEC §3).
///
/// `(change_id, attempt)` is unique: a retry is a new run, not a mutated one, so
/// the history of what was tried survives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    /// Primary key.
    pub id: RunId,
    /// The change under review.
    pub change_id: ChangeId,
    /// 1 for the first attempt, incrementing per retry.
    pub attempt: u32,
    /// Where the run is in its lifecycle.
    pub status: RunStatus,
    /// Which engine ran. Recorded per run, since a repo's engine can change.
    pub engine: EngineKind,
    /// How thoroughly it was reviewed (SPEC §9.3).
    pub depth: Depth,
    /// What caused the run.
    pub trigger: TriggerSource,
    /// Why the run was skipped. Set exactly when `status` is
    /// [`RunStatus::Skipped`] — SPEC §18's "no silent caps" means a skip always
    /// says why.
    pub skip_reason: Option<String>,
    /// Why the run failed. Set exactly when `status` is [`RunStatus::Failed`].
    pub error: Option<String>,
    /// Token and cost accounting.
    pub usage: Usage,
    /// When the engine process started.
    pub started_at: Option<Timestamp>,
    /// When the run reached a terminal status.
    pub finished_at: Option<Timestamp>,
    /// Path to the raw engine stdout on disk. Pruned by retention (SPEC §5.1).
    pub transcript_path: Option<String>,
    /// Whether the diff was reduced before the engine saw it (SPEC §9.4).
    ///
    /// SPEC §18: "a review that saw 60% of the diff must never look like a review
    /// that saw all of it." This is what stops that, so it is stored on the run
    /// rather than living only in the in-memory context.
    pub truncated: bool,
    /// What was left out, **in full** (SPEC §9.4).
    ///
    /// §9.4: "Truncation must never silently hide a file: the omitted file list is
    /// always included in full." A count would not satisfy that; the names are the
    /// point.
    pub omitted_files: Vec<String>,
    /// The verdict this review reached (SPEC §10.2).
    ///
    /// Stored rather than recomputed from findings. It is a **historical fact** —
    /// what was posted — and recomputing it would change retroactively as findings
    /// are suppressed or superseded, so a run that requested changes would silently
    /// become one that approved.
    pub verdict: Option<Verdict>,
    /// The engine's own summary (SPEC §8.3), at most 1200 characters.
    ///
    /// Not derivable from anything else, and it outlives the transcript, which
    /// retention prunes after 30 days (§5.1).
    pub summary: Option<String>,
    /// Why the engine output had to be salvaged, when it did.
    ///
    /// `Some` exactly when a step of the §8.2 fallback ladder was used. Typed as a
    /// reason rather than a flag to match `EngineOutcome::degraded` in SPEC §8.1,
    /// and because "no silent caps" (§18) means a degraded run has to say what was
    /// degraded about it. A degraded run escalates every publish action to high
    /// risk (§12.3), so this is load-bearing, not diagnostic.
    pub degraded: Option<String>,
    /// When the row was created.
    pub created_at: Timestamp,
}

impl Run {
    /// Whether the run's output had to be salvaged (SPEC §8.2).
    pub const fn is_degraded(&self) -> bool {
        self.degraded.is_some()
    }

    /// Whether this run's state is self-consistent.
    ///
    /// SPEC §18 forbids silent caps: a skipped run must say why, and a failed run
    /// must carry its error. This is the invariant the store asserts on write.
    pub fn is_consistent(&self) -> bool {
        let skip_ok = (self.status == RunStatus::Skipped) == self.skip_reason.is_some();
        let error_ok = (self.status == RunStatus::Failed) == self.error.is_some();
        // §9.4/§18: a truncated run must say WHAT was omitted. Claiming truncation
        // with an empty list is the silent cap the rule exists to prevent.
        let truncation_ok = !self.truncated || !self.omitted_files.is_empty();
        skip_ok && error_ok && truncation_ok
    }
}

/// Token and cost accounting for one run (`run.tokens_*`, `run.cost_usd`).
///
/// `cost_usd` is optional because not every engine reports a price; a missing cost
/// is recorded as missing rather than as zero, so budget maths cannot silently
/// treat an unknown spend as free (SPEC §18).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    /// Input tokens consumed.
    pub tokens_in: u64,
    /// Output tokens produced.
    pub tokens_out: u64,
    /// Cost in USD, when the engine reports one.
    pub cost_usd: Option<f64>,
}

impl Usage {
    /// Total tokens, which is what per-repo budgets are denominated in (D10).
    pub const fn total_tokens(&self) -> u64 {
        self.tokens_in + self.tokens_out
    }

    /// Add another run's usage to this one.
    ///
    /// An unknown cost stays unknown rather than being read as zero: if either side
    /// reports a cost the sum is `Some`, and the unknown side contributes nothing —
    /// which is why [`cost_is_complete`](Self::cost_is_complete) exists to say
    /// whether the total can be trusted.
    pub fn add(&mut self, other: &Self) {
        self.tokens_in += other.tokens_in;
        self.tokens_out += other.tokens_out;
        self.cost_usd = match (self.cost_usd, other.cost_usd) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
        };
    }

    /// Whether `cost_usd` accounts for everything measured.
    pub const fn cost_is_complete(&self) -> bool {
        self.cost_usd.is_some()
    }
}

/// A run status change that the lifecycle does not allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a run cannot move from `{from}` to `{to}`")]
pub struct IllegalTransition {
    /// The status the run is in.
    pub from: RunStatus,
    /// The status that was requested.
    pub to: RunStatus,
}

impl RunStatus {
    /// The statuses a run may move to from here (SPEC §5, §9.1).
    ///
    /// The pipeline is mostly linear, with three ways out of any active stage:
    /// failure, cancellation (the kill switch, §12.1), and — before the engine
    /// runs — a skip.
    ///
    /// Terminal statuses have no successors. That is the property worth having:
    /// a finished run cannot be quietly restarted in place, so its history stays
    /// readable. A retry is a new run with a higher `attempt`, which is why
    /// `(change_id, attempt)` is the unique key rather than `change_id`.
    pub const fn allowed_next(self) -> &'static [Self] {
        match self {
            Self::Queued => &[
                Self::Preparing,
                Self::Skipped,
                Self::Cancelled,
                Self::Failed,
            ],
            Self::Preparing => &[
                Self::Reviewing,
                Self::Skipped,
                Self::Failed,
                Self::Cancelled,
            ],
            Self::Reviewing => &[Self::Synthesizing, Self::Failed, Self::Cancelled],
            Self::Synthesizing => &[Self::Publishing, Self::Done, Self::Failed, Self::Cancelled],
            // Publishing reaches awaiting_approval when a high-risk action is
            // queued to the inbox (§12.4), and done when every action resolved.
            Self::Publishing => &[
                Self::AwaitingApproval,
                Self::Done,
                Self::Failed,
                Self::Cancelled,
            ],
            // An approval decision sends it back to publishing; a rejection of
            // everything outstanding finishes it.
            Self::AwaitingApproval => &[Self::Publishing, Self::Done, Self::Cancelled],
            Self::Done | Self::Failed | Self::Skipped | Self::Cancelled => &[],
        }
    }

    /// Whether a run may move from this status to `next`.
    pub fn can_transition_to(self, next: Self) -> bool {
        self.allowed_next().contains(&next)
    }

    /// Check a transition, naming both ends when it is refused.
    pub fn check_transition(self, next: Self) -> Result<(), IllegalTransition> {
        if self.can_transition_to(next) {
            Ok(())
        } else {
            Err(IllegalTransition {
                from: self,
                to: next,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_happy_path_through_the_pipeline_is_walkable() {
        let path = [
            RunStatus::Queued,
            RunStatus::Preparing,
            RunStatus::Reviewing,
            RunStatus::Synthesizing,
            RunStatus::Publishing,
            RunStatus::Done,
        ];
        for pair in path.windows(2) {
            let (from, to) = (pair[0], pair[1]);
            assert!(from.can_transition_to(to), "{from} -> {to} must be allowed");
        }
    }

    #[test]
    fn a_finished_run_cannot_be_restarted_in_place() {
        // The acceptance criterion's example, and the reason `(change_id,
        // attempt)` is the unique key: a retry is a new run, so the history of
        // what was tried survives.
        let error = RunStatus::Done
            .check_transition(RunStatus::Reviewing)
            .expect_err("done -> reviewing must be refused");
        assert_eq!(error.from, RunStatus::Done);
        assert_eq!(error.to, RunStatus::Reviewing);
        assert!(error.to_string().contains("done"), "{error}");
        assert!(error.to_string().contains("reviewing"), "{error}");
    }

    #[test]
    fn every_terminal_status_is_a_dead_end() {
        for status in RunStatus::ALL {
            if status.is_terminal() {
                assert!(
                    status.allowed_next().is_empty(),
                    "{status} is terminal but has successors"
                );
                for next in RunStatus::ALL {
                    assert!(!status.can_transition_to(*next), "{status} -> {next}");
                }
            }
        }
    }

    #[test]
    fn terminality_and_the_transition_table_agree() {
        // Two independent statements of the same fact; if they disagree, one of
        // them is a bug rather than a redundancy.
        for status in RunStatus::ALL {
            assert_eq!(
                status.is_terminal(),
                status.allowed_next().is_empty(),
                "{status}: is_terminal() and allowed_next() disagree"
            );
        }
    }

    #[test]
    fn every_active_status_can_be_cancelled() {
        // SPEC §12.1: the kill switch cancels every token and drains the queues.
        // A stage that cannot be cancelled would survive it.
        for status in RunStatus::ALL {
            if !status.is_terminal() {
                assert!(
                    status.can_transition_to(RunStatus::Cancelled),
                    "{status} must be cancellable by the kill switch"
                );
            }
        }
    }

    #[test]
    fn a_run_cannot_be_skipped_once_the_engine_has_started() {
        // Skipping means "not reviewed" (§9.4). After `reviewing`, tokens have
        // been spent, so the honest terminal states are done, failed or cancelled.
        assert!(RunStatus::Queued.can_transition_to(RunStatus::Skipped));
        assert!(RunStatus::Preparing.can_transition_to(RunStatus::Skipped));
        for started in [
            RunStatus::Reviewing,
            RunStatus::Synthesizing,
            RunStatus::Publishing,
        ] {
            assert!(
                !started.can_transition_to(RunStatus::Skipped),
                "{started} -> skipped would report spent tokens as not reviewed"
            );
        }
    }

    #[test]
    fn an_approval_decision_returns_the_run_to_publishing() {
        // §12.4: an approved action still has to be sent.
        assert!(RunStatus::AwaitingApproval.can_transition_to(RunStatus::Publishing));
        assert!(RunStatus::AwaitingApproval.can_transition_to(RunStatus::Done));
    }

    #[test]
    fn no_status_transitions_to_itself() {
        // A self-transition would let a stage silently "restart" without an
        // attempt increment, which is exactly what the unique key prevents.
        for status in RunStatus::ALL {
            assert!(!status.can_transition_to(*status), "{status} -> {status}");
        }
    }

    #[test]
    fn no_status_can_go_backwards_through_the_pipeline() {
        // Declaration order of the non-terminal statuses is the pipeline order.
        let pipeline = [
            RunStatus::Queued,
            RunStatus::Preparing,
            RunStatus::Reviewing,
            RunStatus::Synthesizing,
            RunStatus::Publishing,
        ];
        for (index, from) in pipeline.iter().enumerate() {
            for earlier in &pipeline[..index] {
                assert!(
                    !from.can_transition_to(*earlier),
                    "{from} must not go back to {earlier}"
                );
            }
        }
    }
}
