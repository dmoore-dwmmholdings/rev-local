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

/// The prompt both engines get.
///
/// **Inlines `RESULT_SCHEMA_V1`**, which is the whole difference between this
/// working and not. The first version of this test described the shape in prose
/// and omitted `schema_version` — a required field — so both engines produced a
/// well-formed review that failed validation, and the ladder reported "no
/// parseable output" for a run that had found the bug.
///
/// That was a defect in the test, not the product: §9.2's assembly inlines the
/// same schema and there is a test asserting it does. Which is the point of using
/// the real constant here rather than a description of it — a prompt written from
/// memory tests the memory.
fn review_prompt(out_dir: &std::path::Path) -> String {
    format!(
        "Review the Rust code in this repository for defects.\n\n\
         Write your findings as JSON to {}, conforming exactly to this schema:\n\n\
         {}\n\n\
         Report every real defect you find. Write only that file; do not modify \
         any source.",
        out_dir.join("result.json").display(),
        revlocal_engine::RESULT_SCHEMA_V1
    )
}

/// Run one engine over the fixture and hand back what it said.
async fn review_the_fixture(
    engine: revlocal_engine::CliEngine,
    dir: &std::path::Path,
    out: &std::path::Path,
) -> Result<revlocal_engine::EngineOutcome, String> {
    use revlocal_engine::Engine as _;

    let task = revlocal_engine::EngineTask {
        cwd: dir.to_path_buf(),
        out_dir: out.to_path_buf(),
        prompt: review_prompt(out),
        attachments: Vec::new(),
        // Generous: a real review of a real tree is not a probe, and a timeout
        // that fired would look like the engine failing rather than being slow.
        timeout: std::time::Duration::from_secs(300),
        depth: revlocal_core::Depth::Standard,
    };

    engine
        .run(task, tokio_util::sync::CancellationToken::new())
        .await
        .map_err(|e| e.to_string())
}

/// Whether any finding points at the planted SQL injection.
///
/// Matched on the **file**, not on wording. Asserting that a model used the phrase
/// "SQL injection" would be testing its vocabulary; what matters is that it looked
/// at `src/db.rs` and called something there a defect.
fn found_the_planted_bug(outcome: &revlocal_engine::EngineOutcome) -> bool {
    outcome.findings.iter().any(|finding| {
        finding
            .file
            .as_deref()
            .is_some_and(|file| file.ends_with("db.rs"))
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "invokes a real engine and spends credits; run with --ignored"]
async fn live_claude_finds_the_planted_sql_injection() -> Result<(), String> {
    let test = "live_claude_finds_the_planted_sql_injection";
    let Some(_binary) = ready_or_skip(EngineKind::Claude, test) else {
        return Ok(());
    };
    let Some(dir) = fixture_or_skip(test) else {
        return Ok(());
    };

    let out = tempfile::tempdir().map_err(|e| e.to_string())?;
    let outcome =
        review_the_fixture(revlocal_engine::CliEngine::claude(), &dir, out.path()).await?;

    assert!(
        found_the_planted_bug(&outcome),
        "claude reviewed the fixture and did not flag src/db.rs, where the SQL \
         injection is planted. Findings: {:?}",
        outcome.findings
    );

    // RL-409: a real run must report what it spent. This is the assertion that
    // would have caught the 99.99% undercount had it been written first.
    assert!(
        outcome.usage.tokens_are_known(),
        "a live run must report its usage"
    );
    assert!(
        outcome.usage.total_tokens() > 0,
        "a review that spent nothing did not happen"
    );

    println!(
        "claude: {} finding(s), {} tokens, {}",
        outcome.findings.len(),
        outcome.usage.total_tokens(),
        outcome
            .usage
            .cost_usd
            .map_or_else(|| "unpriced".to_owned(), |c| format!("${c:.4}"))
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "invokes a real engine and spends credits; run with --ignored"]
async fn live_codex_finds_the_planted_sql_injection() -> Result<(), String> {
    let test = "live_codex_finds_the_planted_sql_injection";
    let Some(_binary) = ready_or_skip(EngineKind::Codex, test) else {
        return Ok(());
    };
    let Some(dir) = fixture_or_skip(test) else {
        return Ok(());
    };

    let out = tempfile::tempdir().map_err(|e| e.to_string())?;
    let outcome = review_the_fixture(revlocal_engine::CliEngine::codex(), &dir, out.path()).await?;

    assert!(
        found_the_planted_bug(&outcome),
        "codex reviewed the fixture and did not flag src/db.rs, where the SQL \
         injection is planted. Findings: {:?}",
        outcome.findings
    );
    assert!(
        outcome.usage.tokens_are_known(),
        "a live run must report its usage"
    );

    println!(
        "codex: {} finding(s), {} tokens",
        outcome.findings.len(),
        outcome.usage.total_tokens()
    );
    Ok(())
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
