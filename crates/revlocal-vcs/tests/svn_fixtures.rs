//! Acceptance tests for `RL-202` — the Subversion fixture.
//!
//! `svn` is not installed everywhere the inner loop runs, so these tests adapt:
//! when it is absent they check the *skip* path, and when it is present they check
//! the fixture itself.
//!
//! The load-bearing detail is that they do **not** simply skip when `svn` is
//! missing and pass. A test that quietly passes on every machine verifies nothing.
//! Instead `the_manifest_agrees_with_whether_svn_is_installed` fails if a machine
//! that *has* `svn` produced a skipped manifest — which is how a broken generator
//! would otherwise hide. CI installs Subversion on all three runners (`RL-102`),
//! so the svn-present path runs there.

mod svn_fixtures {
    use serde::Deserialize;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    #[derive(Debug, Clone, Deserialize)]
    struct RevisionEntry {
        role: String,
        rev: u32,
        #[allow(dead_code)]
        subject: String,
    }

    #[derive(Debug, Clone, Deserialize)]
    struct PseudoPr {
        branch: String,
        reintegration_revision: u32,
        detected_by: Vec<String>,
        #[serde(default)]
        fork_revision: Option<u32>,
    }

    #[derive(Debug, Clone, Deserialize)]
    struct SvnManifest {
        fixture: String,
        skipped: bool,
        #[serde(default)]
        reason: Option<String>,
        #[serde(default)]
        remediation: Option<String>,
        #[serde(default)]
        repo_url: Option<String>,
        #[serde(default)]
        fork_revision: Option<u32>,
        revisions: Vec<RevisionEntry>,
        #[serde(default)]
        pseudo_pr: Option<PseudoPr>,
        #[serde(default)]
        pseudo_pr_mergeinfo_only: Option<PseudoPr>,
    }

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
    }

    /// Say out loud that a test verified nothing on this machine.
    ///
    /// A green "6 passed" on a box without Subversion would otherwise read as
    /// coverage it does not have. Visible with `--nocapture`, and
    /// `svn_the_manifest_agrees_with_whether_svn_is_installed` is what stops this
    /// from being the only signal.
    fn note_skipped(test: &str) {
        println!("SKIPPED (svn not installed, nothing verified): {test}");
    }

    fn svn_is_installed() -> bool {
        ["svn", "svnadmin"].iter().all(|tool| {
            Command::new(tool)
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success())
        })
    }

    /// Run the fixture generator into `out` and parse the svn manifest.
    fn build_into(out: &Path) -> Result<SvnManifest, String> {
        let root = workspace_root();
        let output = Command::new(revlocal_vcs::bash_program())
            .arg(root.join("fixtures/build.sh"))
            .arg("--out")
            .arg(out)
            .current_dir(&root)
            .output()
            .map_err(|e| format!("running build.sh: {e}"))?;

        if !output.status.success() {
            return Err(format!(
                "build.sh failed ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        let path = out.join("svn-basic/.manifest.json");
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("parsing manifest: {e}"))
    }

    #[test]
    fn svn_the_manifest_exists_whether_or_not_svn_is_installed() {
        // The item's gate is `test -f fixtures/out/svn-basic/.manifest.json`, and it
        // has to hold on a machine with no Subversion. A fixture that was not built
        // must say so rather than simply not existing (SPEC §18).
        let dir = tempfile::TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let manifest = build_into(dir.path()).unwrap_or_else(|e| panic!("build: {e}"));
        assert_eq!(manifest.fixture, "svn-basic");
    }

    #[test]
    fn svn_the_manifest_agrees_with_whether_svn_is_installed() {
        // The test that keeps the rest of this file honest. Without it, a broken
        // generator on a machine WITH svn would write a skip manifest and every
        // svn-gated test below would quietly not run.
        let dir = tempfile::TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let manifest = build_into(dir.path()).unwrap_or_else(|e| panic!("build: {e}"));

        if svn_is_installed() {
            assert!(
                !manifest.skipped,
                "svn is on PATH but the generator skipped the svn fixture: {:?}",
                manifest.reason
            );
        } else {
            assert!(
                manifest.skipped,
                "svn is not on PATH, so the fixture cannot have been built"
            );
        }
    }

    #[test]
    fn svn_a_skipped_build_says_why_and_how_to_fix_it() {
        if svn_is_installed() {
            // The skip path is not taken on this machine, so there is nothing to
            // assert about it here.
            println!(
                "SKIPPED (svn IS installed, so the skip path was not exercised): \
                 svn_a_skipped_build_says_why_and_how_to_fix_it"
            );
            return;
        }
        let dir = tempfile::TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let manifest = build_into(dir.path()).unwrap_or_else(|e| panic!("build: {e}"));

        assert!(manifest.skipped);
        let reason = manifest.reason.unwrap_or_default();
        assert!(
            reason.to_lowercase().contains("svn"),
            "the reason must name svn: {reason}"
        );
        let remediation = manifest.remediation.unwrap_or_default();
        assert!(
            remediation.contains("install"),
            "a skip must say how to un-skip it: {remediation}"
        );
        assert!(manifest.revisions.is_empty());
    }

    #[test]
    fn svn_the_manifest_records_the_reintegration_revision_and_its_fork_point() {
        if !svn_is_installed() {
            note_skipped("svn_the_manifest_records_the_reintegration_revision_and_its_fork_point");
            return;
        }
        let dir = tempfile::TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let manifest = build_into(dir.path()).unwrap_or_else(|e| panic!("build: {e}"));

        assert!(
            manifest
                .revisions
                .iter()
                .any(|r| r.role == "reintegration_rev"),
            "no revision has role reintegration_rev"
        );
        for role in [
            "planted_bug_off_by_one",
            "planted_bug_sql_injection",
            "lockfile_only",
            "branch_created",
        ] {
            assert!(
                manifest.revisions.iter().any(|r| r.role == role),
                "no revision has role {role}"
            );
        }

        let pseudo = manifest
            .pseudo_pr
            .clone()
            .unwrap_or_else(|| panic!("manifest has no pseudo_pr block"));
        assert_eq!(pseudo.branch, "/branches/feature-x");

        // §6.4's pseudo-PR diff is trunk@fork_rev vs branch@rev, so the fork point
        // is not optional detail — without it the diff cannot be computed.
        let fork = pseudo
            .fork_revision
            .or(manifest.fork_revision)
            .unwrap_or_else(|| panic!("no fork revision recorded"));
        assert!(
            fork < pseudo.reintegration_revision,
            "the fork point must precede the reintegration: {fork} vs {}",
            pseudo.reintegration_revision
        );
    }

    #[test]
    fn svn_the_reintegration_revision_genuinely_changes_mergeinfo() {
        if !svn_is_installed() {
            note_skipped("svn_the_reintegration_revision_genuinely_changes_mergeinfo");
            return;
        }
        let dir = tempfile::TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let manifest = build_into(dir.path()).unwrap_or_else(|e| panic!("build: {e}"));
        let repo_url = manifest
            .repo_url
            .clone()
            .unwrap_or_else(|| panic!("manifest has no repo_url"));

        // Verified with `svn propget`, as the acceptance criterion asks. If
        // `svn merge` silently recorded nothing, pseudo-PR heuristic 1 would be
        // untestable and the failure would look like the adapter's fault.
        let output = Command::new("svn")
            .args(["propget", "svn:mergeinfo", "--non-interactive"])
            .arg(format!("{repo_url}/trunk"))
            .output()
            .unwrap_or_else(|e| panic!("svn propget: {e}"));
        let mergeinfo = String::from_utf8_lossy(&output.stdout);

        assert!(
            mergeinfo.contains("/branches/feature-x"),
            "trunk's svn:mergeinfo does not mention feature-x: {mergeinfo:?}"
        );
    }

    #[test]
    fn svn_each_pseudo_pr_heuristic_can_be_tested_without_the_other() {
        if !svn_is_installed() {
            note_skipped("svn_each_pseudo_pr_heuristic_can_be_tested_without_the_other");
            return;
        }
        // §6.4 lists three detection heuristics in order. A fixture where every
        // signal fires on every reintegration cannot tell you which signal your
        // code is actually using — so there is a second reintegration whose log
        // message deliberately does not match `merge_detect_regex`.
        let dir = tempfile::TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let manifest = build_into(dir.path()).unwrap_or_else(|e| panic!("build: {e}"));

        let both = manifest
            .pseudo_pr
            .clone()
            .unwrap_or_else(|| panic!("no pseudo_pr block"));
        assert!(both.detected_by.iter().any(|d| d == "mergeinfo"));
        assert!(both.detected_by.iter().any(|d| d == "log_message"));

        let mergeinfo_only = manifest
            .pseudo_pr_mergeinfo_only
            .clone()
            .unwrap_or_else(|| panic!("no mergeinfo-only reintegration in the fixture"));
        assert_eq!(mergeinfo_only.detected_by, ["mergeinfo"]);

        // And the claim has to be true, not just asserted in the manifest: the
        // subject of that revision must NOT match the default merge_detect_regex.
        let subject = manifest
            .revisions
            .iter()
            .find(|r| r.rev == mergeinfo_only.reintegration_revision)
            .map(|r| r.subject.clone())
            .unwrap_or_default();
        let lowered = subject.to_lowercase();
        assert!(
            !lowered.contains("merge") && !lowered.contains("reintegrat"),
            "the mergeinfo-only revision's message must not match merge_detect_regex, \
             or heuristic 1 cannot be tested alone: {subject:?}"
        );
    }
}
