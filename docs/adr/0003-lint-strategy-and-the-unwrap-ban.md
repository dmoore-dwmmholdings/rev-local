# 3. Lint strategy and the unwrap/expect ban

Date: 2026-08-27
Status: accepted
Item: RL-101

## Context

A hard constraint of this project forbids `unwrap()` and `expect()` outside tests,
and every new public item requires a doc comment. Both need to
be enforced by the gate (`cargo clippy --workspace --all-targets -- -D warnings`)
rather than by review, and enforced once for all eight crates rather than repeated in
eight `lib.rs` headers.

## Decision

Lints are configured in two places, and member crates opt in with `[lints] workspace = true`:

- `[workspace.lints]` in the root `Cargo.toml` sets the policy —
  `clippy::unwrap_used = "deny"`, `clippy::expect_used = "deny"`,
  `clippy::panic = "warn"`, `clippy::todo = "warn"`, `missing_docs = "warn"`,
  `unsafe_code = "forbid"`.
- `clippy.toml` sets `allow-unwrap-in-tests`, `allow-expect-in-tests` and
  `allow-panic-in-tests`, which are the only exemptions. Each lint needs its own
  key — `allow-unwrap-in-tests` does not cover `clippy::panic`, which is why a
  `panic!()` inside a `#[test]` function still failed the gate in `RL-102`.

`warn` and `deny` are equivalent under the gate's `-D warnings`; the distinction records
intent. `missing_docs` at `warn` is what makes "every public item has a doc comment"
an observed gate result instead of a habit.

## Consequence worth knowing

`allow-unwrap-in-tests` exempts code inside a `#[test]` function or a `#[cfg(test)]`
module — **not** a plain helper function in an integration test file, which clippy sees
as ordinary code. `crates/revlocal-cli/tests/workspace_layout.rs` hit this: its
`workspace_root()` and `read()` helpers failed the gate.

The fix, and the pattern for future test helpers: helpers return `Option`/`Result` and
the `#[test]` function does the unwrapping. Do not `#[allow]` the lint locally — the
ban is a hard constraint, and a helper that cannot fail is easy to write.

`workspace_root()` therefore builds its path as `CARGO_MANIFEST_DIR/../..` rather than
via `Path::ancestors().nth(2)`, which returns an `Option`.

## Alternatives rejected

- **`#![deny(...)]` in each `lib.rs`** — eight copies to keep in sync, and it does not
  cover integration tests or build scripts.
- **`clippy::pedantic`** — too noisy to keep at `-D warnings` across a workspace this
  size; specific lints are added as they earn their place.
