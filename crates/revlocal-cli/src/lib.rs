//! The parts of `revlocal` that are contract rather than plumbing.
//!
//! A binary crate's modules cannot be reached from an integration test, and §14
//! makes this CLI the acceptance-test API — so the pieces a test must assert on
//! live here, and `main.rs` uses them like any other caller.
//!
//! Deliberately small. Command implementations stay in the binary; what is shared
//! is the contract: exit codes today, and the `--json` report shapes as they
//! stabilise.

pub mod backfill;
pub mod control;
pub mod decide;
/// `doctor` lives in the daemon so the desktop app can run it too (§15 screen 6).
///
/// Re-exported rather than moved-and-forgotten: `revlocal doctor` is §14's
/// command and this is still where the CLI reaches for it. The alternative was
/// the Tauri crate depending on `revlocal-cli`, which is a front end depending on
/// another front end — the shape RL-1105 already rejected once.
pub use revlocal_daemon::doctor;
pub mod exit;
pub mod export;
pub mod hooks;
pub mod inspect;
/// Repository commands live in the daemon so the desktop app can add a repository
/// too — §15's onboarding walks somebody through exactly that, and a front end
/// depending on another front end for it is the shape RL-1105 rejected.
pub use revlocal_daemon::repos as repo;
pub mod watch;
pub mod webhook;

pub use exit::Exit;
