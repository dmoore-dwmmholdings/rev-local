# 29. Retry jitter is derived from the action, not from a random number generator

Date: 2026-08-28

## Status

Accepted

## Context

SPEC §11.6 requires exponential backoff **with jitter**, and RL-702's fourth
criterion states it as "two concurrent retries do not align".

The obvious implementation multiplies the delay by a random factor. That needs a
RNG dependency, and it makes the criterion a statistical property: a test can
sample it, and a sampled test either flakes or asserts something so weak it would
pass for a broken implementation.

## Decision

Jitter is derived from `(action id, attempt)` through splitmix64's finaliser, a
fixed four-line mixer, and scaled to ±25% of the computed delay.

A target's own `Retry-After`, where it gives one, replaces the curve entirely and
is capped at 60 seconds.

## Consequences

- **The property jitter exists for is preserved.** The failure being avoided is a
  thundering herd: a target goes down, fifty actions fail in the same second, and
  all fifty retry together, then again at two seconds, and four. Breaking that up
  requires only that *different* actions choose different delays. Nothing about it
  requires unpredictability, and there is no adversary here to be unpredictable
  against.
- **The criterion becomes assertable.** `idempotency_two_actions_retrying_together_do_not_align`
  computes all fifty delays and requires more than forty distinct values, and that
  every one lands inside the ±25% band. Neither half is possible against a RNG
  without either fixing a seed — which is this decision by another name — or
  accepting a flaky test.
- **The schedule is reproducible**, which ADR 0024 already makes a property of
  this system. Two runs against the same failure produce the same backoff.
- `DefaultHasher` was rejected for the mixer: it is explicitly not stable across
  Rust releases, so a compiler upgrade would silently change every retry schedule.
- The cost is that an observer who knows an action's id can predict when it will
  retry. There is no threat model in which that matters: the id is local, and the
  actions go to systems the user already authenticated to.
