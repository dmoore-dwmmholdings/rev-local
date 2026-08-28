//! Acceptance test for `RL-204` — the mock MCP server fixture.
//!
//! The fixture's own gate is `node fixtures/mock-mcp/selftest.js`. Running it from
//! here as well means the loop's `cargo test --workspace` catches a broken fixture
//! in the same pass as everything else, rather than only when someone remembers to
//! run the node script.

mod mock_mcp {
    use std::path::PathBuf;
    use std::process::Command;

    fn workspace_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
    }

    #[test]
    fn mock_mcp_selftest_passes() {
        let root = workspace_root();
        let output = Command::new("node")
            .arg(root.join("fixtures/mock-mcp/selftest.js"))
            .current_dir(&root)
            .output()
            .unwrap_or_else(|e| panic!("running the mock-mcp selftest: {e}"));

        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "the mock MCP fixture is broken; every MCP test downstream would be \
             testing the fixture rather than the client:\n{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains("checks passed"),
            "the selftest produced no summary line: {stdout}"
        );
    }

    #[test]
    fn mock_mcp_profiles_cover_the_cases_capability_mapping_needs() {
        // SPEC §11.2's claim is that the Andare integration does not require
        // knowing Andare's tool names at build time. Testing that needs a server
        // willing to report names rev-local does NOT look for first, and one that
        // reports nothing bindable at all.
        let profiles = workspace_root().join("fixtures/mock-mcp/profiles");
        for profile in ["default.json", "andare-renamed.json", "unmappable.json"] {
            assert!(
                profiles.join(profile).is_file(),
                "missing profile {profile}"
            );
        }

        let renamed = std::fs::read_to_string(profiles.join("andare-renamed.json"))
            .unwrap_or_else(|e| panic!("reading the renamed profile: {e}"));
        assert!(
            renamed.contains("create_work_item"),
            "the renamed profile must expose a name rev-local does not look for first"
        );
        assert!(
            !renamed.contains("\"create_issue\""),
            "...and must NOT also expose the obvious name, or the resolution is never exercised"
        );
    }
}
