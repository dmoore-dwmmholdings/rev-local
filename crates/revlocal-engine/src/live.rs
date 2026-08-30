//! Deciding whether a live-engine test can run at all (RL-1203, SPEC §16.1).
//!
//! The acceptance suite for real engines invokes `claude` and `codex` against the
//! planted-bug fixture. That spends real credits, so the tests behind it are gated
//! twice — a cargo feature *and* `#[ignore]` — and this module is the part that
//! runs everywhere.
//!
//! # Why the skip decision lives here rather than in the test
//!
//! §16.1's fourth criterion is that the suite "skips cleanly with a clear message
//! when a binary is absent". That is a real behaviour with a real failure mode: a
//! suite that skips *silently* is indistinguishable from one that ran and found
//! nothing, which is the §18 failure this project keeps finding.
//!
//! Putting the decision in a plain function means it can be tested on a machine
//! that deliberately never runs the engines — including this one. The tests below
//! prove the skip path without spending anything.

use std::path::PathBuf;

use revlocal_core::EngineKind;

/// Whether a live-engine test can run, and what to say when it cannot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
    /// The binary is on PATH at this location.
    Ready {
        /// Where it was found, so a report can say which one it used.
        binary: PathBuf,
    },

    /// It is not installed, so the test skips.
    ///
    /// Carries the sentence to print. Constructed here rather than at the call
    /// site so every skip reads the same and none can be a bare `return`.
    Skip {
        /// Why, in a form a person reading CI output can act on.
        reason: String,
    },
}

impl Readiness {
    /// Whether the engine can actually be invoked.
    pub const fn is_ready(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }

    /// The line a skipping test prints.
    ///
    /// Always says **nothing was verified**. A skip that only says "skipped" reads
    /// as a pass in a wall of green, and the whole point of this suite is that it
    /// is the only thing checking a real engine.
    pub fn skip_line(&self, test: &str) -> Option<String> {
        match self {
            Self::Ready { .. } => None,
            Self::Skip { reason } => Some(format!("SKIPPED ({reason}, nothing verified): {test}")),
        }
    }
}

/// Look for an engine's binary without running it.
///
/// Deliberately does not invoke anything, not even `--version`: this is asked
/// before a test that costs money, and the answer must not itself cost anything.
pub fn readiness(engine: EngineKind) -> Readiness {
    let program = match engine {
        EngineKind::Claude => "claude",
        EngineKind::Codex => "codex",
        // The fixture engine is not a live engine. Saying so is better than
        // silently reporting it ready and having a "live" test pass against a
        // mock, which would make the whole suite meaningless.
        EngineKind::Mock => {
            return Readiness::Skip {
                reason: "the mock engine is not a live engine".to_owned(),
            }
        }
    };

    match which(program) {
        Some(binary) => Readiness::Ready { binary },
        None => Readiness::Skip {
            reason: format!("`{program}` is not on PATH"),
        },
    }
}

/// The first `program` on PATH, if any.
///
/// Hand-rolled rather than pulled in as a dependency: it is fifteen lines, and a
/// crate for it would be a supply-chain entry for something this small.
fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for name in candidates(program) {
            let candidate = dir.join(&name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// The filenames a program might have on this platform.
fn candidates(program: &str) -> Vec<String> {
    if cfg!(windows) {
        // A CLI installed by npm on Windows is a `.cmd` shim, which is exactly
        // the shape that made the Job Object necessary (SPEC §8.5).
        vec![
            format!("{program}.exe"),
            format!("{program}.cmd"),
            format!("{program}.bat"),
            program.to_owned(),
        ]
    } else {
        vec![program.to_owned()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_a_missing_binary_skips_with_a_reason() {
        // The behaviour §16.1 asks for, tested without spending anything.
        let skip = Readiness::Skip {
            reason: "`claude` is not on PATH".to_owned(),
        };

        let line = skip
            .skip_line("live_claude_finds_the_planted_sql_injection")
            .unwrap_or_default();

        assert!(line.contains("not on PATH"), "{line}");
        assert!(
            line.contains("nothing verified"),
            "a skip that does not say it verified nothing reads as a pass: {line}"
        );
        assert!(
            line.contains("live_claude_finds"),
            "and name the test: {line}"
        );
    }

    #[test]
    fn live_a_ready_engine_has_no_skip_line() {
        let ready = Readiness::Ready {
            binary: PathBuf::from("/usr/local/bin/claude"),
        };
        assert!(ready.is_ready());
        assert_eq!(ready.skip_line("anything"), None);
    }

    #[test]
    fn live_the_mock_is_never_treated_as_a_live_engine() {
        // A "live" suite that passed against the fixture engine would be worse
        // than no suite: it would report that a real engine works.
        let readiness = readiness(EngineKind::Mock);
        assert!(!readiness.is_ready());
        assert!(
            readiness
                .skip_line("x")
                .unwrap_or_default()
                .contains("not a live engine"),
            "{readiness:?}"
        );
    }

    #[test]
    fn live_looking_for_an_engine_does_not_run_anything() {
        // `readiness` is asked before a test that costs money. If it invoked the
        // binary to check, asking would itself cost. A program that certainly
        // does not exist must resolve to Skip without erroring.
        let absent = which("revlocal-a-program-that-does-not-exist");
        assert_eq!(absent, None);
    }
}
