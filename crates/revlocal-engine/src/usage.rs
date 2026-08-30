//! Whether an engine's token usage can be measured at all (RL-409, SPEC §8.1).
//!
//! # The gap this names
//!
//! §8.1 gives `EngineOutcome` a `usage` field and decision D10 puts per-repo token
//! budgets on top of it. But §8.3's `result.json` schema carries no usage field, so
//! a runner reading `result.json` has no counts to report. Counts come from the
//! CLI's *own* output instead, and that output is engine-specific: Claude Code's
//! `--output-format json` and Codex's `exec --json` are different shapes.
//!
//! Until an extractor exists for an engine, its runs report **unknown** tokens.
//! `Usage::default()` says so — `tokens_known` defaults to `false` — so nothing is
//! recorded as free that merely went uncounted. That is the half of RL-409 that is
//! already true.
//!
//! # What this module adds
//!
//! The other half: saying so *before* somebody sets a budget and waits for it to
//! work. A budget against an engine whose usage nobody can read is advisory, and an
//! operator who does not know that will believe a ceiling is protecting them.
//!
//! An exhaustive `match` on [`EngineKind`], deliberately. Adding an engine forces a
//! decision here rather than defaulting to "supported", which is the direction that
//! would quietly overstate what is known.

use revlocal_core::EngineKind;

/// Whether rev-local can read token counts out of an engine's output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageSupport {
    /// Counts are read from the engine's output and are trustworthy.
    Measured,

    /// No extractor exists, so every run reports unknown tokens.
    ///
    /// Carries where the counts *would* come from, because "not supported" and
    /// "not supported, and here is the flag that would give it to us" are
    /// different amounts of help to somebody deciding whether to wait for it.
    Unmeasured {
        /// The engine output that carries the counts, once somebody reads it.
        source: &'static str,
    },
}

impl UsageSupport {
    /// Whether a token budget can actually be enforced against this engine.
    pub const fn is_measured(self) -> bool {
        matches!(self, Self::Measured)
    }

    /// A line for `doctor` and the run detail.
    ///
    /// Phrased as what it means for the operator rather than as a capability
    /// list: "a budget is advisory" is the consequence, and the consequence is
    /// what somebody needs to act on.
    pub fn summary_line(self, engine: EngineKind) -> String {
        match self {
            Self::Measured => format!("{}: token usage is measured", engine.as_str()),
            Self::Unmeasured { source } => format!(
                "{}: does not report token usage yet, so a token budget is \
                 advisory for it — counts would come from {source} (RL-409)",
                engine.as_str()
            ),
        }
    }
}

/// What is known about an engine's usage reporting.
///
/// Exhaustive on purpose: a new engine cannot be added without answering this,
/// and the answer that would be wrong to default to is "measured".
pub const fn support(engine: EngineKind) -> UsageSupport {
    match engine {
        // The fixture engine reports counts because they are fabricated by it.
        // Worth stating rather than leaving implicit: it is the reason every
        // existing test passes with the real gap present — the fixtures are more
        // honest than the thing they stand in for.
        EngineKind::Mock => UsageSupport::Measured,

        EngineKind::Claude => UsageSupport::Unmeasured {
            source: "`claude --output-format json`",
        },
        EngineKind::Codex => UsageSupport::Unmeasured {
            source: "`codex exec --json`",
        },
    }
}

/// Every engine that cannot report usage, for a report that lists them.
pub fn unmeasured_engines() -> Vec<(EngineKind, UsageSupport)> {
    [EngineKind::Claude, EngineKind::Codex, EngineKind::Mock]
        .into_iter()
        .map(|engine| (engine, support(engine)))
        .filter(|(_, support)| !support.is_measured())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_a_real_engine_is_not_claimed_to_be_measured() {
        // The whole point. Claiming measurement rev-local does not have is how a
        // budget silently stops being a ceiling.
        assert!(!support(EngineKind::Claude).is_measured());
        assert!(!support(EngineKind::Codex).is_measured());
    }

    #[test]
    fn usage_the_mock_is_measured_because_it_makes_the_numbers_up() {
        assert!(support(EngineKind::Mock).is_measured());
    }

    #[test]
    fn usage_an_unmeasured_engine_says_what_it_means_for_a_budget() {
        // §18: a limitation nobody can act on is not reported. "Advisory" is the
        // word that tells an operator their ceiling is not holding anything.
        let line = support(EngineKind::Claude).summary_line(EngineKind::Claude);
        assert!(line.contains("advisory"), "{line}");
        assert!(
            line.contains("output-format json"),
            "and where it would come from: {line}"
        );
    }

    #[test]
    fn usage_the_unmeasured_list_covers_both_real_engines() {
        let unmeasured = unmeasured_engines();
        assert_eq!(unmeasured.len(), 2, "{unmeasured:?}");
        assert!(unmeasured
            .iter()
            .all(|(engine, _)| *engine != EngineKind::Mock));
    }
}
