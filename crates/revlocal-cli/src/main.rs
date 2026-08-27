//! The `revlocal` binary — the full headless surface of rev-local.
//!
//! Scaffolded by `RL-101`. The complete command surface is `RL-1201`; commands
//! land here as the work items that need them do.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

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

/// Dispatch one command.
async fn run(command: Command) -> Result<(), revlocal_store::StoreError> {
    match command {
        Command::Db {
            command: DbCommand::Migrate { database },
        } => {
            let pool = revlocal_store::open(&database).await?;
            pool.close().await;
            println!("revlocal: schema is up to date at {}", database.display());
            Ok(())
        }
    }
}
