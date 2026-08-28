# 15. Discovered state does not live in `config_json`

Date: 2026-08-27
Status: accepted
Item: RL-307

## Context

SPEC §6.3 says the selected GitHub transport "is reported by `revlocal doctor` and
**stored on the repo row**". §5's `repo` table had nowhere to put it.

The cheap option was `repo.config_json`, which already exists and is schemaless.

## Decision

A new column, `repo.github_transport TEXT`, with a `CHECK` constraint listing the
three ladder outcomes. SPEC §5 is updated in the same commit (migration `0003`).

`config_json` holds what the **user chose**. The transport is what the **ladder
found**. Putting them in one place makes it impossible to answer the question a
doctor report exists to answer: *is this how you configured it, or is this what
rev-local fell back to?* A user who configured an MCP server and is silently on
unauthenticated REST needs those to be different-looking things.

The column is nullable, and `NULL` means **not probed yet** — deliberately
distinguishable from `'unauthenticated'`. One means nobody has looked; the other
means we looked and this is as good as it gets. Collapsing them would make a
never-probed repo indistinguishable from a degraded one.

`RepoStore::set_github_transport` is separate from `update` for the same reason it
is a separate column: it is written by the probe, not by anyone editing settings,
and folding it into the general update would let a stale in-memory `Repo` clobber a
fresher probe result.

## Consequences

- `RepoStore::update` does not touch it. A caller that wants both must do both.
- The `CHECK` constraint means a transport name is a stored value with the same
  status as any other enum in §5 — renaming one is a migration, and the test
  `github_transport_names_are_stable_because_they_are_stored` says so.
- Third schema amendment so far (`run.degraded`, `budget_ledger.cost_complete`,
  `repo.github_transport`). All three were the same shape of gap: §5's DDL predating
  a fact another section requires to be persisted. Worth checking §5 against §§6–12
  in one pass rather than discovering the fourth the same way.
