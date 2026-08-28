# 0020 — Truncation order, and what "omitted" means

- Status: accepted
- Date: 2026-08-27
- Item: RL-504 (REVL-49)
- Supersedes: none

## Context

SPEC §9.4 gives two caps — per file (64 KB, hunks become a stat line) and in total
(512 KB, whole files dropped in descending interest order) — and one absolute rule:
"Truncation must never silently hide a file: the omitted file list is always included
in full."

## Decision 1 — the per-file cap runs first, and the order is load-bearing

§9.4 lists them in this order and implementing them in it turns out to matter.
Capping per file first bounds every remaining section at 64 KB, so the total pass is
choosing between comparably sized files. Reversed, a single 500 KB file could evict
the entire rest of a change before the per-file rule ever ran — and the file that
survived would be the one least worth reading, because a half-megabyte diff of one
file is nearly always generated or vendored.

`truncation_the_per_file_cap_runs_before_the_total_cap` pins it: a diff that blows the
total budget raw but fits once capped comes through with **nothing** omitted.

## Decision 2 — "reduced" and "omitted" are different, and reported separately

- **Reduced** — hunks replaced by a stat line. The file is still in the diff, saying
  itself that it was too large to show. Self-announcing.
- **Omitted** — dropped entirely. Invisible in the diff, therefore it *must* be named
  outside it, which is what §9.2's prompt section 3 does.

Conflating them would put a file in the prompt's "omitted in full" list that the
engine can plainly see, which teaches the engine that the list is unreliable — the
one thing that list cannot afford to be.

A file that is reduced and *then* omitted is reported only as omitted. It is not in
the diff to have been reduced, and reporting both would misdescribe what happened.

## Decision 3 — the omitted list is never itself truncated

It is a list of names. Ten thousand names cost less than one unexplained silence:
a review that saw 60% of a diff and a review that saw all of it produce the same shape
of output — same findings, same clean verdict — and nothing distinguishes them unless
the omission is carried forward.

## Decision 4 — interest classification, where §9.4 names only four tiers

`Data < Config < Tests < Source`, exactly §9.4's four. `Data` doubles as the catch-all
lowest tier: not code, not exercising code, not configuring it. Documentation lands
there, which is a demotion §9.4 does not state but which follows from what the tiers
are for.

Two classification calls worth recording:

- **Tests are checked before config.** A `tests/fixtures/config.toml` is a test asset,
  not the build's configuration. Classifying it as config would drop it before the
  test that reads it, leaving a test in the diff that cannot be understood.
- **An unrecognised `.json` is data, not config.** The config ones are named
  explicitly (`package.json`, `tsconfig.json`, …); everything else is a fixture, a
  snapshot, or an export.

**Within a tier, later in the diff is dropped first.** Deterministic, so two runs over
one change omit the same files. Smallest-first would fit more files, but after the
per-file cap every section is already bounded at 64 KB, so it would buy little and
introduce a systematic bias against the largest real change in a tier.

## Decision 5 — binaries are summarised regardless of size

Size is irrelevant: a 12-byte blob is still bytes, and an engine handed them spends
its budget tokenising noise at best. `file.binary` alone triggers the stat line.

## Consequences

- A section that cannot be matched to a `FileDiff` is **kept** and treated as source.
  Dropping something we failed to identify is the wrong way to be wrong.
- The preamble (anything before the first `diff --git`) is never dropped: it is small,
  it is usually the commit header, and losing it changes what the engine thinks it is
  looking at.
- `header_path` splits a `diff --git a/x b/y` header on the **last** ` b/`, and
  returns `None` rather than guessing when it cannot. A path containing a literal
  ` b/` would need `-z`-quoted output to disambiguate; `None` keeps the section rather
  than misattributing it.
- `TruncationOutcome::is_consistent()` asserts §18 in both directions — truncation
  claimed with nothing named, and omissions named while claiming nothing was cut.
