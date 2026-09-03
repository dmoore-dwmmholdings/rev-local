# Build state

- current_milestone: M15 — the unattended loop (epic REVL-127)
- current_item: none in flight
- item_status: n/a
- last_gate_command: cargo clippy --workspace --all-targets -- -D warnings; cargo test --workspace; (ui) npx tsc --noEmit && npx vitest run
- last_gate_result: PASS — clippy clean, 1454 Rust tests passing, 139 UI tests passing
- last_visual: end-to-end run of the built CLI against a scratch install, 2026-09-03
  — two commits reviewed, two reports delivered, two audit rows, the commit named
  in each report
- next_action: nothing in REVL-127 is in flight. REVL-184 (GitHub pull-request
  adapter) sits in the backlog and is **not** blocked on a decision — it is
  blocked on an unexplained `git_hooks` timing failure, bisected and written up on
  that issue. Read those comments before starting it.
- blocked_on: nothing
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

**Thirteen-plus defects of one shape: capability that was built, unit-tested, and
never called by anything.**

Small ones: `RepoConfig.targets`, `pair_has_succeeded`, `delete_finished_before`
(retention), `KillSwitch`, the engine pid, the approval TTL, `max_concurrent_runs`
/ `coalesce_window_ms`, `review_commits` / `review_prs`.

Large ones, all found later in the same milestone and all bigger than the small
ones put together:

- **force-push recovery** — `recover.rs` implements the whole of §6.2 with its own
  test file, and `GitAdapter::discover` never classified the cursor, so a rebase
  re-reviewed every replayed commit
- **the Subversion adapter** — five modules under `svn/`, no `impl VcsAdapter`, so
  a `--kind svn` repository was handed to the git adapter and told its working
  copy "is not a git repository"
- **the GitHub adapter** — same shape, still open as REVL-184
- **the audit log** — one writer, for approval expiry, while SPEC's third
  principle requires *every* outbound write to be recorded with a receipt

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

**Those two greps would not have found the adapters.** They look for functions
nothing calls; a missing `impl VcsAdapter` is a *trait implementation* that does
not exist, and every function under it is called by its own tests. The check that
finds those is to enumerate the trait's implementors and compare against the enum
the system dispatches on:

```sh
grep -rn "impl VcsAdapter for" crates/*/src   # one line per kind, or a gap
grep -n "=> \"" crates/revlocal-core/src/enums.rs   # the kinds that exist
```

More generally: for any `enum` the system dispatches on, ask what handles each
variant. A variant with no handler is the same defect wearing a different hat.

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

## REVL-131, and why it was not a decision after all

`ActionIntent::CreateIssue` is `RiskClass::High` inherently, so `auto_low_ask_high`
asks before filing **any** tracker issue, forever. The first-use rule cannot season
it down because the baseline is unconditional.

This was carried as "a product decision" for most of the milestone. It was not.
The specification answers it in two places that had been read separately and never
put together — §12.3 lists creating an Andare issue as high risk, and §12.2's mode
table says `auto_low_ask_high` queues high-risk actions to the inbox. So `auto` is
correctly the only mode that files a tracker issue unattended, and the code was
right.

The real defect inside that issue was separate and is fixed: `queue_actions` passed
`pair_previously_succeeded: false` unconditionally, so the code lied about history.

**Twice in this milestone something escalated as needing a human turned out to be
answered in writing.** Before escalating, grep the specification for the nouns in
the question.

## Not code, and still true of the live install

1. `/Applications/rev-local.app` is from 2026-08-30 and predates all of M15.
   `cargo tauri build --features desktop`, then replace the bundle. Nothing in
   this milestone reaches the machine until that happens.
2. **Autopilot has never been switched on.** The `setting` table is empty, which
   is the literal answer to "it doesn't want to process automatically".
3. Two of the three watched repositories no longer exist on disk — 43 of the 51
   queued runs are theirs and can never run. The loop leaves them alone, says so,
   and now counts them apart from work that can actually proceed.
4. Three publish actions sit in the approvals inbox, long past
   `approval_ttl_hours`. The next tick that runs will expire them.

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
