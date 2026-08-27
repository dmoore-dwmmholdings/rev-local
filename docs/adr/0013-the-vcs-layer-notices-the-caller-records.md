# 13. The VCS layer notices; the caller records

Date: 2026-08-27
Status: accepted
Items: RL-303b, RL-304

## Context

Recovering from a force-push has to produce an audit row: SPEC §6.2 names the
event (`history_rewritten`) and §5 defines the table. The obvious implementation
is for `revlocal-vcs` to write it, which means `revlocal-vcs` depending on
`revlocal-store`.

## Decision

It does not. Recovery returns `DiscoveryEvent` values, each carrying its own
`audit_kind()`, and whichever layer is orchestrating writes them.

Three reasons, in increasing order of how much they cost to get wrong:

1. **A dependency edge is easy to add and hard to remove.** Once the VCS layer can
   write to the database, something else in it will, and the boundary that makes
   adapters testable without a database is gone.
2. **The events become assertable as values.** A test can check that a rewrite
   produces the right event with the right fields without standing up SQLite, and
   `git_force_push_writes_an_audit_row` plays the caller to prove the event carries
   everything a row needs.
3. **Audit kinds have to be distinct**, and that is a property of the event set,
   not of any one call site. Two events sharing a kind would be indistinguishable in
   the log — precisely where the difference matters — so there is a test over the
   whole set.

`revlocal-store` is a **dev**-dependency of `revlocal-vcs` for that one test. The
production dependency edge does not exist, and `RL-104`'s no-I/O test does not
cover this crate, so the discipline is the ADR plus the absence of the edge in
`Cargo.toml`.

## Related: why the choke point grew a stdin method

`git patch-id` reads a diff from stdin and has no file-argument form. The
alternative to `GitRunner::run_with_stdin` was a second call site spawning `git`
to pipe into it — which is exactly what `git_cmd_no_module_spawns_git_directly`
exists to prevent, and it would have had no timeout and no prompt suppression. The
choke point owns piping too.

## Consequences

- The daemon (`RL-50x`) must record every returned `DiscoveryEvent`. An event that
  is returned and dropped is a silent cap, and nothing in the type system prevents
  it — this is a review point when the orchestrator is written.
- If a third crate ever needs the same "notice, do not record" shape, the event
  enum belongs in `revlocal-core` rather than being duplicated.
