//! First-run onboarding (RL-1205, SPEC §15).
//!
//! # The default autonomy is never `auto`, and that is a property of the flow
//!
//! §12.2's modes are a ceiling on what rev-local may do to somebody else's
//! systems. A repository added a moment ago has never been reviewed and nobody
//! has seen a finding from it, so the one thing onboarding must not do is leave
//! it able to publish. [`Draft::autonomy`] starts at `dry_run` and
//! [`Draft::is_safe_default`] is asserted rather than assumed — a default is a
//! decision somebody did not make, and this is the one where that matters most.
//!
//! # Where onboarding stops
//!
//! At a *dry-run review whose result is on screen*. Not at "a repository is
//! configured": somebody who has added a repository and seen nothing does not yet
//! know whether any of it works, and the first thing they would do is try to find
//! out. §15's own path ends at "show the result" for that reason.
//!
//! # It is re-runnable
//!
//! From Settings, at any time. Onboarding that can only happen once is a thing
//! people are afraid to leave — and the second repository deserves the same walk
//! as the first.

use revlocal_core::{AutonomyMode, EngineKind, GlobalConfig, RepoKind, Timestamp};
use revlocal_store::Pool;
use serde::{Deserialize, Serialize};

use crate::doctor::DoctorReport;

/// The five steps §15 names, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// `doctor`: what is installed, and what is not.
    Check,
    /// Which repository to watch.
    AddRepo,
    /// Which engine reviews it (D3).
    PickEngine,
    /// How much it may do unattended (§12.2).
    PickAutonomy,
    /// One review, and its result on screen.
    FirstReview,
}

impl Step {
    /// Every step, in order.
    pub const ALL: [Self; 5] = [
        Self::Check,
        Self::AddRepo,
        Self::PickEngine,
        Self::PickAutonomy,
        Self::FirstReview,
    ];

    /// The heading this step shows.
    pub const fn title(self) -> &'static str {
        match self {
            Self::Check => "Check what is installed",
            Self::AddRepo => "Choose a repository",
            Self::PickEngine => "Choose an engine",
            Self::PickAutonomy => "Choose how much it may do",
            Self::FirstReview => "Review one change",
        }
    }

    /// The step after this one, or `None` at the end.
    pub fn next(self) -> Option<Self> {
        let index = Self::ALL.iter().position(|s| *s == self)?;
        Self::ALL.get(index + 1).copied()
    }
}

/// What onboarding is building.
///
/// A draft rather than a half-written repository row: somebody who abandons
/// onboarding halfway should not find a repository they did not finish adding,
/// polling a path they were still choosing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    /// The path or URL being added.
    pub path: String,
    /// What to call it. Derived from the path when empty.
    pub name: String,
    /// Which VCS.
    pub kind: RepoKind,
    /// Which engine (D3).
    pub engine: EngineKind,
    /// How much it may do unattended (§12.2).
    pub autonomy: AutonomyMode,
}

impl Default for Draft {
    /// The safe starting point.
    ///
    /// `dry_run`, not `auto`: rev-local reviews and publishes nothing until
    /// somebody has seen what it produces. `mock` for the engine, because a first
    /// review that spends money before anybody has decided to is a worse surprise
    /// than one that is obviously a rehearsal — and the engine step is where that
    /// gets changed on purpose.
    fn default() -> Self {
        Self {
            path: String::new(),
            name: String::new(),
            kind: RepoKind::Git,
            engine: EngineKind::Mock,
            autonomy: AutonomyMode::DryRun,
        }
    }
}

impl Draft {
    /// Whether this draft would publish without anybody having seen a finding.
    ///
    /// The criterion, as a function rather than a comment. `auto` is the only mode
    /// that publishes unattended, and a newly added repository has no history for
    /// anybody to have judged.
    pub const fn is_safe_default(&self) -> bool {
        !matches!(self.autonomy, AutonomyMode::Auto)
    }

    /// What is missing before this step can be completed.
    ///
    /// Returned rather than enforced by disabling a button: the reason is what
    /// somebody needs, and a control that is simply dead teaches nothing.
    pub fn blocker(&self, step: Step) -> Option<String> {
        match step {
            Step::AddRepo if self.path.trim().is_empty() => {
                Some("choose a repository directory or URL".to_owned())
            }
            _ => None,
        }
    }
}

/// Why onboarding could not continue.
#[derive(Debug, thiserror::Error)]
pub enum OnboardingError {
    /// The repository could not be added.
    #[error("{detail}")]
    AddRepo {
        /// What went wrong, in the terms the screen shows.
        detail: String,
    },

    /// The first review could not run.
    #[error("{detail}")]
    Review {
        /// What went wrong.
        detail: String,
    },
}

/// What the first review produced, for the last screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirstReview {
    /// The run, so the screen can link to its detail.
    pub run_id: i64,
    /// Which repository.
    pub repo: String,
    /// Where it got to.
    pub status: String,
    /// What it concluded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    /// How many findings it recorded.
    pub findings: usize,
    /// Which engine ran — `mock` means nothing was spent and nothing is real.
    pub engine: String,
    /// Said out loud when the engine was the mock (§18).
    ///
    /// A rehearsal that reads like a real review is the worst possible first
    /// impression: everything that follows is judged against findings that were
    /// invented.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caveat: Option<String>,
}

/// Where onboarding stands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Onboarding {
    /// Which step is showing.
    pub step: Step,
    /// What is being built.
    pub draft: Draft,
    /// `doctor`'s output, once it has run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doctor: Option<DoctorReport>,
    /// The first review, once it has run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review: Option<FirstReview>,
    /// Whether this is a fresh install rather than a re-run from Settings.
    pub first_run: bool,
}

impl Onboarding {
    /// Start at the beginning.
    pub fn start(first_run: bool) -> Self {
        Self {
            step: Step::Check,
            draft: Draft::default(),
            doctor: None,
            review: None,
            first_run,
        }
    }
}

/// Whether this installation has never been set up.
///
/// "No repositories" rather than a stored flag. A flag can be true on a machine
/// with nothing configured — after a database is moved, or restored, or deleted —
/// and then onboarding does not offer itself to the one person who needs it.
pub async fn is_first_run(pool: &Pool) -> Result<bool, revlocal_store::StoreError> {
    Ok(revlocal_store::RepoStore::new(pool)
        .list()
        .await?
        .is_empty())
}

/// Add the drafted repository (§14's `repo add`, through onboarding).
///
/// Delegates to [`crate::repos::add`], which is what `revlocal repo add` calls.
/// A second path that created repositories would eventually disagree with it
/// about a default — and the default this flow must not get wrong is autonomy.
pub async fn add_repo(
    pool: &Pool,
    draft: &Draft,
    at: Timestamp,
) -> Result<crate::repos::RepoWriteReport, OnboardingError> {
    // `auto` is not refused here. §12.2's modes exist to be chosen, and a safety
    // property that cannot be switched off is a bug report waiting to happen. The
    // guarantee is about the *default*, which lives in `Draft::default`, where a
    // test asserts it — not in a check here that somebody would eventually add a
    // flag to bypass.
    let name = (!draft.name.trim().is_empty()).then(|| draft.name.trim());

    crate::repos::add(
        pool,
        draft.path.trim(),
        draft.kind.as_str(),
        name,
        draft.engine.as_str(),
        draft.autonomy.as_str(),
        at,
    )
    .await
    .map_err(|source| OnboardingError::AddRepo {
        detail: source.to_string(),
    })
}

/// Run the first review, end to end, through the same executor `watch` uses.
///
/// Not a special onboarding path. A first review that worked differently from
/// every later one would demonstrate something that does not exist, and the
/// failure would arrive on the second day.
pub async fn first_review(
    pool: &Pool,
    config: &GlobalConfig,
    data_dir: &std::path::Path,
    repo_name: &str,
    at: Timestamp,
) -> Result<FirstReview, OnboardingError> {
    let repos = revlocal_store::RepoStore::new(pool)
        .list()
        .await
        .map_err(|e| OnboardingError::Review {
            detail: e.to_string(),
        })?;
    let repo =
        repos
            .iter()
            .find(|r| r.name == repo_name)
            .ok_or_else(|| OnboardingError::Review {
                detail: format!("no repository called {repo_name}"),
            })?;

    crate::executor::enqueue(pool, repo, at)
        .await
        .map_err(|e| OnboardingError::Review {
            detail: e.to_string(),
        })?;

    let report = crate::executor::drain(
        pool,
        config,
        &crate::state_machine::NullSink,
        data_dir,
        1,
        at,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .map_err(|e| OnboardingError::Review {
        detail: e.to_string(),
    })?;

    let outcome = report
        .finished
        .into_iter()
        .next()
        .ok_or_else(|| OnboardingError::Review {
            detail: report.held.first().cloned().unwrap_or_else(|| {
                "there is nothing to review yet — rev-local has not discovered a change in \
                 this repository. Make a commit, or run `revlocal watch --once`."
                    .to_owned()
            }),
        })?;

    Ok(FirstReview {
        caveat: (outcome.engine == EngineKind::Mock.as_str()).then(|| {
            "This was the mock engine: it spends nothing and invents its findings. \
             Choose Claude Code or Codex in Settings for a real review."
                .to_owned()
        }),
        run_id: outcome.run_id,
        repo: outcome.repo,
        status: outcome.status,
        verdict: outcome.verdict,
        findings: outcome.findings,
        engine: outcome.engine,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onboarding_never_defaults_a_new_repository_to_auto() {
        // The criterion, at the level where it is decided. A repository added a
        // moment ago has never been reviewed and nobody has seen a finding from
        // it, so leaving it able to publish is the one mistake this flow must not
        // make.
        let draft = Draft::default();

        assert_eq!(draft.autonomy, AutonomyMode::DryRun);
        assert!(draft.is_safe_default());
    }

    #[test]
    fn onboarding_auto_is_still_reachable_when_it_is_chosen() {
        // A safety property that cannot be switched off is a bug report waiting to
        // happen. The rule is about the default, not a prohibition.
        let draft = Draft {
            autonomy: AutonomyMode::Auto,
            ..Draft::default()
        };

        assert!(!draft.is_safe_default());
    }

    #[test]
    fn onboarding_defaults_to_an_engine_that_spends_nothing() {
        // A first review that costs money before anybody decided to is a worse
        // surprise than one that is obviously a rehearsal.
        assert_eq!(Draft::default().engine, EngineKind::Mock);
    }

    #[test]
    fn onboarding_walks_the_five_steps_in_order() {
        // §15's own path: doctor → add repo → pick engine → choose autonomy →
        // dry-run review → show the result.
        assert_eq!(Step::ALL.len(), 5);
        assert_eq!(Step::Check.next(), Some(Step::AddRepo));
        assert_eq!(Step::AddRepo.next(), Some(Step::PickEngine));
        assert_eq!(Step::PickEngine.next(), Some(Step::PickAutonomy));
        assert_eq!(Step::PickAutonomy.next(), Some(Step::FirstReview));
        // It ends at a result on screen, not at a configured repository.
        assert_eq!(Step::FirstReview.next(), None);
    }

    #[test]
    fn onboarding_says_what_is_missing_rather_than_only_refusing() {
        // A control that is simply dead teaches nothing about why.
        let empty = Draft::default();

        let blocker = empty.blocker(Step::AddRepo).expect("a path is required");
        assert!(blocker.contains("repository"), "{blocker}");

        let chosen = Draft {
            path: "/home/me/acme".to_owned(),
            ..Draft::default()
        };
        assert!(chosen.blocker(Step::AddRepo).is_none());
    }

    #[test]
    fn onboarding_starts_at_the_check() {
        // Doctor first, because §8.4 says its output "is the first thing the UI
        // shows on a fresh install" — somebody with no engine installed should
        // learn that before choosing one.
        assert_eq!(Onboarding::start(true).step, Step::Check);
    }
}
