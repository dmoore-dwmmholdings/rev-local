# 30. Cancellation gets a shorter grace period than a timeout

Date: 2026-08-28

## Status

Accepted

## Context

ADR 0017 kept SPEC §8.5's five-second SIGTERM grace when it conflicted with
RL-405's "killed within timeout + 2s", reasoning that a CLI cut off before it can
flush `result.json` loses a review whose tokens were already spent.

RL-804's criterion is that the kill switch cancels a running engine **within three
seconds** (§12.1). Against the fixture's `hang` mode — which ignores SIGTERM on
purpose — the five-second grace makes that impossible: the observed time was
5.02s.

This is the same shape of conflict ADR 0017 resolved, and it resolves the other
way.

## Decision

`GRACE` stays at five seconds for a **timeout**. A **cancellation** gets
`CANCEL_GRACE`, two seconds. `supervise` picks between them by `KillReason`.

## Consequences

- A timeout still means "you have had long enough, finish up", and a well-behaved
  engine still gets its five seconds to write the file.
- A kill switch means *stop now*. The person pulling it has already accepted
  losing the run — that is what a kill switch is — so spending five seconds of
  their emergency budget on a courtesy they explicitly declined is the wrong
  trade.
- Two seconds is not zero. An engine that respects SIGTERM still flushes; the
  shorter budget only bites for one that is ignoring the signal, which is exactly
  the case §12.1's three seconds is about.
- §12.1 and §8.5 are both satisfied without amending either. Had they still
  conflicted, the spec would have needed the change rather than the test — which
  is the rule ADR 0017 set and this follows.
- The two constants are separate and named, so a future change to one does not
  silently move the other. `CANCEL_GRACE + overhead < 3s` is the invariant
  RL-804's test measures.
