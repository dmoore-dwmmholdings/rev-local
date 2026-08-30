//! The `revlocal` binary — the full headless surface of rev-local.
//!
//! Scaffolded by `RL-101`. The complete command surface is `RL-1201`; commands
//! land here as the work items that need them do.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use revlocal_cli::{
    backfill, control, decide, doctor, exit, export, hooks, inspect, repo, watch, webhook,
};

mod publish;
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

    /// Review history, behind live work (SPEC §7.4).
    Backfill {
        /// Which repository, by name.
        #[arg(long, value_name = "NAME")]
        repo: String,
        /// Where to start: a ref this repository knows.
        #[arg(long, value_name = "REF")]
        since: String,
        /// How many changes to take.
        #[arg(long, value_name = "N")]
        limit: Option<usize>,
        /// Enumerate without enqueueing anything.
        ///
        /// The default today, because execution needs the run registry. Kept as a
        /// flag so the invocation does not change when it starts doing more.
        #[arg(long)]
        dry_run: bool,
        /// The database to read.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },

    /// Run the daemon in the foreground (SPEC §4.2, §7).
    Watch {
        /// Do one tick and stop, rather than looping.
        ///
        /// What a test and a cron job both want, and what makes the loop's
        /// decision observable without waiting for an interval.
        #[arg(long)]
        once: bool,
        /// Seconds between ticks when looping.
        #[arg(long, default_value_t = 30)]
        interval: u64,
        /// The database to use.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },

    /// Inspect runs (SPEC §14).
    Runs {
        #[command(subcommand)]
        command: RunsCommand,
    },

    /// Inspect findings across runs (SPEC §14).
    Findings {
        #[command(subcommand)]
        command: FindingsCommand,
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

    /// Control the GitHub webhook listener and its tunnel (SPEC §7.3).
    Webhook {
        #[command(subcommand)]
        command: WebhookCommand,
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
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },

    /// Write the review record as one JSON document on stdout.
    ///
    /// Repositories, runs and findings. Not a backup — the store is one SQLite
    /// file, so `cp` is a better backup than any format could be — and not a
    /// debug dump, which `runs show` and the audit log already cover.
    Export {
        /// The output format. `json` is the only one §14 names.
        #[arg(long, value_name = "FORMAT", default_value = "json")]
        format: String,
        /// Database file.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,
        /// Accepted and ignored: the document *is* the output.
        ///
        /// Present so `--json` is true of every command, as `revlocal --help`
        /// claims. A flag that silently did something different would be worse
        /// than one that says it changes nothing.
        #[arg(long)]
        json: bool,
    },

    /// Delete runs that finished before a date, with their findings.
    ///
    /// §5.1 keeps run and finding rows forever in v1; this is the manual escape
    /// hatch. Transcript files go with their runs, because the row is the only
    /// thing that knows where the file is.
    Vacuum {
        /// Delete runs that finished before this day, `YYYY-MM-DD`.
        #[arg(long, value_name = "DATE")]
        before: String,
        /// Database file.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
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

    /// Put one failed action back in the queue.
    ///
    /// One action, not one target — which is the difference from `replay`. When a
    /// run produced eight comments and one was rejected for a bad path, replaying
    /// the target re-posts the seven that already landed.
    Retry {
        /// The action to retry, from `revlocal publish status`.
        #[arg(value_name = "ACTION_ID")]
        action_id: i64,
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
        /// Print the machine-readable report instead of the human one.
        #[arg(long)]
        json: bool,
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
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
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
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
}

/// `revlocal repo …`.
#[derive(Debug, Subcommand)]
enum RepoCommand {
    /// Register a repository so rev-local reviews it.
    Add {
        /// A working copy on disk, or a remote URL.
        #[arg(value_name = "PATH|URL")]
        path_or_url: String,
        /// `git`, `github` or `svn`.
        #[arg(long)]
        kind: String,
        /// What to call it. Derived from the path when omitted.
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        /// Which engine reviews it (decision D3: per repo, not global).
        #[arg(long, default_value = "claude")]
        engine: String,
        /// How much it may do unattended.
        ///
        /// Defaults to `dry_run`: a repository added a moment ago has never been
        /// reviewed and nobody has seen its findings.
        #[arg(long, default_value = "dry_run")]
        autonomy: String,
        /// The database to write to.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },

    /// List every configured repository.
    List {
        /// The database to read.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },

    /// Forget a repository. Its runs and findings go with it.
    Remove {
        /// Which repository.
        #[arg(value_name = "NAME")]
        name: String,
        /// The database to write to.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },

    /// Change settings: `engine=`, `autonomy=`, `enabled=`, `default_branch=`.
    Set {
        /// Which repository.
        #[arg(value_name = "NAME")]
        name: String,
        /// One or more `key=value` pairs.
        #[arg(value_name = "KEY=VALUE", required = true)]
        pairs: Vec<String>,
        /// The database to write to.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },

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

/// `revlocal runs …`.
#[derive(Debug, Subcommand)]
enum RunsCommand {
    /// Queue another attempt at the same change.
    ///
    /// The old run is left as it is. A run is the record of one attempt, and
    /// rewriting it would lose the evidence of what went wrong the first time.
    Retry {
        /// The run to retry.
        #[arg(value_name = "RUN_ID")]
        run_id: i64,
        /// The database to use.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },

    /// List recent runs, newest first.
    List {
        /// Narrow to one repository.
        #[arg(long, value_name = "ID")]
        repo: Option<i64>,
        /// Narrow to one status.
        #[arg(long, value_name = "STATUS")]
        status: Option<String>,
        /// How many to show.
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// The database to read.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },

    /// Show one run in full, with its findings.
    Show {
        /// Which run.
        #[arg(value_name = "RUN_ID")]
        run_id: i64,
        /// The database to read.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
}

/// `revlocal findings …`.
#[derive(Debug, Subcommand)]
enum FindingsCommand {
    /// Stop proposing a finding, by its fingerprint.
    Suppress {
        /// The fingerprint to suppress, from `revlocal findings list`.
        #[arg(value_name = "FINGERPRINT")]
        fingerprint: String,
        /// Scope it to one repository. Omitted suppresses it everywhere.
        ///
        /// Global is the wider choice, not the safer one, so it is what you get
        /// by asking rather than by leaving something out — and the report always
        /// says which it did.
        #[arg(long, value_name = "NAME")]
        repo: Option<String>,
        /// The database to use.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },

    /// List findings from recent runs.
    List {
        /// Narrow to one repository.
        #[arg(long, value_name = "ID")]
        repo: Option<i64>,
        /// Show only this severity and worse.
        #[arg(long, value_name = "SEVERITY")]
        severity: Option<String>,
        /// How many runs to read.
        #[arg(long, default_value_t = 20)]
        limit: u32,
        /// The database to read.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,
        /// Machine-readable output.
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

    /// Approve one action, every action in a run, or everything waiting.
    Approve {
        /// The action to approve.
        ///
        /// Exactly one of this, `--run` or `--all`. Approving is the one
        /// irreversible half of §12.4, so the scope is stated rather than defaulted.
        #[arg(value_name = "ID", group = "scope")]
        id: Option<i64>,
        /// Approve every waiting action for one run.
        #[arg(long, value_name = "RUN", group = "scope")]
        run: Option<i64>,
        /// Approve everything waiting.
        #[arg(long, group = "scope")]
        all: bool,
        /// The database to use.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },

    /// Reject one action, optionally suppressing its finding.
    Reject {
        /// The action to reject.
        #[arg(value_name = "ID")]
        id: i64,
        /// Also suppress the finding, so it is not proposed again.
        #[arg(long)]
        suppress: bool,
        /// The database to use.
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
    /// Clear today's spend so work can resume before midnight.
    ///
    /// The allowance accounting only: runs, findings and the audit log are
    /// untouched, so the spend is still explainable afterwards.
    Reset {
        /// Which repository, by name.
        #[arg(long, value_name = "NAME")]
        repo: String,
        /// The database to use.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },

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
/// `revlocal webhook start | stop | status` (SPEC §7.3, §14).
#[derive(Debug, Subcommand)]
enum WebhookCommand {
    /// Enable the listener and choose a tunnel.
    Start {
        /// Which tunnel exposes the listener: cloudflared, ngrok or manual.
        ///
        /// Omitted keeps whatever was chosen last, so re-starting after a stop
        /// does not silently lose the choice.
        #[arg(long, value_name = "PROVIDER")]
        tunnel: Option<String>,
        /// The global config file to read.
        #[arg(long, value_name = "PATH")]
        config: PathBuf,
        /// The database to use.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Disable the listener, keeping the tunnel choice.
    Stop {
        /// The global config file to read.
        #[arg(long, value_name = "PATH")]
        config: PathBuf,
        /// The database to use.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
    /// Report both switches, what is on the port, and what is still missing.
    Status {
        /// Ask what a provider would look like, without choosing it.
        #[arg(long, value_name = "PROVIDER")]
        tunnel: Option<String>,
        /// The global config file to read.
        #[arg(long, value_name = "PATH")]
        config: PathBuf,
        /// The database to use.
        #[arg(long, value_name = "PATH")]
        database: PathBuf,
        /// Machine-readable output.
        #[arg(long)]
        json: bool,
    },
}

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

    /// A watch tick failed.
    #[error(transparent)]
    Watch(#[from] watch::WatchError),

    /// A backfill could not be planned.
    #[error(transparent)]
    Backfill(#[from] backfill::BackfillError),

    /// A webhook command failed.
    #[error(transparent)]
    Webhook(#[from] webhook::WebhookError),

    /// An export could not be produced.
    #[error(transparent)]
    Export(#[from] export::ExportError),

    /// A decision could not be recorded.
    #[error(transparent)]
    Decide(#[from] decide::DecideError),

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
        Command::Db { command } => match command {
            DbCommand::Migrate { database, json } => {
                let pool = revlocal_store::open(&database).await?;
                pool.close().await;
                let path = database.display().to_string();
                if json {
                    println!(
                        "{}",
                        serde_json::json!({"database": path, "migrated": true})
                    );
                } else {
                    println!("revlocal: schema is up to date at {path}");
                }
                Ok(())
            }

            DbCommand::Export {
                format,
                database,
                json,
            } => {
                let _ = json;
                let pool = revlocal_store::open(&database).await?;
                let document = export::export(&pool, &format, chrono::Utc::now()).await;
                pool.close().await;
                let document = document?;
                // Exactly one document on stdout; the summary goes to stderr, so
                // the output stays safe to pipe.
                eprintln!("revlocal: {}", document.summary_line());
                println!("{}", export::render(&document)?);
                Ok(())
            }

            DbCommand::Vacuum {
                before,
                database,
                json,
            } => {
                let pool = revlocal_store::open(&database).await?;
                let report = decide::vacuum(&pool, &before).await;
                pool.close().await;
                let report = report?;
                println!("{}", decide::render(&report, report.render_human(), json)?);
                Ok(())
            }
        },
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
                PublishSubcommand::Retry {
                    action_id,
                    database,
                    json,
                } => {
                    let pool = revlocal_store::open(&database).await?;
                    let report = decide::retry_action(&pool, action_id).await;
                    pool.close().await;
                    let report = report?;
                    println!("{}", decide::render(&report, report.render_human(), json)?);
                }

                PublishSubcommand::Replay {
                    run,
                    target,
                    database,
                    json,
                } => publish::replay(&database, run, &target, json)
                    .await
                    .map_err(Box::new)?,
            }
            Ok(())
        }
        Command::Backfill {
            repo,
            since,
            limit,
            dry_run,
            database,
            json,
        } => {
            let _ = dry_run;
            let pool = revlocal_store::open(&database).await?;
            let report =
                backfill::plan_backfill(&pool, &repo, &since, limit, chrono::Utc::now()).await?;
            pool.close().await;
            println!("{}", backfill::render(&report, json)?);
            Ok(())
        }

        Command::Watch {
            once,
            interval,
            database,
            json,
        } => {
            let pool = revlocal_store::open(&database).await?;

            // Ctrl-C ends the loop rather than killing the process: §4.2 runs the
            // daemon in-process, and a half-written run row is worse than a
            // slightly later exit. RL-501's recovery exists for the case where
            // that promise cannot be kept.
            let stop = tokio_util::sync::CancellationToken::new();
            let signal = stop.clone();
            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    eprintln!("revlocal: stopping after this tick");
                    signal.cancel();
                }
            });

            loop {
                let report = watch::tick(&pool, chrono::Utc::now()).await?;
                println!("{}", watch::render(&report, json)?);

                if once || stop.is_cancelled() {
                    break;
                }
                tokio::select! {
                    () = tokio::time::sleep(std::time::Duration::from_secs(interval)) => {}
                    () = stop.cancelled() => break,
                }
            }

            pool.close().await;
            Ok(())
        }

        Command::Runs { command } => match command {
            RunsCommand::Retry {
                run_id,
                database,
                json,
            } => {
                let pool = revlocal_store::open(&database).await?;
                let report = decide::retry_run(&pool, run_id, chrono::Utc::now()).await;
                pool.close().await;
                let report = report?;
                println!("{}", decide::render(&report, report.render_human(), json)?);
                Ok(())
            }

            RunsCommand::List {
                repo,
                status,
                limit,
                database,
                json,
            } => {
                let status = match status.as_deref() {
                    None => None,
                    Some(raw) => Some(inspect::parse_status(raw)?),
                };
                let pool = revlocal_store::open(&database).await?;
                let report =
                    inspect::runs(&pool, repo.map(revlocal_core::RepoId::new), status, limit)
                        .await?;
                pool.close().await;
                let human = report.render_human();
                println!("{}", inspect::render(&report, human, json)?);
                Ok(())
            }

            RunsCommand::Show {
                run_id,
                database,
                json,
            } => {
                let pool = revlocal_store::open(&database).await?;
                let report = inspect::run_detail(&pool, revlocal_core::RunId::new(run_id)).await?;
                pool.close().await;
                let human = report.render_human();
                println!("{}", inspect::render(&report, human, json)?);
                Ok(())
            }
        },

        Command::Findings { command } => match command {
            FindingsCommand::Suppress {
                fingerprint,
                repo,
                database,
                json,
            } => {
                let pool = revlocal_store::open(&database).await?;
                let report =
                    decide::suppress(&pool, &fingerprint, repo.as_deref(), chrono::Utc::now())
                        .await;
                pool.close().await;
                let report = report?;
                println!("{}", decide::render(&report, report.render_human(), json)?);
                Ok(())
            }

            FindingsCommand::List {
                repo,
                severity,
                limit,
                database,
                json,
            } => {
                let severity = match severity.as_deref() {
                    None => None,
                    Some(raw) => Some(inspect::parse_severity(raw)?),
                };
                let pool = revlocal_store::open(&database).await?;
                let report =
                    inspect::findings(&pool, repo.map(revlocal_core::RepoId::new), severity, limit)
                        .await?;
                pool.close().await;
                let human = report.render_human();
                println!("{}", inspect::render(&report, human, json)?);
                Ok(())
            }
        },

        Command::Approvals { command } => match command {
            ApprovalsCommand::Approve {
                id,
                run,
                database,
                json,
                ..
            } => {
                // `--all` is the remaining case: clap's group has already refused
                // any two of the three together.
                let scope = match (id, run) {
                    (Some(id), _) => decide::Scope::One(id),
                    (_, Some(run)) => decide::Scope::Run(run),
                    _ => decide::Scope::All,
                };
                let pool = revlocal_store::open(&database).await?;
                let report = decide::approve(&pool, scope).await;
                pool.close().await;
                let report = report?;
                println!("{}", decide::render(&report, report.render_human(), json)?);
                Ok(())
            }

            ApprovalsCommand::Reject {
                id,
                suppress,
                database,
                json,
            } => {
                let pool = revlocal_store::open(&database).await?;
                let report = decide::reject(&pool, id, suppress, chrono::Utc::now()).await;
                pool.close().await;
                let report = report?;
                println!("{}", decide::render(&report, report.render_human(), json)?);
                Ok(())
            }

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
            BudgetCommand::Reset {
                repo,
                database,
                json,
            } => {
                let pool = revlocal_store::open(&database).await?;
                let report = decide::reset_budget(&pool, &repo, chrono::Utc::now()).await;
                pool.close().await;
                let report = report?;
                println!("{}", decide::render(&report, report.render_human(), json)?);
                Ok(())
            }

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

        Command::Webhook { command } => {
            let (config, database, json) = match &command {
                WebhookCommand::Start {
                    config,
                    database,
                    json,
                    ..
                }
                | WebhookCommand::Stop {
                    config,
                    database,
                    json,
                }
                | WebhookCommand::Status {
                    config,
                    database,
                    json,
                    ..
                } => (config.clone(), database.clone(), *json),
            };
            let pool = revlocal_store::open(&database).await?;
            let now = chrono::Utc::now();
            let report = match &command {
                WebhookCommand::Start { tunnel, .. } => {
                    webhook::start(&pool, &config, tunnel.as_deref(), now).await
                }
                WebhookCommand::Stop { .. } => webhook::stop(&pool, &config, now).await,
                WebhookCommand::Status { tunnel, .. } => {
                    webhook::status(&pool, &config, tunnel.as_deref(), now).await
                }
            };
            pool.close().await;
            println!("{}", webhook::render(&report?, json)?);
            Ok(())
        }

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
            RepoCommand::Add {
                path_or_url,
                kind,
                name,
                engine,
                autonomy,
                database,
                json,
            } => {
                let pool = revlocal_store::open(&database).await?;
                let report = repo::add(
                    &pool,
                    &path_or_url,
                    &kind,
                    name.as_deref(),
                    &engine,
                    &autonomy,
                    chrono::Utc::now(),
                )
                .await?;
                pool.close().await;
                println!("{}", repo::render_write(&report, json)?);
                Ok(())
            }

            RepoCommand::List { database, json } => {
                let pool = revlocal_store::open(&database).await?;
                let out = repo::run(&pool, None, json).await?;
                pool.close().await;
                println!("{out}");
                Ok(())
            }

            RepoCommand::Remove {
                name,
                database,
                json,
            } => {
                let pool = revlocal_store::open(&database).await?;
                let report = repo::remove(&pool, &name).await?;
                pool.close().await;
                println!("{}", repo::render_write(&report, json)?);
                Ok(())
            }

            RepoCommand::Set {
                name,
                pairs,
                database,
                json,
            } => {
                let pool = revlocal_store::open(&database).await?;
                let report = repo::set(&pool, &name, &pairs, chrono::Utc::now()).await?;
                pool.close().await;
                println!("{}", repo::render_write(&report, json)?);
                Ok(())
            }

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
                    json,
                } => {
                    targets::map(
                        &config,
                        overrides.as_deref(),
                        &target,
                        &capability,
                        &tool,
                        &args,
                        json,
                    )
                    .await?;
                }
                TargetsCommand::Test {
                    target,
                    config,
                    overrides,
                    json,
                } => targets::test(&config, overrides.as_deref(), &target, json).await?,
            }
            Ok(())
        }
    }
}
