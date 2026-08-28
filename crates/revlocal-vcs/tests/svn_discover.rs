//! Per-revision SVN discovery (RL-902, SPEC §6.4).
//!
//! The parsing tests use svn's real XML, captured from the fixture repository.
//! The end-to-end tests run against that repository where Subversion is
//! installed, and say so clearly when it is not.
//!
//! Helpers return `Result` (ADR 0003); only the `#[test]` functions panic.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use revlocal_vcs::svn::{discover, parse_log_xml, SvnRunner, WatchedPaths};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn svn_is_installed() -> bool {
    std::process::Command::new("svn")
        .args(["--version", "--quiet"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Build the fixture and return the repository URL, or `None` when svn is absent.
fn fixture_repo(out: &Path) -> Result<Option<String>, String> {
    if !svn_is_installed() {
        return Ok(None);
    }

    let output = std::process::Command::new(revlocal_vcs::bash_program())
        .arg(workspace_root().join("fixtures/build.sh"))
        .arg("--out")
        .arg(out)
        .current_dir(workspace_root())
        .output()
        .map_err(|e| format!("running build.sh: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "build.sh failed (exit {}):\nstdout: {}\nstderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let manifest = std::fs::read_to_string(out.join("svn-basic/.manifest.json"))
        .map_err(|e| format!("reading the svn manifest: {e}"))?;
    let parsed: serde_json::Value =
        serde_json::from_str(&manifest).map_err(|e| format!("parsing the manifest: {e}"))?;

    if parsed["skipped"].as_bool().unwrap_or(false) {
        return Ok(None);
    }

    Ok(parsed["repo_url"].as_str().map(str::to_owned))
}

// --- parsing svn's real XML ------------------------------------------------

/// Captured verbatim from `svn log --xml -v` against the fixture. Note the
/// attribute order differs between r3 and r5 — svn does not promise one, which is
/// why this is parsed rather than pattern-matched.
const REAL_LOG: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<log>
<logentry
   revision="3">
<author>dmoore</author>
<date>2026-08-28T03:59:03.081501Z</date>
<paths>
<path
   action="A"
   prop-mods="false"
   text-mods="true"
   kind="file">/trunk/src/util.rs</path>
</paths>
<msg>Add a clamp helper</msg>
</logentry>
<logentry
   revision="5">
<author>dmoore</author>
<date>2026-08-28T03:59:05.092003Z</date>
<paths>
<path
   prop-mods="false"
   text-mods="true"
   kind="file"
   action="A">/trunk/src/db.rs</path>
</paths>
<msg>Add user lookup</msg>
</logentry>
</log>"#;

#[test]
fn svn_discover_parses_svns_own_xml_whatever_the_attribute_order() {
    let revisions = parse_log_xml(REAL_LOG).unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(revisions.len(), 2);
    assert_eq!(revisions[0].revision, 3);
    assert_eq!(revisions[0].message, "Add a clamp helper");
    assert_eq!(revisions[0].author.as_deref(), Some("dmoore"));
    assert_eq!(revisions[0].paths[0].path, "/trunk/src/util.rs");
    assert_eq!(revisions[0].paths[0].action, "A");
    assert_eq!(revisions[0].paths[0].kind.as_deref(), Some("file"));

    assert_eq!(
        revisions[1].paths[0].action, "A",
        "r5 lists `action` last; svn does not promise attribute order, and a \
         parser that depended on it would work until it did not"
    );
    assert_eq!(revisions[0].external_id(), "r3");
}

// --- criterion 4: a revision with no changed paths ------------------------

#[test]
fn svn_discover_handles_a_revision_with_no_changed_paths() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<log>
<logentry revision="42">
<author>someone</author>
<date>2026-08-28T04:00:00.000000Z</date>
<msg>A property change on the repository root</msg>
</logentry>
</log>"#;

    let revisions = parse_log_xml(xml).unwrap_or_else(|e| {
        panic!(
            "a discovery pass that fails on an unusual commit is a poller that \
             stops there and never advances again: {e}"
        )
    });

    assert_eq!(revisions.len(), 1);
    assert!(revisions[0].paths.is_empty());
    assert!(!revisions[0].touches(&WatchedPaths::trunk_only("/trunk")));
}

#[test]
fn svn_discover_handles_an_empty_log() {
    let xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<log>\n</log>";
    assert!(parse_log_xml(xml)
        .unwrap_or_else(|e| panic!("{e}"))
        .is_empty());
}

#[test]
fn svn_discover_handles_a_message_with_xml_in_it() {
    let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<log>
<logentry revision="7">
<msg>Fix &lt;script&gt; escaping &amp; the parser</msg>
</logentry>
</log>"#;

    let revisions = parse_log_xml(xml).unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(
        revisions[0].message, "Fix <script> escaping & the parser",
        "entities are decoded, which a regex over angle brackets would not do"
    );
}

// --- criterion 3: path filtering -----------------------------------------

#[test]
fn svn_discover_watched_paths_match_on_a_boundary_not_a_prefix() {
    let watched = WatchedPaths::trunk_only("/trunk");

    assert!(watched.matches("/trunk"));
    assert!(watched.matches("/trunk/src/main.rs"));
    assert!(
        !watched.matches("/trunk-old/src/main.rs"),
        "a plain starts_with would quietly review a second repository layout \
         nobody asked about"
    );
    assert!(!watched.matches("/branches/feature-x/src/main.rs"));
    assert!(!watched.matches("/tags/v1"));
}

#[test]
fn svn_discover_branches_are_watched_only_when_asked_for() {
    let trunk_only = WatchedPaths::trunk_only("/trunk");
    let with_branches = WatchedPaths::with_branches("/trunk", "/branches");

    assert!(!trunk_only.matches("/branches/feature-x/a.rs"));
    assert!(with_branches.matches("/branches/feature-x/a.rs"));
    assert!(with_branches.matches("/trunk/a.rs"));
    assert!(!with_branches.matches("/tags/v1/a.rs"));
}

#[test]
fn svn_discover_a_path_without_a_leading_slash_still_matches() {
    let watched = WatchedPaths::trunk_only("trunk");
    assert!(watched.matches("/trunk/src/main.rs"));
    assert!(watched.matches("trunk/src/main.rs"));
}

// --- against the real fixture --------------------------------------------

#[tokio::test]
async fn svn_discover_finds_the_fixtures_revisions_in_ascending_order() {
    let dir = tempfile::tempdir().expect("tempdir");
    let Some(url) = fixture_repo(dir.path()).unwrap_or_else(|e| panic!("{e}")) else {
        println!("SKIPPED (svn not installed, nothing verified): svn_discover_finds...");
        return;
    };

    let found = discover(
        &SvnRunner::new(),
        &url,
        0,
        100,
        &WatchedPaths::with_branches("/trunk", "/branches"),
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));

    let numbers: Vec<u64> = found
        .reviewable
        .iter()
        .map(|revision| revision.revision)
        .collect();

    assert!(!numbers.is_empty(), "the fixture has revisions");
    assert!(
        numbers.windows(2).all(|pair| pair[0] < pair[1]),
        "§6.4 discovers oldest first, and the cursor only makes sense against an \
         ascending sequence: {numbers:?}"
    );
    assert_eq!(numbers.first(), Some(&1), "starting from cursor 0 means r1");
    assert_eq!(
        found.highest_seen(),
        Some(13),
        "the fixture ends at r13 (its manifest says so)"
    );
}

#[tokio::test]
async fn svn_discover_starts_after_the_cursor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let Some(url) = fixture_repo(dir.path()).unwrap_or_else(|e| panic!("{e}")) else {
        println!("SKIPPED (svn not installed, nothing verified): svn_discover_starts_after...");
        return;
    };

    let found = discover(
        &SvnRunner::new(),
        &url,
        10,
        100,
        &WatchedPaths::with_branches("/trunk", "/branches"),
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));

    assert!(
        found
            .reviewable
            .iter()
            .chain(found.filtered.iter())
            .all(|revision| revision.revision > 10),
        "the cursor is the last revision already recorded, so discovery starts at \
         cursor + 1"
    );
}

#[tokio::test]
async fn svn_discover_a_cursor_at_head_finds_nothing_and_is_not_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let Some(url) = fixture_repo(dir.path()).unwrap_or_else(|e| panic!("{e}")) else {
        println!("SKIPPED (svn not installed, nothing verified): svn_discover_a_cursor_at_head...");
        return;
    };

    let found = discover(
        &SvnRunner::new(),
        &url,
        13,
        100,
        &WatchedPaths::trunk_only("/trunk"),
    )
    .await
    .unwrap_or_else(|e| panic!("no new commits is the normal state of a poll, not a fault: {e}"));

    assert_eq!(found.seen(), 0);
    assert_eq!(found.highest_seen(), None);
}

#[tokio::test]
async fn svn_discover_filters_branch_revisions_when_branches_are_not_watched() {
    let dir = tempfile::tempdir().expect("tempdir");
    let Some(url) = fixture_repo(dir.path()).unwrap_or_else(|e| panic!("{e}")) else {
        println!("SKIPPED (svn not installed, nothing verified): svn_discover_filters...");
        return;
    };

    let trunk_only = discover(
        &SvnRunner::new(),
        &url,
        0,
        100,
        &WatchedPaths::trunk_only("/trunk"),
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));

    // r8 and r9 are branch-only work in the fixture.
    for branch_revision in [8, 9] {
        assert!(
            !trunk_only
                .reviewable
                .iter()
                .any(|r| r.revision == branch_revision),
            "r{branch_revision} is branch-only work and trunk is all that is watched"
        );
        assert!(
            trunk_only
                .filtered
                .iter()
                .any(|r| r.revision == branch_revision),
            "but it was still SEEN — filtering decides what gets reviewed, not what \
             gets looked at, and the cursor has to advance past it or every poll \
             re-reads it forever"
        );
    }

    assert_eq!(
        trunk_only.highest_seen(),
        Some(13),
        "the cursor advances past filtered revisions too"
    );
}

#[tokio::test]
async fn svn_discover_the_limit_is_respected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let Some(url) = fixture_repo(dir.path()).unwrap_or_else(|e| panic!("{e}")) else {
        println!("SKIPPED (svn not installed, nothing verified): svn_discover_the_limit...");
        return;
    };

    let found = discover(
        &SvnRunner::new(),
        &url,
        0,
        3,
        &WatchedPaths::with_branches("/trunk", "/branches"),
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));

    assert_eq!(
        found.seen(),
        3,
        "a backlog has to be drained in bounded passes, or the first poll after a \
         long outage reads the entire history into memory"
    );
    assert_eq!(found.highest_seen(), Some(3));
}
