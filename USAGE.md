# Usage

What the `revlocal` binary can do today. This tracks the implemented surface, not
the specification — commands appear here once they work.

```
cargo build --workspace
./target/debug/revlocal --help
```

## Reviewing a change

```
revlocal review --repo <PATH> --rev <REV> [--json]
```

`--repo` is a working copy or a mirror; `--rev` is any revision git can resolve.
The command discovers the change, materializes it into a scratch worktree, runs the
review pipeline over it and prints the result. The repository you point it at is not
modified.

```
$ revlocal review --repo ~/code/myproject --rev HEAD
revlocal: reviewing with the mock engine (live engine selection is not wired yet)
...
```

**The mock engine is what runs today.** Selecting `claude` or `codex` from the CLI
is not wired up yet, so this exercises the pipeline rather than producing a real
review.

### Machine-readable output

```
revlocal review --repo <PATH> --rev <REV> --json
```

Exactly one JSON document reaches stdout and nothing else. Everything
informational — progress, warnings, the mock-engine notice above — goes to stderr,
so the output is safe to pipe:

```
revlocal review --repo . --rev HEAD --json | jq '.findings[].severity'
```

Output is byte-stable: the same change and the same engine output produce the same
document, including finding order and fingerprints.

## The local database

```
revlocal db migrate --database <PATH>
```

Creates the SQLite database if it does not exist, and upgrades an existing one.
Safe to run against an up-to-date database.

## Exit codes

`0` on success. Non-zero with a message on stderr naming what to do about it — an
unreadable repository, a revision that does not resolve, a database that cannot be
opened.

## Test fixtures

The test suite builds its own git repository rather than depending on one:

```
./fixtures/build.sh
```

This writes `fixtures/out/git-basic` (12 commits with planted bugs, a lockfile-only
commit, a bot commit and a merge) plus a bare mirror, deterministically — two runs
produce identical commit SHAs. The Subversion fixture is skipped with a message if
`svn` is not installed.
