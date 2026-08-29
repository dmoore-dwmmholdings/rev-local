//! Pseudo-PR authority and demotion (RL-905, SPEC §6.4).
//!
//! The three acceptance criteria are three ways of asking the same question — what
//! happens to a defect that two reviews both found — and they pull in opposite
//! directions on purpose. File it once (criterion 1), but do not lose the second
//! copy (criterion 3). Demotion is what satisfies both: the row survives, and it
//! stops being something a human is asked to act on twice.
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use revlocal_core::{Category, Finding, FindingId, FindingState, RunId, Severity, Timestamp};
use revlocal_vcs::svn::{constituent_revisions, plan, prior_context, Disposition, SvnRunner};
/// Say out loud that a test verified nothing on this machine.
///
/// A green "N passed" on a box without Subversion reads as coverage it does not
/// have. `svn_fixtures.rs` has said this since RL-202 and these files were written
/// without it; REVL-106's third criterion — no test skipped without an explicit
/// documented reason — is what caught the omission.
///
/// Visible with `--nocapture`. CI installs Subversion on all three runners, so
/// this path is not taken there; it is the developer machine, and the case where
/// the install step silently fails, that this exists for.
fn note_skipped(test: &str) {
    println!("SKIPPED (svn not installed, nothing verified): {test}");
}

fn svn_is_installed() -> bool {
    std::process::Command::new("svn")
        .args(["--version", "--quiet"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A finding with only the fields this item cares about set.
fn finding(fingerprint: &str, severity: Severity, title: &str) -> Finding {
    Finding {
        id: FindingId::new(1),
        run_id: RunId::new(1),
        fingerprint: fingerprint.to_owned(),
        severity,
        category: Category::Correctness,
        confidence: 0.9,
        file: Some("src/pager.rs".to_owned()),
        line_start: Some(10),
        line_end: Some(12),
        title: title.to_owned(),
        body: "…".to_owned(),
        failure_scenario: None,
        suggested_fix: None,
        state: FindingState::Open,
        created_at: Timestamp::default(),
    }
}

#[test]
fn a_defect_found_on_both_is_filed_once() {
    // Criterion 1. The branch revision r8 found it; so did the pseudo-PR.
    let shared = "fp-off-by-one";
    let pseudo = vec![finding(shared, Severity::High, "off-by-one in page_count")];

    let mut constituents = BTreeMap::new();
    constituents.insert(
        8,
        vec![finding(shared, Severity::High, "off-by-one in page_count")],
    );

    let built = plan("/branches/feature-x@r10", &pseudo, &constituents);

    // Exactly one copy is filed at a severity a human is asked to act on.
    let actionable: Vec<_> = built
        .all()
        .filter(|planned| planned.effective_severity > Severity::Info)
        .collect();
    assert_eq!(actionable.len(), 1, "the same defect must be filed once");
    assert_eq!(
        actionable[0].revision, None,
        "the pseudo-PR's copy is the one kept"
    );
}

#[test]
fn the_demoted_copy_is_still_there() {
    // Criterion 3. "Filed once" must not become "the other one vanished" — §18.
    let shared = "fp-off-by-one";
    let pseudo = vec![finding(shared, Severity::High, "off-by-one in page_count")];
    let mut constituents = BTreeMap::new();
    constituents.insert(
        8,
        vec![finding(shared, Severity::High, "off-by-one in page_count")],
    );

    let built = plan("/branches/feature-x@r10", &pseudo, &constituents);

    assert_eq!(built.constituent_findings.len(), 1);
    let demoted = &built.constituent_findings[0];

    assert_eq!(demoted.effective_severity, Severity::Info);
    // The original severity is kept, so "why is this only an info?" has an answer.
    assert_eq!(demoted.original_severity, Severity::High);

    match &demoted.disposition {
        Disposition::Demoted {
            superseded_by,
            reason,
        } => {
            assert_eq!(superseded_by, "/branches/feature-x@r10");
            assert!(reason.contains("authoritative"), "reason was {reason:?}");
        }
        Disposition::Filed => panic!("a duplicate must be demoted, not filed"),
    }

    // And it is visible in the plan a human reads, not only in a struct field.
    let lines = built.summary_lines().join("\n");
    assert!(
        lines.contains("1 of 1 per-revision finding(s) demoted"),
        "{lines}"
    );
    assert!(lines.contains("high -> info"), "{lines}");
}

#[test]
fn a_branch_finding_the_pseudo_pr_did_not_see_keeps_its_severity() {
    // Found on r8, absent from the branch-vs-trunk diff — usually because r9 fixed
    // it. "We found this and you fixed it" is a different statement from "we found
    // this twice", and demoting it would blur the two.
    let pseudo = vec![finding("fp-still-there", Severity::Medium, "unused import")];
    let mut constituents = BTreeMap::new();
    constituents.insert(
        8,
        vec![finding("fp-fixed-later", Severity::High, "null deref")],
    );

    let built = plan("/branches/feature-x@r10", &pseudo, &constituents);

    assert_eq!(built.demoted_count(), 0);
    assert_eq!(
        built.constituent_findings[0].effective_severity,
        Severity::High
    );
    assert_eq!(
        built.constituent_findings[0].disposition,
        Disposition::Filed
    );
}

#[test]
fn nothing_is_dropped_on_the_way_through() {
    // The property that makes this a plan rather than a filter: every finding that
    // went in comes out, so a caller cannot publish a subset by forgetting a match
    // arm.
    let pseudo = vec![
        finding("a", Severity::High, "a"),
        finding("b", Severity::Low, "b"),
    ];
    let mut constituents = BTreeMap::new();
    constituents.insert(8, vec![finding("a", Severity::High, "a")]);
    constituents.insert(9, vec![finding("c", Severity::Medium, "c")]);

    let built = plan("/branches/feature-x@r10", &pseudo, &constituents);

    assert_eq!(built.all().count(), 4);
    assert_eq!(built.demoted_count(), 1);
}

#[test]
fn prior_context_deduplicates_and_keeps_the_worst_sighting() {
    // Criterion 2's input. The same defect on three consecutive revisions is one
    // piece of prior context, not three — and if one sighting called it `high`,
    // that is the one the engine should be told about.
    let mut constituents = BTreeMap::new();
    constituents.insert(8, vec![finding("dup", Severity::Low, "same defect")]);
    constituents.insert(9, vec![finding("dup", Severity::High, "same defect")]);
    constituents.insert(
        10,
        vec![
            finding("dup", Severity::Medium, "same defect"),
            finding("other", Severity::Low, "another"),
        ],
    );

    let context = prior_context(&constituents);

    assert_eq!(
        context.len(),
        2,
        "three sightings of one defect are one context entry"
    );
    let dup = context
        .iter()
        .find(|f| f.fingerprint == "dup")
        .expect("the deduplicated finding must be present");
    assert_eq!(dup.severity, Severity::High);
}

#[test]
fn prior_context_is_ordered_the_same_way_every_time() {
    // ADR 0024: the prompt has to be byte-identical across runs, and a HashMap
    // iteration order would make it not be.
    let mut constituents = BTreeMap::new();
    constituents.insert(8, vec![finding("zeta", Severity::Low, "z")]);
    constituents.insert(9, vec![finding("alpha", Severity::Low, "a")]);
    constituents.insert(10, vec![finding("mid", Severity::Low, "m")]);

    let once: Vec<String> = prior_context(&constituents)
        .into_iter()
        .map(|f| f.fingerprint)
        .collect();
    let twice: Vec<String> = prior_context(&constituents)
        .into_iter()
        .map(|f| f.fingerprint)
        .collect();

    assert_eq!(once, twice);
    assert_eq!(once, vec!["alpha", "mid", "zeta"]);
}

#[test]
fn the_constituents_are_the_branchs_own_revisions() -> Result<(), String> {
    if !svn_is_installed() {
        note_skipped("the_constituents_are_the_branchs_own_revisions");
        return Ok(());
    }

    let Some(url) = fixture_repo_url()? else {
        return Ok(());
    };
    let url = url.as_str();

    tokio::runtime::Runtime::new()
        .map_err(|e| e.to_string())?
        .block_on(async {
            let runner = SvnRunner::default();
            let revisions = constituent_revisions(&runner, url, "/branches/feature-x", 10)
                .await
                .map_err(|e| e.to_string())?;

            // r7 created the branch — trunk's content at the fork, not the branch's
            // work. r8 and r9 are the branch's own revisions.
            assert_eq!(
                revisions,
                vec![8, 9],
                "the copy revision is not a constituent, and trunk's history is not either"
            );
            let _ = Path::new(".");
            Ok(())
        })
}

/// The generated `svn-basic` fixture.
///
/// Returns `None` only when the fixture genuinely could not be built — which on a
/// machine with Subversion installed is never. **A stale manifest saying
/// `skipped: true` is an error, not a reason to pass.**
///
/// This is not hypothetical caution. The manifest is written once and reused, so a
/// fixture generated before Subversion was installed keeps reporting `skipped`
/// forever; every fixture-backed test then returns early and passes having checked
/// nothing. `svn_fixtures.rs` already guards this at the manifest level, and the
/// guard belongs here too — a helper that turns "the fixture is missing" into
/// "nothing to verify" makes every test that calls it silently optional.
fn fixture_repo_url() -> Result<Option<String>, String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");
    let manifest = root.join("fixtures/out/svn-basic/.manifest.json");

    let read = |path: &std::path::Path| -> Result<serde_json::Value, String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        serde_json::from_str(&text).map_err(|e| e.to_string())
    };
    let build = || -> Result<(), String> {
        // `bash_program()`, not `"bash"`. On Windows `bash` on PATH is the WSL
        // launcher, which on a machine with no distribution prints its refusal to
        // *stdout*, in UTF-16, and exits 1. RL-102 hit this and wrote the helper;
        // these files were written afterwards and did not use it.
        //
        // CI reported `build.sh failed: ` with an empty message, because the
        // message was on the stream this did not read — the exact failure
        // `bash_program`'s own doc comment describes.
        let output = std::process::Command::new(revlocal_vcs::bash_program())
            .arg(root.join("fixtures/build.sh"))
            .current_dir(&root)
            .output()
            .map_err(|e| format!("running build.sh: {e}"))?;
        if !output.status.success() {
            // Both streams. A harness that reports only stderr is how the WSL
            // message stayed invisible for a whole milestone.
            return Err(format!(
                "build.sh failed ({}):\n  stdout: {}\n  stderr: {}",
                output.status,
                String::from_utf8_lossy(&output.stdout).trim(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(())
    };

    if !manifest.exists() {
        build()?;
    }
    let mut parsed = read(&manifest)?;

    // A manifest that says it skipped may simply predate Subversion being
    // installed. Rebuild once before believing it.
    if parsed["skipped"].as_bool().unwrap_or(false) && svn_is_installed() {
        build()?;
        parsed = read(&manifest)?;
    }

    if parsed["skipped"].as_bool().unwrap_or(false) {
        if svn_is_installed() {
            return Err(format!(
                "svn is installed but the fixture still reports skipped ({}); \
                 these tests would otherwise pass having verified nothing",
                parsed["reason"].as_str().unwrap_or("no reason given")
            ));
        }
        return Ok(None);
    }

    parsed["repo_url"]
        .as_str()
        .map(str::to_owned)
        .map(Some)
        .ok_or_else(|| "a non-skipped manifest must carry repo_url".to_owned())
}
