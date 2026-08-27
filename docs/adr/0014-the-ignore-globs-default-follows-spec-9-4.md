# 14. The `ignore_globs` default follows SPEC §9.4, not §13.2

Date: 2026-08-27
Status: accepted
Item: RL-305

## Context

Two sections of the spec gave different defaults for the same field.

- **§13.2**, the per-repo config example, listed three globs:
  `**/node_modules/**`, `**/vendor/**`, `**/*.lock`.
- **§9.4**, the skip table, said the default is those three plus `**/dist/**`,
  `**/*.min.*`, `**/target/**`, and "generated-file markers".

This was found by a test, not by reading: `skip_rules_globs_cross_directories_the_way_the_defaults_assume`
asserted `dist/bundle.js` is ignored, which is true under §9.4 and false under
§13.2.

A contradiction like this does not stay harmless. Whichever section someone reads
first becomes the implementation, and the other becomes a bug report.

## Decision

**§9.4's list wins**, and §13.2's example document is updated to match.

§9.4 is the section actually about skipping and gives the rationale; §13.2 is an
illustrative config document. The larger list is also the better default on its
own merits — `dist/`, `target/` and `*.min.*` are build output, and reviewing
generated bundles spends engine budget to report nothing.

The default is now:

```
**/node_modules/**  **/vendor/**  **/*.lock
**/dist/**          **/*.min.*    **/target/**
```

`RL-107`'s `the_repo_defaults_are_the_spec_13_2_document` test parses §13.2's
printed document and compares it against `RepoConfig::default()`, so the spec and
the code cannot drift apart again without that test failing.

## Deferred: "generated-file markers"

§9.4's list ends with "generated-file markers", which is **not a glob**. It means a
marker inside the file — conventionally a `@generated` line near the top, as Go,
protobuf and Prettier all emit.

Deciding that requires reading file *content*, which discovery does not fetch: it
has paths and a numstat, not bodies. Implementing it belongs with materialization
(`RL-306`), where the tree is already checked out and the read costs nothing extra.

It is **not** in the default globs and is not silently pretended to be handled.

## Consequences

- A repo relying on the old three-glob default will now also skip `dist/`,
  `target/` and minified files. That is the intended behaviour and it only ever
  *reduces* spend, but it is a behaviour change for an existing config.
- A malformed glob compiles to nothing and the change is **reviewed**, not skipped.
  Treating an uncompilable ignore rule as "matches everything" would skip every
  change in the repository and look exactly like rev-local silently doing nothing.
  There is a test.
