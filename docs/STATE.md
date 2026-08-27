# Build state
- current_milestone: M1
- current_item: RL-105
- item_status: not_started
- last_gate_command: cargo test -p revlocal-core --test no_io_deps
- last_gate_result: PASS — exit 0, 2 passed. Negative case observed by injection (below).
- last_visual: n/a
- next_action: RL-105 (REVL-18) — risk classification model (SPEC §12.3), including
    "first use of a capability is always high risk", which is a decision of record.
- blocked_on: none
- adrs_open: none
- iterations_this_item: 1
- items_closed: [RL-101, RL-102, RL-103, RL-103b, RL-104]
- andare_connected: true

## Environment observed (2026-08-27)

| Tool | Status |
|---|---|
| `cargo` / `rustc` | 1.98.0 |
| `clippy`, `rustfmt` | installed this iteration via `rustup component add` |
| `claude` | on PATH |
| `codex` | **absent** — RL-408 and the Codex half of RL-1203 stay blocked |
| `svn` | **absent** — RL-202 and RL-E9 will need it installed |
| `framewatch` | on PATH |
| `git` | on PATH; repo initialised this iteration (it was not a git repo) |

## Known blockers

1. ~~Andare MCP not connected~~ — **resolved.** Andare is connected, the backlog is
   already imported as project `REVL` (RL-101 = `REVL-14`), and Andare is now the
   status source of truth. `RL-606` (discover Andare's tool surface) is unblocked.
2. `codex` is not installed. `RL-408` and the Codex half of `RL-1203` are blocked.
   Everything else proceeds — the mock engine covers the inner loop.
3. `svn` is not installed. Needed from `RL-202` onward; not yet blocking.
4. Framewatch in headless CI is unverified (`RL-1104`). GUI gates are loop-local.

## negative cases observed

A guard that has only ever passed is not known to work. Where an item's criteria say
a check must *fail* on bad input, the failure was produced deliberately and observed:

- **RL-104** — `tokio` added to `revlocal-core`'s manifest, test run, observed
  `FAILED` with `revlocal-core -> tokio (normal)`; then `tokio-util` substituted to
  force a transitive arrival, observed `revlocal-core -> tokio-util (normal) ->
  tokio (normal)`. Manifest restored, `git diff` empty, test green again.

## spec_amendments

The spec has been changed once, under the SPEC §5 implementation note ("if you must
deviate, do it — but record why in docs/adr/ and update this section in the same
commit").

- **`run.degraded TEXT`** added to the `run` table in SPEC §5 (`RL-103b`, ADR 0005).
  §8.1 gives `EngineOutcome` a `degraded: Option<String>` and §12.3 escalates every
  action on a degraded run to high risk, but the `run` table had nowhere to keep it.
  Nullable reason, not a flag. **`RL-108`'s `0001_init.sql` must include it.**

## ci_green_unobserved

`RL-102` acceptance criterion 2 — "all three OS legs green" — **cannot be observed from
this container**: `act` is not installed and there is no GitHub remote configured. What
was observed is that `.github/workflows/ci.yml` parses and asserts its own contract:
`crates/revlocal-cli/tests/ci_workflow.rs` (6 tests) checks the push/PR triggers, the
three-runner matrix with `fail-fast: false`, an `svn` installer per OS plus an
unconditional `svn --version` verification, the four gates in BUILD_LOOP §2 order, the
cargo cache, and the `if: failure()` upload of `artifacts/logs/**` + `artifacts/gui/*.png`.

**The legs stay unverified until the first push to a GitHub remote.** Re-check this
section then; if a leg is red, RL-102 reopens.

## live_engine_notes

None yet — no engine-live suite exists before M5.
