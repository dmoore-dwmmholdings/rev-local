//! The parts of `revlocal` that are contract rather than plumbing.
//!
//! A binary crate's modules cannot be reached from an integration test, and §14
//! makes this CLI the acceptance-test API — so the pieces a test must assert on
//! live here, and `main.rs` uses them like any other caller.
//!
//! Deliberately small. Command implementations stay in the binary; what is shared
//! is the contract: exit codes today, and the `--json` report shapes as they
//! stabilise.

pub mod control;
pub mod doctor;
pub mod exit;
pub mod hooks;
pub mod inspect;

pub use exit::Exit;
