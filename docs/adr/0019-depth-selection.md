# 0019 — When size says summary and risk says deep, risk wins

- Status: accepted
- Date: 2026-08-27
- Item: RL-503 (REVL-48)
- Supersedes: none

## Context

SPEC §9.3 gives three depths as a table of trigger conditions. The `summary` row
fires on size (over `deep_file_limit`, over 20k changed lines, docs/lockfiles only);
the `deep` row fires on risk (sensitive globs, deep labels, security-relevant
patterns, a critical/high finding in `standard`).

The table does not say what happens when both fire. A 200-file commit that touches
`**/auth/**` matches the first row and the third row at once, and the two acceptance
criteria for this item — "a 200-file fixture commit selects summary" and "a commit
touching a sensitive_glob selects deep" — describe that change contradictorily.

## Decision

**Deep wins.** The depths are ordered `Summary < Standard < Deep` and the deepest
firing reason takes the decision.

`summary` is a *cost* degradation: the diff is too large to read carefully, so we stop
pretending we are reading it. `deep` is a *risk* escalation. Letting cost silently
downgrade a security-relevant review is the failure §18 exists to prevent, and it
fails in the worst possible way — a summary review of an auth change reports nothing
and is indistinguishable from a clean one.

The size problem does not vanish; it is handled independently by §9.4's truncation,
which reduces the diff and *says that it did*. A deep review of a truncated diff is
worth more than a summary of a whole one.

**Every reason is kept, not just the winning one.** `DepthDecision.reasons` carries
all of them, deepest first, and `is_contested()` says whether one lost. The UI can
then say "deep, despite 201 files" rather than leaving the size rule looking broken to
anyone who reads the config and then the run.

## Escalation is "exactly once" structurally

§9.3 escalates on "≥1 critical/high finding in `standard`". Only a `standard` run
escalates — not a counter someone has to remember to increment, but the shape of the
rule:

- a `deep` run has nowhere deeper to go, and a deep re-run that finds another high
  finding would otherwise escalate forever;
- a `summary` run does not escalate either. It ran because the diff was too large or
  too dull to read properly, and noticing something did not change that. Escalating
  would hand a 25-minute budget to a 200-file diff and produce a worse review, slowly.

Verified negatively: removing the guard fails exactly the two tests that assert it.

## Two smaller calls

**Invalid `sensitive_globs` escalate rather than disappear.** `reviewable_paths`
already treats an uncompilable `ignore_globs` as "ignore nothing, review everything".
The mirror image here would be "nothing is sensitive", which is the opposite
direction — a config typo would quietly reduce scrutiny on exactly the paths the user
was trying to protect. So a broken sensitive list makes every path sensitive. Both
rules err toward looking harder.

**§9.3's security substrings over-match, deliberately.** `auth` matches `authors.rs`.
§9.3 wrote these as patterns rather than globs, and the cost of being wrong is
asymmetric: a needless deep review spends 25 minutes of local compute, a missed one
ships a vulnerability. An extensionless file (`Dockerfile`, `Makefile`) is
correspondingly *not* treated as documentation.

## Consequences

- `deep_file_limit` (150) and `deep_labels` (empty) added to `RepoConfig` and SPEC
  §13.2 — see the config sweep below. `deep_labels` defaults to empty rather than a
  guessed `["security"]`: rev-local does not know what labels a repository uses, and
  a guess would be a rule that silently never fires.
- `MAX_DEEP_LINES` (20k) is a constant, not config: §9.3 states it and no per-repo
  reason to vary it has appeared.
- The daemon chooses the depth and `revlocal-engine::timeout_for` spends the budget.
  `depth_timeouts_scale_with_depth` cross-checks the two crates, because a
  disagreement would run a deep review on three minutes and report it as an engine
  timeout.
- `requires_self_verification` is separate from the timeout so RL-502's prompt can add
  §9.3's self-refutation instruction without knowing about budgets.

## The config sweep this item triggered, and its result

RL-502 recorded a fourth instance of "§7–§12 prose names a default that §13's
documents do not carry" and recommended one sweep before M6. Done here, mechanically:
every backticked lowercase identifier in §7–§12, minus the keys the two config structs
actually declare.

Five genuine misses, out of a lot of noise (enum values, MCP tool names, run statuses):

| Key | Spec | Document | Status |
|---|---|---|---|
| `deep_file_limit` | §9.3 | §13.2 | added here |
| `deep_labels` | §9.3 | §13.2 | added here |
| `file_medium_issues` | §11.4 | §13.2 | REVL-116 |
| `andare_transition_on` | §11.4 | §13.2 | REVL-116 |
| `max_attempts` | §9.1 (via ADR 0012) | §13.1 | REVL-116 |

Two false positives worth naming so the sweep is not re-run on them: `engine_timeout`
is a *failure reason* string, not a key, and `version_args` already lives on
`InvocationTemplate` where it belongs.

The three remaining are filed rather than added, because each belongs to the item that
will use it and adding a key with no reader is how a config document acquires fields
nobody can explain.
