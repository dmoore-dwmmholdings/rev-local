//! Acceptance test for `RL-103` — newtype ids are not interchangeable.
//!
//! A `RepoId` reaching a function that wanted a `RunId` is a bug that unit tests
//! cannot catch, because both are `i64` underneath and both look plausible. The
//! only place to catch it is the type checker, so the assertion is that certain
//! programs **fail to compile**.
//!
//! trybuild pins the exact rustc diagnostic, which makes it sensitive to compiler
//! version. Regenerate the `.stderr` files with `TRYBUILD=overwrite cargo test -p
//! revlocal-core` and read the diff before committing — a changed message is
//! usually a new rustc, but it can be a real weakening of the type boundary.

#[test]
fn ids_of_different_entities_do_not_substitute_for_one_another() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/*.rs");
}
