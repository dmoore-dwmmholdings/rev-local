# 31. Gaining `svn:mergeinfo` is not the same as reintegrating a branch

Date: 2026-08-28

## Status

Accepted

## Context

SPEC §6.4 gives three heuristics for detecting a branch reintegration, and states
the first as:

> 1. `svn:mergeinfo` on the target path gained ranges from a branch path;

RL-904 implemented it literally. RL-906 was a timeboxed spike to find out how
reliable that is across merge styles.

It is not reliable. Measured against Subversion 1.14.5 on a purpose-built
repository, **four** distinct merge styles satisfy the stated condition, and only
one of them is a reintegration:

| style | mergeinfo gained | content changed | source path | range reaches branch head |
|---|---|---|---|---|
| reintegrate (`svn merge branch trunk`) | `/branches/reint:3-8` | yes | a branch | yes |
| sync merge (`svn merge trunk branch`) | `/trunk:4-9` | yes | **trunk** | n/a |
| cherry-pick (`svn merge -c N branch trunk`) | `/branches/cherry:7` | yes | a branch | **no** |
| `svn merge --record-only -c N` | `/branches/recordonly:8` | **no** | a branch | yes |

The consequences are not symmetric, which is what makes this worth fixing rather
than noting.

- **`--record-only` is the worst.** The idiom exists to mark a revision as
  *deliberately never to be merged* — it writes mergeinfo and changes no content.
  Synthesising a pseudo-PR from it produces a review of code a human explicitly
  rejected.
- **Cherry-pick is nearly as bad.** One revision was taken; the pseudo-PR diff is
  `trunk@fork` against `branch@rev`, which is the *whole branch*. The review would
  be dominated by work nobody merged.
- **Sync merge** is trunk flowing into a branch. Nothing new arrived on the watched
  path, so there is nothing to review, and the "branch" named in the gain is
  `/trunk` itself.

RL-904 already argued that a false positive is the expensive direction — it invents
a change that never happened and files findings against it. RL-905 makes it worse
still: the invented pseudo-PR is *authoritative*, so it demotes the genuine
per-revision reviews in its favour. A false positive does not merely add noise; it
suppresses real signal.

## Decision

Heuristic 1 is necessary but not sufficient. A mergeinfo gain is classified before
it is acted on, using three facts carried in `MergeEvidence`:

1. **Direction** — the gain's source path must not be the watched trunk. Rejects a
   sync merge.
2. **Content** — the revision must change file content, not only `svn:mergeinfo`.
   Rejects `--record-only`. Reuses `ChangedPath::is_property_only`, which RL-903
   already needed for a different reason.
3. **Completeness** — the gained range must reach the branch's last-changed
   revision as of that point. Rejects a cherry-pick.

`classify_gain` returns a `MergeStyle` rather than a boolean, and every non-
reintegration variant carries an `explain_rejection` string. "We saw mergeinfo move
and did not synthesise a change" is exactly the kind of decision §18 says must be
visible, and an operator asking why a pseudo-PR is missing needs to know which of
the three it was.

**§6.4's heuristic ordering is not changed.** The spike's question was whether the
ordering was wrong; it is not. Heuristics 2 and 3 are unaffected — a log message
naming a branch, and a file count plus an existing branch path, are both already
narrower than heuristic 1 was. What changed is that heuristic 1 is now as narrow as
the other two.

An **absent** branch head means "do not reject". The completeness test is the one
most likely to misfire on an unusual history — a branch whose last revision was
itself a sync merge from trunk, for instance — and a missed rejection costs less
than a missed reintegration.

## Consequences

- Detection is stricter, so a repository using an unusual reintegration style may
  now be missed where it was previously caught by accident. That failure mode is
  the cheap one: the branch is reviewed revision by revision, which is worse but
  not wrong.
- `detect` takes a sixth argument. Callers must supply the trunk path, whether the
  revision changed content, and any branch heads they know. The two they cannot
  cheaply omit — trunk and content — are the two that reject the two worst false
  positives.
- `tests/svn_merge_styles.rs` builds the four-style repository and asserts the
  table above, so it is a measurement rather than a claim. It runs against whatever
  Subversion is installed: 1.14.5 locally and 1.8.15 on the Windows CI runner,
  which is the cross-version half of the spike's question.

## Not covered

The spike asked about svn 1.8 through 1.14. Only 1.14.5 (local) and 1.8.15 (Windows
CI) were exercised, because those are the versions actually reachable from this
project's machines; 1.9 through 1.13 were not installed. Mergeinfo's on-disk format
has been stable since 1.5 and the four styles above are all expressible in 1.8, so
the classification is expected to hold — but "expected to hold" is not "observed to
hold", and the fixture is written so that any runner with a different Subversion
tests it for free.
