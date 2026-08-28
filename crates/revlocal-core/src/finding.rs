//! The `finding` and `suppression` tables (SPEC §5, §10).

use crate::{Category, FindingId, FindingState, RepoId, RunId, Severity, SuppressionId, Timestamp};
use serde::{Deserialize, Serialize};

/// The longest a finding title may be (SPEC §5, §8.3).
pub const TITLE_MAX_CHARS: usize = 80;

/// The confidence below which an action is escalated to high risk (SPEC §12.3).
pub const LOW_CONFIDENCE_THRESHOLD: f64 = 0.6;

/// One reviewer observation (`finding`, SPEC §3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    /// Primary key.
    pub id: FindingId,
    /// The run that produced it.
    pub run_id: RunId,
    /// Stable dedupe key (SPEC §10.3). Deliberately line-number independent, so a
    /// finding survives a rebase.
    pub fingerprint: String,
    /// How bad it is.
    pub severity: Severity,
    /// What kind of problem it is.
    pub category: Category,
    /// The engine's confidence, `0.0..=1.0`.
    pub confidence: f64,
    /// Path relative to the repository root, when the finding is file-scoped.
    pub file: Option<String>,
    /// First line of the implicated range.
    pub line_start: Option<u32>,
    /// Last line of the implicated range.
    pub line_end: Option<u32>,
    /// The claim alone, at most [`TITLE_MAX_CHARS`] characters.
    pub title: String,
    /// Markdown: what is wrong and why.
    pub body: String,
    /// Concrete inputs or state leading to wrong output or a crash.
    pub failure_scenario: Option<String>,
    /// Optional markdown or diff.
    pub suggested_fix: Option<String>,
    /// Where the finding is in its life.
    pub state: FindingState,
    /// When it was recorded.
    pub created_at: Timestamp,
}

impl Finding {
    /// Whether the title fits the limit SPEC §5 and §8.3 both state.
    ///
    /// Counted in characters, not bytes: an 80-byte limit would truncate a title
    /// with any non-ASCII in it at a different place than the spec intends.
    pub fn title_within_limit(&self) -> bool {
        self.title.chars().count() <= TITLE_MAX_CHARS
    }

    /// Whether this finding's confidence escalates its actions to high risk.
    ///
    /// SPEC §12.3: `confidence < 0.6` escalates.
    pub fn is_low_confidence(&self) -> bool {
        self.confidence < LOW_CONFIDENCE_THRESHOLD
    }

    /// Whether the finding blocks the change (SPEC §10.2).
    pub const fn is_blocking(&self) -> bool {
        self.severity.is_blocking()
    }
}

/// A standing instruction never to report something again (`suppression`, SPEC §5).
///
/// Either `fingerprint` or `glob` carries the match; both are optional in the
/// schema, and a suppression with neither matches nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suppression {
    /// Primary key.
    pub id: SuppressionId,
    /// Scoped to one repo, or global when absent.
    pub repo_id: Option<RepoId>,
    /// Suppress exactly this finding fingerprint.
    pub fingerprint: Option<String>,
    /// Suppress anything under this path glob.
    pub glob: Option<String>,
    /// Why the user suppressed it.
    pub reason: Option<String>,
    /// When it was created.
    pub created_at: Timestamp,
}

impl Suppression {
    /// Whether this suppression can ever match anything.
    ///
    /// A row with neither a fingerprint nor a glob is inert, and silently keeping
    /// one would look like a suppression that stopped working.
    pub const fn is_actionable(&self) -> bool {
        self.fingerprint.is_some() || self.glob.is_some()
    }
}
