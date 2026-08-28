# 12. Scratch directories default to failed

Date: 2026-08-27
Status: accepted
Item: RL-301

## Context

SPEC §6.1 puts materialization in `{data_dir}/scratch/{run_id}/` and says the
directory is "removed when the run terminates, unless `global.keep_scratch_on_failure`
is set". §13.1 defaults that flag to `true`.

That describes two paths — success removes, failure keeps — but a run has more
exits than two. A `?` in a stage nobody wrote a branch for, a cancellation from
the kill switch (§12.1), a panic unwinding through the pipeline: each of those
drops the guard without either path having been chosen.

## Decision

`ScratchDir` starts in `RunOutcome::Failed`. A caller must say `mark_succeeded()`
for the directory to be cleaned up.

The alternative — defaulting to success and marking failure on the error path —
deletes the materialized tree on exactly the runs where someone needs it, and does
it silently. The paths that would be missed are the unplanned ones, which are the
same paths where debugging matters most.

This is why the lifecycle is RAII rather than a cleanup call at the end of the
happy path: a cleanup call is only reached when things went well, which is the
case that needs it least.

Two tests hold this: one panics through the guard with `keep_scratch_on_failure`
set and asserts the tree survives, and one panics with it unset and asserts
cleanup still happens.

## Related decisions

**A pre-existing directory is refused, not reused.** A collision means two runs
share a `run_id`. Reusing the directory would review one run's tree under the
other's id, and the result would look like a correct review of the wrong thing.
`AlreadyExists` names the run id.

**Nothing about the repository appears in the path.** The `run_id` alone is the
isolation mechanism, which is what lets the same repository be reviewed twice
concurrently — §4.3 allows two concurrent runs by default.

**`Drop` never panics and never propagates.** A cleanup failure during an unwind
would abort the process and lose the original error, which is the one that
mattered. It logs instead. `remove_now()` exists for callers that need to know
whether cleanup worked, since `Drop` cannot tell them.

**A kept directory says why it was kept.** A directory left behind with no
explanation looks like a leak, and the next person to find it will delete it —
including, eventually, an automated cleanup someone writes for exactly that reason.

## Consequences

- The pipeline must call `mark_succeeded()` at the point a run reaches a
  successful terminal state, not earlier. Marking success before publishing would
  discard the tree while it was still needed.
- `RL-1204` (`revlocal scratch`) and the startup pruner both need
  `ScratchDir::path_for`, which is public for that reason.
