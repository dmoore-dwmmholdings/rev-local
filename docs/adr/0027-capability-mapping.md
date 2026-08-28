# 27. Capability mapping resolves exactly, renders typed, and validates before sending

Date: 2026-08-28

## Status

Accepted

## Context

SPEC §11.2 claims the Andare integration does not require knowing Andare's tool
names at build time. Config lists `tool_candidates` per capability and the mapper
resolves them against whatever the server actually exposes. Three decisions inside
that sentence are not obvious, and each has an appealing alternative that fails
quietly.

## Decision

**Candidates match exactly.** No fuzzy matching, no prefix or case-insensitive
comparison, no edit distance. A server exposing `create_ticket` when the candidate
list says `create_issue`, `create_work_item`, `issue_create` is reported as
unmapped.

**A string that is exactly one placeholder keeps the referenced value's type.**
`args = { line = "{finding.line}" }` renders as the number `42`, not `"42"`.
Interpolation into surrounding text still produces a string.

**Rendered arguments are validated against the tool's own `inputSchema` before the
call.** A violation is an error naming the offending field, not a call.

**A placeholder that resolves to nothing is an error, not an empty string.**

## Consequences

- A server that renames a tool without listing the old name breaks loudly at
  `targets list` rather than silently binding to something adjacent. This is the
  intended trade: the failure mode of a near-match is filing findings into
  whatever `create_thing` happened to be, discovered when somebody reads the wrong
  tracker a week later.
- Numeric and boolean arguments are expressible. Stringifying every substitution
  would make `{"type": "integer"}` fields impossible to satisfy, and the schema
  would reject them at the last possible moment — after the payload looked right.
- Validation needs the schema, so it needs discovery to have cached it. That is
  why RL-603's cache stores tools *with* their input schemas rather than names
  alone; a name-only cache would make this check impossible without a second round
  trip per call.
- §18's no-silent-caps rule reaches arguments: an issue filed with an empty title
  is worse than an issue not filed, because it looks like it worked.
- `revlocal targets list` reports resolution without calling anything. A command
  that reports on publish configuration must not be able to publish.
