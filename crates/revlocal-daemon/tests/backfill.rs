//! Manual review and resumable backfill (RL-1007, SPEC §7.4).
//!
//! Backfill is the one operation in rev-local that can starve everything else. A
//! repository with four years of history is tens of thousands of commits, and
//! every other trigger source produces work at roughly the rate a human produces
//! it. So the first test is about fairness, and it checks the property at *every*
//! step rather than once — a fairness rule that only applies sometimes is one that
//! fails under exactly the load it exists for.

use revlocal_core::{BudgetLedgerEntry, BudgetSettings, OnExhausted, RepoId, TriggerSource, Usage};
use revlocal_daemon::backfill::{
    backfill_scope, plan, Backfill, BackfillItem, ManualReview, Step, Yielded, BACKFILL_TRIGGER,
    MANUAL_TRIGGER,
};
use revlocal_daemon::budgets::{check, BudgetVerdict};

fn item(id: &str) -> BackfillItem {
    BackfillItem {
        external_id: id.to_owned(),
        summary: format!("commit {id}"),
    }
}

fn items(ids: &[&str]) -> Vec<BackfillItem> {
    ids.iter().map(|id| item(id)).collect()
}

fn within_budget() -> BudgetVerdict {
    BudgetVerdict::WithinBudget
}

#[test]
fn backfill_yields_to_live_triggers_rather_than_starving_them() {
    // Criterion 1. Not throttled, not rate-limited: strictly behind. A single live
    // trigger outranks the entire backlog.
    let plan = plan(
        RepoId::new(1),
        &backfill_scope("commits:main"),
        &items(&["a", "b", "c"]),
        None,
        None,
    );
    let mut backfill = Backfill::new(plan);

    assert_eq!(
        backfill.next_step(true, &within_budget()),
        Step::Yield(Yielded::LiveWorkPending),
        "live work must outrank the backlog"
    );

    // And it is asked at every step, not once at the start. A backfill of twenty
    // thousand commits runs for hours; a check that only ran at the beginning
    // would let the whole backlog run ahead of a commit pushed a minute later.
    let Step::Review(first) = backfill.next_step(false, &within_budget()) else {
        panic!("with no live work, backfill should proceed");
    };
    backfill.recorded(&first);

    assert_eq!(
        backfill.next_step(true, &within_budget()),
        Step::Yield(Yielded::LiveWorkPending),
        "live work arriving mid-backfill must still win"
    );

    // Yielding is not abandoning. When live work clears, it picks up where it was.
    let Step::Review(second) = backfill.next_step(false, &within_budget()) else {
        panic!("backfill should resume once live work clears");
    };
    assert_eq!(second.external_id, "b");
    assert_eq!(backfill.remaining(), 2);
}

#[test]
fn interrupting_and_rerunning_resumes_rather_than_restarting() {
    // Criterion 2. Ctrl-C during a long backfill is the expected way to stop one,
    // not an exceptional case.
    let all = items(&["a", "b", "c", "d", "e"]);
    let scope = backfill_scope("commits:main");

    // First run gets through two and is interrupted.
    let mut first = Backfill::new(plan(RepoId::new(1), &scope, &all, None, None));
    for _ in 0..2 {
        let Step::Review(next) = first.next_step(false, &within_budget()) else {
            panic!("expected an item");
        };
        first.recorded(&next);
    }
    let cursor = first
        .cursor_value()
        .map(str::to_owned)
        .expect("the cursor must have advanced");
    assert_eq!(cursor, "b", "the cursor advances per item, not per run");

    // Second run resumes from the cursor.
    let resumed = plan(RepoId::new(1), &scope, &all, Some(&cursor), None);
    assert_eq!(
        resumed
            .items
            .iter()
            .map(|item| item.external_id.as_str())
            .collect::<Vec<_>>(),
        vec!["c", "d", "e"],
        "resume must exclude the cursor's own item, which was already reviewed"
    );
    assert_eq!(resumed.resumed_from.as_deref(), Some("b"));
}

#[test]
fn a_cursor_naming_something_gone_starts_over_rather_than_skipping() {
    // History was rewritten, or `--since` moved. Re-reviewing is wasteful;
    // skipping is wrong. This picks the wasteful one on purpose.
    let all = items(&["a", "b", "c"]);
    let resumed = plan(
        RepoId::new(1),
        &backfill_scope("commits:main"),
        &all,
        Some("rebased-away"),
        None,
    );

    assert_eq!(resumed.len(), 3);
}

#[test]
fn dry_run_enumerates_without_spending_engine_tokens() {
    // Criterion 3. The reason to dry-run a backfill is to find out what it would
    // cost *before* spending it, so `plan` takes no engine and cannot reach one —
    // it is not a flag on the execution path, it is a different function.
    let planned = plan(
        RepoId::new(1),
        &backfill_scope("commits:main"),
        &items(&["a", "b", "c"]),
        None,
        None,
    );

    assert_eq!(planned.len(), 3);
    let lines = planned.summary_lines().join("\n");
    assert!(lines.contains("3 change(s) to review"), "{lines}");
    assert!(lines.contains("commit a"), "{lines}");
}

#[test]
fn limit_is_honoured_exactly() {
    // Criterion 4. Exactly: not "about", and not silently rounded to a batch size.
    for limit in 0..=5_usize {
        let planned = plan(
            RepoId::new(1),
            &backfill_scope("commits:main"),
            &items(&["a", "b", "c", "d", "e"]),
            None,
            Some(limit),
        );
        assert_eq!(planned.len(), limit.min(5), "--limit {limit}");
    }
}

#[test]
fn what_limit_excluded_is_stated_not_hidden() {
    // §18. "showing 50 of 3,000" and "there are 50" are different statements, and
    // a plan that reported only the first would let somebody conclude their
    // history was smaller than it is.
    let planned = plan(
        RepoId::new(1),
        &backfill_scope("commits:main"),
        &items(&["a", "b", "c", "d", "e"]),
        None,
        Some(2),
    );

    assert_eq!(planned.len(), 2);
    assert_eq!(planned.excluded_by_limit, 3);

    let lines = planned.summary_lines().join("\n");
    assert!(
        lines.contains("3 more match --since and were excluded by --limit"),
        "{lines}"
    );
}

#[test]
fn limit_counts_from_the_resume_point_not_from_the_start() {
    // Otherwise `--limit 2` reviews two changes on the first run and zero on every
    // resume, and the backfill never finishes.
    let all = items(&["a", "b", "c", "d", "e"]);
    let resumed = plan(
        RepoId::new(1),
        &backfill_scope("commits:main"),
        &all,
        Some("b"),
        Some(2),
    );

    assert_eq!(
        resumed
            .items
            .iter()
            .map(|item| item.external_id.as_str())
            .collect::<Vec<_>>(),
        vec!["c", "d"]
    );
    assert_eq!(resumed.excluded_by_limit, 1);
}

#[test]
fn backfill_respects_budgets_and_says_why_it_stopped() {
    // §7.4: backfill respects budgets. §18: it says so rather than simply going
    // quiet, because a backfill that stopped with no record is indistinguishable
    // from one that finished.
    let budget = BudgetSettings {
        daily_runs_per_repo: 10,
        on_exhausted: OnExhausted::Pause,
        ..BudgetSettings::default()
    };
    let spent = BudgetLedgerEntry {
        repo_id: RepoId::new(1),
        day: "2026-08-28".to_owned(),
        runs: 10,
        usage: Usage::default(),
        known_cost_usd: 0.0,
    };
    let verdict = check(Some(&spent), &budget);

    let backfill = Backfill::new(plan(
        RepoId::new(1),
        &backfill_scope("commits:main"),
        &items(&["a"]),
        None,
        None,
    ));

    match backfill.next_step(false, &verdict) {
        Step::Yield(Yielded::BudgetExhausted { reason }) => {
            assert!(reason.contains("run budget"), "{reason}");
        }
        other => panic!("expected a budget yield, got {other:?}"),
    }
}

#[test]
fn live_work_outranks_even_an_exhausted_budget_check() {
    // Ordering, not preference: if both are true the answer must be the one that
    // makes backfill stand aside, and the reason recorded should be the live work
    // rather than a budget the backfill never reached.
    let backfill = Backfill::new(plan(
        RepoId::new(1),
        &backfill_scope("commits:main"),
        &items(&["a"]),
        None,
        None,
    ));

    let exhausted = BudgetVerdict::Exhausted {
        which: revlocal_daemon::budgets::Exhausted::Runs,
        action: OnExhausted::Pause,
        reason: "spent".to_owned(),
    };

    assert_eq!(
        backfill.next_step(true, &exhausted),
        Step::Yield(Yielded::LiveWorkPending)
    );
}

#[test]
fn a_finished_backfill_reports_done_rather_than_yielding_forever() {
    let mut backfill = Backfill::new(plan(
        RepoId::new(1),
        &backfill_scope("commits:main"),
        &items(&["a"]),
        None,
        None,
    ));

    let Step::Review(only) = backfill.next_step(false, &within_budget()) else {
        panic!("expected the one item");
    };
    backfill.recorded(&only);

    assert_eq!(backfill.next_step(false, &within_budget()), Step::Done);
    assert_eq!(backfill.remaining(), 0);
    assert_eq!(backfill.completed(), 1);
}

#[test]
fn the_backfill_cursor_is_distinct_from_the_discovery_cursor() {
    // §7.4 requires a separate `backfill:` cursor, and the reason is that the two
    // move in opposite directions: discovery advances toward HEAD, backfill walks
    // away from it. Sharing one would make a backfill rewind live discovery and
    // re-review everything.
    let discovery = revlocal_core::Cursor::commits_scope("main");
    let backfill = backfill_scope(&discovery);

    assert_eq!(discovery, "commits:main");
    assert_eq!(backfill, "backfill:commits:main");
    assert_ne!(discovery, backfill);
}

#[test]
fn a_manual_review_does_not_yield_and_is_not_coalesced() {
    // §7.4's other half. Manual is the one source that does not go through the
    // trigger bus: §7 has triggers schedule *discovery*, and discovery decides
    // what changed — but a human naming a revision has already decided. Coalescing
    // it with whatever else was in flight would review something else instead.
    let request = ManualReview::new(RepoId::new(1), "deadbeef", chrono::Utc::now());

    assert!(
        !request.yields_to_live_work(),
        "a human is waiting for this one"
    );
    assert_eq!(request.rev, "deadbeef");
    assert_eq!(MANUAL_TRIGGER, TriggerSource::Manual);
    assert_eq!(BACKFILL_TRIGGER, TriggerSource::Backfill);
}

#[test]
fn since_is_kept_as_the_user_wrote_it() {
    // A date, a sha and a revision number are interpreted by the adapter that
    // knows the repository's kind. Guessing between them in the CLI is how
    // `--since 12345` becomes a date on one repo and a revision on another.
    let since = revlocal_daemon::backfill::Since("12345".to_owned());
    assert_eq!(since.0, "12345");
}
