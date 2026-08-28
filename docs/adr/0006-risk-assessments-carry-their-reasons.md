# 6. Risk assessments carry their reasons

Date: 2026-08-27
Status: accepted
Item: RL-105

## Context

SPEC §12.3 classifies each publish action as `low` or `high`. Under
`auto_low_ask_high` a high-risk action is queued to the approvals inbox (§12.4),
where a human decides. The obvious signature is
`fn classify(...) -> RiskClass`.

That signature makes the inbox unusable. Five independent rules can produce
`high` — the action's own nature, first use of a `(target, capability)` pair, a
degraded run, a finding below 0.6 confidence, and the burst threshold — and they
compose. A reviewer looking at a queued comment cannot tell whether it is waiting
because the engine output was salvaged or because the repo has been noisy for an
hour, and those call for opposite responses.

## Decision

`classify` returns a `RiskAssessment { class, reasons }`. Every rule that fired is
recorded, including rules that were redundant: a first use of `create_issue`
reports both `inherently_high_risk` and `first_use_of_capability`, because the
audit log otherwise loses the fact that this was the first time rev-local ever
wrote to that system.

`reasons` is empty exactly when `class` is `Low`. There is nothing to explain about
an action that simply proceeds, and the emptiness is asserted as an invariant
across the whole input space rather than left as a convention.

Two further decisions inside the function:

**`ActionIntent` rather than `Capability` as the input.** §12.3 splits
`post_review` by verdict, `set_check` by conclusion, and `upsert_doc` by whether
the page is published — those distinctions *are* the classification. Carrying them
in the type means the pipeline cannot lose them, and `ActionIntent::capability()`
derives the `publish_action.capability` column so the two cannot disagree.

**An unmeasurable confidence escalates.** `f64::NAN < 0.6` is `false`, so the
natural comparison lets a NaN through as though it were confident. The check is
written with `partial_cmp` and an explicit `None => true` arm: an unknown
confidence is not a high one, and the safe direction for an unknown is to ask a
human.

## Two cases the spec does not enumerate

§12.3's lists are not exhaustive over the action space. Both gaps are resolved
against its own stated principle — low is "additive, easily reversible, low blast
radius", high is "blocks people, notifies broadly, or creates work":

- **`Check { in_progress }`** — §11.3 says the check is "always `in_progress` while
  the run is active". It is a progress report that blocks nobody: **low**.
- **`LinkDocToIssue`** — an additive, reversible cross-reference: **low**.

If either turns out to matter, they are one line each to change and both have a
named test.

## Not reinterpreted

*First use of a `(target, capability)` pair is always high risk* is a decision of
record (SPEC §12.3). It is implemented literally: checked
independently of the baseline, applying to every intent, with no exemption for
actions that look harmless. `RiskInputs` deliberately has no `Default` — defaulting
`pair_previously_succeeded` to `true` would silently skip the one rule that must
never be skipped, so a caller has to state it.

## Consequences

- The store must be able to answer "has this `(target, capability)` pair ever
  succeeded?" and "how many actions has this repo posted in the last hour?".
  `RL-109`/`RL-110` own those queries; `classify` stays pure and takes the answers.
- `RiskAssessment` is serializable and round-trips, so it can be written into the
  audit log's `detail_json` verbatim rather than re-derived later from inputs that
  may no longer exist.
