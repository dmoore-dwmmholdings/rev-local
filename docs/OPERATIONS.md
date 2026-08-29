# Operations

What to do when rev-local is not doing what you expect. Every command and every
output in this document was run against the built binary.

Start with `revlocal doctor`. It is the only command that checks prerequisites,
engines and publish targets in one pass, and it exits non-zero when something is
blocking a review, so it works in a script.

```
$ revlocal doctor
Prerequisites
  [ok  ] git: available
  [ok  ] svn: available (no SVN repositories configured)
  [ok  ] node: available (used by the fixture engine)

Platform
  [ok  ] platform:macos: process-group termination available

4 ok, 0 warning(s), 0 failure(s), 0 not needed
Nothing is blocking a review.
```

A clean report ends with that last line rather than with silence. A report that
just stops looks like one that stopped early.

---

## A publish target is down

A run can finish with one target posted and another failed. That is a normal
outcome, not an error state, and a failed target does not hold the run open.

```
$ revlocal publish status --run 1 --database <PATH>
andare: failed — 0 of 1 delivered (andare refused the request with 422: the project does not exist)
github: delivered — 1 of 1 delivered
revlocal: retry one with `revlocal publish replay --run 1 --target <TARGET>`
```

Two commands put work back in the queue, and the difference is the unit:

| command | re-queues |
|---|---|
| `revlocal publish retry <action_id>` | exactly one action |
| `revlocal publish replay --run R --target T` | every failed action for that target |

Prefer `retry` when some of a target's actions already landed. If a run produced
eight comments and one was rejected for a bad path, replaying the target re-posts
the seven that succeeded.

Retrying is safe to do twice. Every action carries an idempotency key, so
delivery is at-least-once with exactly-once effect — a retry does not become a
duplicate issue in somebody's tracker.

### The target is misconfigured rather than down

```
revlocal targets list --config <PATH>
```

Contacts every MCP server in your config, discovers the tools each one exposes,
and reports which capabilities bound to which tool — and which did not bind at
all. Nothing is guessed: a capability that matches no candidate is reported, never
silently bound to a tool that merely looks similar.

`revlocal targets test <target>` renders every mapped capability against a sample
finding and exits non-zero if any would not render. Nothing is called.

---

## A budget is exhausted

```
$ revlocal budget show --repo 1 --database <PATH>
repo 1 on 2026-08-29
  runs    200 of 200
  tokens  2100000 of 2000000
  cost    $12.50

Holding: daily run budget reached: 200 of 200 runs today
```

The last line is the decision, and it names which ceiling was hit. What happens
next is `budgets.on_exhausted` in your config: `pause`, `queue` or `skip`. There
is deliberately no "drop" — exhaustion never silently discards a change.

Budgets roll over on your local midnight, not UTC, because a daily allowance is a
human-facing thing. To resume before then:

```
revlocal budget reset --repo <NAME> --database <PATH>
```

That clears the day's *allowance accounting only*. Runs, findings and the audit
log are untouched, so the spend is still explainable afterwards.

### A total that is a lower bound

Tokens and cost are reported as complete only when **every** run that day reported
them. One run that reported no count makes the day's total a lower bound, and the
output says so rather than quietly presenting a partial sum as a total. An
unmeasured run is not a free one.

---

## A run is stuck

```
revlocal runs list --database <PATH>
revlocal runs show <run_id> --database <PATH>
```

A run in a non-terminal stage that has not been touched for
`global.stale_run_minutes` (default 10) is considered abandoned. The daemon
recovers those itself on the next pass: it marks the run interrupted, then
enqueues a successor — up to `global.max_attempts` (default 3), after which it
stops and records why. A change that keeps interrupting the daemon will keep
interrupting it, so retrying forever only buries the evidence.

To retry one yourself:

```
revlocal runs retry <run_id> --database <PATH>
```

This queues another attempt at the same change, under the same engine and depth.
Three things to know:

- **The old run is left exactly as it was.** A run is the record of one attempt,
  and rewriting it would lose the evidence of what went wrong — which is what you
  most want next.
- **The successor starts clean.** It carries none of the previous attempt's
  tokens, cost or salvaged output. Carrying them forward would charge the budget
  twice for work that was thrown away.
- **It is not bounded by `max_attempts`.** That ceiling governs automatic
  recovery, whose job is to stop a crash-looping change when nobody is watching.
  You are watching. The attempt number is in the output so an unusual one is
  visible.

A run that is still in flight is refused rather than duplicated:

```
$ revlocal runs retry 1 --database <PATH>
revlocal: run 1 is still reviewing — wait for it to finish, or stop it with `revlocal kill --hard`
```

### Stopping everything

```
revlocal pause     # reversible, loses nothing
revlocal resume
revlocal kill --hard
```

`pause` stops new work and **holds** publish actions rather than dropping them;
`resume` releases both. It survives a restart, deliberately — if you paused
because something was wrong, restarting the daemon must not quietly start
reviewing again.

`kill --hard` additionally reaps engine processes by their recorded PID. Reach for
it only when a pause is not enough: it takes a running engine's output with it.

---

## A branch was force-pushed

A cursor is the last reviewed SHA on a branch, and a rebase-and-force-push makes
it no longer an ancestor. rev-local notices rather than failing:

- **History rewritten** — the cursor is no longer an ancestor. An audit event
  `history_rewritten` is recorded, the cursor is reset to the merge-base, and
  discovery resumes forward from there. Some commits may be re-reviewed; findings
  are deduplicated by fingerprint, so this does not produce duplicate comments.
- **Cursor object missing** — the SHA is not in the repository at all, usually
  after garbage collection. This is kept distinct from the case above because
  there is no merge-base to compute and therefore no safe resume point. It is
  recorded, and discovery starts from the branch head.

Both are in the audit log. Neither passes silently — a cursor that stopped meaning
what it meant is exactly the failure that otherwise looks like a quiet week.

---

## A GitHub check run is stuck in progress

rev-local owns one check run, `rev-local/review`, and sets it `in_progress` while
a review is active. A check left in progress shows as a spinning yellow dot on the
commit forever, and in a repository with required checks it blocks the merge.

**This resolves itself.** Whether a check is still owed is *derived* from the run
and the actions recorded for it, not remembered by the process that started it —
so a run that died between starting the check and finishing the review is resolved
at the next startup, from the database. rev-local crashing cannot block your merge
indefinitely.

If a run failed, the check reports **neutral**, never failure. `failure` is a
statement about your code; a run that crashed or timed out has made no statement
about your code at all, and saying otherwise costs somebody an afternoon.

---

## Exit codes

Every command follows these, so a script can branch on them:

| code | meaning | what to do |
|---|---|---|
| `0` | succeeded | — |
| `1` | failed | retrying may work |
| `2` | the command was wrong | fix the invocation |
| `3` | a daily budget stopped it | retrying today will not help |
| `4` | it needs a human to approve it | retrying will not help |

Codes `3` and `4` exist because the usual response to failure — retry — is wrong
for both. A job waiting for approval must not look like one that failed.

**Not every code is reachable yet.** Commands that would return `3` or `4` are
not built, and no command returns them today. They are documented because they are
the contract, and a script written against them will not need changing.

---

## Reclaiming space

```
revlocal db vacuum --before <YYYY-MM-DD> --database <PATH>
```

Deletes runs that **finished** before that date, with their findings and publish
actions. Run and finding rows are never deleted automatically; this is the manual
escape hatch.

Transcript files go with their rows, because the row is the only thing that knows
where the file is — deleting one without the other would leak disk space
permanently and invisibly. Files that could not be removed are listed rather than
swallowed.

Runs that have not finished are never deleted, whatever the date: one may be in
flight, and deleting it would leave the daemon writing to a row that is gone.
