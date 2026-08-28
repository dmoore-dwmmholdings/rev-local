# 11. Redaction happens at the field, not at the line

Date: 2026-08-27
Status: accepted
Item: RL-111

## Context

SPEC §18 requires a redaction layer that "scrubs anything matching token/secret
patterns **before it reaches a sink**". There are two places that could happen:

1. **At the field**, as each value is recorded by `tracing`.
2. **At the line**, scrubbing the serialized output on its way to the writer.

Line-level scrubbing is easier and catches everything in one place. It is also
wrong in two ways that matter. The secret still exists in memory as part of a
formatted line, and — worse operationally — every *additional* sink needs the same
treatment. A second file, stderr, a future exporter: each one is a new place to
forget, and forgetting is silent.

## Decision

Redact at the field. `RedactingVisitor` implements `tracing`'s field API and
rewrites values as they are recorded, so nothing downstream ever sees the original.

This forced a second decision. `tracing_subscriber::fmt`'s field formatting
**cannot be intercepted** — `RecordFields` is a sealed trait, so a wrapper cannot
be written — and going through `fmt` would have pushed redaction back to the
line-level position this decision exists to avoid. So `revlocal-daemon` emits the
JSON itself, in `RedactingJsonLayer`. That is more code than configuring `fmt`, and
it is the reason for it.

### Span fields, and fields recorded later

Both are covered, and both matter more than event fields:

- A span's fields are attached to **every event inside it**, so one leaked span
  field leaks on every line of that span rather than once.
- `Span::record` after creation is a different code path from span creation, and
  it is the normal way a value learned later gets attached. Missing it would leak
  exactly the fields that arrive from a connect or an auth step.

### Field names first, patterns second

Two mechanisms, in this order:

- **Field names are primary.** A field whose name contains `token`, `secret`,
  `password`, `api_key`, `credential`, `authorization` (and similar) has its value
  replaced wholesale, whatever shape it has. This is reliable *because it does not
  have to recognise anything* — a credential format nobody anticipated is exactly
  the one worth protecting.
- **Patterns are a safety net** for secrets inside free text: an error quoting a
  request header, a transcript echoing a command line. Pattern matching cannot be
  complete, so it is defence in depth and never what is relied on.

Two details in the patterns are load-bearing:

- **The prefix survives.** `ghp_[redacted]` rather than `[redacted]`, because
  "something leaked" is not actionable and "a `ghp_` token leaked" tells an
  operator which credential to rotate.
- **The suite's own prefixes require a digit** in the value. `andare_` and
  `trama_` are prefixes of real `RepoConfig` field names — `andare_min_severity`,
  `trama_space` — and without that requirement the redactor would blank ordinary
  configuration in the logs while hiding nothing. There is a test naming those
  fields.

## Verification

The tests drive the real layer over a real sink, and one reads the log file back
off disk, because a test of `redact()` alone would pass even if the layer forgot
to call it.

Checked negatively: with redaction disabled in the visitor, **7 of the 9 layer
tests fail**. The two that still pass are the ones asserting ordinary logging is
unharmed and still valid JSON, which correctly do not depend on redaction.

## Consequences

- Numeric and boolean fields pass through as JSON numbers and booleans. They
  cannot carry a credential, and stringifying them would make the log harder to
  query for no benefit.
- A logging failure is swallowed rather than propagated: it must not take the
  process down, and it must not recurse into `tracing` to report itself.
- `init` installs a global subscriber, which can only happen once per process.
  Tests use `with_default` instead, which is also why the on-disk test builds its
  own file writer rather than calling `init`.
