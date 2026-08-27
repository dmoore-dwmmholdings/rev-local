# 0025 — A fixture that never triggers the rule it exists for

- Status: accepted
- Date: 2026-08-27
- Item: RL-508 (REVL-118)
- Supersedes: none

## What happened

M6's seven stories all had observed passing gates. The milestone's own §17 exit gate,
run rather than inferred, failed:

```
200-file commit:  depth=summary   PASS
                  truncated=false FAIL
```

`fixtures/build.sh` generated 200 files of **two lines each** — a 44,662-byte diff
against a 524,288-byte default `max_total_diff_bytes`. Neither §9.4 cap could fire.
The step's own comment said it existed "for depth selection **and truncation**"; it
had only ever done the first, since the fixture was written.

## Why seven passing story gates missed it

`pipeline_e2e`'s truncation test lowers `max_total_diff_bytes` to 4,000. That is a
*correct* unit test of the truncation logic — you cannot exercise a 512 KB boundary
with a fixture you also want to keep small, so lowering the budget is the right call
for testing the algorithm.

But every test that touched truncation did the same thing, so **§9.4's default path
had never run end to end**. The tests all agreed with each other and none of them
tested what a user gets.

This generalises past this bug:

> **A test that adjusts a default to reach its subject verifies the mechanism, not the
> configuration.** If every test of a rule adjusts the same default, nothing checks
> that the shipped default ever reaches the rule at all.

That is the gap a milestone exit gate exists to close, and it only closes it if the
gate is *run as written* rather than treated as a summary of the story gates.

## Decision

Enlarge the generated files to ~3.5 KB each (a header plus 40 one-line functions), so
200 of them produce a **737,462-byte** diff — comfortably over the 512 KB default,
while each file stays well under the 64 KB per-file cap so the *total* cap is what
fires and files are **omitted** rather than reduced. That is the case §17's gate names.

Changing the fixture here **strengthens** the gate rather than weakening it:
afterwards, the gate tests what it says. Relaxing `truncated=true` would have left a
§18 safety property permanently unexercised at its shipped settings.

`the_m6_exit_gate_runs_at_default_settings` now asserts the whole gate — including
"the full omitted-file list present in the prompt", **name by name** rather than by
count, since a count would pass against a list naming 58 wrong files.

## The parity problem, and what was actually verified

`build.sh` and `build.ps1` must produce identical bytes, and `fixture_parity` compares
them — but **`pwsh` is not installed here**, so that test reports itself skipped
rather than passing.

Rather than ship an unverified generator, the PowerShell string construction was
independently simulated and compared against the bytes `build.sh` actually wrote, for
five files across the range. Zero mismatches. That is weaker than running `pwsh` and
is stated as such: **`build.ps1`'s new branch has not been executed anywhere.** It
stays on REVL-29's ticket.

Both generators are ASCII-only in this step, with no locale-dependent formatting,
specifically so the two have less room to disagree.

## Consequences

- Every commit sha from step 9 onward changed. **No test needed editing**, because
  tests reference fixture commits by *role*, never by sha — the invariant recorded
  when the fixture was first built, now paid off.
- The fixture grows from ~44 KB to ~700 KB of generated content. Acceptable: it is
  the only fixture that exercises a size-triggered rule, and a size-triggered rule
  needs size.
- The per-file cap (`max_file_diff_bytes`, 64 KB) is **still** only exercised with a
  lowered budget — no fixture file approaches 64 KB. Same class of gap as this one,
  smaller blast radius, and now written down rather than rediscovered.
