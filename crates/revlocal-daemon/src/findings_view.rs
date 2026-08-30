//! The findings screen (RL-1108, SPEC §15 screen 4).
//!
//! # Filtering happens here, not in the browser
//!
//! A cross-repository findings table is the one screen that can be large, and the
//! filters are the thing that makes it usable. Applying them in the front end
//! means fetching everything first and discarding most of it — so the size of the
//! table is paid on every keystroke, by the machine least able to pay it.
//!
//! # Manual "file to Andare" is gated exactly like an automatic one
//!
//! §15 offers a manual file-to-Andare from this screen, and the temptation is to
//! send it directly: a person asked for it, so who is it protecting?
//!
//! The answer is the *repository owner*, who set an autonomy mode. §12.3 makes
//! `CreateIssue` high risk and §12.2 says a high-risk action under
//! `auto_low_ask_high` awaits approval. A manual action that bypassed that would
//! make the UI a hole in the ceiling every other path respects — and the person
//! clicking is not always the person whose tracker it lands in.
//!
//! So [`file_to_andare`] classifies with `baseline_risk` and resolves with
//! `disposition`, the same two functions the pipeline uses. Under the default mode
//! a manual file lands in the approvals inbox, which is the correct surprise.

use revlocal_core::{
    AutonomyMode, Capability, FindingState, PublishAction, PublishActionId, RepoId, RiskClass,
    Severity, Timestamp,
};
use revlocal_store::{FindingStore, Pool, PublishActionStore, RepoStore, RunStore};
use serde::{Deserialize, Serialize};

use crate::autonomy::disposition;

/// How many runs to read when gathering findings.
///
/// Not a silent cap: [`FindingsView::truncated`] says when it was hit and the
/// screen shows it. A findings table that quietly stopped at a boundary would
/// present "no more findings" and "we stopped looking" as the same thing.
pub const FINDINGS_RUN_SCAN: u32 = 500;

/// Why the findings could not be read.
#[derive(Debug, thiserror::Error)]
pub enum FindingsError {
    /// The database could not be read.
    #[error("could not read the local database: {source}\n  try: revlocal db migrate")]
    Store {
        /// Why.
        #[source]
        source: Box<revlocal_store::StoreError>,
    },

    /// The finding names no repository to file against.
    #[error("finding {id} belongs to no repository, so there is nowhere to file it")]
    Orphaned {
        /// Which finding.
        id: i64,
    },
}

fn boxed(source: revlocal_store::StoreError) -> FindingsError {
    FindingsError::Store {
        source: Box::new(source),
    }
}

/// What the screen filters on.
///
/// Every field is optional and they **compose** — `severity=high` and
/// `state=open` together mean both, not either. Independent fields rather than
/// one enum, because a filter set that could only express one dimension at a time
/// is not a filter set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingFilter {
    /// This severity and worse. `None` means every severity.
    pub min_severity: Option<Severity>,
    /// Exactly this category. `None` means every category.
    pub category: Option<String>,
    /// Exactly this state. `None` means every state.
    pub state: Option<FindingState>,
    /// One repository. `None` means all of them.
    pub repo_id: Option<i64>,
}

impl FindingFilter {
    /// Whether a finding survives every filter that is set.
    ///
    /// `all` rather than `any`: filters narrow. A screen where adding a second
    /// filter widened the result would be one nobody could reason about.
    pub fn matches(&self, finding: &FindingRow) -> bool {
        if let Some(min) = self.min_severity {
            if severity_rank(finding.severity) < severity_rank(min) {
                return false;
            }
        }
        if let Some(category) = &self.category {
            if &finding.category != category {
                return false;
            }
        }
        if let Some(state) = self.state {
            if finding.state != state {
                return false;
            }
        }
        if let Some(repo_id) = self.repo_id {
            if finding.repo_id != repo_id {
                return false;
            }
        }
        true
    }
}

/// Severity as an order, worst first.
///
/// §10.1 lists them by consequence rather than alphabetically, and "this severity
/// and worse" needs that order to mean anything.
const fn severity_rank(severity: Severity) -> u8 {
    match severity {
        Severity::Critical => 4,
        Severity::High => 3,
        Severity::Medium => 2,
        Severity::Low => 1,
        Severity::Info => 0,
    }
}

/// One row of the table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingRow {
    /// The finding's id.
    pub id: i64,
    /// The run that produced it, so the screen can jump there.
    pub run_id: i64,
    /// Which repository, so a cross-repo table can say where each row came from.
    pub repo_id: i64,
    /// The repository's name, because an id is not something to scan a table by.
    pub repo: String,
    /// How bad.
    ///
    /// The typed value rather than a string beside it. `string_enum!` serialises
    /// these as the name the config and the spec use, so the screen reads "high"
    /// either way — and one field cannot disagree with itself the way a typed
    /// value and a rendered copy eventually do.
    pub severity: Severity,
    /// What kind.
    pub category: String,
    /// Where it stands.
    pub state: FindingState,
    /// One line.
    pub title: String,
    /// The file it names, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// How the same finding is recognised across runs (§10.3).
    pub fingerprint: String,
}

/// The screen's data (§15 screen 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingsView {
    /// The rows that survived the filter.
    pub rows: Vec<FindingRow>,
    /// Every category present *before* filtering, so the filter can offer them.
    ///
    /// From the data rather than a fixed list: §10's categories can grow, and a
    /// dropdown that offered a category nothing has is a dead end.
    pub categories: Vec<String>,
    /// How many findings existed before the filter narrowed them.
    ///
    /// So the screen can say "12 of 340" — a count of what is shown, with no
    /// sense of what was hidden, is how somebody concludes a filter found
    /// everything.
    pub total_before_filter: usize,
    /// Whether the scan stopped at [`FINDINGS_RUN_SCAN`] (§18).
    pub truncated: bool,
}

/// Read findings across repositories (SPEC §15 screen 4).
pub async fn gather(pool: &Pool, filter: &FindingFilter) -> Result<FindingsView, FindingsError> {
    let repos = RepoStore::new(pool).list().await.map_err(boxed)?;
    let name_of = |repo_id: RepoId| {
        repos
            .iter()
            .find(|r| r.id == repo_id)
            .map_or_else(|| format!("repo {}", repo_id.get()), |r| r.name.clone())
    };

    let runs = RunStore::new(pool)
        .list_recent(None, None, FINDINGS_RUN_SCAN)
        .await
        .map_err(boxed)?;
    let truncated = u32::try_from(runs.len()).unwrap_or(u32::MAX) >= FINDINGS_RUN_SCAN;

    let changes = revlocal_store::ChangeStore::new(pool);
    let findings = FindingStore::new(pool);

    let mut all = Vec::new();
    for run in &runs {
        let change = changes.get(run.change_id).await.map_err(boxed)?;
        for finding in findings.list_for_run(run.id).await.map_err(boxed)? {
            all.push(FindingRow {
                id: finding.id.get(),
                run_id: run.id.get(),
                repo_id: change.repo_id.get(),
                repo: name_of(change.repo_id),
                severity: finding.severity,
                category: finding.category.as_str().to_owned(),
                state: finding.state,
                title: finding.title,
                file: finding.file,
                fingerprint: finding.fingerprint,
            });
        }
    }

    let total_before_filter = all.len();
    let mut categories: Vec<String> = all.iter().map(|r| r.category.clone()).collect();
    categories.sort();
    categories.dedup();

    Ok(FindingsView {
        rows: all.into_iter().filter(|row| filter.matches(row)).collect(),
        categories,
        total_before_filter,
        truncated,
    })
}

/// File a finding to Andare by hand, gated exactly like an automatic action.
///
/// Returns the status the action was given, so the caller can tell somebody
/// whether it was sent or is waiting — "filed" would be a lie under the default
/// mode, where it is queued for approval.
pub async fn file_to_andare(
    pool: &Pool,
    finding_id: i64,
    global_mode: AutonomyMode,
    at: Timestamp,
) -> Result<revlocal_core::PublishActionStatus, FindingsError> {
    let finding = FindingStore::new(pool)
        .get(revlocal_core::FindingId::new(finding_id))
        .await
        .map_err(boxed)?;

    let run = RunStore::new(pool)
        .get(finding.run_id)
        .await
        .map_err(boxed)?;
    let change = revlocal_store::ChangeStore::new(pool)
        .get(run.change_id)
        .await
        .map_err(boxed)?;
    let repo = RepoStore::new(pool)
        .list()
        .await
        .map_err(boxed)?
        .into_iter()
        .find(|r| r.id == change.repo_id)
        .ok_or(FindingsError::Orphaned { id: finding_id })?;

    // The same two functions the pipeline uses. §12.3 makes `CreateIssue` high
    // risk; §12.2 says a high-risk action under `auto_low_ask_high` awaits a
    // human. A manual action that skipped these would make this screen a hole in
    // the ceiling every other path respects.
    let risk: RiskClass = revlocal_core::ActionIntent::CreateIssue.baseline_risk();
    let effective = AutonomyMode::effective(global_mode, repo.autonomy);
    let Some(status) = disposition(effective, risk).initial_status() else {
        // `Off` means no actions at all. Reporting that plainly beats creating a
        // row nobody will ever dispatch.
        return Err(FindingsError::Orphaned { id: finding_id });
    };

    let payload = serde_json::json!({
        "title": finding.title,
        "body": finding.body,
        "rev-local-fingerprint": finding.fingerprint,
    });

    PublishActionStore::new(pool)
        .insert(&PublishAction {
            id: PublishActionId::new(0),
            run_id: finding.run_id,
            finding_id: Some(finding.id),
            target: "andare".to_owned(),
            capability: Capability::CreateIssue,
            risk,
            // §11.6: the fingerprint makes redelivery safe. A manual file and an
            // automatic one for the same finding must not become two issues.
            idempotency_key: format!("manual-andare-{}", finding.fingerprint),
            payload_json: payload.to_string(),
            status,
            attempts: 0,
            response_json: None,
            external_ref: None,
            error: None,
            created_at: at,
            sent_at: None,
        })
        .await
        .map_err(boxed)?;

    Ok(status)
}

/// Suppress a finding from the table (§14 `findings suppress`, §15 screen 4).
///
/// Two writes, because a suppression and a finding's state answer different
/// questions. The suppression stops §10.3 raising the same fingerprint on a
/// *future* run; the state is what this row says *now*. Doing only the first
/// leaves somebody looking at an unchanged table wondering whether the click
/// registered, which is the acceptance criterion's "the row updates immediately".
///
/// Scoped to the finding's own repository, never globally. The row names one
/// repository and that is the scope somebody reading it has in mind — the wider
/// choice exists, on the command line, where it has to be typed rather than
/// implied. Narrowing wrongly costs a second suppression; widening wrongly
/// silences a rule everywhere and says nothing.
pub async fn suppress(
    pool: &Pool,
    finding_id: i64,
    at: Timestamp,
) -> Result<FindingState, FindingsError> {
    let findings = FindingStore::new(pool);
    let finding = findings
        .get(revlocal_core::FindingId::new(finding_id))
        .await
        .map_err(boxed)?;

    let run = RunStore::new(pool)
        .get(finding.run_id)
        .await
        .map_err(boxed)?;
    let change = revlocal_store::ChangeStore::new(pool)
        .get(run.change_id)
        .await
        .map_err(boxed)?;

    revlocal_store::SuppressionStore::new(pool)
        .insert(&revlocal_core::Suppression {
            id: revlocal_core::SuppressionId::new(0),
            repo_id: Some(change.repo_id),
            fingerprint: Some(finding.fingerprint.clone()),
            glob: None,
            reason: Some("suppressed from the findings screen".to_owned()),
            created_at: at,
        })
        .await
        .map_err(boxed)?;

    findings
        .set_state(finding.id, FindingState::Suppressed)
        .await
        .map_err(boxed)?;

    Ok(FindingState::Suppressed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(severity: Severity, category: &str, state: FindingState, repo_id: i64) -> FindingRow {
        FindingRow {
            id: 1,
            run_id: 1,
            repo_id,
            repo: "acme".to_owned(),
            severity,
            category: category.to_owned(),
            state,
            title: "something".to_owned(),
            file: None,
            fingerprint: "fp".to_owned(),
        }
    }

    #[test]
    fn findings_filters_narrow_rather_than_widen() {
        // The property the acceptance criterion calls "filters compose". A screen
        // where adding a second filter *widened* the result is one nobody can
        // reason about — and `any` instead of `all` is a one-word mistake.
        let high_security = row(Severity::High, "security", FindingState::Open, 1);

        let both = FindingFilter {
            min_severity: Some(Severity::High),
            category: Some("security".to_owned()),
            ..FindingFilter::default()
        };
        assert!(both.matches(&high_security));

        // One filter that does not match is enough to exclude it.
        let mismatched = FindingFilter {
            min_severity: Some(Severity::High),
            category: Some("performance".to_owned()),
            ..FindingFilter::default()
        };
        assert!(!mismatched.matches(&high_security));
    }

    #[test]
    fn findings_severity_filters_by_consequence_not_alphabet() {
        // "this severity and worse" needs §10.1's order. Alphabetically `critical`
        // sorts before `high` and `info` before `low`, which would make the filter
        // quietly wrong in both directions.
        let filter = FindingFilter {
            min_severity: Some(Severity::High),
            ..FindingFilter::default()
        };

        assert!(filter.matches(&row(Severity::Critical, "c", FindingState::Open, 1)));
        assert!(filter.matches(&row(Severity::High, "c", FindingState::Open, 1)));
        assert!(!filter.matches(&row(Severity::Medium, "c", FindingState::Open, 1)));
        assert!(!filter.matches(&row(Severity::Info, "c", FindingState::Open, 1)));
    }

    #[test]
    fn findings_an_empty_filter_matches_everything() {
        // The default view is unfiltered. A default that hid anything would make
        // the first thing somebody sees an incomplete table they did not ask for.
        let filter = FindingFilter::default();

        assert!(filter.matches(&row(Severity::Info, "style", FindingState::Suppressed, 9)));
    }

    #[test]
    fn findings_filtering_by_repository_is_exact() {
        let filter = FindingFilter {
            repo_id: Some(1),
            ..FindingFilter::default()
        };

        assert!(filter.matches(&row(Severity::High, "c", FindingState::Open, 1)));
        assert!(!filter.matches(&row(Severity::High, "c", FindingState::Open, 2)));
    }

    #[test]
    fn findings_a_manual_file_is_high_risk_like_an_automatic_one() {
        // Criterion 3, at the level where it is decided. §12.3 makes `CreateIssue`
        // high risk, and a manual path that classified it differently would make
        // this screen a hole in the ceiling every other path respects.
        assert_eq!(
            revlocal_core::ActionIntent::CreateIssue.baseline_risk(),
            RiskClass::High
        );
    }

    #[test]
    fn findings_a_manual_file_awaits_approval_under_the_default_mode() {
        // The consequence of the above, through the same `disposition` the
        // pipeline uses: under `auto_low_ask_high` a manual file lands in the
        // approvals inbox rather than being sent. That is the correct surprise.
        let status = disposition(
            AutonomyMode::AutoLowAskHigh,
            revlocal_core::ActionIntent::CreateIssue.baseline_risk(),
        )
        .initial_status();

        assert_eq!(
            status,
            Some(revlocal_core::PublishActionStatus::AwaitingApproval)
        );

        // And a dry-run repository records it without sending, rather than the
        // manual path quietly overriding the mode somebody chose.
        assert_eq!(
            disposition(AutonomyMode::DryRun, RiskClass::High).initial_status(),
            Some(revlocal_core::PublishActionStatus::SkippedDryRun)
        );
    }
}
