# rev-local

Local, autonomous code review for git, GitHub and Subversion.

rev-local watches repositories you already have on disk, reviews each new change
with a coding CLI you already have installed (`claude`, `codex`), and publishes the
findings back to wherever the work lives — a GitHub pull request, an issue tracker,
a wiki. Everything runs on your machine: the repository is never uploaded, and the
review engine is a process you control.

## Status

**Early development.** The library and its test suite are substantial; reviews are
not yet run automatically.

Working today:

- **The full command line.** Every command in the specification exists except
  `db export`, and each is exercised by a test that reads the specification rather
  than a transcription of it.
- git discovery, materialization and skip rules — a review never mutates the
  repository under review
- the review pipeline end to end: depth selection, diff truncation, finding
  normalization, fingerprinting and dedupe
- engine layer for `claude` and `codex`, with process supervision and an
  environment denylist that keeps credentials away from the review process
- Subversion, including branch-merge detection
- SQLite store, audit log and budget ledger
- publish queue with idempotent delivery, approval gating and per-target retry
- triggers: polling, git hooks, and a signature-checked webhook listener
- a desktop shell that receives live run events without polling

**Not wired up yet:** `revlocal review` runs against a mock engine rather than a
real one, and `revlocal watch` discovers changes and records them but does not run
reviews. Every command that is not doing the whole job says so when you run it,
rather than looking like it worked.

See [USAGE.md](USAGE.md) for what you can run right now, and
[docs/OPERATIONS.md](docs/OPERATIONS.md) for what to do when something goes wrong.

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
