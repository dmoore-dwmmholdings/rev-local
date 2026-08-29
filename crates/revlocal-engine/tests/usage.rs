//! Token accounting when nobody counted (RL-409, SPEC §18, ADR 0010).
//!
//! The bug this file exists for was invisible for a reason worth remembering: the
//! mock engine reports token counts, so **every test passed with the gap present**.
//! §8.3's `result.json` schema has no usage field, so a runner reading it had no
//! counts to report and returned `Usage::default()` — zero tokens. A run that spent
//! forty thousand was recorded as spending none, and a repo with a two-million
//! token daily budget never reached it.
//!
//! The fixture was more honest than the thing it stood in for. That is the failure
//! mode a fixture is least able to warn you about, so these tests are about the
//! *absence* of measurement rather than about any engine's output.

use revlocal_core::{BudgetLedgerEntry, RepoId, Usage};

fn ledger(usage: Usage) -> BudgetLedgerEntry {
    BudgetLedgerEntry {
        repo_id: RepoId::new(1),
        day: "2026-08-29".to_owned(),
        runs: 1,
        known_cost_usd: 0.0,
        usage,
    }
}

#[test]
fn usage_default_means_nobody_counted_not_nothing_was_spent() {
    // Criterion 1, at its source. `Usage::default()` is what a runner with no
    // counts returns, and it must not assert that the run was free.
    let unmeasured = Usage::default();

    assert!(
        !unmeasured.tokens_are_known(),
        "a default Usage claims counts nobody took"
    );
    assert_eq!(unmeasured.total_tokens(), 0, "the known portion is zero");
}

#[test]
fn usage_measured_is_the_only_thing_that_claims_completeness() {
    let measured = Usage::measured(1_200, 340);

    assert!(measured.tokens_are_known());
    assert_eq!(measured.total_tokens(), 1_540);
    assert!(!Usage::unmeasured().tokens_are_known());
}

#[test]
fn tokens_exhausted_cannot_say_not_exhausted_for_an_unmeasured_day() {
    // Criterion 2, and the whole point. A caller treating "cannot tell" as "fine"
    // is doing exactly what §18 forbids, which is why this is not a bool.
    let unmeasured = ledger(Usage::unmeasured());

    assert_eq!(
        unmeasured.tokens_exhausted(1_000),
        None,
        "an unmeasured day must not report a budget as unspent"
    );
    assert!(!unmeasured.tokens_are_complete());
}

#[test]
fn a_known_overspend_is_still_reported_even_when_the_day_is_incomplete() {
    // The known portion already passed the limit; an unmeasured remainder can only
    // make that more true. Answering "cannot tell" here would be over-cautious to
    // the point of uselessness.
    let mut usage = Usage::measured(900, 200);
    usage.add(&Usage::unmeasured());

    let entry = ledger(usage);
    assert!(!entry.tokens_are_complete());
    assert_eq!(entry.tokens_exhausted(1_000), Some(true));
}

#[test]
fn one_unmeasured_run_makes_the_whole_day_incomplete() {
    // The property that makes the ledger's MIN correct: a later measured run must
    // not restore a claim an earlier unmeasured one destroyed.
    let mut day = Usage::measured(500, 100);
    assert!(day.tokens_are_known());

    day.add(&Usage::unmeasured());
    assert!(!day.tokens_are_known(), "one unmeasured run taints the day");

    day.add(&Usage::measured(400, 100));
    assert!(
        !day.tokens_are_known(),
        "a later measured run must not restore a completeness claim"
    );

    // The known portion still accumulates — it is a lower bound, not nothing.
    assert_eq!(day.total_tokens(), 1_100);
}

#[test]
fn a_measured_day_answers_both_ways() {
    let entry = ledger(Usage::measured(600, 400));

    assert_eq!(entry.tokens_exhausted(1_000), Some(true));
    assert_eq!(entry.tokens_exhausted(1_001), Some(false));
    assert!(entry.tokens_are_complete());
}

#[test]
fn tokens_and_cost_now_answer_the_same_three_ways() {
    // ADR 0010 gave cost three answers and left tokens with two, on the stated
    // grounds that "token counts are always known". They are not. This asserts the
    // two halves are now shaped alike, so the next person reasoning about one can
    // rely on the other behaving the same.
    let unmeasured = ledger(Usage::unmeasured());
    assert_eq!(unmeasured.tokens_exhausted(10), None);
    assert_eq!(unmeasured.cost_exhausted(10.0), None);

    let measured = ledger(Usage::measured(1, 1).with_cost(1.0));
    assert_eq!(measured.tokens_exhausted(10), Some(false));
    assert_eq!(measured.cost_exhausted(10.0), Some(false));
}

#[test]
fn usage_round_trips_through_serde_with_its_measurement_flag() {
    // The flag crosses the store and the IPC boundary; losing it in
    // serialisation would put the original bug back with extra steps.
    for usage in [
        Usage::default(),
        Usage::measured(10, 20),
        Usage::measured(10, 20).with_cost(0.5),
        Usage::unmeasured(),
    ] {
        let json = serde_json::to_string(&usage).unwrap_or_default();
        let back: Usage = serde_json::from_str(&json).unwrap_or(Usage::measured(0, 0));
        assert_eq!(back, usage, "round trip changed {json}");
        assert_eq!(back.tokens_are_known(), usage.tokens_are_known());
    }
}
