//! Budgets and concurrency caps (RL-805, SPEC §13.1, §4.3).
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::TimeZone;
use revlocal_core::{BudgetLedgerEntry, BudgetSettings, OnExhausted, RepoId, Timestamp, Usage};
use revlocal_daemon::{
    check_budget, day_of, exhausted_detail, records_the_change, resumes_after_reset, BudgetVerdict,
    Exhausted, RunSlots, DEFAULT_MAX_CONCURRENT_RUNS,
};

fn at(hour: u32) -> Timestamp {
    chrono::Utc
        .with_ymd_and_hms(2026, 8, 28, hour, 0, 0)
        .single()
        .unwrap_or_default()
}

fn budget() -> BudgetSettings {
    BudgetSettings {
        daily_tokens_per_repo: 1_000,
        daily_runs_per_repo: 5,
        daily_cost_usd_per_repo: 0.0,
        on_exhausted: OnExhausted::Pause,
        ..BudgetSettings::default()
    }
}

fn spent(runs: u32, tokens_in: u64, tokens_out: u64, cost: Option<f64>) -> BudgetLedgerEntry {
    BudgetLedgerEntry {
        repo_id: RepoId::new(1),
        day: "2026-08-28".to_owned(),
        runs,
        usage: Usage {
            tokens_in,
            tokens_out,
            cost_usd: cost,
        },
        known_cost_usd: cost.unwrap_or(0.0),
    }
}

// --- criterion 1: exhausting a budget pauses and says why ----------------

#[test]
fn budgets_exhausting_the_token_budget_pauses_and_surfaces_a_reason() {
    let verdict = check_budget(Some(&spent(1, 600, 500, None)), &budget());

    let BudgetVerdict::Exhausted {
        which,
        action,
        reason,
    } = &verdict
    else {
        panic!("expected exhaustion, got {verdict:?}");
    };

    assert_eq!(*which, Exhausted::Tokens);
    assert_eq!(*action, OnExhausted::Pause);
    assert!(
        reason.contains("1100 of 1000 tokens"),
        "the reason has to be readable by somebody who did not write it: {reason}"
    );
    assert!(!verdict.allows_run());
}

#[test]
fn budgets_the_run_ceiling_bites_too() {
    let verdict = check_budget(Some(&spent(5, 0, 0, None)), &budget());
    let BudgetVerdict::Exhausted { which, .. } = &verdict else {
        panic!("{verdict:?}");
    };
    assert_eq!(*which, Exhausted::Runs);
}

#[test]
fn budgets_a_day_with_no_ledger_row_is_zero_spend_not_no_data() {
    assert_eq!(
        check_budget(None, &budget()),
        BudgetVerdict::WithinBudget,
        "the first run of the day has no row yet, and that has to mean nothing \
         spent rather than nothing known"
    );
}

#[test]
fn budgets_a_zero_ceiling_means_unlimited() {
    let unlimited = BudgetSettings {
        daily_tokens_per_repo: 0,
        daily_runs_per_repo: 0,
        daily_cost_usd_per_repo: 0.0,
        ..budget()
    };

    assert_eq!(
        check_budget(
            Some(&spent(9_999, 9_999_999, 9_999_999, Some(500.0))),
            &unlimited
        ),
        BudgetVerdict::WithinBudget,
        "SPEC §13.1: 0 means unlimited"
    );
}

// --- decision D10 reaches the budget check --------------------------------

#[test]
fn budgets_an_unmeasured_day_is_not_a_cheap_day() {
    let with_cost_ceiling = BudgetSettings {
        daily_cost_usd_per_repo: 10.0,
        ..budget()
    };

    // A day with an unpriced run: the ledger reports `None` (decision D10).
    let verdict = check_budget(Some(&spent(1, 10, 10, None)), &with_cost_ceiling);

    let BudgetVerdict::CostUnmeasurable { reason, .. } = &verdict else {
        panic!(
            "an unmeasured day read as under budget is a cost ceiling that stops \
             enforcing exactly when an engine stops reporting prices: {verdict:?}"
        );
    };
    assert!(reason.contains("cannot be measured"), "{reason}");
    assert!(!verdict.allows_run());
}

#[test]
fn budgets_a_measured_day_under_the_ceiling_proceeds() {
    let with_cost_ceiling = BudgetSettings {
        daily_cost_usd_per_repo: 10.0,
        ..budget()
    };
    assert_eq!(
        check_budget(Some(&spent(1, 10, 10, Some(2.50))), &with_cost_ceiling),
        BudgetVerdict::WithinBudget
    );
}

#[test]
fn budgets_a_measured_day_over_the_ceiling_stops() {
    let with_cost_ceiling = BudgetSettings {
        daily_cost_usd_per_repo: 10.0,
        ..budget()
    };
    let verdict = check_budget(Some(&spent(1, 10, 10, Some(10.01))), &with_cost_ceiling);
    let BudgetVerdict::Exhausted { which, reason, .. } = &verdict else {
        panic!("{verdict:?}");
    };
    assert_eq!(*which, Exhausted::Cost);
    assert!(reason.contains("$10.01 of $10.00"), "{reason}");
}

// --- criterion 2: nothing is dropped -------------------------------------

#[test]
fn budgets_every_exhausted_verdict_still_records_the_change() {
    for action in [OnExhausted::Pause, OnExhausted::Queue, OnExhausted::Skip] {
        let verdict = check_budget(
            Some(&spent(1, 600, 500, None)),
            &BudgetSettings {
                on_exhausted: action,
                ..budget()
            },
        );
        assert!(
            records_the_change(&verdict),
            "§18: none of on_exhausted's three values is `forget about it`. {action:?} \
             looks most like a drop and is not — it is a row saying it was skipped \
             and why"
        );
        assert!(verdict.reason().is_some(), "and the reason is on the row");
    }
}

#[test]
fn budgets_pause_and_queue_come_back_after_a_reset_but_skip_does_not() {
    assert!(resumes_after_reset(OnExhausted::Pause));
    assert!(resumes_after_reset(OnExhausted::Queue));
    assert!(
        !resumes_after_reset(OnExhausted::Skip),
        "a skipped change was recorded with a reason and a decision was made about \
         it; re-reviewing it tomorrow would contradict the row that says it was \
         skipped"
    );
}

// --- criterion 4: the day rolls over -------------------------------------

#[test]
fn budgets_a_new_day_starts_with_a_fresh_allowance() {
    let yesterday = spent(5, 999_999, 0, None);
    assert!(!check_budget(Some(&yesterday), &budget()).allows_run());

    // Tomorrow's ledger row does not exist yet, which is the rollover: the key is
    // the calendar day, so nothing has to be reset for the allowance to return.
    assert_eq!(check_budget(None, &budget()), BudgetVerdict::WithinBudget);

    assert_ne!(
        day_of(at(23)),
        day_of(at(23) + chrono::Duration::hours(2)),
        "the ledger is keyed by day, so a rollover is a different key rather than a \
         scheduled job that could fail to run"
    );
}

#[test]
fn budgets_the_day_key_matches_the_ledger_format() {
    assert_eq!(day_of(at(12)), "2026-08-28");
}

// --- criterion 3: the concurrency cap is real ----------------------------

#[tokio::test]
async fn budgets_max_concurrent_runs_is_genuinely_enforced() {
    let slots = RunSlots::new(2);
    let in_flight = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));

    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..5 {
        let slots = slots.clone();
        let in_flight = Arc::clone(&in_flight);
        let peak = Arc::clone(&peak);

        tasks.spawn(async move {
            let permit = slots.acquire().await;
            let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);

            tokio::time::sleep(Duration::from_millis(60)).await;

            in_flight.fetch_sub(1, Ordering::SeqCst);
            drop(permit);
        });
    }

    while tasks.join_next().await.is_some() {}

    assert_eq!(
        peak.load(Ordering::SeqCst),
        2,
        "five queued runs, a cap of two: the peak must be exactly the cap. Lower \
         would mean nothing overlapped and the cap proved nothing"
    );
    assert_eq!(in_flight.load(Ordering::SeqCst), 0);
    assert_eq!(slots.available(), 2, "every permit came back");
}

#[tokio::test]
async fn budgets_a_permit_is_held_for_the_whole_run_not_just_its_start() {
    let slots = RunSlots::new(1);
    let first = slots.acquire().await.expect("a slot");

    assert_eq!(slots.available(), 0);
    let second = tokio::time::timeout(Duration::from_millis(100), slots.acquire()).await;
    assert!(
        second.is_err(),
        "a check-then-act cap has a window between the two, and under exactly the \
         load a cap exists for, that window is when it is widest"
    );

    drop(first);
    assert!(slots.acquire().await.is_some());
}

#[test]
fn budgets_a_cap_of_zero_is_treated_as_one() {
    assert_eq!(
        RunSlots::new(0).limit(),
        1,
        "a cap of zero would mean nothing ever runs, which is a configuration \
         mistake rather than an instruction"
    );
    assert_eq!(RunSlots::default().limit(), DEFAULT_MAX_CONCURRENT_RUNS);
    assert_eq!(DEFAULT_MAX_CONCURRENT_RUNS, 2, "SPEC §13.1");
}

// --- what the audit log says ----------------------------------------------

#[test]
fn budgets_exhaustion_is_audited_with_whether_it_comes_back() {
    let verdict = check_budget(
        Some(&spent(1, 600, 500, None)),
        &BudgetSettings {
            on_exhausted: OnExhausted::Skip,
            ..budget()
        },
    );

    let detail = exhausted_detail("rev-local", "2026-08-28", &verdict);
    assert_eq!(detail["repo"], "rev-local");
    assert_eq!(detail["day"], "2026-08-28");
    assert!(detail["reason"].is_string());
    assert_eq!(
        detail["resumes_after_reset"], false,
        "somebody reading the log tomorrow needs to know whether the work is coming \
         back or was decided about"
    );
}
