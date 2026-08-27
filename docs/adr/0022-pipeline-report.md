# 0022 — The review report is a stable document, and escalation asks the normalized findings

- Status: accepted
- Date: 2026-08-27
- Item: RL-506a (REVL-51)
- Supersedes: none

## Context

RL-506 wires §9's stages into one call and exposes the result as JSON. Two decisions
inside it are worth recording; a third is a note about how the item was split.

## Decision 1 — "stable JSON" is a constraint on content, not on formatting

The criterion reads "output is stable JSON suitable for test assertions". That is not
a request for pretty-printing: it means two reviews of the same commit with the same
config must produce **byte-identical** documents.

That rules out more than it first appears. The worktree is a scratch directory whose
path differs every run, so **no report field may carry an absolute path**. A single
leaked `cwd` would destroy reproducibility while every other assertion still passed.
Database ids and timestamps are absent for the same reason — they describe the *run*,
not what the review found.

`ReportFinding` is therefore a separate type from `Finding` rather than a re-export:
`Finding` carries `id` and `created_at`, both of which vary.

Two guards: `report_is_byte_stable_across_runs` compares two full reviews, and
`report_carries_no_absolute_paths` names the specific failure so a future reader knows
*why* the first one exists.

## Decision 2 — escalation asks the normalized findings, not the engine's

§9.3 escalates a `standard` run to `deep` on "≥1 `critical`/`high` finding". The
question is *which* findings.

**The ones that survived normalization.** A critical finding the user has suppressed
must not buy them a 25-minute re-run they explicitly asked not to have, and a
superseded one is a repeat of something already filed. Asking the raw engine output
would also mean any engine hallucinating a filename could spend the escalation budget
at will — an out-of-diff critical is capped to `medium` by §9.5, and a capped finding
is not an escalation trigger.

This required normalizing *inside* the attempt loop rather than after it, which reads
slightly worse and is correct.

## Decision 3 — the item was split

RL-506 as written asked for the pipeline, a `VcsAdapter` implementation (none exists —
the trait is declared and only free functions implement its parts), and a
`revlocal review` CLI subcommand. That is three things.

All four acceptance criteria and the gate live in the daemon, so the pipeline plus its
e2e suite is the item. The adapter assembly and the CLI surface are **REVL-117**.

## The no-network wrapper, and why a weak one is worse than none

Criterion 4 asks for "zero network access, asserted by a no-network test wrapper".

A wrapper that merely claims to block the network is *worse* than no wrapper, because
it makes the assertion look done. The obvious implementation — run an outbound command
and assert it fails — is exactly that: on a machine with no network it passes whether
the wrapper works or not.

So the wrapper sets `GIT_ALLOW_PROTOCOL=file` and points every proxy variable at a
closed port, and `the_no_network_wrapper_actually_blocks_the_network` requires the
**specific refusal** git emits *before opening a socket*. That assertion fails on a
machine where the wrapper is not applied, and passes only where it is.

Its first run failed, usefully: I had guessed the wording as "not supported"; git
actually says `transport 'https' not allowed`. Same lesson as RL-505 — the assertion
now matches what the tool really emits rather than what I assumed it would.

## Consequences

- A change with nothing left after `ignore_globs` is **skipped**, not reviewed empty.
  Sending an engine an empty diff spends a budget to be told nothing is wrong.
- `failure` carries a code, not a message: §8.2's failure reasons are a fixed set the
  UI and audit log key on, and prose would change whenever an error's wording did.
- `ReviewReport::is_consistent()` asserts §18 across three claims at once — truncated
  implies something named, failed implies a reason, skipped implies a rule.
- `suppressed_fingerprints` is sorted before it reaches the prompt, so the rendered
  prompt — and any transcript diff — is stable too.
- `reviewing_a_repository_does_not_mutate_it` extends M4's guarantee to the layer that
  actually hands a worktree to a third-party binary.

## A negative probe that found a hole in the tests

Three probes were run against this module. Two initially failed to bite:

- Leaking a path into `summary` changed nothing, because `summary` is overwritten on
  the success path. **The probe was ineffective, not the test** — re-probing through
  `repo` failed both stability tests, as it should.
- Swapping `publishable()` for every finding changed nothing, because the only
  capped-finding case was still `Open`. **That was a real gap**: Decision 2 above had
  no test. `a_suppressed_critical_does_not_escalate` was written in response, and the
  re-probe now fails exactly it.

Worth keeping as method: when a negative probe does not bite, establish which of the
two it is before moving on. Assuming the test is fine leaves an untested claim behind.
