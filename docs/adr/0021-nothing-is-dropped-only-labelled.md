# 0021 — Normalization labels findings; it does not discard them

- Status: accepted
- Date: 2026-08-27
- Item: RL-505 (REVL-50)
- Amends: SPEC §9.5
- Supersedes: none

## Context

§9.5 as written says three things that conflict with each other and with §18:

1. "Clamp `severity` to the allowed set; unknown → `medium`" — but §8.3's schema
   restricts severity to five literals, so an unknown one is rejected before
   normalization ever sees it. Under the spec as written the clamp is unreachable.
2. "**Drop** findings whose `file` doesn't exist in the change's file set **unless**
   `allow_out_of_diff_findings = true` … — an out-of-diff finding is retained but
   forced to `severity <= medium`". The first clause drops when the flag is false;
   the second says out-of-diff findings are retained and capped. RL-505's own
   acceptance criterion 1 states the second reading.
3. "**Drop** findings matching an active `suppression`."

§18 ("no silent caps") is a decision of record and is not mine to weaken. §8.3's
schema likewise.

## Decision

**Normalization never discards a finding. It labels it, and the publish plan filters
on the label.**

`FindingState` already has `Suppressed` and `Superseded` for exactly this. A dropped
finding is unanswerable: a user who asks "why didn't it mention the thing in
`src/other.rs`?" gets an answer from a labelled row and silence from a discarded one.
It costs nothing — these are rows in a local SQLite file, not tokens in a prompt.

`NormalizedFinding::is_publishable()` is the single place the question is answered, so
"suppressed things are not published" is one rule rather than a condition every target
re-implements slightly differently.

### Out-of-diff findings are retained and capped, never dropped

Resolving conflict 2 in favour of criterion 1. Beyond the grammar, the substance: an
out-of-diff finding says "you changed X, and Y — which you didn't touch — is now
broken". That is exactly the class of defect a human reviewer misses and this product
exists to catch. `allow_out_of_diff_findings` defaults to `false` outside `deep`, so
the spec-as-written would have discarded rev-local's most distinctive output by
default, silently.

Capped at `medium`, and `deep` keeps full severity: a deep review is explicitly sent
to look beyond the diff, since refuting a finding means reading the code around it.

A finding with **no** file is repo-wide, not out-of-diff. Treating it as out-of-diff
would cap every architectural observation at medium.

### Unknown severity is salvaged from the drop, not admitted by the schema

§8.3 stays exactly as it is: the schema rejects `"warning"`, the drop is recorded, and
§8.3's audit trail is unchanged. Normalization then reads the recorded drops and
re-admits any whose *sole* violation was the severity, rewriting it to `medium`.

This satisfies §9.5's clamp without relaxing a decision of record, and it fails in the
right direction: a finding with a bad severity **and** a missing title stays dropped
rather than being half-guessed into existence.

`medium`, not `low`. An engine that could not spell its severity gives no evidence the
finding is unimportant — only that its output was sloppy. Rounding down would let a
formatting bug quietly demote a real defect.

**The salvage recognises a severity-only failure by looking for `"severity"` in the
violation text `revlocal-engine` produced** — an assumption about another crate's
wording. If it were wrong the salvage would never fire in production while every
hand-written test still passed: a silent cap hidden behind a green suite. So
`normalize_salvages_what_the_real_validator_actually_drops` runs the real validator
over a real document and feeds its real output in. Verified this iteration.

## Consequences

- SPEC §9.5 rewritten to say all of the above. The old wording is quoted in the
  amendment so the change is legible rather than silent.
- **Suppression is checked before supersession.** A user who asked never to hear
  something again should see "suppressed", not "already told you" — the second is a
  worse answer to the same question.
- An **uncompilable suppression glob suppresses nothing**. Note this is the *opposite*
  direction from `sensitive_globs` (ADR 0019), and deliberately: there, erring means
  looking harder; here, erring toward matching would silence findings the user never
  asked to silence. Both rules err toward *saying more*.
- A suppression with neither fingerprint nor glob is inert, per `is_actionable()`.
  Matching everything would silence a whole repo from one malformed row.
- A repeat **within one run** is superseded too, so an engine that reports the same
  defect twice files it once.
- `out_of_diff` is carried on `NormalizedFinding` rather than stored. §9.5 asks the
  publish layer to render these as non-anchored (GitHub cannot anchor a comment to a
  line it cannot see) and that layer runs in the same process. If a later item needs
  it to survive a restart it becomes a column; nothing needs it yet.
- A missing `confidence` is 1.0, not 0.0. Absent evidence about confidence is not
  evidence of no confidence, and 0.0 would sort every such finding below
  `LOW_CONFIDENCE_THRESHOLD` and out of sight.
