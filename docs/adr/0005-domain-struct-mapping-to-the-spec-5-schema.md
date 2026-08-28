# 5. Domain struct mapping to the SPEC §5 schema

Date: 2026-08-27
Status: accepted
Item: RL-103b

## Context

SPEC §5's DDL is normative, and its implementation note says a deviation must be
recorded here **and** the section updated in the same commit. Building the structs
surfaced four places where the Rust types and the DDL do not correspond
one-to-one. Three are shape, one is a real schema change.

## Decisions

### 1. `run.degraded` is a new column — this is the schema change

SPEC §8.1 gives `EngineOutcome` a field `degraded: Option<String>`, and §12.3 says
every publish action is escalated to high risk "if the run is `degraded`". So the
run has to carry that state past the end of the engine invocation, and the `run`
table had nowhere to put it.

Added `degraded TEXT` to the `run` table, and SPEC §5 is updated in this commit.
It is a nullable **reason**, not a boolean flag, for two reasons: §8.1 already
types it that way, and "no silent caps" (§18) means a run whose actions are all
escalated should be able to say what was degraded about it. An operator looking at
an approvals inbox full of high-risk actions needs to see *why*.

### 2. Timestamps are `chrono::DateTime<Utc>`, and core cannot read the clock

SPEC §5 stores timestamps as `TEXT`. `Timestamp` is aliased to
`chrono::DateTime<Utc>` and serialized with chrono's serde support, which is
RFC 3339 — lexicographically sortable, which matters because `idx_audit_at` orders
by a text column.

chrono is taken **without its `clock` feature**. `revlocal-core` represents
instants but never calls `Utc::now()`, which keeps it free of ambient reads (SPEC
§4.1) and means every test pins its own time rather than depending on when it ran.
Callers supply timestamps.

### 3. `Usage` groups three columns; `DiffStat` unpacks one

`run.tokens_in`, `run.tokens_out` and `run.cost_usd` become a single `Usage` field,
and `change.diff_stat_json` becomes a typed `DiffStat`. Neither changes the
schema — one groups adjacent columns behind the invariant that travels with them,
the other gives a JSON column a type instead of a `String`.

The invariant worth naming: `Usage::cost_usd` is `Option<f64>`, and `add` keeps an
unknown cost unknown rather than folding it to `0.0`. An engine that reports no
price must not make a budget look like it has headroom (D10, §18);
`cost_is_complete()` is how a caller asks whether the total can be trusted.

### 4. `BudgetLedgerEntry` omits the surrogate `id`

`budget_ledger` has both an `id INTEGER PRIMARY KEY` and `UNIQUE (repo_id, day)`.
The natural key is what every call site uses — "this repo's spend today" — so the
struct carries `(repo_id, day)` and not the surrogate. The column stays in the
schema for SQLite's rowid; nothing in the domain needs to name it.

## Consequences

- `Run::is_consistent()` encodes the §18 invariant as code: a `Skipped` run has a
  `skip_reason` and a `Failed` run has an `error`, both exactly. `RL-109` should
  assert it on write rather than trusting callers.
- Migration `0001_init.sql` (`RL-108`) must include `degraded TEXT` on `run`.
- Adding chrono to `revlocal-core` widens its dependency set; `RL-104`'s
  no-I/O test should assert the *absence* of tokio/sqlx/reqwest rather than an
  allowlist, or it will need updating every time a pure dependency is added.
