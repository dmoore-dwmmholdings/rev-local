//! Which merge styles heuristic 1 actually catches (RL-906, SPEC §6.4).
//!
//! SPEC §6.4 states heuristic 1 as "`svn:mergeinfo` on the target path gained
//! ranges from a branch path". Measured against Subversion 1.14.5, that is true of
//! **four** merge styles and only one of them is a reintegration:
//!
//! | style | mergeinfo gained | content | source | reaches branch head |
//! |---|---|---|---|---|
//! | reintegrate | `/branches/reint:3-8` | yes | a branch | yes |
//! | sync merge | `/trunk:4-9` | yes | **trunk** | n/a |
//! | cherry-pick | `/branches/cherry:7` | yes | a branch | **no** |
//! | `--record-only` | `/branches/recordonly:8` | **no** | a branch | yes |
//!
//! This file builds exactly that repository and asserts the classification, so the
//! table above is a measurement rather than a claim. It runs against whatever
//! Subversion is on the machine — 1.14 locally, 1.8.15 on the Windows CI runner —
//! which is the cross-version half of the spike.
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use std::path::Path;
use std::process::Stdio;

use revlocal_vcs::svn::{classify_gain, gained_branches, MergeEvidence, MergeInfo, MergeStyle};

fn svn_is_installed() -> bool {
    ["svn", "svnadmin"].iter().all(|tool| {
        std::process::Command::new(tool)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

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

/// `svn:mergeinfo` on a path at a revision, or empty.
fn mergeinfo(url: &str, path: &str, revision: u64) -> MergeInfo {
    let target = format!("{url}{path}@{revision}");
    std::process::Command::new("svn")
        .args(["propget", "svn:mergeinfo", &target])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| MergeInfo::parse(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

/// A repository exercising all four merge styles.
///
/// r9 reintegrates, r10 sync-merges trunk into a branch, r11 cherry-picks one
/// revision, r12 records mergeinfo with `--record-only` and no content.
fn four_merge_styles(dir: &Path) -> Result<String, String> {
    let repo = dir.join("repo");
    run("svnadmin", &["create", &repo.display().to_string()], dir)?;
    // Three slashes so a Windows drive letter reads as a path, not a host.
    let url = format!(
        "file:///{}",
        repo.display().to_string().trim_start_matches('/')
    );

    run(
        "svn",
        &[
            "-q",
            "mkdir",
            "-m",
            "layout",
            &format!("{url}/trunk"),
            &format!("{url}/branches"),
        ],
        dir,
    )?;
    let wc = dir.join("wc");
    run(
        "svn",
        &["-q", "checkout", &url, &wc.display().to_string()],
        dir,
    )?;

    // r2: three files on trunk, one per branch to touch.
    for n in 1..=3 {
        std::fs::write(wc.join("trunk").join(format!("f{n}.txt")), "base\n")
            .map_err(|e| e.to_string())?;
    }
    run(
        "svn",
        &["-q", "add", "trunk/f1.txt", "trunk/f2.txt", "trunk/f3.txt"],
        &wc,
    )?;
    run("svn", &["-q", "commit", "-m", "trunk files"], &wc)?;

    // r3, r4, r5: one branch per style that needs a source.
    for branch in ["reint", "cherry", "recordonly"] {
        run(
            "svn",
            &[
                "-q",
                "copy",
                "-m",
                &format!("create branches/{branch}"),
                &format!("{url}/trunk"),
                &format!("{url}/branches/{branch}"),
            ],
            dir,
        )?;
    }
    run("svn", &["-q", "update"], &wc)?;

    // r6, r7, r8: work on each branch.
    for (branch, file) in [
        ("reint", "f1.txt"),
        ("cherry", "f2.txt"),
        ("recordonly", "f3.txt"),
    ] {
        let path = wc.join("branches").join(branch).join(file);
        std::fs::write(&path, format!("base\n{branch} work\n")).map_err(|e| e.to_string())?;
        run(
            "svn",
            &["-q", "commit", "-m", &format!("work on {branch}")],
            &wc,
        )?;
        run("svn", &["-q", "update"], &wc)?;
    }

    // r9: reintegrate — the only true positive.
    run(
        "svn",
        &["-q", "merge", &format!("{url}/branches/reint"), "trunk"],
        &wc,
    )?;
    run(
        "svn",
        &["-q", "commit", "-m", "Merge branches/reint into trunk"],
        &wc,
    )?;
    run("svn", &["-q", "update"], &wc)?;

    // r10: sync merge — trunk into a branch, the opposite direction.
    run(
        "svn",
        &["-q", "merge", &format!("{url}/trunk"), "branches/cherry"],
        &wc,
    )?;
    run(
        "svn",
        &["-q", "commit", "-m", "Sync trunk into branches/cherry"],
        &wc,
    )?;
    run("svn", &["-q", "update"], &wc)?;

    // r11: cherry-pick — one revision, not the branch.
    run(
        "svn",
        &[
            "-q",
            "merge",
            "-c",
            "7",
            &format!("{url}/branches/cherry"),
            "trunk",
        ],
        &wc,
    )?;
    run(
        "svn",
        &["-q", "commit", "-m", "Pick one fix from the cherry line"],
        &wc,
    )?;
    run("svn", &["-q", "update"], &wc)?;

    // r12: --record-only — mergeinfo, no content. "Never merge this."
    run(
        "svn",
        &[
            "-q",
            "merge",
            "--record-only",
            "-c",
            "8",
            &format!("{url}/branches/recordonly"),
            "trunk",
        ],
        &wc,
    )?;
    run(
        "svn",
        &["-q", "commit", "-m", "Block r8 from ever being merged"],
        &wc,
    )?;

    Ok(url)
}

#[test]
fn only_a_reintegration_out_of_four_merge_styles_synthesises_a_pseudo_pr() -> Result<(), String> {
    if !svn_is_installed() {
        return Ok(());
    }
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let url = four_merge_styles(dir.path())?;

    // r9 — reintegrate. Content changed, source is a branch, the range reaches the
    // branch's head (r6). The one true positive.
    let gains = gained_branches(&mergeinfo(&url, "/trunk", 8), &mergeinfo(&url, "/trunk", 9));
    assert_eq!(gains.len(), 1, "r9 must gain exactly one branch: {gains:?}");
    assert_eq!(gains[0].branch, "/branches/reint");
    assert_eq!(
        classify_gain(
            &gains[0],
            &MergeEvidence::new("/trunk").with_branch_head("/branches/reint", 6)
        ),
        MergeStyle::Reintegration
    );

    // r10 — sync merge. The gain is on the *branch*, and it is from trunk. Nothing
    // new arrived on the watched path, so there is nothing to review.
    let gains = gained_branches(
        &mergeinfo(&url, "/branches/cherry", 9),
        &mergeinfo(&url, "/branches/cherry", 10),
    );
    let from_trunk = gains
        .iter()
        .find(|gain| gain.branch == "/trunk")
        .ok_or("r10 must record a gain from /trunk")?;
    assert_eq!(
        classify_gain(from_trunk, &MergeEvidence::new("/trunk")),
        MergeStyle::SyncMerge
    );

    // r11 — cherry-pick. One revision was taken; the branch's head is r10. A
    // pseudo-PR here would diff the whole branch, including work nobody merged.
    let gains = gained_branches(
        &mergeinfo(&url, "/trunk", 10),
        &mergeinfo(&url, "/trunk", 11),
    );
    let cherry = gains
        .iter()
        .find(|gain| gain.branch == "/branches/cherry")
        .ok_or("r11 must record a gain from /branches/cherry")?;
    assert_eq!(cherry.through, 7, "only r7 was picked");
    assert_eq!(
        classify_gain(
            cherry,
            &MergeEvidence::new("/trunk").with_branch_head("/branches/cherry", 10)
        ),
        MergeStyle::CherryPick
    );

    // r12 — --record-only. The worst false positive: this idiom exists to mark a
    // revision as deliberately never to be merged, so a pseudo-PR would review
    // code a human explicitly rejected.
    let gains = gained_branches(
        &mergeinfo(&url, "/trunk", 11),
        &mergeinfo(&url, "/trunk", 12),
    );
    let recorded = gains
        .iter()
        .find(|gain| gain.branch == "/branches/recordonly")
        .ok_or("r12 must record a gain from /branches/recordonly")?;
    assert_eq!(
        classify_gain(
            recorded,
            &MergeEvidence::new("/trunk")
                .without_content()
                .with_branch_head("/branches/recordonly", 8)
        ),
        MergeStyle::RecordOnly
    );

    Ok(())
}

#[test]
fn every_rejection_says_why() {
    // §18: "we saw mergeinfo move and did not synthesise a change" is worth being
    // able to say out loud, and an operator debugging a missing pseudo-PR needs to
    // know which of the three it was.
    assert!(MergeStyle::Reintegration.explain_rejection().is_none());
    for style in [
        MergeStyle::SyncMerge,
        MergeStyle::CherryPick,
        MergeStyle::RecordOnly,
    ] {
        let reason = style
            .explain_rejection()
            .unwrap_or_else(|| panic!("{style:?} must explain itself"));
        assert!(!reason.is_empty());
    }
}

#[test]
fn an_unknown_branch_head_does_not_reject() {
    // The completeness test is the one most likely to be wrong on an unusual
    // history. A missed rejection costs less than a missed reintegration, so an
    // absent branch head means "do not reject" rather than "assume cherry-pick".
    let gain = revlocal_vcs::svn::GainedRange {
        branch: "/branches/x".to_owned(),
        through: 5,
    };
    assert_eq!(
        classify_gain(&gain, &MergeEvidence::new("/trunk")),
        MergeStyle::Reintegration
    );
}
