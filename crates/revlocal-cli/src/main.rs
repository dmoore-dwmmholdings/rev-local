//! The `revlocal` binary — the full headless surface of rev-local.
//!
//! Scaffolded by `RL-101`. The complete command surface is `RL-1201`; commands
//! land here as the work items that need them do.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod review;
mod targets;

/// Autonomous local code review for git, GitHub and Subversion.
#[derive(Debug, Parser)]
#[command(name = "revlocal", version, about, long_about = None)]
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
    /// Inspect publish targets and their capability mapping.
    Targets {
        #[command(subcommand)]
        command: TargetsCommand,
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
            return ExitCode::FAILURE;
        }
    };

    match runtime.block_on(run(command)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("revlocal: {e}");
            ExitCode::FAILURE
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
