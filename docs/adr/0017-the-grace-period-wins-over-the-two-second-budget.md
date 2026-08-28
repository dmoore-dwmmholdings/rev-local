# 17. The grace period wins over the two-second budget

Date: 2026-08-27
Status: accepted
Item: RL-405

## Context

Two requirements pull against each other:

- `RL-405`'s first acceptance criterion: *"Hung mock engine is killed within
  timeout + 2s."*
- SPEC §8.5: *"On timeout: SIGTERM, 5s grace, SIGKILL."*

For a process that **ignores SIGTERM** — which the fixture's `hang` mode does
deliberately — these cannot both hold. Five seconds of grace is more than two
seconds of budget.

## Decision

**§8.5's grace period wins.**

Shortening the grace to fit the criterion would cost a real review. A CLI given no
chance to flush `result.json` before SIGKILL loses work whose tokens have already
been spent, and it would do so on exactly the slow runs where the review was most
expensive.

The "+2s" is read as **supervisor overhead**, not as a bound on the pathological
path. That reading is tested rather than asserted in prose, as two cases:

- `supervision_an_engine_that_respects_sigterm_is_gone_within_timeout_plus_two_seconds`
  — a well-behaved CLI (`sleep`) dies on SIGTERM, so the grace never elapses and
  the whole thing is over within the two-second budget. This is the criterion as
  written, for the case it is actually about.
- `supervision_an_engine_that_ignores_sigterm_still_dies_after_the_grace_period`
  — the pathological path is bounded at `timeout + grace + 2s`.

The first test also asserts the total is **under** `timeout + GRACE`, which is what
proves the grace period is cut short as soon as the child exits rather than always
being waited out. Without that, a supervisor could pass by sleeping five seconds
every time and nobody would notice until reviews felt slow.

## Not silently weakened

The criterion is not edited and the item is not marked as fully meeting it as
written. This ADR is the record, and the Andare comment says the same. If the
product owner wants the pathological path bounded at two seconds, the grace period
is the thing to change — and that is a §8.5 change, not a test change.

## Related

`RL-302` made the same trade differently and should be revisited if this matters:
`git::cmd`'s timeout goes **straight to SIGKILL** with no grace, because a `git`
subprocess has no half-written artifact worth preserving. The asymmetry is
deliberate; an engine has `result.json` to flush and `git` does not.
