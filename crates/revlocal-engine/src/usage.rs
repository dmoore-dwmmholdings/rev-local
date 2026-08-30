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
        // RL-408/RL-409: `from_codex_jsonl` reads it, against a captured stream.
        EngineKind::Codex => UsageSupport::Measured,
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

    /// Nothing in the output parsed as JSON at all.
    ///
    /// Separate from [`NotJson`](Self::NotJson), which is about a single document
    /// failing to parse: this is a JSONL stream in which no line was JSON, so the
    /// engine printed something else entirely.
    #[error("no line of engine output was JSON; the run was not measured")]
    NoJsonLines,

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

/// Read token usage out of `codex exec --json` (RL-408, RL-409, SPEC §8.1).
///
/// # Codex counts the opposite way to Claude, and that is the whole point
///
/// Claude reports **additive buckets** — `input_tokens` excludes cached ones, so
/// they must be summed. Codex reports an **inclusive total with a breakdown**:
///
/// ```json
/// "input_tokens": 35945, "cached_input_tokens": 28160,
/// "output_tokens": 230,  "reasoning_output_tokens": 69
/// ```
///
/// Summing here would double-count 28,160 tokens — the exact inverse of the
/// mistake that undercounts Claude by summing nothing.
///
/// Two pieces of evidence, because inferring this from one number would be
/// guessing. `reasoning_output_tokens` (69) sits inside `output_tokens` (230),
/// which is a subset by definition and establishes the convention. And across two
/// captured runs with very different prompt sizes, `input_tokens` exceeded
/// `cached_input_tokens` both times while tracking total context — additive
/// buckets would not behave that way.
///
/// **One extractor per engine, never a shared one.** These two conventions cannot
/// both be right in the same function, and the failure would be silent in either
/// direction.
///
/// # Why the last `turn.completed` wins
///
/// `--json` emits JSONL and a session can have several turns. The last completed
/// turn carries the cumulative figure; taking the first would report the opening
/// turn as though it were the run.
pub fn from_codex_jsonl(stdout: &str) -> Result<Usage, UsageError> {
    let mut usage: Option<Usage> = None;
    let mut saw_json = false;

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // A non-JSON line is not fatal: the stream is JSONL and a stray banner or
        // progress line should not lose the counts that came after it.
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        saw_json = true;

        if event.get("type").and_then(serde_json::Value::as_str) != Some("turn.completed") {
            continue;
        }
        let Some(counts) = event.get("usage") else {
            continue;
        };
        let field = |name: &str| {
            counts
                .get(name)
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0)
        };

        usage = Some(Usage {
            // Already the total. `cached_input_tokens` is a breakdown of it.
            tokens_in: field("input_tokens"),
            tokens_out: field("output_tokens"),
            tokens_known: true,
            // Codex reports no price. `None` rather than a computed one: ADR 0010
            // keeps an unmeasured cost from reading as a free one, and arithmetic
            // over rates this crate hard-coded would go stale silently.
            cost_usd: None,
        });
    }

    match usage {
        Some(usage) => Ok(usage),
        // Nothing parsed at all is a different failure from a stream that parsed
        // and carried no completed turn — one means the engine printed something
        // else, the other that it stopped early.
        None if !saw_json => Err(UsageError::NoJsonLines),
        None => Err(UsageError::NoUsage),
    }
}

/// Read usage from whichever engine produced this output (RL-409, ADR 0033).
///
/// The dispatch is the point. Claude and Codex report cache tokens with opposite
/// meanings — additive buckets against an inclusive total — so a shared parser
/// would undercount one by 99.99% or double-count the other, silently. Routing on
/// the engine id is what keeps each extractor reading only the format it was
/// written against.
pub fn for_engine(engine: EngineKind, stdout: &str) -> Result<Usage, UsageError> {
    match engine {
        EngineKind::Claude => from_claude_json(stdout),
        EngineKind::Codex => from_codex_jsonl(stdout),
        // The fixture engine reports counts inside its `result.json`, which the
        // ladder has already parsed by the time this is asked. Nothing to add.
        EngineKind::Mock => Ok(Usage::default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_every_engine_is_measured() {
        // As of RL-408 and RL-409, both real engines have an extractor tested
        // against a captured payload, and the mock reports counts it invents.
        //
        // This is the state, not a permanent property: `unmeasured_engines` and
        // the doctor check that reads it stay, because the next engine added
        // starts unmeasured and must say so rather than inheriting this.
        for engine in [EngineKind::Claude, EngineKind::Codex, EngineKind::Mock] {
            assert!(
                support(engine).is_measured(),
                "{} is not measured",
                engine.as_str()
            );
        }
        assert!(unmeasured_engines().is_empty());
    }

    #[test]
    fn usage_an_unmeasured_engine_would_say_what_it_means_for_a_budget() {
        // No engine is unmeasured today, so this exercises the value rather than
        // the table — the wording is what an operator would act on, and it must
        // keep working for the next engine that arrives without an extractor.
        let unmeasured = UsageSupport::Unmeasured {
            source: "`someengine --json`",
        };

        let line = unmeasured.summary_line(EngineKind::Codex);
        assert!(line.contains("advisory"), "{line}");
        assert!(
            line.contains("someengine"),
            "and where it would come from: {line}"
        );
        assert!(!unmeasured.is_measured());
    }
}
