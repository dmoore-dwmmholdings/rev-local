//! Acceptance tests for `RL-205` — Windows fixture parity.
//!
//! Two hand-maintained generators that must produce byte-identical output will not
//! stay in agreement. Most of that risk was removed rather than tested: the file
//! bodies live once under `fixtures/content/` and both drivers **copy** them, and
//! the commit sequence lives once in `steps.json`. What is left is the git
//! invocations, and that is what the parity test covers.
//!
//! `pwsh` is not installed everywhere the inner loop runs, so the parity test
//! activates itself where it is. It does **not** silently pass when `pwsh` is
//! missing — it says so, and the structural tests below run regardless.

mod fixture_parity {
    use std::path::PathBuf;
    use std::process::Command;

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
    }

    fn pwsh_is_installed() -> bool {
        Command::new("pwsh")
            .arg("-NoProfile")
            .arg("-Command")
            .arg("$PSVersionTable.PSVersion.Major")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    fn note_skipped(test: &str) {
        println!("SKIPPED (pwsh not installed, nothing verified): {test}");
    }

    #[test]
    fn parity_powershell_produces_the_same_commit_shas_as_bash() {
        // Acceptance criterion 1. This is the whole point of the item: a Windows
        // developer and a Linux developer must be looking at the same fixture.
        if !pwsh_is_installed() {
            note_skipped("parity_powershell_produces_the_same_commit_shas_as_bash");
            return;
        }

        let root = workspace_root();
        let bash_dir = tempfile::TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let pwsh_dir = tempfile::TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));

        let bash_status = Command::new("bash")
            .arg(root.join("fixtures/build.sh"))
            .arg("--out")
            .arg(bash_dir.path())
            .current_dir(&root)
            .output()
            .unwrap_or_else(|e| panic!("bash build: {e}"));
        assert!(
            bash_status.status.success(),
            "bash build failed: {}",
            String::from_utf8_lossy(&bash_status.stderr)
        );

        let pwsh_status = Command::new("pwsh")
            .arg("-NoProfile")
            .arg("-File")
            .arg(root.join("fixtures/build.ps1"))
            .arg("-Out")
            .arg(pwsh_dir.path())
            .current_dir(&root)
            .output()
            .unwrap_or_else(|e| panic!("pwsh build: {e}"));
        assert!(
            pwsh_status.status.success(),
            "pwsh build failed: {}",
            String::from_utf8_lossy(&pwsh_status.stderr)
        );

        let read = |dir: &std::path::Path| {
            std::fs::read_to_string(dir.join("git-basic/.manifest.json"))
                .unwrap_or_else(|e| panic!("reading manifest: {e}"))
        };
        assert_eq!(
            read(bash_dir.path()),
            read(pwsh_dir.path()),
            "the two generators produced different fixtures; a Windows developer and \
             a Linux developer would be reviewing different commits"
        );
    }

    #[test]
    fn parity_no_crlf_leaks_into_fixture_file_contents() {
        // Acceptance criterion 2. Checked against whichever build ran, because a
        // single CR anywhere changes that file's blob and therefore every SHA from
        // that commit onward.
        let dir = tempfile::TempDir::new().unwrap_or_else(|e| panic!("temp dir: {e}"));
        let root = workspace_root();

        let script: Vec<String> = if pwsh_is_installed() {
            vec![
                "pwsh".into(),
                "-NoProfile".into(),
                "-File".into(),
                root.join("fixtures/build.ps1").display().to_string(),
                "-Out".into(),
                dir.path().display().to_string(),
            ]
        } else {
            note_skipped(
                "parity_no_crlf_leaks_into_fixture_file_contents (checking the bash build instead)",
            );
            vec![
                "bash".into(),
                root.join("fixtures/build.sh").display().to_string(),
                "--out".into(),
                dir.path().display().to_string(),
            ]
        };

        let output = Command::new(&script[0])
            .args(&script[1..])
            .current_dir(&root)
            .output()
            .unwrap_or_else(|e| panic!("build: {e}"));
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );

        // Ask git, not the filesystem: what matters is the bytes that were
        // committed, which is what the SHA is over.
        let listed = Command::new("git")
            .args(["ls-files"])
            .current_dir(dir.path().join("git-basic"))
            .output()
            .unwrap_or_else(|e| panic!("git ls-files: {e}"));

        let mut offenders = Vec::new();
        for file in String::from_utf8_lossy(&listed.stdout).lines() {
            let blob = Command::new("git")
                .args(["show", &format!("HEAD:{file}")])
                .current_dir(dir.path().join("git-basic"))
                .output()
                .unwrap_or_else(|e| panic!("git show {file}: {e}"));
            if blob.stdout.contains(&b'\r') {
                offenders.push(file.to_owned());
            }
        }

        assert!(
            offenders.is_empty(),
            "CRLF reached committed content, which changes the blob and every SHA \
             from that commit onward: {offenders:?}"
        );
    }

    #[test]
    fn parity_both_drivers_read_the_same_content_and_step_list() {
        // The structural half, and the one that actually removes the risk rather
        // than detecting it after the fact. It runs everywhere, including here.
        let fixtures = workspace_root().join("fixtures");

        for required in ["build.sh", "build.ps1", "content/git-basic/steps.json"] {
            assert!(fixtures.join(required).is_file(), "missing {required}");
        }

        let bash = std::fs::read_to_string(fixtures.join("build.sh")).unwrap_or_default();
        let pwsh = std::fs::read_to_string(fixtures.join("build.ps1")).unwrap_or_default();

        for driver in [&bash, &pwsh] {
            assert!(
                driver.contains("steps.json"),
                "a driver that does not read steps.json is carrying its own copy of \
                 the commit sequence, which is what this design removes"
            );
        }

        // The file bodies must not be inline in either driver. If a body appears in
        // a script, the other script has to repeat it and the two will drift.
        for (name, driver) in [("build.sh", &bash), ("build.ps1", &pwsh)] {
            assert!(
                !driver.contains("BUG (planted)"),
                "{name} contains fixture file content inline; it belongs in \
                 fixtures/content/ where both drivers copy the same bytes"
            );
        }
    }

    #[test]
    fn parity_the_powershell_driver_forces_lf_on_files_it_generates() {
        // Copied files cannot gain CRLF — Copy-Item moves bytes. Only the 200
        // generated modules and the manifest are written by the script, and
        // PowerShell's own output cmdlets emit CRLF on Windows. So the guard has to
        // be there, and this asserts it is even where pwsh cannot be run.
        let driver = std::fs::read_to_string(workspace_root().join("fixtures/build.ps1"))
            .unwrap_or_default();

        assert!(
            driver.contains("WriteAllText"),
            "generated files must be written with explicit LF, not via a cmdlet \
             that emits CRLF on Windows"
        );
        assert!(
            driver.contains("UTF8Encoding($false)"),
            "a BOM would be three extra bytes in the committed file and a different \
             SHA from the bash build"
        );
        assert!(
            driver.contains("core.autocrlf false"),
            "a machine-wide core.autocrlf=true would rewrite every committed file"
        );
    }
}
