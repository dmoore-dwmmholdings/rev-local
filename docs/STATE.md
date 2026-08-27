# Build state
- current_milestone: M1
- current_item: RL-107b
- item_status: in_progress
- last_gate_command: cargo test -p revlocal-core config::
- last_gate_result: PASS — exit 0, 22 passed.
- last_visual: n/a
- next_action: RL-107b (REVL-109) — the .rev-local.toml overlay and the rule that a
    repository may narrow scope/ignores but never widen autonomy or add targets
- blocked_on: none
- adrs_open: none
- iterations_this_item: 1
- items_closed: [RL-101, RL-102, RL-103, RL-103b, RL-104, RL-105, RL-106]

## RL-107 was split

The two config documents, their defaults and unknown-key handling landed as the first
half. The in-repo overlay and its security rule are **REVL-109 / RL-107b**, a subtask of
REVL-20. REVL-20 stays `In Progress`: three of its five criteria concern the overlay and
are not yet true.
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

## frozen_by_golden_vectors

Changing these silently corrupts data already stored. Each has committed vectors that
fail loudly on drift; regenerating them is a **data migration**, not a test update.

- **Finding fingerprints** (`RL-106`, SPEC §10.3). 6 vectors in
  `crates/revlocal-core/src/fingerprint.rs`, cross-checked against an independent
  implementation written from the spec text. A normalization change re-fingerprints
  every stored finding, which means every one re-files as new. Write an ADR and plan
  the migration before touching `normalize_title` or `normalize_path`.

## spec_gaps_resolved_by_principle

Places where the spec's lists are not exhaustive over the input space, resolved
against the principle the spec itself states. Each has a named test; each is one
line to change if a decision comes back differently.

- **SPEC §12.3 risk lists** (`RL-105`, ADR 0006). Two action shapes are not
  enumerated. Both taken as **low**, per §12.3's own "additive, easily reversible,
  low blast radius": a `Check { in_progress }` (a progress report that blocks
  nobody — §11.3 says the check is always in-progress while a run is active), and
  `LinkDocToIssue` (an additive, reversible cross-reference).

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
