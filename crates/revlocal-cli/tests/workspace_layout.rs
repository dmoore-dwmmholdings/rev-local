//! Acceptance tests for `RL-101` — the workspace scaffold itself.
//!
//! These assert the shape SPEC §4.1 requires, so that a crate silently
//! disappearing from the workspace, or the unwrap/expect ban being relaxed,
//! fails the build rather than passing unnoticed.

use std::path::PathBuf;

/// The eight crates SPEC §4.1 requires, in the order the spec lists them.
const REQUIRED_CRATES: [&str; 8] = [
    "revlocal-core",
    "revlocal-store",
    "revlocal-vcs",
    "revlocal-engine",
    "revlocal-mcp",
    "revlocal-publish",
    "revlocal-daemon",
    "revlocal-cli",
];

/// The workspace root, resolved from this crate's manifest directory.
///
/// Returns a path rather than panicking so that the fallible step stays inside a
/// `#[test]` function, where clippy's unwrap/expect ban is lifted (see ADR 0003).
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// Read a workspace-relative file, surfacing the path in the error.
fn read(relative: &str) -> std::io::Result<String> {
    std::fs::read_to_string(workspace_root().join(relative))
}

#[test]
fn every_spec_crate_exists_and_is_a_workspace_member() {
    let root = workspace_root();
    let manifest = read("Cargo.toml").expect("workspace Cargo.toml is readable");

    for crate_name in REQUIRED_CRATES {
        let dir = root.join("crates").join(crate_name);
        assert!(
            dir.join("Cargo.toml").is_file(),
            "SPEC §4.1 requires crates/{crate_name}, but it has no Cargo.toml"
        );
        assert!(
            manifest.contains(&format!("\"crates/{crate_name}\"")),
            "crates/{crate_name} exists but is not a member of the workspace"
        );
    }
}

#[test]
fn placeholder_directories_for_the_tauri_shell_and_ui_exist() {
    let root = workspace_root();
    for dir in ["src-tauri", "ui", "fixtures"] {
        assert!(
            root.join(dir).is_dir(),
            "SPEC §4.1 requires a {dir}/ directory at the workspace root"
        );
    }
}

#[test]
fn unwrap_and_expect_are_denied_outside_tests() {
    let manifest = read("Cargo.toml").expect("workspace Cargo.toml is readable");
    let clippy = read("clippy.toml").expect("clippy.toml is readable");

    for lint in ["unwrap_used", "expect_used"] {
        assert!(
            manifest.contains(&format!("{lint} = \"deny\"")),
            "[workspace.lints.clippy] must deny {lint}"
        );
    }
    for key in [
        "allow-unwrap-in-tests",
        "allow-expect-in-tests",
        "allow-panic-in-tests",
    ] {
        assert!(
            clippy.contains(&format!("{key} = true")),
            "clippy.toml must set {key} so the ban applies to non-test code only"
        );
    }
}

#[test]
fn revlocal_core_declares_no_io_dependencies() {
    // SPEC §4.1: revlocal-core must stay unit-testable — no tokio, sqlx or reqwest.
    // RL-104 turns this into a full transitive check; RL-101 asserts the direct one.
    let manifest =
        read("crates/revlocal-core/Cargo.toml").expect("revlocal-core manifest is readable");
    for forbidden in ["tokio", "sqlx", "reqwest"] {
        assert!(
            !manifest.contains(forbidden),
            "revlocal-core must not depend on {forbidden} (SPEC §4.1)"
        );
    }
}
