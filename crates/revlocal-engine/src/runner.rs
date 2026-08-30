//! Concrete [`Engine`] implementations over a CLI (SPEC §8.4).
//!
//! Claude Code and Codex differ only in their invocation template, so there is one
//! runner parameterised by [`InvocationTemplate`] rather than two nearly-identical
//! implementations. Decision D3 makes engine selection per-repository, so both are
//! constructed the same way and held behind `Box<dyn Engine>`.
//!
//! # `probe` never spends tokens
//!
//! `probe` is called at startup, for every configured engine, on every repository's
//! behalf. It runs `version_args` and stops. §8.4's smoke task — which *does* invoke
//! the model — is [`CliEngine::smoke_test`], called by `revlocal doctor` when a user
//! asks. An engine probe that quietly billed someone for starting the app would be a
//! genuinely bad surprise.
//!
//! That is why [`EngineProbe::honours_output_contract`] is an `Option`: `None` means
//! "not smoke-tested", which is not the same as "failed".

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use revlocal_core::EngineKind;
use tokio_util::sync::CancellationToken;

use crate::engine::{
    Engine, EngineError, EngineId, EngineOutcome, EngineProbe, EngineProblem, EngineTask, Result,
};
use crate::ladder::{self, RepairPass};
use crate::supervise::{self, KillReason};
use crate::template::{InvocationTemplate, RenderContext};

/// How long a version probe gets.
///
/// Short on purpose: `probe` runs at startup for every engine, and a CLI that hangs
/// on `--version` must not hold the app's launch.
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// The prompt file written into `out_dir` (SPEC §8.4, `{prompt_file}`).
pub const PROMPT_FILE: &str = "prompt.md";

/// An engine backed by a command-line tool.
pub struct CliEngine {
    id: EngineId,
    template: InvocationTemplate,
    /// Where to look for the binary's environment. Injected so tests can supply one
    /// without mutating the process's own, which is a global and racy under a
    /// parallel test runner.
    parent_env: BTreeMap<String, String>,
}

impl CliEngine {
    /// A runner for `id` using `template`, inheriting the real environment.
    pub fn new(id: EngineId, template: InvocationTemplate) -> Self {
        Self {
            id,
            template,
            parent_env: std::env::vars().collect(),
        }
    }

    /// Claude Code with SPEC §8.4's default template.
    pub fn claude() -> Self {
        Self::new(EngineKind::Claude, InvocationTemplate::claude())
    }

    /// Codex with SPEC §8.4's default template.
    pub fn codex() -> Self {
        Self::new(EngineKind::Codex, InvocationTemplate::codex())
    }

    /// Use a specific parent environment rather than the process's.
    pub fn with_parent_env(mut self, env: BTreeMap<String, String>) -> Self {
        self.parent_env = env;
        self
    }

    /// This runner's template.
    pub const fn template(&self) -> &InvocationTemplate {
        &self.template
    }

    /// The environment this engine's child processes receive (§8.5).
    fn child_env(&self) -> BTreeMap<String, String> {
        supervise::filtered_env(
            self.parent_env
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str())),
            &self.template.pass_env,
        )
    }

    /// Credentials withheld from this engine that it may have needed (§8.5).
    ///
    /// Surfaced on the probe so `revlocal doctor` can say why an engine that looks
    /// installed reports itself unauthenticated.
    fn withheld_credentials(&self) -> Vec<String> {
        supervise::withheld_auth_variables(
            self.parent_env
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str())),
            &self.template.pass_env,
        )
    }

    /// §8.4's smoke task: a tiny review, checked for a `result.json`.
    ///
    /// **This invokes the model and costs tokens.** Separate from [`Engine::probe`]
    /// for that reason: probing is something the app does on its own, a smoke test
    /// is something a user asks for.
    ///
    /// `fixture_dir` is a tiny tree to review; `out_dir` must be writable and is
    /// where `result.json` is looked for.
    pub async fn smoke_test(&self, fixture_dir: &Path, out_dir: &Path) -> Result<bool> {
        let task = EngineTask {
            cwd: fixture_dir.to_path_buf(),
            out_dir: out_dir.to_path_buf(),
            prompt: "Reply with the smallest valid result.json describing no findings.".to_owned(),
            attachments: Vec::new(),
            timeout: PROBE_TIMEOUT,
            depth: revlocal_core::Depth::Summary,
        };

        match self.run(task, CancellationToken::new()).await {
            Ok(_) => Ok(true),
            // A smoke test that fails is an answer, not an error: `doctor` reports
            // it alongside every other engine rather than stopping at the first.
            Err(EngineError::NotInstalled { .. }) => Ok(false),
            Err(EngineError::OutputUnparseable { .. } | EngineError::Timeout { .. }) => Ok(false),
            Err(other) => Err(other),
        }
    }

    /// Run the engine, optionally allowing one repair (§8.2 rung c).
    ///
    /// Whether to spend tokens on a repair belongs to the budget guard, not here, so
    /// it is a parameter rather than a policy.
    pub async fn run_with_repair(
        &self,
        task: EngineTask,
        cancel: CancellationToken,
        repair: Option<&dyn RepairPass>,
    ) -> Result<EngineOutcome> {
        task.is_runnable()?;

        std::fs::create_dir_all(&task.out_dir).map_err(|e| EngineError::Failed {
            id: self.id,
            detail: format!("creating {}: {e}", task.out_dir.display()),
        })?;

        // Written even when the template passes the prompt inline: §8.4's
        // `{prompt_file}` must resolve to something real whichever form is used, and
        // a copy on disk is what makes a failed run reproducible by hand.
        let prompt_file = task.out_dir.join(PROMPT_FILE);
        std::fs::write(&prompt_file, &task.prompt).map_err(|e| EngineError::Failed {
            id: self.id,
            detail: format!("writing {}: {e}", prompt_file.display()),
        })?;

        let invocation = self
            .template
            .render(
                self.id.as_str(),
                &RenderContext {
                    cwd: task.cwd.clone(),
                    out_dir: task.out_dir.clone(),
                    prompt_file,
                    prompt: task.prompt.clone(),
                    timeout: task.timeout,
                },
            )
            .map_err(|e| EngineError::InvalidTask {
                detail: e.to_string(),
            })?;

        let supervised = supervise::supervise(
            self.id,
            &invocation,
            &task.cwd,
            &self.child_env(),
            task.timeout,
            &cancel,
        )
        .await?;

        match supervised.killed {
            Some(KillReason::Cancelled) => return Err(EngineError::Cancelled { id: self.id }),
            Some(KillReason::Timeout) => {
                // §8.5: the transcript is retained. It is the only record of what the
                // engine said before it hung, and the first thing anyone debugging
                // this will want.
                tracing::warn!(
                    engine = %self.id,
                    timeout_secs = task.timeout.as_secs(),
                    transcript_bytes = supervised.stdout.len(),
                    "engine timed out; transcript retained"
                );
                return Err(EngineError::Timeout {
                    id: self.id,
                    timeout: task.timeout,
                });
            }
            None => {}
        }

        let climbed = ladder::resolve(self.id, &task.out_dir, &supervised.stdout, repair).await?;

        let mut outcome = climbed.outcome;

        // What the run actually spent (RL-409, SPEC §8.1). §8.3's `result.json`
        // schema carries no usage field, so the counts are not in what the ladder
        // parsed — they are in the CLI's own envelope, and reading them is
        // per-engine because the two CLIs report them in opposite directions
        // (ADR 0033).
        //
        // Added rather than assigned: `climbed` may already carry a repair's
        // tokens, and replacing would charge the run for the repair alone.
        //
        // A failure to read is a warning, not an error. The review succeeded and
        // is worth more than its accounting — but `Usage::default()` leaves
        // `tokens_known` false, so the run is recorded as *unmeasured* rather than
        // as free, which is the distinction ADR 0010 exists for.
        match crate::usage::for_engine(self.id, &supervised.stdout) {
            Ok(usage) => outcome.usage.add(&usage),
            Err(error) => tracing::warn!(
                %error,
                engine = self.id.as_str(),
                "could not read token usage from the engine's output; this run is \
                 recorded as unmeasured rather than free"
            ),
        }

        // The transcript is attached here rather than in the ladder, because the
        // ladder does not run the process and has nothing else to say about it.
        outcome.transcript = supervised.stdout;

        Ok(outcome)
    }
}

#[async_trait::async_trait]
impl Engine for CliEngine {
    fn id(&self) -> EngineId {
        self.id
    }

    async fn probe(&self) -> Result<EngineProbe> {
        let invocation = crate::template::Invocation {
            program: self.template.bin.clone(),
            args: self.template.version_args.clone(),
            stdin: None,
        };

        let supervised = supervise::supervise(
            self.id,
            &invocation,
            Path::new("."),
            &self.child_env(),
            PROBE_TIMEOUT,
            &CancellationToken::new(),
        )
        .await;

        let mut problems = Vec::new();

        let (installed, version) = match supervised {
            Ok(output) if output.exit_code == Some(0) => (
                true,
                Some(
                    output
                        .stdout
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .trim()
                        .to_owned(),
                ),
            ),
            Ok(output) => {
                // Present but unhappy — usually a flag this build does not know.
                problems.push(EngineProblem {
                    problem: format!(
                        "`{} {}` exited with {:?}",
                        self.template.bin,
                        self.template.version_args.join(" "),
                        output.exit_code
                    ),
                    remediation: format!(
                        "check `engines.{}.version_args` in config.toml matches what \
                         this version of the CLI accepts",
                        self.id
                    ),
                });
                (true, None)
            }
            // Not installed is a REPORT, not an error. `doctor` shows every engine's
            // state at once, and failing here would stop at the first missing one.
            Err(EngineError::NotInstalled { remediation, .. }) => {
                problems.push(EngineProblem {
                    problem: format!("`{}` is not on PATH", self.template.bin),
                    remediation,
                });
                (false, None)
            }
            Err(other) => {
                problems.push(EngineProblem {
                    problem: other.to_string(),
                    remediation: format!(
                        "check `engines.{}` in config.toml, then run `revlocal doctor` again",
                        self.id
                    ),
                });
                (false, None)
            }
        };

        // A credential the user set and rev-local withheld is the likeliest reason a
        // present, working CLI reports itself unauthenticated — and the least
        // guessable, because nothing on screen connects the two.
        for withheld in self.withheld_credentials() {
            problems.push(EngineProblem {
                problem: format!("`{withheld}` is set but is withheld from review engines"),
                remediation: supervise::withheld_auth_remediation(&withheld, self.id.as_str()),
            });
        }

        Ok(EngineProbe {
            id: self.id,
            installed,
            version,
            // A CLI's login state cannot be read without invoking it, and invoking
            // it costs tokens. §8.4's smoke task is what answers this, and it is
            // deliberately not run here.
            authenticated: installed,
            honours_output_contract: None,
            problems,
        })
    }

    async fn run(&self, task: EngineTask, cancel: CancellationToken) -> Result<EngineOutcome> {
        self.run_with_repair(task, cancel, None).await
    }
}
