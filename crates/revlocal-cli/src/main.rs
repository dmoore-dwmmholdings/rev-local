//! The `revlocal` binary — the full headless surface of rev-local.
//!
//! Scaffolded by `RL-101`. The complete command surface is `RL-1201`; commands
//! land here as the work items that need them do.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use revlocal_cli::{control, doctor, exit, hooks, inspect};

mod publish;
mod repo;
mod review;
mod targets;

/// Autonomous local code review for git, GitHub and Subversion.
#[derive(Debug, Parser)]
#[command(
    name = "revlocal",
    version,
    about,
    long_about = None,
    after_help = "Exit codes:\n  0  the command succeeded\n  1  the command failed; retrying may work\n  2  the command was wrong; fix it rather than retrying\n  3  a daily budget stopped this; retrying today will not help (SPEC §13.1)\n  4  this needs a human to approve it; retrying will not help (SPEC §12.4)\n\nEvery command accepts --json for machine-readable output."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

/// Top-level commands (SPEC §14).
#[derive(Debug, Subcommand)]
enum Command {
    /// Inspect and maintain the local database.
    Db {
        #[command(subcommand)]
        command: DbCommand,
    },
    /// Inspect and retry publish actions.
    Publish {
        #[command(subcommand)]
        command: PublishSubcommand,
    },

    /// Inspect publish targets and their capability mapping.
    Targets {
        #[command(subcommand)]
        command: TargetsCommand,
    },

    /// Show what is waiting for a human (SPEC §12.4).
    Approvals {
        #[command(subcommand)]
        command: ApprovalsCommand,
    },

    /// Show a repository's spend against its budget (SPEC §13.1).
    Budget {
        #[command(subcommand)]
        command: BudgetCommand,
    },

    /// Install or remove the git hooks that trigger reviews (SPEC §7.2).
    Hooks {
        #[command(subcommand)]
        command: HooksCommand,
    },

    /// Check prerequisites, engines and publish targets (SPEC §8.4).
    ///
    /// The first thing to run on a fresh install, and the thing to run again when
    /// reviews have quietly stopped.
    Doctor {
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
        /// How many configured repositories use Subversion.
        ///
        /// Temporary: once `repo add` lands this comes from the database. Until
        /// then, a missing `svn` cannot be judged blocking or not without it.
        #[arg(long, value_name = "N", default_value_t = 0)]
        svn_repos: usize,
    },

    /// Stop all reviewing (SPEC §12.1). Reversible; nothing is lost.
    Pause {
        /// The database to record it in.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },

    /// Resume reviewing, releasing any held publish actions.
    Resume {
        /// The database to record it in.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },

    /// Stop everything and reap engine processes.
    ///
    /// Separate from `pause` because it is not the same act: a pause loses
    /// nothing, while reaping takes a running engine's output with it.
    Kill {
        /// Required. There is no soft `kill` — that is `pause`.
        #[arg(long)]
        hard: bool,
        /// The database to record it in.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },

    /// Inspect configured repositories and their polling health.
    Repo {
        #[command(subcommand)]
        command: RepoCommand,
    },

    /// Review one change and print the result.
    Review {
        /// The repository's working copy or mirror.
        #[arg(long, value_name = "PATH")]
        repo: PathBuf,
        /// The revision to review.
        #[arg(long, value_name = "REV")]
        rev: String,
        /// Print the machine-readable report instead of the human one.
        ///
        /// Exactly one JSON document reaches stdout and nothing else — see
        /// `review::run`.
        #[arg(long)]
        json: bool,
    },
}

/// `revlocal db …`.
#[derive(Debug, Subcommand)]
enum DbCommand {
    /// Create or upgrade the schema. Safe to run on an up-to-date database.
    Migrate {
        /// Database file. Created if it does not exist.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,
    },
}

/// `revlocal publish …`.
#[derive(Debug, Subcommand)]
enum PublishSubcommand {
    /// Show each target's status for one run.
    Status {
        /// The run to report on.
        #[arg(long, value_name = "ID")]
        run: i64,
        /// Database file.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,
        /// Print the machine-readable report instead of the human one.
        #[arg(long)]
        json: bool,
    },

    /// Put one target's failed actions for one run back in the queue.
    Replay {
        /// The run to replay.
        #[arg(long, value_name = "ID")]
        run: i64,
        /// The target to retry. Other targets are untouched.
        #[arg(long, value_name = "TARGET")]
        target: String,
        /// Database file.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,
    },
}

/// `revlocal targets …`.
#[derive(Debug, Subcommand)]
enum TargetsCommand {
    /// Show each target's capability mapping, and what did not bind.
    List {
        /// The global config file to read.
        #[arg(long, value_name = "PATH")]
        config: PathBuf,
        /// Print the machine-readable report instead of the human one.
        ///
        /// Exactly one JSON document reaches stdout; warnings and progress go to
        /// stderr, so the output is safe to pipe.
        #[arg(long)]
        json: bool,
    },

    /// Bind one capability to a tool by hand, when resolution could not.
    ///
    /// The override is checked against the tool's schema before it is saved.
    Map {
        /// Which target.
        target: String,
        /// Which capability.
        capability: String,
        /// The tool to bind it to.
        #[arg(long, value_name = "TOOL")]
        tool: String,
        /// An argument template, `key=value`. Repeatable.
        #[arg(long = "arg", value_name = "KEY=VALUE")]
        args: Vec<String>,
        /// The global config file to read.
        #[arg(long, value_name = "PATH")]
        config: PathBuf,
        /// Where overrides are kept. Defaults to beside the config.
        #[arg(long, value_name = "PATH")]
        overrides: Option<PathBuf>,
    },

    /// Dry-run render every mapped capability. Calls nothing.
    Test {
        /// Which target.
        target: String,
        /// The global config file to read.
        #[arg(long, value_name = "PATH")]
        config: PathBuf,
        /// Where overrides are kept. Defaults to beside the config.
        #[arg(long, value_name = "PATH")]
        overrides: Option<PathBuf>,
    },
}

/// `revlocal repo …`.
#[derive(Debug, Subcommand)]
enum RepoCommand {
    /// Show configured repositories and their polling health (SPEC §7.1).
    ///
    /// Reports only. A command that shows you a repository's state must not be
    /// able to change it.
    Show {
        /// One repository by name. Omit for every configured repository.
        #[arg(value_name = "NAME")]
        name: Option<String>,
        /// The database to read.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,
        /// Print the machine-readable report instead of the human one.
        ///
        /// Exactly one JSON document reaches stdout; anything else goes to stderr,
        /// so the output is safe to pipe.
        #[arg(long)]
        json: bool,
    },
}

/// `revlocal approvals …`.
#[derive(Debug, Subcommand)]
enum ApprovalsCommand {
    /// List everything waiting, with the target each would be sent to.
    List {
        /// The database to read.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
}

/// `revlocal budget …`.
#[derive(Debug, Subcommand)]
enum BudgetCommand {
    /// Show today's spend against the configured ceilings.
    Show {
        /// Which repository.
        #[arg(long, value_name = "ID")]
        repo: i64,
        /// The database to read.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
}

/// `revlocal hooks …`.
#[derive(Debug, Subcommand)]
enum HooksCommand {
    /// Add rev-local's hooks. Existing hooks are appended to, never overwritten.
    Install {
        /// The repository's working copy, or the bare mirror itself.
        #[arg(long, value_name = "PATH")]
        repo: PathBuf,
        /// The repository's configured name, sent with each trigger.
        #[arg(long, value_name = "NAME")]
        name: String,
        /// `reference` for a developer's clone, `bare-mirror` to review pushes.
        #[arg(long, default_value = "reference")]
        mode: String,
        /// The loopback port the receiver listens on.
        #[arg(long, default_value_t = revlocal_daemon::trigger_receiver::DEFAULT_TRIGGER_PORT)]
        port: u16,
        /// The environment variable the hook reads its shared secret from.
        ///
        /// A name, never the secret: hooks live in `.git`, which is not committed
        /// but is backed up and copied between machines.
        #[arg(long, default_value = "REVLOCAL_HOOK_SECRET")]
        secret_env: String,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },

    /// Remove rev-local's hooks, leaving anything else byte-identical.
    Uninstall {
        /// The repository's working copy, or the bare mirror itself.
        #[arg(long, value_name = "PATH")]
        repo: PathBuf,
        /// The repository's configured name.
        #[arg(long, value_name = "NAME", default_value = "")]
        name: String,
        /// Which set of hooks to remove.
        #[arg(long, default_value = "reference")]
        mode: String,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
}

/// Parse §7.2's two modes, naming both when the answer is neither.
fn hook_mode(raw: &str) -> Result<revlocal_daemon::hooks::HookMode, CliError> {
    match raw {
        "reference" => Ok(revlocal_daemon::hooks::HookMode::Reference),
        "bare-mirror" => Ok(revlocal_daemon::hooks::HookMode::BareMirror),
        other => {
            eprintln!(
                "revlocal: `{other}` is not a hook mode\n  try: --mode reference (a \
                 developer's clone) or --mode bare-mirror (a mirror developers push to)"
            );
            Err(CliError::Usage)
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let Some(command) = cli.command else {
        println!("revlocal {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    };

    // The daemon runs in-process in both the CLI and the Tauri shell (SPEC §4.2),
    // so the CLI owns the runtime rather than receiving one.
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("revlocal: could not start the async runtime: {e}");
            return exit::Exit::Error.into();
        }
    };

    match runtime.block_on(run(command)) {
        Ok(()) => exit::Exit::Ok.into(),
        // §14's 2: the caller's mistake. Retrying will not help, and the command
        // has already printed what to do instead.
        Err(CliError::Usage) => exit::Exit::Usage.into(),
        // `doctor` printed the failing checks and their remediation; repeating a
        // summary line here would say less than the report already did.
        Err(CliError::Unhealthy) => exit::Exit::Error.into(),
        Err(e) => {
            eprintln!("revlocal: {e}");
            // Every other error is `Error` until a command has a reason to say
            // otherwise. §14's 3 and 4 are claims about *why* something stopped,
            // and a command that cannot yet be stopped that way must not pretend
            // it can.
            exit::Exit::Error.into()
        }
    }
}

/// Anything a command can fail with.
#[derive(Debug, thiserror::Error)]
enum CliError {
    /// The store could not be opened or migrated.
    #[error(transparent)]
    Store(#[from] revlocal_store::StoreError),

    /// A review could not be run.
    #[error(transparent)]
    Review(#[from] review::ReviewCommandError),

    /// Targets could not be listed.
    #[error(transparent)]
    Targets(#[from] targets::TargetsCommandError),

    /// A publish command failed.
    #[error(transparent)]
    Publish(#[from] Box<publish::PublishCommandError>),

    /// A repository could not be shown.
    #[error(transparent)]
    Repo(#[from] repo::RepoCommandError),

    /// A control command failed.
    #[error(transparent)]
    Control(#[from] control::ControlError),

    /// A hooks command failed.
    #[error(transparent)]
    Hooks(#[from] hooks::HooksCommandError),

    /// An inspection failed.
    #[error(transparent)]
    Inspect(#[from] inspect::InspectError),

    /// A report could not be serialised.
    #[error("could not render the report: {0}")]
    Json(#[from] serde_json::Error),

    /// `doctor` found something blocking. Exits 1, having already said what.
    #[error("")]
    Unhealthy,

    /// The invocation was wrong. Exits 2 rather than 1 (§14).
    ///
    /// Carries no message: the command that returns it has already said what was
    /// wrong and what to do instead, in terms only it knows.
    #[error("")]
    Usage,
}

/// Dispatch one command.
async fn run(command: Command) -> Result<(), CliError> {
    match command {
        Command::Db {
            command: DbCommand::Migrate { database },
        } => {
            let pool = revlocal_store::open(&database).await?;
            pool.close().await;
            println!("revlocal: schema is up to date at {}", database.display());
            Ok(())
        }
        Command::Review { repo, rev, json } => {
            review::run(&repo, &rev, json).await?;
            Ok(())
        }
        Command::Publish { command } => {
            match command {
                PublishSubcommand::Status {
                    run,
                    database,
                    json,
                } => publish::status(&database, run, json)
                    .await
                    .map_err(Box::new)?,
                PublishSubcommand::Replay {
                    run,
                    target,
                    database,
                } => publish::replay(&database, run, &target)
                    .await
                    .map_err(Box::new)?,
            }
            Ok(())
        }
        Command::Approvals { command } => match command {
            ApprovalsCommand::List { database, json } => {
                let pool = revlocal_store::open(&database).await?;
                let report = inspect::approvals(&pool).await?;
                pool.close().await;
                let human = report.render_human();
                println!("{}", inspect::render(&report, human, json)?);
                Ok(())
            }
        },

        Command::Budget { command } => match command {
            BudgetCommand::Show {
                repo,
                database,
                json,
            } => {
                let pool = revlocal_store::open(&database).await?;
                let report = inspect::budget(
                    &pool,
                    revlocal_core::RepoId::new(repo),
                    chrono::Utc::now(),
                    // TODO(RL-1201): per-repo settings arrive with `repo add`.
                    // §13.1's defaults until then, which is what a fresh install
                    // actually has.
                    &revlocal_core::BudgetSettings::default(),
                )
                .await?;
                pool.close().await;
                let human = report.render_human();
                println!("{}", inspect::render(&report, human, json)?);
                Ok(())
            }
        },

        Command::Hooks { command } => match command {
            HooksCommand::Install {
                repo,
                name,
                mode,
                port,
                secret_env,
                json,
            } => {
                let mode = hook_mode(&mode)?;
                let report = hooks::install(&repo, &name, mode, port, &secret_env)?;
                println!("{}", hooks::render(&report, json)?);
                Ok(())
            }
            HooksCommand::Uninstall {
                repo,
                name,
                mode,
                json,
            } => {
                let mode = hook_mode(&mode)?;
                let report = hooks::uninstall(&repo, &name, mode)?;
                println!("{}", hooks::render(&report, json)?);
                Ok(())
            }
        },

        Command::Doctor { json, svn_repos } => {
            let report = doctor::gather(svn_repos);
            println!("{}", doctor::render(&report, json)?);
            // §14: a doctor that always exits 0 is a doctor no script can use.
            if report.has_failures() {
                return Err(CliError::Unhealthy);
            }
            Ok(())
        }

        Command::Pause { database, json } => {
            let pool = revlocal_store::open(&database).await?;
            let report = control::pause(&pool, chrono::Utc::now()).await?;
            pool.close().await;
            println!("{}", control::render(&report, json)?);
            Ok(())
        }

        Command::Resume { database, json } => {
            let pool = revlocal_store::open(&database).await?;
            let report = control::resume(&pool, chrono::Utc::now()).await?;
            pool.close().await;
            println!("{}", control::render(&report, json)?);
            Ok(())
        }

        Command::Kill {
            hard,
            database,
            json,
        } => {
            if !hard {
                // §14 spells it `kill --hard`. A bare `kill` is somebody reaching
                // for the reversible thing, and it should point at it rather than
                // guessing which they meant.
                eprintln!(
                    "revlocal: `kill` requires --hard, because it reaps engine \
                     processes and loses their output\n  try: revlocal pause, which \
                     stops reviewing and loses nothing"
                );
                return Err(CliError::Usage);
            }
            let pool = revlocal_store::open(&database).await?;
            let report = control::kill_hard(&pool, chrono::Utc::now()).await?;
            pool.close().await;
            println!("{}", control::render(&report, json)?);
            Ok(())
        }

        Command::Repo { command } => match command {
            RepoCommand::Show {
                name,
                database,
                json,
            } => {
                let pool = revlocal_store::open(&database).await?;
                let out = repo::run(&pool, name.as_deref(), json).await?;
                pool.close().await;
                println!("{out}");
                Ok(())
            }
        },

        Command::Targets { command } => {
            match command {
                TargetsCommand::List { config, json } => targets::run(&config, json).await?,
                TargetsCommand::Map {
                    target,
                    capability,
                    tool,
                    args,
                    config,
                    overrides,
                } => {
                    targets::map(
                        &config,
                        overrides.as_deref(),
                        &target,
                        &capability,
                        &tool,
                        &args,
                    )
                    .await?;
                }
                TargetsCommand::Test {
                    target,
                    config,
                    overrides,
                } => targets::test(&config, overrides.as_deref(), &target).await?,
            }
            Ok(())
        }
    }
}
