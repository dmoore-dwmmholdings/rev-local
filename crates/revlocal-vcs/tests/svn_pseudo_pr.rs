//! Pseudo-PR synthesis on branch reintegration (RL-904, SPEC §6.4, decision D6).
//!
//! Two repositories are used, for two different reasons.
//!
//! The **shared fixture** (`svn-basic`, RL-202) carries the reintegration cases
//! deliberately: r10 trips both the mergeinfo and the log-message heuristic, and
//! r13 trips mergeinfo with a message — "Sync work from the y line" — chosen so it
//! does *not* match `merge_detect_regex`. Without r13 a test could not tell which
//! heuristic was doing the work, and heuristic 1 could rot behind heuristic 2 for a
//! year without a single test going red.
//!
//! A **purpose-built** repository covers fork-point determination against a branch
//! with trunk merged into it partway. The shared fixture's branches have no
//! intervening trunk merges, and adding one would change every SHA downstream of it
//! — RL-201 went to some trouble to make those stable.
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use regex::Regex;
use revlocal_vcs::svn::{
    detect, fork_point, gained_branches, mergeinfo_at, parse_log_xml, pseudo_pr_diff,
    pseudo_pr_external_id, Detection, Heuristics, MergeEvidence, MergeInfo, SvnRevision, SvnRunner,
};

/// SPEC §13.2's default, which is what a repository gets unless it overrides it.
const DEFAULT_MERGE_DETECT: &str = r"(?i)\b(merge|reintegrat\w+)\b.*\b(branches?/[\w./-]+)";

fn merge_detect() -> Result<Regex, String> {
    Regex::new(DEFAULT_MERGE_DETECT).map_err(|e| format!("SPEC §13.2's default pattern: {e}"))
}
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

/// Run a command, failing with its output rather than a bare status.
///
/// `svn` gets `--non-interactive` injected, exactly as `SvnRunner` does in
/// production. Without it a prompt — for credentials, for a certificate, for
/// conflict resolution — blocks on stdin that no CI runner will ever answer, and
/// the test does not fail, it *hangs*. A hung job is worse than a failing one: it
/// produces no logs and no signal until the runner's own timeout fires.
fn run(program: &str, args: &[&str], cwd: &Path) -> Result<String, String> {
    let mut full: Vec<&str> = Vec::with_capacity(args.len() + 1);
    full.extend_from_slice(args);
    if program == "svn" {
        full.push("--non-interactive");
    }

    let output = std::process::Command::new(program)
        .args(&full)
        .current_dir(cwd)
        .output()
        .map_err(|e| format!("running {program}: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "{program} {full:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

// --- unit tests: no repository needed --------------------------------------

#[test]
fn the_shipped_default_pattern_actually_compiles() -> Result<(), String> {
    // This caught a real defect. `regex` is built with `default-features = false`,
    // and `(?i)` needs `unicode-case` — which was not in the feature list. SPEC
    // §13.2's own default `merge_detect_regex` opens with `(?i)`, so every SVN
    // repository using the default would have failed to compile its pattern at
    // runtime, in production, on the first reintegration.
    //
    // The constant this file uses is checked against the one the product actually
    // ships, so a test passing here cannot mean a different pattern from the one a
    // repository gets.
    let shipped = revlocal_core::config::RepoConfig::default().merge_detect_regex;
    assert_eq!(shipped, DEFAULT_MERGE_DETECT);
    assert!(
        Regex::new(&shipped).is_ok(),
        "the shipped default must compile"
    );
    Ok(())
}

#[test]
fn mergeinfo_parses_single_revisions_and_ranges() {
    let info = MergeInfo::parse("/branches/feature-x:7-9\n/branches/feature-y:11\n");

    assert_eq!(info.highest_for("/branches/feature-x"), Some(9));
    assert_eq!(info.highest_for("/branches/feature-y"), Some(11));
    assert_eq!(info.highest_for("/branches/nope"), None);
}

#[test]
fn a_non_inheritable_merge_is_still_a_merge() {
    // svn writes `*` on a non-inheritable range. Refusing to parse one would turn
    // an ordinary repository into an unreviewable one.
    let info = MergeInfo::parse("/branches/feature-x:7-9*\n");

    assert_eq!(info.highest_for("/branches/feature-x"), Some(9));
}

#[test]
fn only_growth_counts_as_a_reintegration() {
    let before = MergeInfo::parse("/branches/feature-x:7-9\n");
    let grew = MergeInfo::parse("/branches/feature-x:7-12\n");
    let shrank = MergeInfo::parse("/branches/feature-x:7-8\n");

    assert_eq!(gained_branches(&before, &grew).len(), 1);
    // Mergeinfo that *loses* ranges is somebody editing history. Treating that as
    // a reintegration would invent a change out of a correction.
    assert!(gained_branches(&before, &shrank).is_empty());
    assert!(gained_branches(&before, &before).is_empty());
}

/// A revision built for a heuristic test, with no repository behind it.
fn revision(number: u64, message: &str, files: usize) -> SvnRevision {
    SvnRevision {
        revision: number,
        author: Some("dmoore".to_owned()),
        date: None,
        message: message.to_owned(),
        paths: (0..files)
            .map(|i| revlocal_vcs::svn::SvnPath {
                action: "M".to_owned(),
                kind: Some("file".to_owned()),
                path: format!("/trunk/src/file{i}.rs"),
                copyfrom_path: None,
                copyfrom_rev: None,
            })
            .collect(),
    }
}

#[test]
fn heuristic_one_fires_with_the_others_disabled() -> Result<(), String> {
    let before = MergeInfo::default();
    let after = MergeInfo::parse("/branches/feature-y:11-12\n");
    // A message that matches nothing and one file, so neither other heuristic
    // could be the one answering.
    let rev = revision(13, "Sync work from the y line", 1);

    let found = detect(
        &rev,
        &before,
        &after,
        &merge_detect()?,
        &[],
        Heuristics::only_mergeinfo(),
        &MergeEvidence::new("/trunk"),
    );

    assert_eq!(
        found,
        Some(Detection::MergeInfo {
            branch: "/branches/feature-y".to_owned(),
            through: 12,
        })
    );
    Ok(())
}

#[test]
fn heuristic_two_fires_with_the_others_disabled() -> Result<(), String> {
    // No mergeinfo movement at all, and one file.
    let rev = revision(10, "Merge branches/feature-x into trunk", 1);

    let found = detect(
        &rev,
        &MergeInfo::default(),
        &MergeInfo::default(),
        &merge_detect()?,
        &[],
        Heuristics::only_log_message(),
        &MergeEvidence::new("/trunk"),
    );

    assert_eq!(
        found,
        Some(Detection::LogMessage {
            branch: "/branches/feature-x".to_owned(),
        })
    );
    Ok(())
}

#[test]
fn heuristic_three_fires_with_the_others_disabled() -> Result<(), String> {
    // §6.4 heuristic 3 requires the message to name a branch *path*, not just a
    // branch's leaf name — a bare "feature-z" is how people refer to work, and
    // treating that as evidence of a merge would fire on half a team's commits.
    //
    // The message deliberately avoids any merge word, so heuristic 2 could not be
    // credited even if it were enabled.
    let rev = revision(20, "Bring in the work from branches/feature-z", 6);
    let existing = vec!["/branches/feature-z".to_owned()];

    let found = detect(
        &rev,
        &MergeInfo::default(),
        &MergeInfo::default(),
        &merge_detect()?,
        &existing,
        Heuristics::only_file_count(5),
        &MergeEvidence::new("/trunk"),
    );

    assert_eq!(
        found,
        Some(Detection::FileCountAndName {
            branch: "/branches/feature-z".to_owned(),
            files: 6,
        })
    );
    Ok(())
}

#[test]
fn heuristic_three_needs_a_branch_that_exists() -> Result<(), String> {
    // Enough files, and a message naming a branch path — but the branch is not
    // there. A message naming a branch that does not exist is somebody talking
    // about a plan, not recording a merge.
    let rev = revision(20, "Bring in the work from branches/feature-z", 6);

    let found = detect(
        &rev,
        &MergeInfo::default(),
        &MergeInfo::default(),
        &merge_detect()?,
        &[],
        Heuristics::only_file_count(5),
        &MergeEvidence::new("/trunk"),
    );

    assert_eq!(found, None);
    Ok(())
}

#[test]
fn a_commit_that_merely_says_merge_does_not_invent_a_change() -> Result<(), String> {
    // Criterion 5. Being wrong is cheap in one direction and not the other: a
    // false positive invents a change that never happened, files findings against
    // it, and — once RL-905 lands — demotes the real reviews in its favour.
    for message in [
        "Merge the config files by hand",
        "merge sort is faster here",
        "Revert the merge that broke CI",
        "reintegrate the settings dialog",
    ] {
        let rev = revision(42, message, 8);
        let found = detect(
            &rev,
            &MergeInfo::default(),
            &MergeInfo::default(),
            &merge_detect()?,
            &[],
            Heuristics::default(),
            &MergeEvidence::new("/trunk"),
        );
        assert_eq!(found, None, "{message:?} should not produce a pseudo-PR");
    }
    Ok(())
}

#[test]
fn the_external_id_is_the_one_spec_names() {
    // §6.4: `external_id = "{branch}@r{rev}"`.
    assert_eq!(
        pseudo_pr_external_id("/branches/feature-x", 10),
        "/branches/feature-x@r10"
    );
}

// --- against the shared fixture --------------------------------------------

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

/// The mergeinfo on `/trunk` immediately before and at `revision`.
async fn mergeinfo_around(
    runner: &SvnRunner,
    url: &str,
    revision: u64,
) -> Result<(MergeInfo, MergeInfo), String> {
    let before = mergeinfo_at(runner, url, "/trunk", revision - 1)
        .await
        .map_err(|e| e.to_string())?;
    let after = mergeinfo_at(runner, url, "/trunk", revision)
        .await
        .map_err(|e| e.to_string())?;
    Ok((before, after))
}

/// One revision from the fixture, by number.
async fn fixture_revision(
    runner: &SvnRunner,
    url: &str,
    number: u64,
) -> Result<SvnRevision, String> {
    let range = format!("{number}:{number}");
    let output = runner
        .run(Path::new("."), &["log", "--xml", "-v", "-r", &range, url])
        .await
        .map_err(|e| e.to_string())?;
    parse_log_xml(&output.stdout)
        .map_err(|e| e.to_string())?
        .into_iter()
        .next()
        .ok_or_else(|| format!("r{number} not found"))
}

#[test]
fn the_reintegration_revision_produces_both_kinds_of_change() -> Result<(), String> {
    if !svn_is_installed() {
        note_skipped("the_reintegration_revision_produces_both_kinds_of_change");
        return Ok(());
    }
    let Some(url) = fixture_repo_url()? else {
        return Ok(());
    };

    tokio::runtime::Runtime::new()
        .map_err(|e| e.to_string())?
        .block_on(async {
            let runner = SvnRunner::default();
            let rev = fixture_revision(&runner, &url, 10).await?;

            // The per-revision change exists regardless — §6.4 says the pseudo-PR
            // is *additional*, not instead of.
            assert_eq!(rev.external_id(), "r10");

            let (before, after) = mergeinfo_around(&runner, &url, 10).await?;
            let found = detect(
                &rev,
                &before,
                &after,
                &merge_detect()?,
                &[],
                Heuristics::default(),
                &MergeEvidence::new("/trunk"),
            )
            .ok_or("r10 is the fixture's reintegration and must be detected")?;

            assert_eq!(found.branch(), "/branches/feature-x");
            assert_eq!(
                pseudo_pr_external_id(found.branch(), 10),
                "/branches/feature-x@r10"
            );
            Ok(())
        })
}

#[test]
fn the_mergeinfo_only_revision_is_detected_without_the_log_message() -> Result<(), String> {
    if !svn_is_installed() {
        note_skipped("the_mergeinfo_only_revision_is_detected_without_the_log_message");
        return Ok(());
    }
    let Some(url) = fixture_repo_url()? else {
        return Ok(());
    };

    tokio::runtime::Runtime::new()
        .map_err(|e| e.to_string())?
        .block_on(async {
            let runner = SvnRunner::default();
            let rev = fixture_revision(&runner, &url, 13).await?;

            // The fixture chose this message so the regex would not match it.
            assert!(
                merge_detect()?.captures(&rev.message).is_none(),
                "r13's message {:?} must not match, or it proves nothing",
                rev.message
            );

            let (before, after) = mergeinfo_around(&runner, &url, 13).await?;
            let found = detect(
                &rev,
                &before,
                &after,
                &merge_detect()?,
                &[],
                Heuristics::only_mergeinfo(),
                &MergeEvidence::new("/trunk"),
            )
            .ok_or("r13 must be detected by mergeinfo alone")?;

            assert_eq!(found.branch(), "/branches/feature-y");
            Ok(())
        })
}

#[test]
fn an_ordinary_revision_is_not_a_reintegration() -> Result<(), String> {
    if !svn_is_installed() {
        note_skipped("an_ordinary_revision_is_not_a_reintegration");
        return Ok(());
    }
    let Some(url) = fixture_repo_url()? else {
        return Ok(());
    };

    tokio::runtime::Runtime::new()
        .map_err(|e| e.to_string())?
        .block_on(async {
            let runner = SvnRunner::default();
            // r4 is the fixture's planted off-by-one: ordinary work on trunk.
            let rev = fixture_revision(&runner, &url, 4).await?;
            let (before, after) = mergeinfo_around(&runner, &url, 4).await?;

            assert_eq!(
                detect(
                    &rev,
                    &before,
                    &after,
                    &merge_detect()?,
                    &[],
                    Heuristics::default(),
                    &MergeEvidence::new("/trunk"),
                ),
                None
            );
            Ok(())
        })
}

#[test]
fn the_pseudo_pr_diff_is_the_branch_not_the_merge_revision() -> Result<(), String> {
    if !svn_is_installed() {
        note_skipped("the_pseudo_pr_diff_is_the_branch_not_the_merge_revision");
        return Ok(());
    }
    let Some(url) = fixture_repo_url()? else {
        return Ok(());
    };

    tokio::runtime::Runtime::new()
        .map_err(|e| e.to_string())?
        .block_on(async {
            let runner = SvnRunner::default();

            // The fixture's branch was created at r7 from trunk@6.
            let fork = fork_point(&runner, &url, "/branches/feature-x")
                .await
                .map_err(|e| e.to_string())?
                .ok_or("feature-x must have a fork point")?;
            assert_eq!(fork, 6, "branches/feature-x was copied from trunk@6");

            let pseudo = pseudo_pr_diff(&runner, &url, "/trunk", "/branches/feature-x", fork, 10)
                .await
                .map_err(|e| e.to_string())?;

            // The merge revision's own diff, which is what reviewing r10 alone
            // would show.
            let merge_rev = runner
                .run(Path::new("."), &["diff", "-c", "10", &url])
                .await
                .map_err(|e| e.to_string())?
                .stdout;

            assert!(!pseudo.is_empty(), "the branch diff must not be empty");
            assert_ne!(
                pseudo, merge_rev,
                "the whole point of a pseudo-PR is that it is not the merge revision's diff"
            );

            // The branch's work: r8 added `page_count`, r9 added paging notes.
            assert!(
                pseudo.contains("page_count"),
                "the branch diff must contain the branch's work, got:\n{pseudo}"
            );
            Ok(())
        })
}

// --- fork point against a branch with trunk merged into it ------------------

/// A repository whose branch has trunk merged into it partway.
///
/// r1 trunk, r2 trunk moves, r3 copies the branch from trunk@2, r4 trunk moves
/// again, r5 branch work, r6 merges trunk into the branch, r7 more branch work.
/// The fork point is **2** — not r1, which is what taking the oldest revision of
/// an unrestricted log would give, and not r6, which is what mistaking the
/// intervening merge for the copy would give.
fn branch_with_intervening_merge(dir: &Path) -> Result<(String, PathBuf), String> {
    let repo = dir.join("repo");
    let wc = dir.join("wc");
    run("svnadmin", &["create", &repo.display().to_string()], dir)?;

    // Three slashes so a Windows drive letter reads as a path, not a host.
    let url = format!(
        "file:///{}",
        repo.display().to_string().trim_start_matches('/')
    );

    run(
        "svn",
        &[
            "mkdir",
            "--quiet",
            "-m",
            "layout",
            &format!("{url}/trunk"),
            &format!("{url}/branches"),
        ],
        dir,
    )?;
    run(
        "svn",
        &["checkout", "--quiet", &url, &wc.display().to_string()],
        dir,
    )?;

    let trunk = wc.join("trunk");

    // r2: trunk gets a file.
    std::fs::write(trunk.join("a.txt"), "one\n").map_err(|e| e.to_string())?;
    run("svn", &["add", "--quiet", "a.txt"], &trunk)?;
    run("svn", &["commit", "--quiet", "-m", "Add a.txt"], &wc)?;

    // r3: the branch is copied from trunk as it stands at r2.
    run(
        "svn",
        &[
            "copy",
            "--quiet",
            "-m",
            "Create branches/feat from trunk",
            &format!("{url}/trunk"),
            &format!("{url}/branches/feat"),
        ],
        dir,
    )?;

    // r4: trunk moves on, so the branch is genuinely behind.
    std::fs::write(trunk.join("b.txt"), "two\n").map_err(|e| e.to_string())?;
    run("svn", &["add", "--quiet", "b.txt"], &trunk)?;
    run(
        "svn",
        &["commit", "--quiet", "-m", "Add b.txt on trunk"],
        &wc,
    )?;

    run("svn", &["update", "--quiet"], &wc)?;
    let branch = wc.join("branches").join("feat");

    // r5: work on the branch.
    std::fs::write(branch.join("c.txt"), "three\n").map_err(|e| e.to_string())?;
    run("svn", &["add", "--quiet", "c.txt"], &branch)?;
    run(
        "svn",
        &["commit", "--quiet", "-m", "Add c.txt on the branch"],
        &wc,
    )?;

    // r6: the intervening merge — trunk into the branch. This is the revision a
    // naive fork-point search mistakes for the copy. svn refuses to merge into a
    // mixed-revision working copy, which is what committing one subtree leaves.
    run("svn", &["update", "--quiet"], &wc)?;
    run(
        "svn",
        &["merge", "--quiet", &format!("{url}/trunk")],
        &branch,
    )?;
    run(
        "svn",
        &["commit", "--quiet", "-m", "Merge trunk into branches/feat"],
        &wc,
    )?;

    // r7: more branch work after the merge.
    std::fs::write(branch.join("d.txt"), "four\n").map_err(|e| e.to_string())?;
    run("svn", &["add", "--quiet", "d.txt"], &branch)?;
    run(
        "svn",
        &["commit", "--quiet", "-m", "Add d.txt on the branch"],
        &wc,
    )?;

    Ok((url, wc))
}

#[test]
fn the_fork_point_survives_an_intervening_trunk_merge() -> Result<(), String> {
    if !svn_is_installed() {
        note_skipped("the_fork_point_survives_an_intervening_trunk_merge");
        return Ok(());
    }

    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let (url, _wc) = branch_with_intervening_merge(dir.path())?;

    tokio::runtime::Runtime::new()
        .map_err(|e| e.to_string())?
        .block_on(async {
            let runner = SvnRunner::default();
            let fork = fork_point(&runner, &url, "/branches/feat")
                .await
                .map_err(|e| e.to_string())?
                .ok_or("the branch must have a fork point")?;

            assert_eq!(
                fork, 2,
                "the branch was copied from trunk@2; r1 is trunk's own history \
                 and r6 is the intervening merge"
            );

            // And the resulting diff is against trunk as it was at the fork, so it
            // contains the branch's work and not trunk's r4.
            let diff = pseudo_pr_diff(&runner, &url, "/trunk", "/branches/feat", fork, 7)
                .await
                .map_err(|e| e.to_string())?;
            assert!(
                diff.contains("c.txt"),
                "branch work must be present:\n{diff}"
            );
            assert!(
                diff.contains("d.txt"),
                "post-merge branch work too:\n{diff}"
            );
            Ok(())
        })
}
