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

/// The desktop shell is a workspace crate, and §4.1 says so (REVL-124).
///
/// It was not always. RL-101 scaffolded `src-tauri/` and `ui/` at the root, as
/// Tauri's own convention has it, and RL-1101 then built the shell at
/// `crates/revlocal-tauri/` instead — leaving two directories containing nothing
/// but a README saying the real thing was built elsewhere. A stranger cloning the
/// public repository was told twice that the app lived somewhere it did not.
///
/// Resolved toward the crate. `src-tauri/` at the root is the convention for a
/// single-app project whose front end is also at the root; this is a workspace of
/// ten crates where the shell is one consumer of the library, and one crate
/// outside `crates/` would be the anomaly. So §4.1's diagram was what needed
/// changing, and the code stayed.
///
/// Both halves are asserted, because only asserting the new location would let
/// the placeholders quietly come back.
#[test]
fn the_desktop_shell_is_a_crate_and_not_a_root_directory() {
    let root = workspace_root();

    for dir in [
        "crates/revlocal-tauri",
        "crates/revlocal-tauri/ui",
        "fixtures",
    ] {
        assert!(root.join(dir).is_dir(), "SPEC §4.1 requires {dir}/");
    }

    for stale in ["src-tauri", "ui"] {
        assert!(
            !root.join(stale).exists(),
            "{stale}/ is back at the workspace root. The shell lives at \
             crates/revlocal-tauri/ and §4.1 says so; a directory here claims to \
             hold code that is somewhere else"
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
