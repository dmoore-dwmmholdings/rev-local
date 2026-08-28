//! Trigger coalescing (RL-1001, SPEC §7).
//!
//! The three acceptance criteria are three faces of one property: **a trigger
//! schedules discovery, it never reviews.** Four sources firing at once must not
//! become four reviews; two repositories must not become one; and a commit landing
//! mid-pass must not be lost because the bus assumed discovery saw it.
//!
//! Time is injected rather than slept through. A test that sleeps for 1.5 seconds
//! to prove a 1.5-second window is a test that goes red on a loaded CI runner and
//! teaches everyone to re-run it (ADR 0024).
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use chrono::{TimeZone, Utc};
use revlocal_core::{RepoId, Timestamp, TriggerSource};
use revlocal_daemon::triggers::{Admission, TriggerBus, TriggerEvent, DEFAULT_COALESCE_WINDOW_MS};

/// A fixed instant plus `ms`, so windows are exercised without waiting for them.
fn at(ms: i64) -> Timestamp {
    Utc.with_ymd_and_hms(2026, 8, 28, 12, 0, 0)
        .single()
        .unwrap_or_default()
        + chrono::Duration::milliseconds(ms)
}

fn event(repo: i64, source: TriggerSource, ms: i64) -> TriggerEvent {
    TriggerEvent::new(RepoId::new(repo), source, at(ms))
}

#[test]
fn four_simultaneous_triggers_for_one_repo_produce_exactly_one_pass() {
    // Criterion 1, and the scenario §7 is written for: a developer commits, the
    // post-commit hook fires, the poll interval elapses a moment later, the
    // webhook for the same push arrives, and somebody clicks "review now" because
    // they are watching. Four sources, one commit.
    let mut bus = TriggerBus::new(DEFAULT_COALESCE_WINDOW_MS);

    let admissions: Vec<Admission> = [
        (TriggerSource::Hook, 0),
        (TriggerSource::Poll, 40),
        (TriggerSource::Webhook, 120),
        (TriggerSource::Manual, 900),
    ]
    .into_iter()
    .map(|(source, ms)| bus.admit(&event(1, source, ms)))
    .collect();

    // Only the first opens a window; the rest fold into it.
    assert_eq!(admissions.iter().filter(|a| a.is_scheduled()).count(), 1);

    // Nothing is due until the window closes. This is the half of the design that
    // makes "one pass" achievable at all: a bus that started on the first event
    // would have to treat the other three as a follow-up, giving two passes.
    assert!(bus.due_passes(at(1_000)).is_empty());

    let due = bus.due_passes(at(2_500));
    assert_eq!(
        due.len(),
        1,
        "four triggers must produce one discovery pass"
    );

    // And the pass knows all four contributed, rather than crediting whichever
    // arrived first.
    assert_eq!(
        due[0].sources,
        vec![
            TriggerSource::Hook,
            TriggerSource::Poll,
            TriggerSource::Webhook,
            TriggerSource::Manual
        ]
    );

    // Draining twice does not run it twice.
    assert!(bus.due_passes(at(3_000)).is_empty());
    let _ = RepoId::new(1);
}

#[test]
fn triggers_for_different_repos_are_not_coalesced_together() {
    // Criterion 2. Collapsing two repositories would not delay one, it would
    // *drop* it — the survivor's discovery pass looks only at its own repository.
    let mut bus = TriggerBus::new(DEFAULT_COALESCE_WINDOW_MS);

    let first = bus.admit(&event(1, TriggerSource::Hook, 0));
    let second = bus.admit(&event(2, TriggerSource::Hook, 1));

    assert!(first.is_scheduled(), "repo 1 must open its own window");
    assert!(
        second.is_scheduled(),
        "repo 2 must open its own window, in the same millisecond"
    );
    assert_eq!(first.repo_id(), RepoId::new(1));
    assert_eq!(second.repo_id(), RepoId::new(2));

    let due = bus.due_passes(at(2_000));
    assert_eq!(due.len(), 2, "two repositories are two passes, never one");
    assert_eq!(due[0].repo_id, RepoId::new(1));
    assert_eq!(due[1].repo_id, RepoId::new(2));
}

#[test]
fn an_event_during_a_pass_schedules_exactly_one_follow_up() {
    // Criterion 3. Discovery reads the repository at a moment in time; a commit
    // landing during that read may or may not have been seen, and the bus cannot
    // tell which. Assuming it was seen loses the change until something else
    // happens to trigger the repo.
    let mut bus = TriggerBus::new(DEFAULT_COALESCE_WINDOW_MS);
    let repo = RepoId::new(1);

    bus.admit(&event(1, TriggerSource::Poll, 0));
    assert_eq!(bus.due_passes(at(2_000)).len(), 1);
    assert!(bus.is_pass_running(repo));

    // Three commits land while the pass is running, well outside the window — so
    // this is the running-pass rule, not the coalescing window, doing the work.
    let during: Vec<Admission> = [5_000, 6_000, 7_000]
        .into_iter()
        .map(|ms| bus.admit(&event(1, TriggerSource::Hook, ms)))
        .collect();

    assert!(
        during.iter().all(|a| !a.is_scheduled()),
        "nothing may open a window while a pass is running"
    );
    let firsts = during
        .iter()
        .filter(|a| matches!(a, Admission::Queued { first: true, .. }))
        .count();
    assert_eq!(firsts, 1, "exactly one of the three created the follow-up");
    assert!(bus.has_follow_up(repo));

    // Nor may they become a second concurrent pass.
    assert!(bus.due_passes(at(7_500)).is_empty());

    // Finishing yields the follow-up: one pass, carrying all three sources.
    let follow_up = bus
        .pass_finished(repo, at(8_000))
        .expect("a follow-up was scheduled and must be returned");
    assert_eq!(follow_up.sources.len(), 3);

    // And finishing *that* one yields nothing, because nothing arrived during it.
    // Without this the bus would re-read forever off a single burst.
    assert!(bus.pass_finished(repo, at(9_000)).is_none());
}

#[test]
fn a_quiet_pass_leaves_nothing_behind() {
    let mut bus = TriggerBus::new(DEFAULT_COALESCE_WINDOW_MS);
    let repo = RepoId::new(1);

    bus.admit(&event(1, TriggerSource::Poll, 0));
    assert_eq!(bus.due_passes(at(2_000)).len(), 1);
    assert!(bus.pass_finished(repo, at(2_100)).is_none());
    assert!(!bus.is_pass_running(repo));
    assert!(!bus.has_follow_up(repo));
}

#[test]
fn a_trigger_after_the_window_closes_starts_a_new_pass() {
    // The window bounds how long events fold together; it is not a rate limit.
    // A commit two seconds after the last one is a separate piece of work.
    let mut bus = TriggerBus::new(DEFAULT_COALESCE_WINDOW_MS);
    let repo = RepoId::new(1);

    bus.admit(&event(1, TriggerSource::Hook, 0));
    assert_eq!(bus.due_passes(at(2_000)).len(), 1);
    assert!(bus.pass_finished(repo, at(2_100)).is_none());

    let later = bus.admit(&event(1, TriggerSource::Hook, 5_000));
    assert!(
        later.is_scheduled(),
        "an event well after the window must open its own"
    );
    assert_eq!(bus.due_passes(at(7_000)).len(), 1);
}

#[test]
fn an_event_at_the_window_boundary_folds_in() {
    // Half-open: `< window`, not `<= window`. An event exactly at the boundary is
    // inside it, so the rule is stated once rather than differing by a millisecond
    // between the bus and whatever reasons about it later.
    let mut bus = TriggerBus::new(1000);

    assert!(bus.admit(&event(1, TriggerSource::Hook, 0)).is_scheduled());
    assert!(!bus
        .admit(&event(1, TriggerSource::Poll, 999))
        .is_scheduled());
    // 999 is inside; the pass therefore carries both.
    assert_eq!(bus.due_passes(at(1_000))[0].sources.len(), 2);
}

#[test]
fn a_zero_window_still_coalesces_a_running_pass() {
    // Coalescing off is a legitimate configuration. It must not become "four
    // concurrent discovery passes for one repository" — the running-pass rule is
    // independent of the window, and this is what says so.
    let mut bus = TriggerBus::new(0);
    let repo = RepoId::new(1);

    bus.admit(&event(1, TriggerSource::Hook, 0));
    // A zero window closes immediately, so the pass is due at once.
    assert_eq!(bus.due_passes(at(0)).len(), 1);
    assert!(bus.is_pass_running(repo));

    assert!(!bus.admit(&event(1, TriggerSource::Poll, 0)).is_scheduled());
    assert!(bus.has_follow_up(repo));
    assert!(bus.due_passes(at(0)).is_empty());
}

#[test]
fn a_clock_that_steps_backwards_does_not_open_a_second_window() {
    // NTP corrections happen, and a laptop waking from sleep is worse. An event
    // stamped before the window opened must fold in rather than open a second
    // window — the one outcome coalescing exists to prevent.
    let mut bus = TriggerBus::new(DEFAULT_COALESCE_WINDOW_MS);

    assert!(bus
        .admit(&event(1, TriggerSource::Hook, 1_000))
        .is_scheduled());
    let backwards = bus.admit(&event(1, TriggerSource::Poll, 500));
    assert!(
        !backwards.is_scheduled(),
        "an out-of-order timestamp must not open a second window"
    );
    assert_eq!(bus.due_passes(at(3_000)).len(), 1);
}

#[test]
fn a_hint_is_carried_but_never_required() {
    // §7 makes `hint` optional and advisory. It is recorded and not acted on: a
    // hook can be fired by hand with any string in it, and a hint that became a
    // lookup key would let a local script make the daemon fetch an arbitrary ref.
    let bare = event(1, TriggerSource::Poll, 0);
    assert!(bare.hint.is_none());

    let hinted = event(1, TriggerSource::Hook, 0).with_hint("deadbeef");
    assert_eq!(hinted.hint.as_deref(), Some("deadbeef"));

    // Two events differing only by hint are still one pass.
    let mut bus = TriggerBus::new(DEFAULT_COALESCE_WINDOW_MS);
    assert!(bus.admit(&hinted).is_scheduled());
    assert!(!bus.admit(&bare).is_scheduled());
    assert_eq!(bus.due_passes(at(2_000)).len(), 1);
}

#[test]
fn finishing_a_pass_for_an_unknown_repo_is_not_an_error() {
    // A daemon restart can leave a caller holding a repo id the bus has never
    // seen. Returning None is the honest answer; panicking would turn a restart
    // into a crash loop.
    let mut bus = TriggerBus::default();
    assert!(bus.pass_finished(RepoId::new(99), at(0)).is_none());
    assert_eq!(bus.window_ms(), DEFAULT_COALESCE_WINDOW_MS);
}
