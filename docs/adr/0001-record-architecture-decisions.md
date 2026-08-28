# ADR 0001 — Record architecture decisions

**Status:** accepted
**Date:** 2026-08-27

## Context

`rev-local` is built by an autonomous loop against `SPEC.md`. The spec fixes the
decisions of record (§2) but deliberately leaves implementation judgement to the
implementer. Without a record, those judgements become invisible
and the spec silently drifts from the code.

## Decision

Every non-obvious implementation decision gets a numbered ADR in `docs/adr/`,
using this template. An ADR proposing a change to a decision of record is created
with `Status: proposed` and does **not** license changing the code — the spec is
implemented as written until a human accepts it.

## Consequences

- The repository carries a readable trail of why the code looks the way it does.
- Spec drift is detectable: a change to behaviour without a matching ADR or spec
  edit is a review finding against ourselves.
