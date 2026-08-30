//! Token usage read from a real engine payload (RL-409, SPEC §8.1).
//!
//! The fixture is a **captured** `claude --output-format json` response, redacted
//! of its session and message ids and otherwise byte-for-byte what the CLI wrote.
//! That matters: the numbers in it are the reason this code is shaped as it is,
//! and a hand-written fixture would have had the shape somebody expected rather
//! than the shape that exists.

use revlocal_engine::usage::{from_claude_json, UsageError};

/// ADR 0003: a helper returns its failure rather than panicking, so a missing
/// fixture is reported as a missing fixture and not as the subject failing.
fn payload() -> Result<String, String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/claude-output-format-json.json");
    std::fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))
}

#[test]
fn usage_cache_tokens_are_counted_as_input_tokens() -> Result<(), String> {
    // The trap this whole function exists to avoid. The captured payload is a
    // one-sentence prompt:
    //
    //   input_tokens                2
    //   cache_creation_input_tokens 8453
    //   cache_read_input_tokens     10143
    //
    // Reading `input_tokens` alone records 2 for a call that processed 18,598 —
    // a 99.99% undercount, and a daily token budget that would never fire.
    let usage = from_claude_json(&payload()?).map_err(|e| e.to_string())?;

    assert_eq!(usage.tokens_in, 2 + 8453 + 10143);
    assert!(
        usage.tokens_in > 18_000,
        "cached tokens are billed, not free; a budget must see them: {}",
        usage.tokens_in
    );
    assert_eq!(usage.tokens_out, 4);
    Ok(())
}

#[test]
fn usage_a_measured_run_says_it_is_measured() -> Result<(), String> {
    // ADR 0010's shape: the flag is the difference between "spent nothing" and
    // "nobody counted", and a real payload must set it.
    let usage = from_claude_json(&payload()?).map_err(|e| e.to_string())?;

    assert!(usage.tokens_are_known());
    assert!(
        usage.cost_usd.is_some(),
        "the payload carries total_cost_usd"
    );
    Ok(())
}

#[test]
fn usage_the_cost_is_the_engines_own_figure() -> Result<(), String> {
    // Better than arithmetic over rates this crate would have to hard-code, and
    // which would go stale silently the next time pricing moved.
    let usage = from_claude_json(&payload()?).map_err(|e| e.to_string())?;

    let cost = usage.cost_usd.unwrap_or_default();
    assert!(cost > 0.0, "a real call cost something: {cost}");
    assert!(
        cost < 10.0,
        "and a one-sentence prompt did not cost that: {cost}"
    );
    Ok(())
}

#[test]
fn usage_output_that_is_not_json_is_an_error_not_a_zero() {
    // §18 and ADR 0010: the failure mode is a run recorded as free because
    // nobody could read what it spent.
    let error = from_claude_json("I am not JSON").expect_err("must not parse");
    assert!(matches!(error, UsageError::NotJson { .. }), "{error}");
}

#[test]
fn usage_json_without_a_usage_object_is_distinguished_from_bad_json() {
    // Two different remedies: one means the engine printed something else
    // entirely, the other that it printed a shape this build does not know.
    let error = from_claude_json(r#"{"result": "ok"}"#).expect_err("must not parse");
    assert!(matches!(error, UsageError::NoUsage), "{error}");
    assert!(
        error.to_string().contains("not measured"),
        "and say what it means for the run: {error}"
    );
}

#[test]
fn usage_a_missing_sub_field_is_zero_but_a_missing_usage_object_is_not() -> Result<(), String> {
    // Inside a present `usage`, an absent token kind genuinely was not used.
    // That is different from an absent `usage`, where nobody counted at all —
    // and collapsing the two is how an unmeasured run reads as a free one.
    let minimal = r#"{"usage": {"input_tokens": 7, "output_tokens": 3}}"#;
    let usage = from_claude_json(minimal).map_err(|e| e.to_string())?;

    assert_eq!(usage.tokens_in, 7, "no cache fields means no cache tokens");
    assert_eq!(usage.tokens_out, 3);
    assert!(usage.tokens_are_known());
    assert!(usage.cost_usd.is_none(), "no total_cost_usd means unpriced");
    Ok(())
}

// --- codex (RL-408) ---------------------------------------------------------

fn codex_stream() -> Result<String, String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/codex-exec-json.jsonl");
    std::fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))
}

#[test]
fn usage_codex_counts_the_opposite_way_to_claude() -> Result<(), String> {
    // The finding this fixture exists for. Codex reports an inclusive total with
    // a breakdown:
    //
    //   input_tokens         35945
    //   cached_input_tokens  28160   <- part of the 35945, not extra
    //
    // Summing them would double-count 28,160 tokens — the exact inverse of the
    // Claude mistake, where *not* summing undercounts by 99.99%. Two conventions,
    // one extractor each, and neither can be shared.
    let usage =
        revlocal_engine::usage::from_codex_jsonl(&codex_stream()?).map_err(|e| e.to_string())?;

    assert_eq!(usage.tokens_in, 35_945, "input_tokens is already the total");
    assert_ne!(
        usage.tokens_in,
        35_945 + 28_160,
        "summing would double-count the cached portion"
    );
    assert_eq!(usage.tokens_out, 230);
    assert!(usage.tokens_are_known());
    Ok(())
}

#[test]
fn usage_codex_reports_no_price() -> Result<(), String> {
    // ADR 0010: unpriced is `None`, not zero. Computing one from hard-coded rates
    // would be a number that goes stale silently the next time pricing moves.
    let usage =
        revlocal_engine::usage::from_codex_jsonl(&codex_stream()?).map_err(|e| e.to_string())?;

    assert!(usage.cost_usd.is_none());
    Ok(())
}

#[test]
fn usage_the_last_completed_turn_wins() -> Result<(), String> {
    // A session can have several turns and the last carries the cumulative
    // figure. Taking the first would report the opening turn as the whole run.
    let stream = concat!(
        r#"{"type":"turn.completed","usage":{"input_tokens":10,"output_tokens":1}}"#,
        "\n",
        r#"{"type":"turn.completed","usage":{"input_tokens":900,"output_tokens":90}}"#,
        "\n",
    );

    let usage = revlocal_engine::usage::from_codex_jsonl(stream).map_err(|e| e.to_string())?;

    assert_eq!(usage.tokens_in, 900);
    assert_eq!(usage.tokens_out, 90);
    Ok(())
}

#[test]
fn usage_a_stray_non_json_line_does_not_lose_the_counts() -> Result<(), String> {
    // JSONL in practice picks up banners and progress lines. Losing a run's
    // counts to one of them would record a measured run as unmeasured.
    let stream = concat!(
        "Codex starting up...\n",
        r#"{"type":"turn.completed","usage":{"input_tokens":42,"output_tokens":7}}"#,
        "\n",
    );

    let usage = revlocal_engine::usage::from_codex_jsonl(stream).map_err(|e| e.to_string())?;

    assert_eq!(usage.tokens_in, 42);
    Ok(())
}

#[test]
fn usage_a_stream_that_never_completed_a_turn_is_not_zero() {
    // The engine started and stopped. That is unmeasured, not free.
    let stream = concat!(r#"{"type":"turn.started"}"#, "\n");

    let error = revlocal_engine::usage::from_codex_jsonl(stream).expect_err("must not parse");
    assert!(
        error.to_string().contains("not measured"),
        "and say what it means for the run: {error}"
    );
}
