# rev-local

Local, autonomous code review for git, GitHub and Subversion.

rev-local watches repositories you already have on disk, reviews each new change
with a coding CLI you already have installed (`claude`, `codex`), and publishes the
findings back to wherever the work lives — a GitHub pull request, an issue tracker,
a wiki. Everything runs on your machine: the repository is never uploaded, and the
review engine is a process you control.

## Status

**Early development.** The library and its test suite are substantial; the
user-facing surface is not finished yet.

Working today:

- git discovery, materialization and skip rules — a review never mutates the
  repository under review
- the review pipeline end to end: depth selection, diff truncation, finding
  normalization, fingerprinting and dedupe
- engine layer for `claude` and `codex`, with process supervision and an
  environment denylist that keeps credentials away from the review process
- SQLite store, audit log and budget ledger
- MCP client over stdio and streamable HTTP

Not wired up yet: live engine selection from the CLI (`revlocal review` runs
against a mock engine), publishing, triggers, Subversion, and the desktop UI.

See [USAGE.md](USAGE.md) for what you can run right now.

## Design

Three properties the code is organised around:

- **Reviewing never writes to the repository under review.** Changes are
  materialized into a scratch worktree, and the fixture tests assert the source
  tree is byte-identical afterwards.
- **Nothing is silently dropped.** Where the system truncates, samples or skips, it
  records the fact and reports it. A review that saw 60% of a diff must never look
  like a review that saw all of it.
- **The first write to any system is always approved by a human**, whatever the
  configured autonomy level.

`SPEC.md` is the design specification; `docs/adr/` records the decisions that were
not obvious. Section references (§) in source comments point into `SPEC.md`.

## Building

Requires a Rust toolchain (1.82+) and `node` for the test fixtures.

```
cargo build --workspace
cargo test --workspace
```

Some tests skip cleanly when an optional tool is absent and say so rather than
passing quietly — `svn` for the Subversion fixtures, `pwsh` for the Windows
fixture-parity check.

## License

Apache-2.0. See [LICENSE](LICENSE).
