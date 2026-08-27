# Build state
- current_milestone: M3
- current_item: RL-204
- item_status: not_started
- last_gate_command: MOCK_ENGINE_MODE=valid REVLOCAL_OUT=$(mktemp -d) fixtures/mock-engine/run
- last_gate_result: PASS — exit 0, result.json written and schema-valid.
- last_visual: n/a
- next_action: RL-204 (REVL-28) — mock MCP server with a request journal
- blocked_on: none
- adrs_open: none
- iterations_this_item: 1
- items_closed: [RL-101, RL-102, RL-103, RL-103b, RL-104, RL-105, RL-106, RL-107,
                 RL-107b, RL-108]
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

## unverified_here

Work that is complete but whose verification cannot run in this container. Each has
a test that **activates itself** where the prerequisite exists, so this is a gap in
where it ran, not a gap in what exists.

- **`svn` is not installed and cannot be** — no root, `apt-get` refuses the dpkg
  lock. `RL-202`'s generator (`fixtures/svn.sh`) is written but **never executed**.
  Two of its three acceptance criteria — the revision→role manifest including
  `reintegration_rev`, and `svn:mergeinfo` verified with `svn propget` — are
  **NOT OBSERVED**. The third (clean skip, exit 0, manifest says so) is observed.

  `crates/revlocal-vcs/tests/svn_fixtures.rs` covers both worlds. The load-bearing
  one is `svn_the_manifest_agrees_with_whether_svn_is_installed`: it **fails** if a
  machine that has `svn` produced a skipped manifest, which is how a broken
  generator would otherwise hide behind a green run. The svn-gated tests print
  `SKIPPED (svn not installed, nothing verified)` so a passing run here cannot be
  read as coverage.

  **CI installs Subversion on all three runners (`RL-102`), so this runs there.**
  It is therefore blocked on the same thing as `ci_green_unobserved`: a git remote.
  If those tests come back red, RL-202 reopens.

## milestone gates observed

Checked by running them, not by assuming the items were finished.

- **M0** — `cargo build --workspace && cargo clippy ... -D warnings && ./target/debug/revlocal --version` → exit 0, `revlocal 0.1.0`.
- **M1** — `cargo test -p revlocal-core` → exit 0, **133 tests** (≥25 required);
  `risk::tests::every_row_of_the_spec_12_3_matrix` and
  `revlocal_core_has_no_io_dependencies` both present.
- **M2** — `cargo test -p revlocal-store` → exit 0, **66 tests**; down-migration,
  three typed `AlreadyExists` tests, and the 2-writer WAL test all present.

Build log published to Trama, space `ENG`: `rev-local build log` with a page per
milestone. **Trama's `update_page` replaces a body — `get_page` first (§11.5).**

## negative cases observed

A guard that has only ever passed is not known to work. Where an item's criteria say
a check must *fail* on bad input, the failure was produced deliberately and observed:

- **RL-111** — redaction was disabled in `RedactingVisitor` and **7 of the 9 layer
  tests failed**; the 2 that still passed are the ones asserting ordinary logging is
  unharmed and still valid JSON, which correctly do not depend on redaction. Then
  restored, `git diff` empty, green again.

- **RL-110** — the append-only audit guard was verified by injecting a
  `DELETE FROM audit` into `publish.rs` and observing the test FAIL, naming the file
  and the statement; then restored, `git diff` empty, green again. The guard is
  structural (it scans the crate's source) because the guarantee is that no such
  method *exists* — a test that only exercises existing methods could never notice
  one being added.

- **RL-108** — the `UNIQUE (target, idempotency_key)` constraint on
  `publish_action` was exercised by inserting a duplicate and observing the
  rejection, and by inserting the same key against a *different* target and
  observing acceptance. `repo.kind = 'mercurial'` observed rejected by its CHECK.
  `an_unmigrated_database_has_no_spec_tables` keeps the positive schema test from
  passing vacuously.

- **RL-104** — `tokio` added to `revlocal-core`'s manifest, test run, observed
  `FAILED` with `revlocal-core -> tokio (normal)`; then `tokio-util` substituted to
  force a transitive arrival, observed `revlocal-core -> tokio-util (normal) ->
  tokio (normal)`. Manifest restored, `git diff` empty, test green again.

## fixture invariants worth not breaking

Properties the fixtures have *on purpose*, where the obvious simplification would
make a downstream test pass for the wrong reason.

- **`fenced_only` emits TWO fenced blocks** (`RL-203`). SPEC §8.2 says the **last**
  one is authoritative; with a single block, a runner that took the first would
  pass and be wrong in production.
- **`hang` installs a no-op SIGTERM handler** (`RL-203`). A hang mode that died on
  SIGTERM would let a runner claiming SIGTERM→grace→SIGKILL escalation pass without
  ever escalating. Verified by signalling it and observing it survive.
- **`nonzero_exit` still writes valid output** (`RL-203`), so a runner keying only
  off the exit code is caught throwing away a review the engine produced.
- **Two SVN reintegrations** (`RL-202`), one detectable by both §6.4 heuristics and
  one by `svn:mergeinfo` alone. A fixture where every signal fires cannot tell you
  which signal the code is using.
- **The git merge commit has two parents** (`RL-201`) — a fast-forward would have
  one and M4's merge skip rule would never fire.

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

- **`budget_ledger.cost_complete INTEGER NOT NULL DEFAULT 1`** added in migration
  `0002` (`RL-109c`, ADR 0010). §5 declared `cost_usd REAL NOT NULL DEFAULT 0` while
  §8.1 types an engine's cost as optional; folding an unreported cost in as `0.0`
  makes an unmeasured day look free. `cost_usd` now sums only reported costs and the
  flag says whether anything was missing. **`BudgetLedgerEntry::cost_exhausted`
  returns `Option<bool>` — the daemon's BudgetGuard must treat `None` as a stop.**

- **SPEC §13.2's in-repo override rule** rewritten (`RL-107b`, ADR 0007). The section
  stated the rule by example ("repo-local wins for scope/ignores, never for autonomy or
  targets"), which reads as a denylist of two keys. Implemented as an **allowlist of
  five**, so a field added to `RepoConfig` later is refused by default rather than
  granted to every repository silently. `trama_publish` and `allow_approve` are refused
  too, though §13.2 did not name them: both change an action's risk class. §13.2 now
  states the rule as built.

- **`run.degraded TEXT`** added to the `run` table in SPEC §5 (`RL-103b`, ADR 0005).
  §8.1 gives `EngineOutcome` a `degraded: Option<String>` and §12.3 escalates every
  action on a degraded run to high risk, but the `run` table had nowhere to keep it.
  Nullable reason, not a flag. **Carried into `0001_init.up.sql` by `RL-108`, with a
  test asserting the column exists — done.**

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
