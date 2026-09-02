# Build state

- current_milestone: M15 — the unattended loop (epic REVL-127)
- current_item: none in flight
- item_status: n/a
- last_gate_command: cargo clippy --workspace --all-targets -- -D warnings; cargo test --workspace; (ui) npx tsc --noEmit && npx vitest run
- last_gate_result: PASS — clippy clean, 1415 Rust tests passing, 129 UI tests passing
- last_visual: smoke run of the built desktop binary, 2026-09-02 (see below)
- next_action: REVL-131 is the only open item in REVL-127 and is **blocked on a
  human decision**, not on code. Everything else in the epic is closed. Pick up
  Trama as an output (`trama_space` / `trama_publish` have no readers) or
  `andare_transition_on` if more is wanted here.
- blocked_on: REVL-131 — see below
- adrs_open: none

## What M15 was

The app was built and the loop was not joined up. On the machine this was written
for: 59 changes discovered, 51 runs sitting in `queued`, zero cursor rows, one run
stuck in `publishing` since August, three publish actions never sent. Every part
worked and nothing ran.

It now runs unattended end to end. Verified twice in the *built binary*, not only
in tests: at default settings two commits went from unseen to reviewed to findings
on disk with nobody pressing anything, and with `review_commits = false` both were
skipped naming the setting.

## The pattern this milestone kept finding

**Eight defects of one shape: capability that was built, unit-tested, and never
called by anything.**

`RepoConfig.targets`, `pair_has_succeeded`, `delete_finished_before` (retention),
`KillSwitch`, the engine pid, the approval TTL, `max_concurrent_runs` /
`coalesce_window_ms`, and `review_commits` / `review_prs`.

Every one was invisible reading the code and obvious asking what the system does.
Two greps find them, and both are cheap enough to run on any change:

```sh
# a public function nothing outside its crate calls
for f in $(grep -rho "pub fn [a-z_]*" crates/<crate>/src/*.rs | sed 's/pub fn //' | sort -u); do
  n=$(grep -rho "\.$f(" crates/*/src | wc -l); [ "$n" -le 1 ] && echo "$f"
done

# a config field with no reader beyond the settings screen
for f in $(grep -o "pub [a-z_]*:" crates/revlocal-core/src/config/global.rs | sed 's/pub //;s/://'); do
  printf "%-28s %s\n" "$f" "$(grep -rho "\.$f\b" crates/*/src | wc -l)"
done
```

The seventh was introduced *by this milestone* — `coalesce_window_ms` hardcoded in
`autopilot.rs` — so this is not archaeology on old code. It belongs on new work.

## Two decisions worth revisiting

**`review_commits` now defaults to `true`** (RL-1525). §13.2 said `false`, which
suits a deployment where the pull request is the unit of review. rev-local watches
local repositories where a pull request may never exist, so honouring the field as
written would have stopped a repository at its own defaults reviewing anything.
SPEC §13.2 is updated to match. One line in `repo.rs` and one in `SPEC.md` to put
back if that is wrong.

**Risk depends on where an action goes** (RL-1519). `RiskInputs` carries a
`Destination`, and `Local` is Low with no reasons — every escalation in §12.3 is
there because an action is visible to others or awkward to undo, and a file on
your own disk is neither. This is what makes the local report work with nothing
configured. It is also half of REVL-131's answer.

## REVL-131, the open question

`ActionIntent::CreateIssue` is `RiskClass::High` inherently, so `auto_low_ask_high`
asks before filing **any** tracker issue, forever. The first-use rule cannot season
it down because the baseline is unconditional.

The code no longer lies about history — `pair_has_succeeded` and
`actions_sent_since` are read for real — but the question is a product one: should
a proven target/capability pair file into a tracker unattended under
`auto_low_ask_high`, or is `auto` correctly the only mode that does? Local reports
no longer depend on the answer.

## Not code, and still true of the live install

1. `/Applications/rev-local.app` is from 2026-08-30 and predates all of M15.
   `cargo tauri build --features desktop`, then replace the bundle.
2. Two of the three watched repositories no longer exist on disk. The loop leaves
   them alone and says so; disabling or repointing them is the operator's call.

## Counting tests

`cargo test --workspace 2>&1 | grep -c '^test .* ok'` **undercounts** — parallel
test binaries interleave their output and some lines are split. Sum the
`test result` lines instead:

```sh
cargo test --workspace 2>&1 | grep -E "^test result" | awk '{s+=$4} END {print s}'
```

## A gate that selected nothing

`RL-406`'s gate was `cargo test -p revlocal-engine env_denylist`. The work had
landed inside `RL-405` with tests named `supervision_*`, so the gate **selected
zero tests and exited 0**.

**When an item's gate is a name filter, check how many tests it selected, not just
its exit code.** A filter matching nothing is the quietest way for a gate to pass
while testing nothing.
