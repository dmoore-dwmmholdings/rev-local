# 0026 — The MCP connection is disposable, and reconnection waits for a caller

- Status: accepted
- Date: 2026-08-27
- Item: RL-601 (REVL-53)
- Supersedes: none

## Context

An MCP server is a third-party process rev-local did not write and cannot fix. It may
be absent, may crash mid-call, may crash *on startup every time*, may speak a protocol
revision we do not, and may name its tools whatever it likes. None of that is allowed
to take the daemon down.

## Decision 1 — one disposable connection, reconnected on next use only

`StdioClient` holds at most one connection and treats it as disposable: any transport
failure drops it, and the **next call** spawns a fresh one.

**Never eagerly.** A client that reconnected the moment a connection died would, faced
with a server that crashes on startup, spin — spawning processes as fast as the OS
allows, forever, reporting nothing. Waiting for a caller who wants something bounds
the retry rate to the rate of real work, which is the only bound that holds for a
failure we cannot diagnose.

Spawning is also lazy: a daemon configures every server at startup and may never use
most of them.

`connect_count()` is exposed so a test can assert a reconnect *happened*. Asserting
that the second call succeeded proves nothing — it would have succeeded anyway if the
first connection had never died.

## Decision 2 — a protocol error and a tool error are different things

A **protocol** error means the call did not happen (unknown tool, bad params) and is
an `Err`. A **tool** error means it happened and the tool refused; it is `Ok` with
`is_error` set.

Trama declining to overwrite a page nobody read (§11.5) is the instance that matters.
Collapsing the two would make "Trama protected your page" indistinguishable from
"Trama has no such tool", and those need opposite responses.

A tool error also does **not** drop the connection, which is asserted.

## Decision 3 — retryability comes from the server, never from the code

`McpError::retryable()` returns `Option<bool>` and is `None` where the server did not
say. Guessing would mean deciding one server's `-32002` means what another's does, and
guessing wrong on a non-retryable error turns a caller bug into a slow failure that
looks like a flaky network. The `Option` is the same shape as ADR 0010's
`cost_exhausted`, for the same reason: unmeasured is not zero.

The two exceptions are deductions, not guesses: a dead connection is retryable
(reconnecting is exactly what fixes it) and a missing binary is not (respawning fails
identically).

## Decision 4 — version skew is recorded, not enforced

A server answering a different `protocolVersion` gets a warning and the conversation
continues. These are third-party servers; refusing one that happens to work would
break publishing over a mismatch that costs nothing. What must not happen is the skew
going unrecorded.

## The bug the handshake test found

`InitializeResult` was declared without `rename_all = "camelCase"`. MCP is camelCase
on the wire, so `protocol_version` deserialized to `None` against **every** server —
which meant Decision 4's warning could never fire.

The same class as RL-506b's guessed `invalid reference`: a code path with a doc
comment explaining it, and no way to run. It was caught only because the test asserted
the handshake's *contents* rather than that a handshake occurred.

## The probe that found an untested criterion

Criterion 3 is "the child process is reaped, not leaked, on drop". Removing
`kill_on_drop` **and** the `Drop` kill entirely changed no test.

Per RL-506a's rule, this was investigated rather than shrugged at, and it was a real
hole: **a well-behaved server exits by itself when its stdin closes**, so a client
that reaps nothing still passes "the process is gone afterwards" — the server left on
its own and the client took the credit.

`MOCK_MCP_IGNORE_EOF=1` was added to the mock: a server that keeps its event loop
alive and ignores SIGTERM, so it will not leave and the client has to actually kill
it. Exactly the reasoning behind the mock engine's existing `hang` mode, which the
fixture's own header already explains.

Re-probing with reaping removed does not merely fail — **it hangs the test harness**,
which is the finding underneath the finding. `stderr` is inherited (a server's
diagnostics belong in the daemon's log, and capturing without draining would fill the
pipe and wedge the server), so a leaked server holds its parent's stderr open forever.
Reaping is not tidiness here: a leak wedges whatever launched the daemon.

## Consequences

- `REQUEST_TIMEOUT` is 30s. A server that has stopped answering is indistinguishable
  from a slow one, and waiting forever on the difference hangs the daemon.
- Replies are read until the matching id arrives, rather than taking the next line.
  A server may interleave notifications; treating the next line as *the* answer would
  misattribute one to whichever call was in flight.
- `shutdown()` closes stdin, waits `SHUTDOWN_GRACE`, then kills. Explicit rather than
  left to `Drop`, which cannot await and so cannot wait for a clean exit.
- `RpcError::data` is carried whole rather than parsed into fields. Servers put
  different things there and a struct with four optional fields would silently drop
  the fifth.
- `Tool::input_schema` is kept whole for the same reason: §11.2 validates rendered
  args against it, and a partially modelled schema validates only the modelled parts.
