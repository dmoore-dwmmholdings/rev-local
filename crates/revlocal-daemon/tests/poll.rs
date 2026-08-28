//! Poll scheduling: clamping, jitter, backoff and health (RL-1002, SPEC §7.1).
//!
//! The jitter tests assert *properties* of the derived offset rather than exact
//! millisecond values. An exact value would pin splitmix64's output, which would
//! make ADR 0029's mixer impossible to change without rewriting the tests that
//! justify it — and the mixer's identity was never the point. That it is fixed,
//! bounded and spread is.

use std::collections::BTreeSet;

use revlocal_core::RepoId;
use revlocal_daemon::poll::{
    PollInterval, PollSchedule, RepoHealth, DEFAULT_POLL_INTERVAL_SECS, JITTER_FRACTION,
    MAX_BACKOFF_SECS, MIN_POLL_INTERVAL_SECS,
};

#[test]
fn an_interval_below_the_minimum_is_clamped_with_a_warning() {
    // Criterion 1. A repo polling every second gets the *user's* account
    // rate-limited, so the value cannot be honoured. It also cannot be rejected:
    // a typo in a config file must not stop the daemon from starting.
    let clamped = PollInterval::clamp(5);

    assert_eq!(clamped.secs, MIN_POLL_INTERVAL_SECS);
    assert!(clamped.was_clamped());

    let warning = clamped.warning.as_deref().unwrap_or_default();
    // §18: the warning has to name the value the user wrote, the value in force,
    // and why — a warning saying "clamped" sends them back to the spec.
    assert!(
        warning.contains('5'),
        "must name what was configured: {warning}"
    );
    assert!(
        warning.contains("30"),
        "must name what is in force: {warning}"
    );
    assert!(
        warning.contains("rate-limited"),
        "must say why it matters: {warning}"
    );
}

#[test]
fn an_interval_at_or_above_the_minimum_is_left_alone() {
    for configured in [MIN_POLL_INTERVAL_SECS, 120, 3600] {
        let interval = PollInterval::clamp(configured);
        assert_eq!(interval.secs, configured);
        assert!(
            !interval.was_clamped(),
            "{configured}s is legal and must not be reported as clamped"
        );
        assert!(interval.warning.is_none());
    }
}

#[test]
fn backoff_escalates_and_then_recovers_on_the_first_success() {
    // Criterion 2. Both halves matter, and the second is the one that is easy to
    // get wrong: a laptop that closed its lid on a train comes back, and working a
    // backoff ladder back down would leave it polling every half hour long after
    // the network returned.
    let mut schedule = PollSchedule::new(RepoId::new(1), 60);
    assert_eq!(schedule.health(), RepoHealth::Healthy);
    assert_eq!(schedule.base_delay().as_secs(), 60);

    let mut delays = Vec::new();
    for attempt in 1..=6 {
        schedule.failed(&format!("connection refused (attempt {attempt})"));
        delays.push(schedule.base_delay().as_secs());
    }

    assert_eq!(schedule.health(), RepoHealth::Degraded);
    assert_eq!(delays, vec![120, 240, 480, 960, 1_800, 1_800]);
    assert_eq!(
        *delays.last().unwrap_or(&0),
        MAX_BACKOFF_SECS,
        "backoff stops at 30 minutes and does not keep doubling"
    );

    // One success undoes all of it.
    schedule.succeeded();
    assert_eq!(schedule.health(), RepoHealth::Healthy);
    assert_eq!(schedule.consecutive_failures, 0);
    assert_eq!(schedule.base_delay().as_secs(), 60);
    assert!(schedule.last_error.is_none());
}

#[test]
fn backing_off_is_not_giving_up() {
    // A degraded repository keeps polling. §7.1 backs off, it does not disable —
    // and a repository that stopped polling with no record is indistinguishable
    // from one where nobody has committed.
    let mut schedule = PollSchedule::new(RepoId::new(1), 60);
    for _ in 0..40 {
        schedule.failed("host is down");
    }

    assert_eq!(schedule.base_delay().as_secs(), MAX_BACKOFF_SECS);
    assert!(
        schedule.next_delay().as_secs() > 0,
        "a degraded repo still has a next poll"
    );
    assert_eq!(schedule.health(), RepoHealth::Degraded);
    assert_eq!(schedule.last_error.as_deref(), Some("host is down"));
}

#[test]
fn health_state_is_observable_as_json() {
    // Criterion 3, at the layer that owns it: `revlocal repo show --json` renders
    // this type, and a degraded repository is precisely the one worth being able
    // to see from a script — it is still configured, still polling, and quietly
    // seeing nothing.
    let mut schedule = PollSchedule::new(RepoId::new(7), 5);
    schedule.failed("could not reach origin");

    let report = schedule.health_report("acme-api");
    let json = serde_json::to_value(&report).unwrap_or_default();

    assert_eq!(json["repo"], "acme-api");
    assert_eq!(json["health"], "degraded");
    assert_eq!(json["consecutive_failures"], 1);
    assert_eq!(json["last_error"], "could not reach origin");
    // The clamp travels with the report rather than only reaching a log line
    // somebody has to have been watching for.
    assert_eq!(json["poll_interval_secs"], MIN_POLL_INTERVAL_SECS);
    assert_eq!(json["configured_interval_clamped"], true);
    assert_eq!(
        json["notes"]
            .as_array()
            .map(|notes| notes.len())
            .unwrap_or_default(),
        1
    );
}

#[test]
fn a_healthy_report_has_an_empty_notes_array_not_a_missing_one() {
    // An absent field and an empty one read the same to a human and differently to
    // a parser.
    let schedule = PollSchedule::new(RepoId::new(1), DEFAULT_POLL_INTERVAL_SECS);
    let json = serde_json::to_value(schedule.health_report("quiet")).unwrap_or_default();

    assert_eq!(json["health"], "healthy");
    assert!(json["notes"].is_array());
    assert_eq!(json["notes"].as_array().map(Vec::len), Some(0));
    assert!(json["last_error"].is_null());
}

#[test]
fn jitter_stays_within_ten_percent() {
    // §7.1 says ±10%. Checked across many repos and poll numbers rather than one,
    // because a mixer that is out of range for one input in a thousand is a mixer
    // that fails in somebody's deployment and not in this test.
    let interval = 600;
    for repo in 1..=50_i64 {
        let mut schedule = PollSchedule::new(RepoId::new(repo), interval);
        for _ in 0..20 {
            let delay = schedule.next_delay().as_secs_f64();
            let lower = interval as f64 * (1.0 - JITTER_FRACTION);
            let upper = interval as f64 * (1.0 + JITTER_FRACTION);
            assert!(
                delay >= lower - 1.0 && delay <= upper + 1.0,
                "repo {repo} poll {} gave {delay}s, outside [{lower}, {upper}]",
                schedule.polls
            );
            schedule.succeeded();
        }
    }
}

#[test]
fn jitter_actually_spreads_repos_apart() {
    // The point of §7.1's jitter is that twenty repositories on the same interval
    // do not all poll on the same second. A mixer that returned a constant would
    // satisfy the range test above and fail at the only job it has.
    let delays: BTreeSet<u64> = (1..=20_i64)
        .map(|repo| {
            PollSchedule::new(RepoId::new(repo), 600)
                .next_delay()
                .as_secs()
        })
        .collect();

    assert!(
        delays.len() >= 15,
        "20 repos produced only {} distinct delays; jitter is not spreading them",
        delays.len()
    );
}

#[test]
fn jitter_is_derived_so_the_same_repo_and_poll_give_the_same_delay() {
    // ADR 0029: derived, not random. A random source would make the schedule
    // untestable and irreproducible, and `DefaultHasher` is explicitly not stable
    // across Rust releases — a poll schedule that shifts under a compiler upgrade
    // is a bug nobody would ever find.
    let first = PollSchedule::new(RepoId::new(3), 300);
    let second = PollSchedule::new(RepoId::new(3), 300);
    assert_eq!(first.next_delay(), second.next_delay());

    // And it moves with the poll number, so a repo does not land on the same
    // offset forever.
    let mut walked = PollSchedule::new(RepoId::new(3), 300);
    let before = walked.next_delay();
    walked.succeeded();
    let after = walked.next_delay();
    assert_ne!(
        before, after,
        "jitter must vary between polls, not only between repos"
    );
}

#[test]
fn jitter_applies_to_the_backed_off_delay_too() {
    // Otherwise every repository that went down together comes back together —
    // which is the thundering herd, arriving at the worst possible moment.
    let mut schedule = PollSchedule::new(RepoId::new(11), 60);
    for _ in 0..10 {
        schedule.failed("down");
    }

    let base = schedule.base_delay().as_secs_f64();
    let jittered = schedule.next_delay().as_secs_f64();
    assert!(
        (jittered - base).abs() <= base * JITTER_FRACTION + 1.0,
        "jittered {jittered}s is not within 10% of base {base}s"
    );
}
