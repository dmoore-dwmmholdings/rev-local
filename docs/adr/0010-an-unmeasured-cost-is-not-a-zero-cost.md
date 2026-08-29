# 10. An unmeasured cost is not a zero cost

Date: 2026-08-27
Status: accepted
Item: RL-109c

## Context

SPEC §5 gives `budget_ledger` a column `cost_usd REAL NOT NULL DEFAULT 0`, and
decision D10 puts per-repo cost budgets on top of it. But §8.1 types
`EngineOutcome`'s cost as optional — an engine need not report a price, and the
mock engine never will.

`NOT NULL DEFAULT 0` and "cost may be unknown" cannot both hold. Folding an
unreported cost into the column as `0.0` makes a day nobody measured
indistinguishable from a day that genuinely cost nothing, and a cost budget reading
that day sees headroom it has never been shown to have. That is exactly the silent
cap SPEC §18 forbids and the "never silently drop" half of D10.

`revlocal-core` already had the right shape — `Usage::add` keeps an unknown cost
unknown rather than folding it to zero (ADR 0005) — but the schema threw the
distinction away on the way to disk.

## Decision

Migration `0002` adds `cost_complete INTEGER NOT NULL DEFAULT 1` to
`budget_ledger`, and SPEC §5 is updated in the same commit.

- `cost_usd` accumulates **only the costs that were actually reported**.
- `cost_complete` is `MIN(existing, incoming)` across increments, so one unpriced
  run clears it for the whole day and it never comes back.
- Reading a day, `usage.cost_usd` is `Some` **only when the day is complete**. An
  incomplete day reports `None`, which is what makes `Usage::cost_is_complete()`
  honest.
- The partial sum is not discarded: `BudgetLedgerEntry::known_cost_usd` carries it,
  as a lower bound on real spend and something the UI can show.

### `cost_exhausted` returns `Option<bool>`, not `bool`

This is the part that matters. A caller asking "is this repo over its cost budget?"
on an unmeasured day must not get `false`. The three answers are genuinely
distinct:

- `Some(true)` — the **known** costs already passed the limit; the unknown
  remainder can only make that more true.
- `Some(false)` — the day is fully measured and under the limit.
- `None` — cannot tell.

Returning `bool` would force `None` to collapse into one of the others at the type
level, and the tempting collapse is to `false`. D10 says an exhausted budget
pauses, queues or skips; "unknown" belongs on that side of the line, not on the
side that proceeds. `Option<bool>` makes a caller write down which it chose.

~~`tokens_exhausted` stays a plain `bool` — token counts are always known, so there
is no third case to represent.~~

**Corrected 2026-08-29 (RL-409).** That sentence was wrong, and wrong in the exact
way this ADR was written to prevent. Token counts are *not* always known: SPEC
§8.3's `result.json` schema carries no usage field, so a runner reading it has no
counts to report and returned `Usage::default()` — zero tokens. A run that spent
forty thousand was recorded as spending none, and a repo with a two-million-token
daily limit never reached it.

It read as true because the mock engine reports counts, so every test passed with
the gap present. The fixture was more honest than the thing it stood in for, which
is the failure mode a fixture is least able to warn you about.

`tokens_exhausted` now returns `Option<bool>`, for exactly the reason the paragraph
above gives for cost, and `Usage` carries `tokens_known` so an unmeasured run is
recorded as unmeasured rather than as free. `Usage::default()` therefore means
"nobody counted", not "nothing was spent" — the safe direction, and the one that
makes the original bug impossible to reintroduce by omission.

## Consequences

- `RL-110` (audit log and budget ledger) and the daemon's `BudgetGuard` must treat
  `cost_exhausted(..) == None` as a stop, not a go. That is a review point for
  whoever writes the guard.
- The mock engine reports no cost, so **every inner-loop run produces an incomplete
  day**. Tests asserting cost behaviour need an explicitly priced `Usage`, and the
  end-to-end pipeline test (`RL-506`) will see incomplete ledgers by default — that
  is correct, not a bug to work around.
