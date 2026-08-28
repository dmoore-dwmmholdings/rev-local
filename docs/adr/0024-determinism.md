# 0024 — Determinism is guarded at the source, not observed at the output

- Status: accepted
- Date: 2026-08-27
- Item: RL-507 (REVL-52)
- Supersedes: none

## Context

§18: the same change and the same engine output must produce the same findings,
fingerprints and publish plan. Two things depend on it directly.

**Dedupe.** §10.3's fingerprint decides whether a finding is new. A fingerprint that
varied between runs would re-file everything on every review, and the symptom would
look like an engine that cannot make up its mind rather than like a hashing bug.

**Suppression.** A user suppresses a fingerprint. If the fingerprint moves, the
suppression silently stops working and the thing they asked never to hear about comes
back — the worst failure this product has, because it destroys the user's reason to
trust any of it.

## Decision — three kinds of assertion, because they catch different things

**1. Repetition.** The same review five times, compared byte for byte. Five, not two:
an ordering that differs between two hash instances agrees by chance about half the
time with four items, so a single comparison is a coin flip dressed as a test.

**2. Golden fingerprints, cross-checked.** A golden is a comparison against a *past
process*, which is the only way to catch something seeded once per process —
repetition inside one process cannot see it by construction.

Each golden was **independently recomputed** from §10.3's text by a separate
implementation, and all four agreed. A golden pasted from the code it tests only pins
current behaviour, bug included; it would happily freeze a wrong algorithm forever.

They also pin §10.3 against accidental change. If they move, that *is* the change, and
it needs a migration for every stored suppression and dedupe key.

**3. A source guard.** No `HashMap`/`HashSet` anywhere on the path from engine output
to report — five crates, scanned.

This is the one that matters most, and it is deliberately a *source* check rather than
a behavioural one. **A `HashMap` with three entries iterates consistently often enough
that a behavioural test passes for months and then fails in someone else's CI.**
Observing that today's output is stable does not keep it stable; this survives the
next contributor.

Same shape as `revlocal-vcs`'s guard that only `git/cmd.rs` may spawn git.

The audit found **zero** existing violations — the codebase already used
`BTreeMap`/`BTreeSet`/`Vec` throughout, including `Extra` for unknown config keys. So
this item's real work was making that true *by rule* instead of by luck.

## Consequences

- The guard strips `//` comments before matching, so prose about the rule is not a
  violation of it. `determinism_the_source_guard_recognises_a_violation` feeds it one
  file it must reject and one it must accept — a guard that cannot fail is decoration.
- The guard walks directories in **sorted** order. A guard that reported its findings
  in filesystem order would itself be nondeterministic, which for this test of all
  tests would be absurd.
- Findings come out in the order the engine reported them. Sorting them would be
  defensible; doing it *sometimes* would not, and
  `determinism_finding_order_follows_the_engine` uses a deliberately unsorted fixture
  (severities ascending, files identical, titles shuffled) so either mistake shows.
- The withheld list is asserted stable too: it is what the UI shows to explain an
  absence, and a list that reshuffles reads as the reasons having changed.

## The publish plan, stated rather than skipped

The criterion names "publish plans". **There is no plan builder yet** — §11 is M7 — so
the guarantee is asserted at the boundary that will feed it: the ordered,
fingerprinted, severity-tagged set of publishable findings, plus the verdict and depth
that drive it.

Said out loud in the test's own doc comment, because a criterion that *looks* covered
and is not is worse than one openly deferred. The source guard already covers
`revlocal-publish`, so the builder cannot introduce the problem when it lands.

## Negative cases observed

- Add a `HashMap` to `pipeline.rs` → the source guard fails, alone.
- Sort the findings → order and golden tests fail (2).
- Change the fingerprint input → the golden test fails, alone.
