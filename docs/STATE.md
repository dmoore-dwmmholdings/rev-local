# Build state

- current_milestone: M6
- current_item: RL-502
- item_status: not_started
- last_gate_command: cargo test -p revlocal-daemon state_machine
- last_gate_result: PASS — exit 0, 10 passed.
- last_visual: n/a
- next_action: RL-502 (REVL-47) — prompt assembly. Open items that are NOT next by
    priority but matter: REVL-115 (RL-409, budgets unenforceable against a real
    engine), REVL-113 (RL-305b), REVL-45 (RL-408, blocked on `codex`).

## config gap: no attempt ceiling

`RL-501` needed a maximum retry count and SPEC §13.1 has none — it has
`stale_run_minutes` but nothing bounding attempts. Without a ceiling, a change that
crashes the daemon is recovered on every startup, crashes again, and rev-local spends
its life re-reviewing one commit while never reaching the rest.

`DEFAULT_MAX_ATTEMPTS = 3` lives in `revlocal-daemon` and is a **parameter**, not a
constant read at the call site, so it can become config without a rewrite. Worth
adding to §13.1's `[global]` when someone touches it.

## a gate that selected nothing

`RL-406`'s gate is `cargo test -p revlocal-engine env_denylist`. The work had landed
inside `RL-405` with tests named `supervision_*`, so the gate **selected zero tests
and exited 0** — observed before fixing it. A filter matching nothing is the quietest
way for a gate to pass while testing nothing.

**When an item's gate is a name filter, check how many tests it selected, not just
its exit code.** `cargo test ... | grep '^test result' | awk '{s+=$4} END {print s}'`
is enough. `RL-108`'s test file already warned about this in a comment; it happened
anyway, one crate over.

## a correction

Last iteration's report said "RL-309 (REVL-38) — diff truncation" was next. **There
is no RL-309.** M4's VCS epic ends at RL-308, REVL-38 is RL-401 (the engine trait),
and truncation is **RL-504 / REVL-49** in the M5 pipeline epic. The RL-1304 audit was
still worth doing first — `publish_action.next_attempt_at` precedes REVL-60, and the
truncation columns now precede REVL-49 — but the claim that it unblocked *the very
next item* was wrong. **Check the item, do not infer the id from the sequence.**

## counting tests

`cargo test --workspace 2>&1 | grep -c '^test .* ok'` **undercounts** — parallel test
binaries interleave their output and some lines are split. Sum the `test result` lines
instead:

```
cargo test --workspace 2>&1 | grep -E "^test result" | awk '{s+=$4} END {print s}'
```

Current total: **308 passing, 0 failing.**
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

## silent caps found

Places where an unmeasured value reads as a benign one. SPEC §18 forbids the class;
these are the instances found so far.

- **Cost** (`RL-109c`, ADR 0010) — fixed. `budget_ledger.cost_complete` plus
  `cost_exhausted -> Option<bool>`.
- **Tokens** (`RL-407`, **REVL-115 open**) — §8.1 requires `EngineOutcome.usage` but
  §8.3's `result.json` has **no usage field**, so a real engine's runner has nothing
  to report and returns `Usage::default()` — *zero*. A run that spent 40,000 tokens
  records none, and a 2,000,000-token daily budget is never reached. **Every existing
  test passes with this gap present, because the mock engine reports tokens and the
  real thing does not.** ADR 0010's "tokens are always known" is wrong and REVL-115
  corrects it.

  The pattern worth carrying: *when a fixture is more honest than the real thing, the
  tests cannot see the gap.*

## criteria not met as written

Recorded rather than quietly satisfied. Each is a place where an acceptance criterion
and the spec pull against each other, resolved deliberately.

- **`RL-405` criterion 1** — "hung mock engine is killed within timeout + 2s" cannot
  hold together with §8.5's five-second SIGTERM grace, for a process that ignores
  SIGTERM. **The grace period wins** (ADR 0017): shortening it would lose a review
  whose tokens were already spent, on exactly the slow runs where it was most
  expensive. The "+2s" is tested as *supervisor overhead* against a SIGTERM-respecting
  process; the pathological path is bounded at `timeout + grace + 2s`. If the two
  seconds are wanted literally, §8.5's grace is the thing to change.

## unverified_here

Work that is complete but whose verification cannot run in this container. Each has
a test that **activates itself** where the prerequisite exists, so this is a gap in
where it ran, not a gap in what exists.

- **`RL-405` criterion 4** — "test passes on all three platforms in CI" is
  **NOT OBSERVED**: CI has never run (no remote). The Unix path is fully exercised
  here; the Windows path needs a Job Object, which §8.5 requires and `RL-1303` owns,
  and until then a timed-out engine can leave grandchildren on Windows. The
  `#[cfg(not(unix))]` branch logs that rather than silently doing nothing.

- **`pwsh` is not installed** — `RL-205`'s gate (`pwsh fixtures/build.ps1`) has
  **never run**, and acceptance criterion 1 (same commit SHAs as the bash script) is
  **NOT OBSERVED**. Most of that risk was removed structurally rather than tested:
  the file bodies live once under `fixtures/content/` and both drivers *copy* them,
  and the commit sequence lives once in `steps.json`, so only the git invocations
  can diverge. Two structural tests run everywhere and assert that design holds.
  `parity_powershell_produces_the_same_commit_shas_as_bash` runs where pwsh exists —
  **GitHub runners ship pwsh on all three platforms**, so it runs in CI.

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

- **M3** — `./fixtures/build.sh && test -d fixtures/out/git-basic && test -d
  fixtures/out/svn-basic` → exit 0. Note §17 allows the svn portion to skip cleanly
  when `svn` is absent, and it did: `svn-basic/` exists and its manifest says
  `skipped: true`. The gate passes **without the svn fixture having been built**.

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

- **RL-302** — two guards, both observed failing when broken. Replacing `killpg`
  with `kill` (child only, not the group) made
  `git_cmd_a_timeout_kills_the_child_and_its_whole_process_group` FAIL with
  "grandchild N survived the timeout". Adding a `Command::new("git")` to
  `scratch.rs` made `git_cmd_no_module_spawns_git_directly` FAIL naming the file.
  Both restored, 12/12 again.

- **RL-204** — read-before-write enforcement was disabled in `server.js` and the
  selftest observed FAILING 4 of 32 checks with exit 1; restored, `git diff` empty,
  32/32 again.

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

## contracts held by a cross-check

Places where two independently-written artefacts must agree, and a test makes them
prove it rather than trusting review.

- **The fixture engine's output vs `result.v1.json`** (`RL-402`). The fixture is
  JavaScript, the validator is Rust, and they encode the same §8.3 contract. The test
  *runs* the fixture and validates what it actually wrote — grepping its source for
  plausible shapes would not catch drift. A second test asserts `partial_findings`
  still produces both valid and invalid findings, or §8.3's drop path stops being
  tested anywhere.
- **The golden fingerprint vectors vs an independent implementation** (`RL-106`).
- **`GH_PR_FIELDS` vs the fields the parser reads** (`RL-308`).

## enforced by a guard test

Rules that hold because a test fails when they are broken, not because everyone
remembers them. Each was observed failing.

- **Materializing never mutates the repository under review** (`RL-306`). Asserted
  over *every* fixture commit by capturing HEAD, tree hash, `status --porcelain`,
  branches, stash, `worktree list` and the index before and after, and separately
  against a repo with uncommitted work and an untracked file. A review that stashed
  someone's work-in-progress at 3am would be worse than no review.

- **An engine invocation is argv, never a command line** (`RL-403`). A rendered
  template is a program plus a `Vec<String>` handed to `Command::args`. Nothing is
  concatenated and nothing reaches a shell, so a prompt containing `; rm -rf /` is
  one argv element — argv has no syntax for it to escape into. The prompt is
  attacker-influenced in the ordinary case, because it contains the diff.
  `template_a_prompt_full_of_shell_metacharacters_is_one_argument` is the guard, and
  `template_rendering_produces_argv_not_a_command_line` is where adding a
  command-line field to `Invocation` should be reconsidered.

- **Only `git::cmd` may spawn `git`** (`RL-302`). A second call site is not a style
  problem — it is a call site with no timeout and no prompt suppression, and it will
  be found when the daemon hangs on someone's private repository. Production code
  only; test code inspecting a fixture repo is exempt and the test says why.
- **The audit log has no update or delete** (`RL-110`), asserted by scanning the
  store crate's source rather than by exercising the methods that exist.
- **`revlocal-core` reaches no I/O crate transitively** (`RL-104`), walked from
  `cargo metadata`'s resolve graph and reported with the full dependency path.

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
- **The mock MCP's read-before-write is per PAGE, not global** (`RL-204`). A rule
  tracking "has read anything" would be satisfied by any startup read and would let
  every page be overwritten blind. The selftest reads one page and asserts writing a
  *different* one is still refused.
- **`profiles/andare-renamed.json` exposes `create_work_item` and NOT
  `create_issue`** (`RL-204`). If it exposed both, capability resolution would never
  be exercised — the client would find the name it looks for first and the §11.2
  claim would go untested.
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

- **Five columns added in ONE migration (`0004`)** by the §5 audit (`RL-1304`, ADR
  0016), rather than one at a time while implementing: `run.truncated`,
  `run.omitted_files_json`, `run.verdict`, `run.summary`,
  `publish_action.next_attempt_at`. Plus two `RepoConfig` fields §7.3 required and
  §13.2 lacked: `webhook_enabled`, `webhook_secret_ref`. **ADR 0016 also lists what
  was deliberately NOT added, so the next audit does not re-litigate it.**

- **`repo.github_transport TEXT`** added in migration `0003` (`RL-307`, ADR 0015).
  §6.3 says the selected transport is "stored on the repo row" and §5 had nowhere to
  put it. A column rather than `config_json`, because that holds what the *user
  chose* and this is what the *ladder found* — a doctor report exists to tell those
  apart. `NULL` = not probed, deliberately distinct from `'unauthenticated'`.

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

- **SPEC §13.2's `ignore_globs` default** corrected to match §9.4 (`RL-305`, ADR
  0014). The two sections gave **different defaults for the same field** — three
  globs in §13.2, seven in §9.4. Found by a test, not by reading. §9.4 wins; §13.2's
  example document now matches, and `RL-107`'s defaults test parses that document,
  so they cannot drift apart again silently.

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
