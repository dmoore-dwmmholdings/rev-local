# rev-local — Autonomous build loop protocol

You are an autonomous coding agent implementing [`SPEC.md`](../SPEC.md). This
document is your operating procedure. Follow it literally.

---

## 0. Invariants

1. **The spec is the contract.** `SPEC.md` §2 (Decisions of record) is fixed. If
   implementation reveals a decision is wrong, you may not silently change it —
   write an ADR proposing the change, mark it `Status: proposed`, implement the
   spec as written, and surface the ADR in your status report.
2. **Milestones are sequential.** Never start M(n+1) before M(n)'s exit gate
   passes. §17 of the spec lists the gates.
3. **Green before forward.** `cargo build --workspace`, `cargo clippy --workspace
   -- -D warnings`, and `cargo test --workspace` must all pass at the end of every
   iteration. If they don't, the next iteration's only job is making them pass.
4. **No network, no model spend in the inner loop.** Inner-loop tests use
   `fixtures/mock-engine` and `fixtures/mock-mcp`. Real engines run only in the
   M14 `engine-live` suite and at checkpoints (§4).
5. **Never mutate the user's repositories.** Fixture repos included: M4's gate
   asserts the fixture working tree is byte-identical after a review.

---

## 1. State file

Maintain `docs/STATE.md` at the repo root of your work. It is the loop's memory —
written after every iteration, read at the start of every iteration.

```markdown
# Build state
- current_milestone: M4
- current_item: RL-305           # the one work item this iteration advances
- item_status: in_progress       # not_started | in_progress | gate_failing | done
- last_gate_command: cargo test -p revlocal-vcs skip_rules
- last_gate_result: FAIL (2/9) — merge-commit skip_reason not set
- last_visual: n/a               # or "dashboard: budget bar missing"
- next_action: implement skip rules in vcs::git::discover, then re-run gate
- blocked_on: none               # or a question + your recommended answer
- adrs_open: [0003-gh-transport-priority]
- iterations_this_item: 3
- items_closed: [RL-101, RL-102, RL-103, RL-104, RL-105, RL-106, RL-107, RL-108,
                 RL-109, RL-110, RL-111, RL-201, RL-202, RL-203, RL-204, RL-205,
                 RL-301, RL-302, RL-303, RL-304]
- andare_connected: false        # when true, Andare is the status source of truth
```

If `iterations_this_item` exceeds **8**, stop iterating on it: write the failure
analysis into `blocked_on`, and either split the item into sub-items or escalate.
Do not grind.

---

## 2. The iteration

Each loop iteration executes exactly this sequence.

```
1. READ    docs/STATE.md. Determine current milestone M and next_action.
2. ORIENT  Read only the SPEC sections M depends on (the milestone row names them).
           Do not re-read the whole spec each iteration.
3. PLAN    Write the smallest change that advances next_action. If it touches more
           than ~5 files or ~400 lines, split it and do the first part only.
4. BUILD   Implement. Write the test FIRST when the gate names a behaviour
           (§17 gates are written as assertions on purpose).
5. VERIFY  Run, in order, stopping at the first failure:
             cargo fmt --all
             cargo clippy --workspace --all-targets -- -D warnings
             cargo test --workspace
             <the item's gate command>
           For anything that changes what the app renders, also run the Framewatch
           gate (scripts/gui-verify.sh <screen>) and READ the captured PNG against
           that screen's SPEC §15 checklist. A capture you did not look at is not
           verification.
6. RECORD  Update docs/STATE.md with the real result. Never write a result you
           did not observe.
7. DECIDE  Gate passed  -> mark milestone done, advance to M+1, set next_action
                           from the new milestone's first deliverable.
           Gate failed  -> set next_action to the single most specific fix implied
                           by the failure output. Increment iterations counter.
```

**Commit discipline:** one commit per iteration, message
`RL-xxx: <what changed>` with a body listing the gate command and its observed
result. Never commit with a failing `cargo build`.

---

## 3. What "done" means for a milestone

A milestone is done when **all** of these hold:

- its gate command exits 0, observed in this iteration (not remembered);
- `cargo clippy --workspace --all-targets -- -D warnings` is clean;
- every new public item has a doc comment;
- the milestone's behaviours are covered by tests that would **fail** if the
  behaviour were removed (delete-the-code check: if you can't name what breaks,
  the test is decorative — rewrite it);
- `SPEC.md` still describes what you built, or you updated it plus an ADR.

---

## 4. Checkpoints

Every time a milestone completes, run the checkpoint:

```bash
cargo test --workspace
./fixtures/build.sh          # fixtures still reproducible from scratch
cargo test --features engine-live -- --ignored   # only from M5 onward, and only
                                                  # if `claude` or `codex` is on PATH
```

Engine-live failures before M14 are **informational**, not blocking — record them
in `docs/STATE.md` under a `live_engine_notes` heading. They become blocking at M14.

---

## 5. Escalation — when to stop and ask

Stop the loop and ask a human only for these. Everything else, decide yourself and
write an ADR.

- A decision of record (SPEC §2) appears to be actually unworkable, not merely
  inconvenient.
- Andare's real MCP tool surface, once discoverable, cannot express `create_issue`
  or `set_status` even with manual mapping (§11.2).
- A prerequisite cannot be installed on a target platform (e.g. no `svn` on the
  Windows CI runner) and the milestone gate depends on it.
- Anything that would require storing a model credential, contradicting §1.1.
- Two milestones' gates are mutually contradictory.

When you escalate: set `blocked_on` in `docs/STATE.md` to a **specific question
with your recommended answer**, and keep working any milestone that does not depend
on it.

---

## 6. Judgement calls the spec deliberately leaves to you

These are yours. Make them, note them in an ADR, move on.

- Crate-internal module layout, error enum granularity, trait object vs generics.
- Choice of HTTP/MCP client libraries, test harness helpers, snapshot testing tool.
- React component structure, state management, styling approach (keep it plain).
- Exact wording of prompts in `crates/revlocal-engine/prompts/`, provided they
  preserve the §9.2 section order and the §8.2 output contract verbatim.
- Retry/backoff constants not fixed in §11.6.
- Whether a milestone needs splitting.

## 7. Judgement calls that are NOT yours

- Anything in SPEC §2.
- The `result.json` schema (§8.3) and the fallback ladder (§8.2).
- The risk classification table (§12.3) — especially "first use of a capability is
  always high risk".
- Read-before-write on Trama `update_page` (§11.5).
- "No silent caps" (§18) — every truncation is recorded and surfaced.
- Hooks must exit 0 even when rev-local is down (§7.2).
