//! Exit codes (RL-1201, SPEC §14).
//!
//! §14 makes this CLI the acceptance-test API, which means the exit code is part
//! of the contract rather than a detail. A script that can only tell "worked" from
//! "did not" has to parse output to decide whether to retry — and parsing output
//! to make a control-flow decision is the thing `--json` exists to avoid.
//!
//! The four non-trivial codes are the ones where **what a caller should do next
//! differs**:
//!
//! - `2` is the caller's mistake. Retrying is pointless; fix the command.
//! - `3` is a budget. Retrying today is pointless; retrying tomorrow works.
//! - `4` is a human. Retrying is pointless until somebody approves.
//! - `1` is everything else, where retrying may well work.
//!
//! Collapsing 3 and 4 into 1 would make a CI job that waits for approval
//! indistinguishable from one that failed, and the usual response to a failure —
//! retry — is exactly wrong for both.

use std::process::ExitCode;

/// What a command's outcome means to whoever called it (SPEC §14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// It worked.
    Ok,
    /// Something went wrong. Retrying may work.
    Error,
    /// The command was wrong. Retrying will not help; fix the invocation.
    Usage,
    /// A budget stopped it (§13.1). Retrying today will not help.
    BlockedByBudget,
    /// It needs a human (§12.4). Retrying will not help until somebody approves.
    AwaitingApproval,
}

impl Exit {
    /// The numeric code §14 specifies.
    pub const fn code(self) -> u8 {
        match self {
            Self::Ok => 0,
            Self::Error => 1,
            Self::Usage => 2,
            Self::BlockedByBudget => 3,
            Self::AwaitingApproval => 4,
        }
    }

    /// Every variant, for the documentation test and `--help`.
    pub const ALL: [Self; 5] = [
        Self::Ok,
        Self::Error,
        Self::Usage,
        Self::BlockedByBudget,
        Self::AwaitingApproval,
    ];

    /// What this means, in the words `--help` uses.
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Ok => "the command succeeded",
            Self::Error => "the command failed; retrying may work",
            Self::Usage => "the command was wrong; fix it rather than retrying",
            Self::BlockedByBudget => {
                "a daily budget stopped this; retrying today will not help (SPEC §13.1)"
            }
            Self::AwaitingApproval => {
                "this needs a human to approve it; retrying will not help (SPEC §12.4)"
            }
        }
    }

    /// Whether retrying the same command could succeed without anything changing.
    ///
    /// This is the question a CI job actually asks, so it is a method rather than
    /// something each caller re-derives from the number.
    pub const fn is_worth_retrying(self) -> bool {
        matches!(self, Self::Error)
    }
}

impl From<Exit> for ExitCode {
    fn from(exit: Exit) -> Self {
        Self::from(exit.code())
    }
}

/// The block `--help` prints, so the contract is visible without the spec.
pub fn help_epilogue() -> String {
    let mut out = String::from("Exit codes:\n");
    for exit in Exit::ALL {
        out.push_str(&format!("  {}  {}\n", exit.code(), exit.describe()));
    }
    out.push_str("\nEvery command accepts --json for machine-readable output.\n");
    out
}
