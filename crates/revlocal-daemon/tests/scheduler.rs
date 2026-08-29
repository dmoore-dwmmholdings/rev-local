//! The daemon's decision loop (RL-1201, SPEC §4.2, §4.3, §7, §12.1, §13.1).
//!
//! M8 through M12 built the parts and nothing ran them: `gate` was never invoked,
//! `RunSlots` bounded nothing, and the kill switch was engaged by no caller. Each
//! piece had tests and the composition had none — the shape of assembly that looks
//! finished and does nothing.
//!
//! These tests are almost entirely about **ordering**, because that is the whole
//! design. Reordering any pair of checks changes what an operator is told when two
//! things are true at once, and being told the wrong reason is worse than being
//! told nothing.
use revlocal_core::{OnExhausted, RepoId, TriggerSource};
use revlocal_daemon::backfill::Yielded;
use revlocal_daemon::budgets::{BudgetVerdict, Exhausted};
use revlocal_daemon::scheduler::{Decision, Idle, Scheduler, WorldState};
use revlocal_daemon::triggers::DiscoveryPass;
fn pass(repo: i64) -> DiscoveryPass {
    DiscoveryPass {
        repo_id: RepoId::new(repo),
        sources: vec![TriggerSource::Poll],
    }
}
fn exhausted() -> BudgetVerdict {
    BudgetVerdict::Exhausted {
        which: Exhausted::Runs,
        action: OnExhausted::Pause,
        reason: "daily run budget reached: 200 of 200 runs today".to_owned(),
    }
}
#[test]
fn nothing_waiting_says_so_rather_than_going_quiet() {
    // §18 applies hardest where a system goes quiet. "rev-local is doing nothing"
    // has five meanings and only one of them is "there is nothing to do".
    let decision = Scheduler.tick(&WorldState::idle(2));
    assert_eq!(decision, Decision::Idle(Idle::NothingToDo));
    assert!(!decision.starts_work());
    assert!(
        !Idle::NothingToDo.needs_attention(),
        "an empty queue is fine"
    );
}
#[test]
fn the_kill_switch_outranks_everything() {
    // §12.1. Checked first, so an operator who has just hit the emergency control
    // is told about the control rather than about a queue or a budget.
    let mut world = WorldState::idle(2);
    world.killed = true;
    world.running = 99;
    world.due = vec![pass(1)];
    world.backfill_waiting = vec![RepoId::new(1)];
    world.budgets.insert(RepoId::new(1), exhausted());
    assert_eq!(Scheduler.tick(&world), Decision::Idle(Idle::Killed));
    let line = Idle::Killed.summary_line();
    assert!(line.contains("kill switch"), "{line}");
    assert!(
        line.contains("revlocal resume"),
        "must say how to undo it: {line}"
    );
    assert!(Idle::Killed.needs_attention());
}
#[test]
fn capacity_is_checked_before_budget() {
    // §4.3 before §13.1. A slot is a physical limit — without one there is nowhere
    // for work to go, and a budget check on work that cannot start is a check
    // nobody needed. It also means the reported reason is the one that will clear
    // first.
    let mut world = WorldState::idle(2);
    world.running = 2;
    world.due = vec![pass(1)];
    world.budgets.insert(RepoId::new(1), exhausted());
    match Scheduler.tick(&world) {
        Decision::Idle(Idle::AtCapacity { running, limit }) => {
            assert_eq!((running, limit), (2, 2));
        }
        other => panic!("capacity must be reported before budget, got {other:?}"),
    }
}
#[test]
fn a_budget_stops_work_and_names_the_repository() {
    // §13.1, and §18: the reason travels with the decision. "Holding" without
    // saying which repository or why is the same as saying nothing.
    let mut world = WorldState::idle(2);
    world.due = vec![pass(7)];
    world.budgets.insert(RepoId::new(7), exhausted());
    match Scheduler.tick(&world) {
        Decision::Idle(idle @ Idle::Budget { .. }) => {
            let line = idle.summary_line();
            assert!(line.contains("repo 7"), "{line}");
            assert!(
                line.contains("200 of 200"),
                "the reason must survive: {line}"
            );
            assert!(idle.needs_attention());
        }
        other => panic!("expected a budget hold, got {other:?}"),
    }
}
#[test]
fn a_cost_that_cannot_be_measured_holds_rather_than_proceeds() {
    // Decision D10, and RL-409's other half. An unmeasured day is not a cheap one,
    // and the scheduler must not read "cannot tell" as "fine" — which is exactly
    // what a `bool` would have made it do.
    let mut world = WorldState::idle(2);
    world.due = vec![pass(1)];
    world.budgets.insert(
        RepoId::new(1),
        BudgetVerdict::CostUnmeasurable {
            known_cost_usd: 3.0,
            ceiling_usd: 10.0,
            reason: "at least one run reported no price".to_owned(),
        },
    );
    let decision = Scheduler.tick(&world);
    assert!(
        !decision.starts_work(),
        "an unmeasurable budget must hold, not proceed: {decision:?}"
    );
}
#[test]
fn live_work_runs_when_nothing_blocks_it() {
    let mut world = WorldState::idle(2);
    world.due = vec![pass(3)];
    match Scheduler.tick(&world) {
        Decision::Discover(started) => assert_eq!(started.repo_id, RepoId::new(3)),
        other => panic!("expected a discovery pass, got {other:?}"),
    }
}
#[test]
fn backfill_runs_only_when_no_live_work_is_waiting() {
    // §7.4. Backfill is strictly behind live work — a repository with four years
    // of history would otherwise starve the commit somebody just pushed, which is
    // the same as not reviewing it.
    let mut world = WorldState::idle(2);
    world.backfill_waiting = vec![RepoId::new(5)];
    assert_eq!(
        Scheduler.tick(&world),
        Decision::Backfill {
            repo_id: RepoId::new(5)
        }
    );
    // One live trigger outranks the entire backlog.
    world.due = vec![pass(9)];
    match Scheduler.tick(&world) {
        Decision::Discover(started) => assert_eq!(started.repo_id, RepoId::new(9)),
        other => panic!("live work must outrank backfill, got {other:?}"),
    }
}
#[test]
fn the_scheduler_and_the_backfill_agree_about_yielding() {
    // RL-1007 answers "should this backfill stand aside?" from the backfill's
    // side; this asks from the scheduler's. Two components with their own answer
    // to one question is how they end up disagreeing under load.
    let mut world = WorldState::idle(2);
    world.due = vec![pass(1)];
    assert_eq!(
        Scheduler.backfill_yield(&world, RepoId::new(1)),
        Some(Yielded::LiveWorkPending)
    );
    world.due.clear();
    world.budgets.insert(RepoId::new(1), exhausted());
    match Scheduler.backfill_yield(&world, RepoId::new(1)) {
        Some(Yielded::BudgetExhausted { reason }) => {
            assert!(reason.contains("200 of 200"), "{reason}");
        }
        other => panic!("expected a budget yield, got {other:?}"),
    }
    world.budgets.clear();
    assert_eq!(Scheduler.backfill_yield(&world, RepoId::new(1)), None);
}
#[test]
fn a_repository_nobody_checked_is_not_a_repository_that_passed() {
    // Absence of a verdict means proceed — the ledger is consulted per run anyway
    // — but the reading is deliberate rather than incidental, because "no entry"
    // and "checked and fine" are different claims and only one is being made.
    let mut world = WorldState::idle(2);
    world.due = vec![pass(11)];
    assert!(world.budgets.is_empty());
    assert!(Scheduler.tick(&world).starts_work());
}
#[test]
fn a_slot_limit_of_zero_still_runs_one() {
    // Zero would mean "never review anything", which is a configuration mistake
    // rather than an instruction — and one that would look exactly like a daemon
    // that had silently stopped.
    let mut world = WorldState::idle(0);
    world.due = vec![pass(1)];
    assert!(Scheduler.tick(&world).starts_work());
    world.running = 1;
    assert!(!Scheduler.tick(&world).starts_work());
}
#[test]
fn every_idle_reason_says_something() {
    // A daemon that prints nothing while doing nothing is indistinguishable from
    // one that has crashed.
    let reasons = [
        Idle::NothingToDo,
        Idle::Killed,
        Idle::AtCapacity {
            running: 2,
            limit: 2,
        },
        Idle::Budget {
            repo_id: RepoId::new(1),
            reason: "spent".to_owned(),
        },
    ];
    for idle in reasons {
        let line = idle.summary_line();
        assert!(line.len() > 20, "{idle:?} says too little: {line:?}");
    }
    // And only the two that mean "not doing what you asked" ask for attention.
    assert!(!Idle::NothingToDo.needs_attention());
    assert!(!Idle::AtCapacity {
        running: 2,
        limit: 2
    }
    .needs_attention());
    assert!(Idle::Killed.needs_attention());
}
