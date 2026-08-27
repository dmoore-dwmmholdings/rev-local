# rev-local — autonomous build loop

You are building **rev-local**, a cross-platform desktop application that autonomously
reviews every change landing in a git, GitHub, or Subversion repository using locally
installed AI coding CLIs, and publishes the results to GitHub, Andare and Trama over MCP.

You are running inside a loop. Each invocation is **one iteration**. Do one increment of
real work, verify it, record it, and stop. The loop will call you again.

---

## Read these, in this order, once per iteration

1. `docs/STATE.md` — where the build is. **If it does not exist, create it** from the
   template in `docs/BUILD_LOOP.md` §1 and set `current_item: RL-101`.
2. The **current work item** (see *Choosing work* below) — its description, acceptance
   criteria and gate.
3. Only the `SPEC.md` sections that item references. Do not re-read the whole spec every
   iteration; it is long and you will burn the context you need for the work.

`docs/BUILD_LOOP.md` is the full operating procedure. This file is the short form you
execute; that file is the reference you consult when something is ambiguous.

---

## Choosing work

**If the Andare MCP server is connected:** the backlog lives in Andare. Pick the
highest-priority open item in the rev-local project whose dependencies are all closed
and whose milestone is the current one. Work items in `RL-` id order within a priority.

**If Andare is not connected:** fall back to `docs/backlog/backlog.json`. Same rule —
lowest unblocked item by `(milestone, priority, id)` whose `status` is `todo`. Track
status in that file and in `docs/STATE.md`.

Never work more than one item per iteration. If an item turns out to be larger than one
iteration, split it: create sub-items (in Andare if available, otherwise in
`scripts/gen_backlog.py`, regenerated), and take the first.

---

## The iteration

```
1. ORIENT   Read docs/STATE.md. Identify the current item and its next action.
2. PLAN     Decide the smallest change that advances it. If it would touch more than
            ~5 files or ~400 lines, do the first part only and say so.
3. TEST     Write the test first whenever the acceptance criteria state a behaviour.
            The criteria are written as assertions on purpose.
4. BUILD    Implement.
5. VERIFY   Run in order, stopping at the first failure:
              cargo fmt --all
              cargo clippy --workspace --all-targets -- -D warnings
              cargo test --workspace
              <the item's gate command>
            For any item touching the UI, also run the Framewatch gate (below).
6. RECORD   Update docs/STATE.md with what you actually observed. Update the item's
            status in Andare (or backlog.json). Commit.
7. STOP     Report (format below) and end the iteration.
```

**Commit per iteration.** Message: `RL-xxx: <what changed>`, body listing the gate
command and its real result. Never commit with a failing `cargo build`.

---

## Gates are observations, not intentions

A gate has passed only if you ran the command **this iteration** and saw it exit 0.
Never record a result you did not observe. Never write "should pass". If you did not run
it, the item is not done.

If a gate fails, the next iteration's only job is making it pass. Increment
`iterations_this_item`. **At 8 iterations on one item, stop grinding**: write the failure
analysis into `blocked_on`, split the item, or escalate.

---

## GUI verification with Framewatch

`framewatch` is installed. Any item that changes what the app renders must be verified
visually — screenshots are the gate, not your belief that the JSX is correct.

```bash
scripts/gui-verify.sh <screen>     # single settled PNG -> artifacts/gui/<screen>.png
scripts/gui-flow.sh <flow>         # captioned frame per step of a scripted flow
```

Then **look at the image you captured** and check it against that screen's checklist in
`SPEC.md` §15. State in your report what you saw. A capture you did not read is not
verification.

Rules:
- A screen that fails to appear must fail the gate. Do not use `--settle-best-effort`
  in a gate — it turns "nothing rendered" into a passing blank frame. Debug only.
- Use `--roi` to clip window chrome so captures are comparable across runs.
- Do not gate on pixel-diffing against golden images. Fonts and DPI differ across
  platforms; the load-bearing check is an agent reading the screenshot.
- Capture with fixture data, never with a real repository.

---

## Reporting status outward

Only when the corresponding MCP server is connected. If it is not, note it in
`docs/STATE.md` and carry on — a missing tracker never blocks building.

- **Andare** — move the item to in-progress when you start it and to done when its gate
  passes. Comment with the gate command and its observed result. If you split an item,
  create the children under the same parent.
- **Trama** — at each **milestone** close (not each item), write or update a page
  `rev-local build log / <milestone>` in the configured space summarising: items closed,
  decisions made, ADRs written, and anything left open. Remember Trama's `update_page`
  **replaces the body** — always `get_page` first and send the whole merged document back.

Do not report a milestone closed until every item in it has an observed passing gate.

---

## Decisions

**Yours** — module layout, error granularity, library choices, React structure, prompt
wording (preserving §9.2 section order and the §8.2 output contract), constants not fixed
by the spec, whether an item needs splitting. Make the call, write a short ADR in
`docs/adr/`, move on.

**Not yours** — anything in `SPEC.md` §2 (decisions of record), the `result.json` schema
(§8.3), the engine fallback ladder (§8.2), the risk table (§12.3) including *first use of
a capability is always high risk*, Trama read-before-write (§11.5), "no silent caps"
(§18), and "hooks must exit 0 even when rev-local is down" (§7.2).

If one of those looks genuinely unworkable, write an ADR with `Status: proposed`,
**implement the spec as written anyway**, and surface it in your report. Do not
unilaterally change a decision of record.

---

## Hard constraints

- **Never mutate a repository under review**, fixtures included. Always work in a scratch
  worktree or export. `git status --porcelain` on a fixture must be empty afterwards.
- **No network and no model spend in the inner loop.** Use `fixtures/mock-engine` and
  `fixtures/mock-mcp`. Real engines run only in the `engine-live` suite.
- **No credentials in committed files, logs, or transcripts.** Ever.
- **No `unwrap()`/`expect()` outside tests.**
- Do not skip a test to make a gate pass. Do not weaken an acceptance criterion to meet
  it. If a criterion is wrong, say so explicitly in your report and propose the change —
  do not quietly edit it.

---

## Stop and ask a human when

- A decision of record appears actually unworkable, not merely inconvenient.
- Andare's real tool surface cannot express `create_issue` or `set_status` even with a
  manual mapping.
- A prerequisite cannot be installed on a target platform and an item's gate depends on it.
- Something would require storing a model credential.
- Two items' gates contradict each other.

When you stop: put a **specific question with your recommended answer** in
`blocked_on`, and keep working any item that does not depend on it.

---

## Report at the end of every iteration

Six lines. No preamble.

```
ITEM      RL-xxx — <title>
DID       <one sentence: what changed>
GATE      <command> → PASS | FAIL (<the actual failure, quoted>)
VISUAL    <screen: what you saw in the capture> | n/a
NEXT      <the single next action>
BLOCKED   <question + your recommendation> | none
```

---

## Current known blockers

Check these each iteration; they may have been resolved since the last one.

1. ~~**Andare MCP is not connected.**~~ **Resolved 2026-08-27.** Andare is connected,
   the backlog is imported as project `REVL` (`RL-101` = `REVL-14`), and Andare is the
   status source of truth — move items there, not in `backlog.json`. `docs/backlog/IMPORT.md`
   has already been run; do not re-run it. `RL-606` (`REVL-58`) is unblocked.

2. **`codex` is not installed.** `claude` is. `RL-408` (Codex CLI spike) and the Codex
   half of `RL-1203` are blocked. Everything else proceeds — engine selection is
   per-repo, and the mock engine covers the inner loop.
3. **Framewatch in headless CI is unverified** (`RL-1104`). GUI gates are local/loop-only
   until that spike lands.
4. **`svn` is not installed.** Blocks `RL-202` and the `RL-E9` stories once they come up.
   Not blocking before then.

`RL-101` is closed. See `docs/STATE.md` for where the loop actually is.
