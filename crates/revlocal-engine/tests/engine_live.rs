//! The live-engine acceptance suite (RL-1203, SPEC §16.1).
//!
//! **Every test here invokes a real engine and spends real credits.**
//!
//! # Gated twice, on purpose
//!
//! 1. `#[cfg(feature = "engine-live")]` — off by default.
//! 2. `#[ignore]` — so even `cargo test --features engine-live` leaves them alone.
//!
//! Only the item's own gate runs them:
//!
//! ```text
//! cargo test --features engine-live -- --ignored
//! ```
//!
//! One gate would have been enough to stop CI. Two is because the person most
//! likely to run this by accident is whoever is working on it, on a laptop, with
//! the feature already enabled from the last command.
//!
//! # What this suite is for
//!
//! Everything else in the workspace is tested against the mock engine, which
//! reports counts it invents and emits findings it was told to emit. That makes the
//! fixtures more honest than the thing they stand in for, and it is why a real
//! engine has never been asked whether it can do the job.
//!
//! The fixture plants a SQL injection in `src/db.rs`:
//!
//! ```text
//! let sql = format!("SELECT id, email FROM users WHERE name = '{}'", name);
//! ```
//!
//! An engine that misses that is not doing code review, whatever its output
//! validates against.

#![cfg(feature = "engine-live")]

use revlocal_core::EngineKind;
use revlocal_engine::live::readiness;

/// The fixture tree with the planted bugs.
fn fixture() -> std::path::PathBuf {
    let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.pop();
    path.pop();
    path.join("fixtures/out/git-basic")
}

/// Skip with a reason, or hand back the engine's path.
///
/// The `Option` is the point: a test that cannot run must return early having
/// *said* so. §16.1's fourth criterion, and §18's rule that a skip which reads as
/// a pass is worse than a failure.
fn ready_or_skip(engine: EngineKind, test: &str) -> Option<std::path::PathBuf> {
    let readiness = readiness(engine);
    match readiness {
        revlocal_engine::live::Readiness::Ready { binary } => Some(binary),
        ref skip => {
            if let Some(line) = skip.skip_line(test) {
                println!("{line}");
            }
            None
        }
    }
}

/// The fixture must exist before any engine is paid to look at it.
fn fixture_or_skip(test: &str) -> Option<std::path::PathBuf> {
    let dir = fixture();
    if dir.join("src/db.rs").is_file() {
        return Some(dir);
    }
    println!(
        "SKIPPED (fixtures/out/git-basic is not built, nothing verified): {test}\n  \
         try: ./fixtures/build.sh"
    );
    None
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "invokes a real engine and spends credits; run with --ignored"]
async fn live_claude_finds_the_planted_sql_injection() {
    let test = "live_claude_finds_the_planted_sql_injection";
    let Some(_binary) = ready_or_skip(EngineKind::Claude, test) else {
        return;
    };
    let Some(dir) = fixture_or_skip(test) else {
        return;
    };

    let _ = dir;
    // Deliberately unfinished. Writing the invocation without ever running it
    // would be writing a string match against a tool's output without reading
    // what the tool says — ADR 0023, which this project has been bitten by.
    //
    // What is settled: the double gate, the skip messages, the fixture check and
    // the assertion this must eventually make (a finding whose file is
    // `src/db.rs` and whose category is injection-shaped). What is not settled is
    // the exact shape `claude --output-format json` returns, and that needs one
    // real invocation to read before anything is asserted about it.
    //
    // Tracked on REVL-100 alongside REVL-115, which needs the same payload.
    panic!(
        "not implemented: this test needs one captured `claude --output-format \
         json` payload to assert against — see REVL-100"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "invokes a real engine and spends credits; run with --ignored"]
async fn live_codex_finds_the_planted_sql_injection() {
    let test = "live_codex_finds_the_planted_sql_injection";
    let Some(_binary) = ready_or_skip(EngineKind::Codex, test) else {
        return;
    };
    let Some(dir) = fixture_or_skip(test) else {
        return;
    };

    let _ = dir;
    panic!(
        "not implemented: this test needs one captured `codex exec --json` \
         payload to assert against — see REVL-100 and REVL-45"
    );
}

/// The suite says what it would cost before anybody runs it.
///
/// Not `#[ignore]`d and does not invoke anything: it reports which engines are
/// present, so `--ignored` is a decision made with the list in front of you rather
/// than a surprise bill.
#[test]
fn live_the_suite_reports_which_engines_it_would_invoke() {
    let mut ready = Vec::new();
    for engine in [EngineKind::Claude, EngineKind::Codex] {
        let readiness = readiness(engine);
        if readiness.is_ready() {
            ready.push(engine.as_str());
        } else if let Some(line) = readiness.skip_line("live suite") {
            println!("{line}");
        }
    }
    println!(
        "live engine suite would invoke: {}",
        if ready.is_empty() {
            "nothing".to_owned()
        } else {
            ready.join(", ")
        }
    );
}
