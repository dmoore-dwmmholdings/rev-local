//! An in-process [`Engine`] for tests (SPEC §16.1).
//!
//! Distinct from `fixtures/mock-engine`, and both are needed. The fixture is a real
//! subprocess and is what exercises §8.2's fallback ladder — spawning, timeouts,
//! process-group kill, malformed files on disk. This one never spawns anything, and
//! is what lets a *pipeline* test say "given this outcome, then that publish plan"
//! without a process, a filesystem or a hundred milliseconds.
//!
//! Using the subprocess fixture for pipeline tests would make them slow and would
//! test the runner over and over instead of the thing under test.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use revlocal_core::{Category, Severity, Usage, Verdict};

use crate::engine::{
    Engine, EngineError, EngineId, EngineOutcome, EngineProbe, EngineTask, RawFinding, Result,
};

/// What the mock should do when run.
#[derive(Debug, Clone)]
pub enum MockBehaviour {
    /// Return this outcome.
    Succeed(Box<EngineOutcome>),
    /// Fail with this error.
    Fail(EngineError),
    /// Wait for cancellation, then report it.
    ///
    /// For asserting the kill switch actually reaches an engine (§12.1) rather than
    /// the run merely finishing quickly enough that nobody noticed.
    BlockUntilCancelled,
}

/// An engine that does what it is told.
pub struct MockEngine {
    id: EngineId,
    behaviour: Mutex<MockBehaviour>,
    probe: Mutex<EngineProbe>,
    runs: AtomicUsize,
    /// Tasks it was given, so a test can assert on what the pipeline built.
    ///
    /// The task is the pipeline's *output* as much as the review is the engine's —
    /// asserting only on the outcome would miss a prompt that forgot its diff.
    pub seen_tasks: Mutex<Vec<EngineTask>>,
}

impl MockEngine {
    /// A mock that returns a plausible successful review.
    pub fn new() -> Self {
        Self {
            id: EngineId::Mock,
            behaviour: Mutex::new(MockBehaviour::Succeed(Box::new(Self::default_outcome()))),
            probe: Mutex::new(EngineProbe {
                id: EngineId::Mock,
                installed: true,
                version: Some("mock 1.0.0".to_owned()),
                authenticated: true,
                honours_output_contract: Some(true),
                problems: Vec::new(),
            }),
            runs: AtomicUsize::new(0),
            seen_tasks: Mutex::new(Vec::new()),
        }
    }

    /// A mock that behaves as told.
    pub fn with_behaviour(behaviour: MockBehaviour) -> Self {
        let mock = Self::new();
        if let Ok(mut slot) = mock.behaviour.lock() {
            *slot = behaviour;
        }
        mock
    }

    /// Replace what `probe` reports.
    pub fn set_probe(&self, probe: EngineProbe) {
        if let Ok(mut slot) = self.probe.lock() {
            *slot = probe;
        }
    }

    /// How many times `run` has been called.
    ///
    /// A pipeline that reviewed the same change twice, or skipped one it should have
    /// reviewed, shows up here and nowhere else.
    pub fn run_count(&self) -> usize {
        self.runs.load(Ordering::SeqCst)
    }

    /// The default outcome, exposed so a test can assert on it directly.
    pub fn default_outcome_for_test() -> EngineOutcome {
        Self::default_outcome()
    }

    /// The findings the fixture engine also reports, so tests can assert on a
    /// specific claim rather than on a count.
    fn default_outcome() -> EngineOutcome {
        EngineOutcome {
            findings: vec![
                RawFinding {
                    severity: Severity::High,
                    category: Category::Correctness,
                    confidence: Some(0.9),
                    file: Some("src/pager.rs".to_owned()),
                    line_start: Some(6),
                    line_end: Some(6),
                    title: "Inclusive range walks one past the last index".to_owned(),
                    body: "`start..=(start + per_page)` yields `per_page + 1` indices.".to_owned(),
                    failure_scenario: Some(
                        "items.len() == 10, per_page == 10, page == 0 -> panics".to_owned(),
                    ),
                    suggested_fix: Some("Use `start..(start + per_page)`.".to_owned()),
                },
                RawFinding {
                    severity: Severity::Critical,
                    category: Category::Security,
                    confidence: Some(0.95),
                    file: Some("src/db.rs".to_owned()),
                    line_start: Some(4),
                    line_end: Some(5),
                    title: "User input is interpolated into SQL".to_owned(),
                    body: "`name` is formatted into the query, so it is executed as SQL."
                        .to_owned(),
                    failure_scenario: Some("name = \"' OR '1'='1\" returns every row".to_owned()),
                    suggested_fix: Some("Bind `name` as a parameter.".to_owned()),
                },
            ],
            summary: "Two defects: an off-by-one in the pager and an unparameterised query."
                .to_owned(),
            verdict: Verdict::RequestChanges,
            // No cost, exactly like the fixture engine — which is why every
            // inner-loop day is cost-incomplete by design (ADR 0010).
            // The mock reports counts, which is exactly why RL-409's gap was
            // invisible: every test passed because the fixture is more honest
            // than the real engine it stands in for.
            usage: Usage::measured(1_200, 340),
            transcript: "mock-engine: wrote result.json".to_owned(),
            degraded: None,
            coverage_notes: None,
        }
    }
}

impl Default for MockEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Engine for MockEngine {
    fn id(&self) -> EngineId {
        self.id
    }

    async fn probe(&self) -> Result<EngineProbe> {
        self.probe
            .lock()
            .map(|probe| probe.clone())
            .map_err(|_| EngineError::Failed {
                id: EngineId::Mock,
                detail: "the mock's probe lock was poisoned".to_owned(),
            })
    }

    async fn run(
        &self,
        task: EngineTask,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<EngineOutcome> {
        // Validated even in the mock: a pipeline that built an unrunnable task would
        // otherwise pass every test here and fail only against a real engine.
        task.is_runnable()?;

        self.runs.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut seen) = self.seen_tasks.lock() {
            seen.push(task);
        }

        let behaviour =
            self.behaviour
                .lock()
                .map(|b| b.clone())
                .map_err(|_| EngineError::Failed {
                    id: EngineId::Mock,
                    detail: "the mock's behaviour lock was poisoned".to_owned(),
                })?;

        match behaviour {
            MockBehaviour::Succeed(outcome) => {
                // Even a successful run must observe cancellation: the kill switch
                // cancelling mid-review is the case §12.1 cares about.
                if cancel.is_cancelled() {
                    return Err(EngineError::Cancelled { id: EngineId::Mock });
                }
                Ok(*outcome)
            }
            MockBehaviour::Fail(error) => Err(error),
            MockBehaviour::BlockUntilCancelled => {
                cancel.cancelled().await;
                Err(EngineError::Cancelled { id: EngineId::Mock })
            }
        }
    }
}
