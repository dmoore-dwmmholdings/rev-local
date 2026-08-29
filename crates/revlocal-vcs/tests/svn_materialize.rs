//! Materialising one Subversion revision (RL-903, SPEC §6.4).
//!
//! The property-only and binary cases are built here rather than taken from the
//! shared fixture: both are about what svn *declines to render*, and a repository
//! purpose-built for them is clearer than one that happens to contain them.
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use std::path::Path;
use std::process::Stdio;

use revlocal_vcs::svn::{materialize, parse_summary, render_property_only, SvnRunner};
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

/// A repository containing exactly the cases this item is about.
///
/// r1 adds a text file and a binary one; r2 changes only a property; r3 changes
/// the binary file's bytes.
fn purpose_built(dir: &Path) -> Result<String, String> {
    let repo = dir.join("repo");
    let wc = dir.join("wc");
    run("svnadmin", &["create", &repo.display().to_string()], dir)?;

    // `file://{path}` is wrong on Windows twice over — backslashes, and a
    // drive letter needing the third slash. svn rejects both as non-canonical
    // and names a line in its own C source when it does.
    let url = revlocal_vcs::svn::file_url(&repo);
    run(
        "svn",
        &["checkout", "--quiet", &url, &wc.display().to_string()],
        dir,
    )?;

    std::fs::write(wc.join("readme.txt"), "hello\n").map_err(|e| e.to_string())?;
    // Bytes that are unambiguously not text, so svn marks it binary on its own.
    std::fs::write(
        wc.join("logo.png"),
        [0x89, b'P', b'N', b'G', 0x00, 0x01, 0x02, 0x03],
    )
    .map_err(|e| e.to_string())?;
    run("svn", &["add", "--quiet", "readme.txt", "logo.png"], &wc)?;
    run(
        "svn",
        &["commit", "--quiet", "-m", "Add a file and a binary"],
        &wc,
    )?;

    // r2: a property change and nothing else.
    run(
        "svn",
        &["propset", "svn:executable", "*", "readme.txt"],
        &wc,
    )?;
    run(
        "svn",
        &["commit", "--quiet", "-m", "Mark it executable"],
        &wc,
    )?;

    // r3: change the binary's bytes.
    std::fs::write(
        wc.join("logo.png"),
        [0x89, b'P', b'N', b'G', 0xFF, 0xFE, 0xFD, 0xFC, 0x00],
    )
    .map_err(|e| e.to_string())?;
    run(
        "svn",
        &["commit", "--quiet", "-m", "Change the binary"],
        &wc,
    )?;

    Ok(url)
}

// --- parsing, without a repository ---------------------------------------

#[test]
fn svn_materialize_a_property_only_change_is_recognised_from_the_summary() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<diff>
<paths>
<path item="none" props="modified" kind="file">file:///repo/trunk/readme.txt</path>
<path item="modified" props="none" kind="file">file:///repo/trunk/src/main.rs</path>
</paths>
</diff>"#;

    let changed = parse_summary(xml, "file:///repo").unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(changed.len(), 2);
    assert_eq!(
        changed[0].path, "trunk/readme.txt",
        "paths are repo-relative"
    );
    assert!(
        changed[0].is_property_only(),
        "item=none with props=modified is the case that produces an empty patch \
         and means something"
    );
    assert!(!changed[1].is_property_only());
}

#[test]
fn svn_materialize_a_summary_with_no_paths_parses() {
    let xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<diff>\n</diff>";
    assert!(parse_summary(xml, "file:///repo")
        .unwrap_or_else(|e| panic!("{e}"))
        .is_empty());
}

#[test]
fn svn_materialize_the_property_only_note_says_why_the_patch_is_empty() {
    let note = render_property_only("trunk/readme.txt");
    assert!(note.contains("trunk/readme.txt"));
    assert!(
        note.contains("contents did not"),
        "§18: an empty patch passed through unchanged tells the engine `nothing \
         happened here`, which is false: {note}"
    );
}

// --- against a real repository -------------------------------------------

#[tokio::test]
async fn svn_materialize_exports_the_tree_at_the_right_revision() {
    if !svn_is_installed() {
        note_skipped("svn_materialize_the_property_only_note_says_why_the_patch_is_empty");
        println!("SKIPPED (svn not installed, nothing verified): svn_materialize_exports...");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let url = purpose_built(dir.path()).unwrap_or_else(|e| panic!("{e}"));
    let scratch = dir.path().join("scratch");

    let context = materialize(&SvnRunner::new(), &url, 1, &scratch)
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    let exported = context.worktree.join("readme.txt");
    assert!(exported.is_file(), "the tree is there: {exported:?}");
    assert_eq!(
        std::fs::read_to_string(&exported).unwrap_or_default(),
        "hello\n"
    );
    assert_eq!(context.message, "Add a file and a binary");
    assert_eq!(context.parents, vec!["r0".to_owned()]);

    assert!(
        !context.worktree.join(".svn").exists(),
        "an export has no .svn, so nothing downstream can commit from it and there \
         is no working copy to leave locked"
    );
}

#[tokio::test]
async fn svn_materialize_never_touches_a_working_copy() {
    if !svn_is_installed() {
        note_skipped("svn_materialize_the_property_only_note_says_why_the_patch_is_empty");
        println!("SKIPPED (svn not installed, nothing verified): svn_materialize_never_touches...");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let url = purpose_built(dir.path()).unwrap_or_else(|e| panic!("{e}"));
    let wc = dir.path().join("wc");

    // A local edit the user has not committed. §6.1's absolute constraint.
    std::fs::write(wc.join("readme.txt"), "MY UNCOMMITTED WORK\n")
        .unwrap_or_else(|e| panic!("{e}"));
    let before = run("svn", &["status"], &wc).unwrap_or_else(|e| panic!("{e}"));

    materialize(&SvnRunner::new(), &url, 1, &dir.path().join("scratch"))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(
        std::fs::read_to_string(wc.join("readme.txt")).unwrap_or_default(),
        "MY UNCOMMITTED WORK\n",
        "svn update and svn switch operate on a user's working copy in place; \
         materialisation must never reach for either"
    );
    assert_eq!(
        run("svn", &["status"], &wc).unwrap_or_else(|e| panic!("{e}")),
        before,
        "and the working copy's status is byte-identical afterwards"
    );
}

#[tokio::test]
async fn svn_materialize_a_property_only_revision_is_summarised_not_empty() {
    if !svn_is_installed() {
        note_skipped("svn_materialize_the_property_only_note_says_why_the_patch_is_empty");
        println!(
            "SKIPPED (svn not installed, nothing verified): svn_materialize_a_property_only..."
        );
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let url = purpose_built(dir.path()).unwrap_or_else(|e| panic!("{e}"));

    let context = materialize(&SvnRunner::new(), &url, 2, &dir.path().join("scratch"))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(context.message, "Mark it executable");
    assert!(
        context.diff_unified.contains("Properties on"),
        "a property-only change renders as nothing in a patch, and passing that \
         through says `nothing happened here`:\n{}",
        context.diff_unified
    );
    assert!(
        context.diff_unified.contains("readme.txt"),
        "and it names the file:\n{}",
        context.diff_unified
    );
    assert!(
        !context.diff_files.is_empty(),
        "the file is still listed as changed"
    );
}

#[tokio::test]
async fn svn_materialize_a_binary_change_is_summarised_with_size_and_type() {
    if !svn_is_installed() {
        note_skipped("svn_materialize_the_property_only_note_says_why_the_patch_is_empty");
        println!("SKIPPED (svn not installed, nothing verified): svn_materialize_a_binary...");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let url = purpose_built(dir.path()).unwrap_or_else(|e| panic!("{e}"));

    let context = materialize(&SvnRunner::new(), &url, 3, &dir.path().join("scratch"))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert!(
        context.diff_unified.contains("Binary file"),
        "svn refuses to render it, so rev-local has to say what changed:\n{}",
        context.diff_unified
    );
    assert!(context.diff_unified.contains("logo.png"));
    assert!(
        context.diff_unified.contains("9 bytes"),
        "the size at this revision is the part a reviewer can actually judge:\n{}",
        context.diff_unified
    );

    let binary = context
        .diff_files
        .iter()
        .find(|file| file.path.ends_with("logo.png"))
        .expect("the binary file is listed");
    assert!(binary.binary, "and it is flagged as binary");
    assert_eq!(
        (binary.insertions, binary.deletions),
        (0, 0),
        "counting `+`/`-` lines in a patch svn did not render would invent numbers"
    );
}

#[tokio::test]
async fn svn_materialize_a_text_change_still_produces_a_real_diff() {
    if !svn_is_installed() {
        note_skipped("svn_materialize_the_property_only_note_says_why_the_patch_is_empty");
        println!("SKIPPED (svn not installed, nothing verified): svn_materialize_a_text_change...");
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let url = purpose_built(dir.path()).unwrap_or_else(|e| panic!("{e}"));

    let context = materialize(&SvnRunner::new(), &url, 1, &dir.path().join("scratch"))
        .await
        .unwrap_or_else(|e| panic!("{e}"));

    assert!(context.diff_unified.contains("+hello"));
    assert!(context.stat.files >= 1);
    assert!(
        context.stat.insertions >= 1,
        "a text file's lines are counted: {:?}",
        context.stat
    );
    assert!(context.is_consistent());
}
