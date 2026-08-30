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

use revlocal_core::{EngineKind, Usage};

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

        // RL-409: `from_claude_json` reads it, against a captured real payload.
        EngineKind::Claude => UsageSupport::Measured,
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

/// Why a payload could not be read for usage.
#[derive(Debug, thiserror::Error)]
pub enum UsageError {
    /// The output was not JSON.
    #[error("engine output is not JSON: {source}")]
    NotJson {
        /// Why.
        #[source]
        source: serde_json::Error,
    },

    /// The JSON carried no `usage` object.
    ///
    /// Distinct from a parse failure: one means the engine printed something else
    /// entirely, the other that it printed a shape this build does not know. The
    /// remedies differ, so the errors do.
    #[error("engine output has no `usage` object; the run was not measured")]
    NoUsage,
}

/// Read token usage out of `claude --output-format json` (RL-409, SPEC §8.1).
///
/// # Cache tokens are input tokens
///
/// This is why the function is more than three lines. A real captured payload,
/// from a one-sentence prompt:
///
/// ```json
/// "input_tokens": 2,
/// "cache_creation_input_tokens": 8453,
/// "cache_read_input_tokens": 10143,
/// "output_tokens": 4
/// ```
///
/// Reading `input_tokens` alone records **2** for a call that processed **18,598**
/// — a 99.99% undercount, and a daily token ceiling that would never fire however
/// much work was done. Cached tokens are billed differently, not for free, and a
/// token budget is about how much the model was asked to process.
///
/// Price is carried separately and exactly by `total_cost_usd`, so nothing is lost
/// by summing them: the number that varies by rate is reported by its own field
/// rather than smuggled into a token count.
pub fn from_claude_json(stdout: &str) -> Result<Usage, UsageError> {
    let document: serde_json::Value =
        serde_json::from_str(stdout).map_err(|source| UsageError::NotJson { source })?;

    let usage = document.get("usage").ok_or(UsageError::NoUsage)?;
    let field = |name: &str| {
        usage
            .get(name)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    };

    // An absent sub-field is zero *here* and only here: the `usage` object was
    // present, so this run was counted, and a token kind that is not listed was
    // genuinely not used. An absent `usage` object means nobody counted, which is
    // the error above — collapsing the two is how an unmeasured run reads as a
    // free one (ADR 0010).
    let tokens_in = field("input_tokens")
        + field("cache_creation_input_tokens")
        + field("cache_read_input_tokens");

    Ok(Usage {
        tokens_in,
        tokens_out: field("output_tokens"),
        tokens_known: true,
        // The engine's own figure, which beats arithmetic over rates this crate
        // would have to hard-code and that would go stale silently.
        cost_usd: document
            .get("total_cost_usd")
            .and_then(serde_json::Value::as_f64),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_codex_is_not_claimed_to_be_measured() {
        // Claiming measurement rev-local does not have is how a budget silently
        // stops being a ceiling. Codex has no extractor yet — RL-408 has to
        // establish what `codex exec --json` emits first.
        assert!(!support(EngineKind::Codex).is_measured());
    }

    #[test]
    fn usage_claude_is_measured_because_its_payload_was_read() {
        // Not an assumption: `from_claude_json` is tested against a captured
        // `--output-format json` response in tests/fixtures.
        assert!(support(EngineKind::Claude).is_measured());
    }

    #[test]
    fn usage_the_mock_is_measured_because_it_makes_the_numbers_up() {
        assert!(support(EngineKind::Mock).is_measured());
    }

    #[test]
    fn usage_an_unmeasured_engine_says_what_it_means_for_a_budget() {
        // §18: a limitation nobody can act on is not reported. "Advisory" is the
        // word that tells an operator their ceiling is not holding anything.
        let line = support(EngineKind::Codex).summary_line(EngineKind::Codex);
        assert!(line.contains("advisory"), "{line}");
        assert!(
            line.contains("codex exec --json"),
            "and where it would come from: {line}"
        );
    }

    #[test]
    fn usage_only_codex_remains_unmeasured() {
        let unmeasured = unmeasured_engines();
        assert_eq!(unmeasured.len(), 1, "{unmeasured:?}");
        assert_eq!(unmeasured[0].0, EngineKind::Codex);
    }
}
