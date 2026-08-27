//! The `revlocal` binary — the full headless surface of rev-local.
//!
//! Scaffolded by `RL-101`. The complete command surface lands in `RL-1201`;
//! for now the binary exists so that `revlocal --version` is answerable and the
//! workspace has a linked artifact to build.

use clap::Parser;

/// Autonomous local code review for git, GitHub and Subversion.
#[derive(Debug, Parser)]
#[command(name = "revlocal", version, about, long_about = None)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
    println!("revlocal {}", env!("CARGO_PKG_VERSION"));
}
