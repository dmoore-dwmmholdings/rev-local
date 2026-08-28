//! Acceptance tests for `RL-201` — the git fixture generator.
//!
//! These run `fixtures/build.sh` for real. That is slower than asserting on a
//! checked-in manifest, and it is the point: the criterion is that the *generator*
//! is deterministic, and a checked-in manifest would still match itself after the
//! generator started drifting.

mod fixtures {
    use serde::Deserialize;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// One commit's entry in `.manifest.json`.
    #[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
    struct CommitEntry {
        role: String,
        sha: String,
        subject: String,
    }

    /// The generated manifest.
    #[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
    struct Manifest {
        fixture: String,
        default_branch: String,
        commits: Vec<CommitEntry>,
    }

    /// Every role a test may look a commit up by.
    ///
    /// Listed here so that renaming a role in `build.sh` breaks this test rather
    /// than silently breaking whichever integration test relied on it.
    const REQUIRED_ROLES: [&str; 7] = [
        "initial",
        "clean",
        "planted_bug_off_by_one",
        "planted_bug_sql_injection",
        "lockfile_only",
        "bot",
        "merge",
    ];

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
    }

    /// Run `fixtures/build.sh` into `out`, returning the parsed manifest.
    ///
    /// Returns `Result`; helpers are not `#[test]` fns (ADR 0003).
    fn build_into(out: &Path) -> Result<Manifest, String> {
        let root = workspace_root();
        let script = root.join("fixtures/build.sh");

        let output = Command::new(revlocal_vcs::bash_program())
            .arg(&script)
            .arg("--out")
            .arg(out)
            .current_dir(&root)
            .output()
            .map_err(|e| format!("running {}: {e}", script.display()))?;

        if !output.status.success() {
            return Err(format!(
                "build.sh failed ({}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ));
        }

        let manifest_path = out.join("git-basic/.manifest.json");
        let text = std::fs::read_to_string(&manifest_path)
            .map_err(|e| format!("reading {}: {e}", manifest_path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("parsing manifest: {e}"))
    }

    #[test]
    fn fixtures_two_consecutive_builds_produce_identical_commit_shas() {
        // The criterion this fixture exists to satisfy. Built into two separate
        // directories so neither run can inherit anything from the other.
        let first_dir = tempfile::TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let second_dir = tempfile::TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));

        let first = build_into(first_dir.path()).unwrap_or_else(|e| panic!("first build: {e}"));
        let second = build_into(second_dir.path()).unwrap_or_else(|e| panic!("second build: {e}"));

        assert_eq!(
            first, second,
            "the generator is not deterministic; tests reference commits by role \
             through this manifest, and the roles are only useful if the SHAs hold"
        );
    }

    #[test]
    fn fixtures_the_manifest_names_every_role_tests_depend_on() {
        let dir = tempfile::TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let manifest = build_into(dir.path()).unwrap_or_else(|e| panic!("build: {e}"));

        assert_eq!(manifest.fixture, "git-basic");
        assert_eq!(manifest.default_branch, "main");
        assert_eq!(
            manifest.commits.len(),
            12,
            "SPEC §16.2 and M4's gate both say 12 commits"
        );

        for role in REQUIRED_ROLES {
            assert!(
                manifest.commits.iter().any(|c| c.role == role),
                "no commit has role {role:?}; roles present: {:?}",
                manifest.commits.iter().map(|c| &c.role).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn fixtures_roles_are_unique_so_a_lookup_is_unambiguous() {
        let dir = tempfile::TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let manifest = build_into(dir.path()).unwrap_or_else(|e| panic!("build: {e}"));

        let mut roles: Vec<&str> = manifest.commits.iter().map(|c| c.role.as_str()).collect();
        roles.sort_unstable();
        let unique = roles.len();
        roles.dedup();
        assert_eq!(
            roles.len(),
            unique,
            "a duplicated role makes `find by role` return an arbitrary commit"
        );
    }

    #[test]
    fn fixtures_the_planted_bugs_are_actually_in_the_files_they_claim() {
        // A manifest that says `planted_bug_sql_injection` while the commit
        // touched something else would send every downstream test looking in the
        // wrong place, and the failure would look like an engine problem.
        let dir = tempfile::TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let manifest = build_into(dir.path()).unwrap_or_else(|e| panic!("build: {e}"));
        let repo = dir.path().join("git-basic");

        for (role, expected_file) in [
            ("planted_bug_off_by_one", "src/pager.rs"),
            ("planted_bug_sql_injection", "src/db.rs"),
        ] {
            let sha = manifest
                .commits
                .iter()
                .find(|c| c.role == role)
                .map(|c| c.sha.clone())
                .unwrap_or_else(|| panic!("no commit with role {role}"));

            let output = Command::new("git")
                .args(["show", "--name-only", "--format=", &sha])
                .current_dir(&repo)
                .output()
                .unwrap_or_else(|e| panic!("git show: {e}"));
            let files = String::from_utf8_lossy(&output.stdout);

            assert!(
                files.lines().any(|f| f.trim() == expected_file),
                "{role} should have touched {expected_file}, touched: {files}"
            );
        }
    }

    #[test]
    fn fixtures_the_merge_commit_has_two_parents() {
        // M4 skips merges by `skip_reason`. A "merge" that fast-forwarded would
        // have one parent and the skip rule would never fire.
        let dir = tempfile::TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let manifest = build_into(dir.path()).unwrap_or_else(|e| panic!("build: {e}"));
        let repo = dir.path().join("git-basic");

        let sha = manifest
            .commits
            .iter()
            .find(|c| c.role == "merge")
            .map(|c| c.sha.clone())
            .unwrap_or_else(|| panic!("no merge commit"));

        let output = Command::new("git")
            .args(["rev-list", "--parents", "-n", "1", &sha])
            .current_dir(&repo)
            .output()
            .unwrap_or_else(|e| panic!("git rev-list: {e}"));
        let parents = String::from_utf8_lossy(&output.stdout);

        assert_eq!(
            parents.split_whitespace().count(),
            3,
            "expected <sha> <parent1> <parent2>, got {parents:?}"
        );
    }

    #[test]
    fn fixtures_the_bot_commit_is_authored_by_a_bot() {
        // The skip rule matches on author (SPEC §13.2 `ignore_authors`), not on
        // the subject line.
        let dir = tempfile::TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let manifest = build_into(dir.path()).unwrap_or_else(|e| panic!("build: {e}"));
        let repo = dir.path().join("git-basic");

        let sha = manifest
            .commits
            .iter()
            .find(|c| c.role == "bot")
            .map(|c| c.sha.clone())
            .unwrap_or_else(|| panic!("no bot commit"));

        let output = Command::new("git")
            .args(["show", "-s", "--format=%an", &sha])
            .current_dir(&repo)
            .output()
            .unwrap_or_else(|e| panic!("git show: {e}"));
        let author = String::from_utf8_lossy(&output.stdout).trim().to_owned();

        assert_eq!(author, "dependabot[bot]");
    }

    #[test]
    fn fixtures_the_large_commit_has_enough_files_to_trigger_truncation() {
        // SPEC §9.3 selects `summary` depth above `deep_file_limit` (default 150).
        let dir = tempfile::TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let manifest = build_into(dir.path()).unwrap_or_else(|e| panic!("build: {e}"));
        let repo = dir.path().join("git-basic");

        let sha = manifest
            .commits
            .iter()
            .find(|c| c.role == "large_200_files")
            .map(|c| c.sha.clone())
            .unwrap_or_else(|| panic!("no large commit"));

        let output = Command::new("git")
            .args(["show", "--name-only", "--format=", &sha])
            .current_dir(&repo)
            .output()
            .unwrap_or_else(|e| panic!("git show: {e}"));
        let count = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count();

        assert_eq!(count, 200, "SPEC §16.2 wants a 200-file commit");
        assert!(
            count > 150,
            "and it must exceed the default deep_file_limit"
        );
    }

    #[test]
    fn fixtures_the_working_tree_is_clean_after_a_build() {
        // M4's gate asserts `git status --porcelain` is empty after a review. If
        // the generator leaves the fixture dirty, that gate can never pass and the
        // failure will look like the adapter's fault.
        let dir = tempfile::TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        build_into(dir.path()).unwrap_or_else(|e| panic!("build: {e}"));

        let output = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(dir.path().join("git-basic"))
            .output()
            .unwrap_or_else(|e| panic!("git status: {e}"));
        let status = String::from_utf8_lossy(&output.stdout);

        assert!(
            status.trim().is_empty(),
            "the generator left the fixture dirty:\n{status}"
        );
    }

    #[test]
    fn fixtures_the_bare_mirror_has_the_same_commits() {
        let dir = tempfile::TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let manifest = build_into(dir.path()).unwrap_or_else(|e| panic!("build: {e}"));
        let bare = dir.path().join("git-bare");

        assert!(
            bare.is_dir(),
            "SPEC §16.2 requires a --mirror clone at git-bare"
        );

        let output = Command::new("git")
            .args(["rev-parse", "refs/heads/main"])
            .current_dir(&bare)
            .output()
            .unwrap_or_else(|e| panic!("git rev-parse: {e}"));
        let head = String::from_utf8_lossy(&output.stdout).trim().to_owned();

        let tip = manifest
            .commits
            .last()
            .map(|c| c.sha.clone())
            .unwrap_or_default();
        assert_eq!(
            head, tip,
            "the mirror must point at the same tip as git-basic"
        );
    }
}
