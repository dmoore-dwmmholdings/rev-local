# Build state

- current_milestone: M6
- current_item: RL-507 (REVL-52)
- item_status: done
- last_gate_command: cargo test -p revlocal-daemon determinism
- last_gate_result: PASS — 7 tests, 7 passed; workspace 612 passed / 0 failed
- last_visual: n/a
- next_action: confirm M6 is complete, run the §4 checkpoint, write the Trama build log for M4/M5/M6
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

## a template that escaped its own comment

`{{! ... }}` in handlebars ends at the **first** `}}`. The template's header comment
explained triple-stache syntax, wrote `{{{diff}}}` as the example, and thereby closed
itself inside its own example — spilling the rest of the comment, including the
literal text `&lt;`, into every rendered prompt.

Nothing failed. The prompt rendered, the sections were in order, the diff was intact.
It was caught only because `prompt_a_diff_is_not_html_escaped` asserts on the *whole*
document rather than on the diff fence, so the stray `&lt;` from the prose tripped it.

Two things worth keeping:

- Scoping that assertion tightly to the diff block — the "cleaner" version — would
  have shipped this. A guard that over-reaches sometimes earns its false positives.
- Use `{{!-- --}}` for any handlebars comment that mentions handlebars.

## the escaping hazard itself

Handlebars HTML-escapes `{{value}}`. A diff run through it is corrupted silently:
`<` → `&lt;`, and the engine reviews code nobody wrote, then cites lines that do not
exist. No error, no warning, nothing odd in the transcript. Every interpolation in
`review.md.hbs` is `{{{...}}}` and a test guards it. Negative case observed: flipping
`{{{diff}}}` to `{{diff}}` fails that test and only that test (20 passed, 1 failed).

## config gap, fourth of its kind

`max_convention_bytes` / `max_file_diff_bytes` / `max_total_diff_bytes` were named
with defaults in §9.2 and §9.4 prose and present in neither §13.1 nor §13.2. Added to
`RepoConfig` and to §13.2's document (ADR 0018). All three at once, per RL-1304.

The pattern now has four instances (`degraded`, `webhook_enabled`, `ignore_globs`,
these). **§9's and §7's prose name defaults that §13's documents do not carry.** Worth
one sweep of §7–§12 prose for `default` against §13 before M6, rather than a fifth
discovery.

## the config sweep is done — five misses, two false positives

RL-502 recommended one sweep of §7–§12 prose against §13's documents rather than a
fifth ad-hoc discovery. Run in RL-503, mechanically (every backticked lowercase
identifier in §7–§12, minus the keys the config structs declare).

Genuine misses: `deep_file_limit`, `deep_labels` (§9.3, added in RL-503);
`file_medium_issues`, `andare_transition_on` (§11.4); `max_attempts` (§13.1, the gap
RL-501 recorded). The last three are **REVL-116**, filed rather than added — a config
key with no reader is how a document acquires fields nobody can explain.

False positives worth not re-checking: `engine_timeout` is a failure-reason string,
`version_args` already lives on `InvocationTemplate`.

**The sweep is closed. Do not re-run it.** If a sixth key turns up, it is new prose,
not a missed one.

## a spec table that contradicted its own acceptance criteria

§9.3's table has a `summary` row (size) and a `deep` row (risk) and does not say which
wins. This item's criteria 1 and 2 describe a 200-file commit touching `**/auth/**`
two different ways.

Resolved in ADR 0019: **risk beats cost**, because a summary review of an auth change
reports nothing and looks exactly like a clean one. The losing reason is retained and
`is_contested()` exposes it, so the size rule does not look broken to whoever reads the
config next.

Worth generalising: when two spec rules fire on the same input, ask which failure is
*silent*. That is the one to design against.

## negative cases observed (RL-503)

- Dropping `if current != Depth::Standard` from `escalate` → fails exactly
  `depth_a_deep_run_never_escalates_again` and `depth_a_summary_run_does_not_escalate`
  (24 passed, 2 failed).
- `max()` → `min()` in `DepthDecision::resolve` (size beats risk) → fails exactly
  `depth_risk_beats_size_when_both_fire` (25 passed, 1 failed).

## a cross-check that caught a formatting slip

`the_repo_defaults_are_the_spec_13_2_document` parses the §13.2 document embedded in
`config/mod.rs` and compares it to `RepoConfig::default()`. It rejected the `// §9.3`
trailing comments I copied over from SPEC.md — JSON has no comments. Cheap catch, but
the reason it works is that the document is *parsed*, not eyeballed: the test would
equally catch a default that drifts from the spec's stated one.

## two test-setup bugs of the same shape (RL-504)

Both failures on the first run of the truncation gate were my *test arithmetic*, not
the code. Worth recording because the shape recurs:

1. I sized budgets as `each * n`, assuming every generated diff section was the same
   length. They are not — the path appears three times in a section header, so
   `README.md` and `tests/engine_test.rs` differ by tens of bytes. The test passed a
   different boundary than the one it claimed to. Fixed by computing budgets from the
   real section lengths.
2. I set a per-file cap low enough that *both* files became stat lines, which made
   room, so nothing was omitted and the assertion about omission never got its case.

**A test whose setup computes a boundary from an assumption about the code under test
is testing the assumption.** Derive the boundary from the actual artefact.

## negative cases observed (RL-504)

- Stop pushing to `omitted_files` → 9 of 24 fail. The breadth is the point: the
  omitted list is load-bearing in nearly every assertion, including the cross-module
  one that renders it into the prompt.
- Reverse the interest sort → 3 fail, including the mixed-commit criterion.
- Skip the `file.binary` branch → exactly the 2 binary tests fail.

## a cross-module assertion worth keeping

`truncation_omitted_files_reach_the_rendered_prompt` runs truncate → build_context →
render and asserts every omitted name appears in the **rendered prompt**. Criterion 2
says "named in the prompt", and asserting it on `TruncationOutcome` alone would have
been asserting it on the wrong artefact — the struct is not what the engine reads.

## §9.5 contradicted itself, §18, and this item's own criteria

Three conflicts, resolved in ADR 0021 (sixth SPEC amendment):

1. §9.5 says "clamp severity; unknown → medium", but §8.3's schema rejects an unknown
   severity before normalization sees it. **The clamp was unreachable as specified.**
   Resolved by salvaging from the recorded drop rather than relaxing §8.3.
2. §9.5 says "drop out-of-diff findings unless allow=true — an out-of-diff finding is
   retained but forced to ≤medium". The two halves of one sentence disagree, and
   criterion 1 states the second. Resolved for retain-and-cap.
3. §9.5 says "drop findings matching a suppression"; criterion 2 says they must not
   reach the *publish plan*. Resolved by labelling, not dropping.

The through-line: **normalization labels, it never discards.** A dropped finding is
unanswerable — "why didn't it mention src/other.rs?" has an answer from a labelled
row and only silence from a discarded one. `FindingState` already had `Suppressed`
and `Superseded` for this.

## the assumption that would have failed silently

The severity salvage recognises a severity-only failure by looking for `"severity"` in
the violation text **`revlocal-engine` produced**. That is an assumption about another
crate's wording. Had it been wrong, salvage would never fire in production while every
hand-written test in this module still passed — a silent cap behind a green suite.

`normalize_salvages_what_the_real_validator_actually_drops` runs the real validator on
a real document and feeds its real output in. It passed, so the assumption held; it is
now pinned rather than assumed.

**Generalise: when a test's fixture stands in for another component's output, at least
one test must use the real output.** Same shape as RL-504's "a test whose setup
computes a boundary from an assumption is testing the assumption."

## two glob rules that err in opposite directions, on purpose

- `sensitive_globs` uncompilable → **everything** sensitive (ADR 0019).
- suppression glob uncompilable → **nothing** suppressed (ADR 0021).

Both err toward *saying more*. Worth stating that way, because "fail safe" alone would
have given the wrong answer for one of them.

## negative cases observed (RL-505)

- Don't cap out-of-diff severity → 1 fails.
- Suppressed findings become publishable → 3 fail.
- Don't supersede repeats → 4 fail.
- Stop salvaging severities → 2 fail, including the real-validator cross-check.

## a negative probe that did not bite — and how to tell why

Three probes against the pipeline. **Two did not fail, for opposite reasons**, and the
difference is the whole lesson:

- Leaking the worktree path into `summary` → nothing failed, because `summary` is
  overwritten on the success path. **The probe was ineffective.** Re-probing through
  `repo` failed both stability tests, as it should.
- Swapping `publishable()` for all findings in the escalation check → nothing failed,
  because every existing capped-finding case was still `Open`. **That was a real
  gap**: the design claim "escalation asks the normalized findings" had no test.
  `a_suppressed_critical_does_not_escalate` was written in response; the re-probe now
  fails exactly it.

**When a negative probe does not bite, establish which of the two it is before moving
on.** Assuming the test is fine leaves an untested claim in the codebase — and the
untested claim was one I had already written into a doc comment as though settled.

## a wrapper that claims more than it does is worse than none

Criterion 4 wanted "zero network access, asserted by a no-network test wrapper". The
obvious version — run an outbound command, assert it fails — **passes on any offline
machine whether the wrapper works or not**, so it asserts nothing while looking done.

`GIT_ALLOW_PROTOCOL=file` makes git refuse `https://` *before opening a socket*, with
a distinctive message. The assertion requires that message, so it fails where the
wrapper is not applied.

Its first run failed usefully: I guessed "not supported"; git says
`transport 'https' not allowed`. Third time this has come up — **assert against what
the tool emits, not what you expect it to.**

## fixture-driven tests found a cross-stage interaction I had not predicted

`a_high_severity_finding_escalates...` failed first time because I gave the finding a
file the fixture commit does not touch. §9.5 capped it to medium as out-of-diff, and a
capped finding correctly does not escalate.

My test was wrong and the code was right — but the interaction is worth a test of its
own, so `an_out_of_diff_critical_does_not_escalate_because_it_was_capped` now pins it.
Otherwise any engine hallucinating a filename could spend the escalation budget.

## RL-506 was split

REVL-51 keeps the §9 pipeline and all four criteria (the gate is daemon-side).
**REVL-117 (RL-506b)** takes the two things it also asked for: there is no
`impl VcsAdapter` anywhere — the trait is declared and only free functions implement
its parts — and the CLI has no `review` subcommand.

## a probe that found a feature which had never once worked

RL-506a's rule ("when a negative probe does not bite, establish which of the two it
is") paid for itself immediately.

Deleting `GitAdapter`'s `NoSuchChange` mapping changed **no test**. Investigating
rather than shrugging found *both* failure modes at once:

- The CLI test asserted the error "names the rev" — but git's own stderr contains the
  sha, so it passed with or without the mapping.
- **The mapping never fired.** Its three patterns were guessed. `git worktree add`
  says `invalid reference`; none of `unknown revision`, `bad revision`,
  `not a valid object name` appears.

A feature with a test, a doc comment and a green suite that did nothing at all.

Both tests now discriminate (the CLI requires the classified wording and rejects a
leaked `git worktree add`; a vcs-level test asserts the variant). Re-probing fails
both.

## THE recurring M6 lesson, now four for four

**Never write a string match against another tool's output without running the tool
and reading what it says.**

1. RL-505 — the schema validator's violation text (checked; assumption held, now pinned).
2. RL-506a — `GIT_ALLOW_PROTOCOL`'s refusal: guessed "not supported", git says
   `transport 'https' not allowed`.
3. RL-506a — a finding's file vs. what the fixture commit actually touches.
4. RL-506b — `invalid reference`, above. **This one shipped a dead code path.**

Every future string match on external output gets the tool run first. It costs one
command.

## M6 is feature-complete pending its exit gate

RL-501..506 all closed with observed gates. REVL-52 (the M6 exit gate) is next.

## the determinism audit found nothing — so the work was making it a rule

Zero `HashMap`/`HashSet` in any library crate. `Extra` (unknown config keys) is a
`BTreeMap`; the only `read_dir` is inside a guard test. The property already held.

So RL-507's real deliverable is the **source guard** that keeps it holding.
Behavioural determinism tests are weak here on their own: a `HashMap` with three
entries iterates consistently often enough to pass for months and fail in someone
else's CI. The guard scans five crates and cannot be satisfied by luck.

It strips `//` comments (prose about the rule is not a violation), walks directories
sorted (a guard reporting in filesystem order would itself be nondeterministic), and
has a self-test feeding it one file it must reject and one it must accept.

## a golden pasted from the code it tests pins the bug too

The four fingerprint goldens were **independently recomputed from §10.3's text** by a
separate implementation before being committed. All four agreed.

Worth stating as a rule: **a golden captured from the implementation only freezes
current behaviour.** If the algorithm were wrong, the golden would guarantee it stayed
wrong — and for fingerprints that means every stored suppression silently breaking on
the day it is fixed.

## why five repetitions, not two

An ordering that differs between two hash instances agrees by chance roughly half the
time with four items. Two runs compared once is a coin flip dressed as a test.

## a criterion openly deferred beats one that looks covered

RL-507's criterion names "publish plans" and there is no plan builder — §11 is M7. The
test asserts the *input* to the plan and says so in its own doc comment rather than
quietly counting the criterion as met. The source guard already covers
`revlocal-publish`, so the builder cannot introduce the problem when it lands.
