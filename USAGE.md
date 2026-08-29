# Usage

What the `revlocal` binary can do today. This tracks the implemented surface, not
the specification — commands appear here once they work.

```
cargo build --workspace
./target/debug/revlocal --help
```

## Getting to a first review

```
revlocal doctor                                   # check prerequisites
revlocal db migrate --database <PATH>             # create the database
revlocal repo add <PATH> --kind git --name acme --database <PATH>
revlocal review --repo <PATH> --rev HEAD
```

`doctor` is the first thing to run on a fresh install and the thing to run again
when reviews have quietly stopped. It exits non-zero when something is blocking a
review, so it works in a script.

A repository is added in `dry_run` autonomy, and `repo add` says so:

```
$ revlocal repo add ~/code/acme --kind git --name acme --database ~/rl.db
added acme (git), engine claude, autonomy dry_run — nothing is published until you widen it
```

## The command surface

| group | what it does |
|---|---|
| `revlocal doctor` | prerequisites, engines, publish targets |
| `revlocal repo` | add, list, show, remove and configure repositories |
| `revlocal review` | review one change now |
| `revlocal watch` | run the daemon in the foreground |
| `revlocal backfill` | review history, behind live work |
| `revlocal runs` | list, show and retry runs |
| `revlocal findings` | list findings, suppress one by fingerprint |
| `revlocal approvals` | see what is waiting, approve or reject it |
| `revlocal publish` | per-target status, retry and replay |
| `revlocal targets` | publish targets and capability mapping |
| `revlocal budget` | spend against the daily ceiling, and reset it |
| `revlocal hooks` | install or remove the git hooks that trigger reviews |
| `revlocal webhook` | the GitHub webhook listener and its tunnel |
| `revlocal pause` · `resume` · `kill` | stop and restart everything |
| `revlocal db` | migrate and vacuum the local database |

Every command takes `--json`. Under `--json`, exactly one document reaches stdout
and everything informational goes to stderr, so the output is safe to pipe.

[docs/OPERATIONS.md](docs/OPERATIONS.md) covers what to do when a target is down,
a budget is exhausted, a run is stuck, a branch was force-pushed, or a GitHub check
is stuck in progress.

## Watching a repository

```
revlocal watch --once --database <PATH>
```

```
$ revlocal watch --once --database ~/rl.db
  acme — 2 discovered, 1 recorded, 1 skipped
      skipped: dd1c97f — all 1 path(s) match ignore_globs

Discovery only: reviews are not executed yet. `revlocal review --repo <path> --rev <ref>` reviews one change today.
```

Discovery is persistent: every change is recorded with its skip reason, and the
cursor advances past skipped changes, so a second pass over a quiet repository
finds nothing rather than rediscovering the same commits forever.

**It does not run reviews yet, and says so on every tick.** A `watch` that
silently reviewed nothing would be indistinguishable from one whose repositories
are quiet.

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

## Publish targets and capability mapping

```
revlocal targets list --config <PATH> [--json]
```

Contacts every MCP server named in your config, discovers the tools each one
actually exposes, and reports which of your configured capabilities bound to which
tool — and which did not bind at all.

```
$ revlocal targets list --config ~/.config/rev-local/config.toml
servers:
  andare: 5 tools, 0 capabilities mapped, 0 unmapped
targets:
  andare → andare: 1 mapped, 1 unmapped
    create_issue → create_issue
    `upload_attachment` is unmapped: none of [upload_attachment, add_attachment] is exposed by the server, which has [create_issue, set_issue_status, get_page, update_page, create_page]
```

Capabilities are bound by name from a candidate list, so a server that calls the
operation something else still works as long as the name is listed. Nothing is
guessed: a capability that matches no candidate is reported, never bound to a
tool that merely looks similar.

This command reads configuration and calls no tools.

### Binding a capability by hand

When a server calls an operation something no candidate list mentions, bind it
yourself:

```
revlocal targets map <target> <capability> --tool <TOOL> --arg key=template ...
```

The override is checked against the tool's schema **before** it is saved — a tool
name the server does not have, or a template missing a field the tool requires, is
refused there rather than at the first publish that needed it. Values are checked
when they are rendered, since that is when they exist.

```
revlocal targets map andare create_issue --tool file_a_thing \
  --arg project=REVL --arg headline="{finding.title}"
```

Overrides are stored beside your config as `target-overrides.json` (or wherever
`--overrides` points), so they survive a restart. An override wins over automatic
resolution, and `targets list` and `targets test` both mark it as one.

### Dry-running a target

```
revlocal targets test <target>
```

Renders every mapped capability against a sample finding and reports what would be
sent. Nothing is called. Exits non-zero if any capability would not render, so it
works as a check.

## Publish status and retries

A run can finish with one target posted and another failed — that is normal, not
an error state, and a failed target does not hold the run open.

```
revlocal publish status --run <ID> --database <PATH> [--json]
```

```
$ revlocal publish status --run 12 --database ~/.local/share/rev-local/rev-local.db
github: delivered — 3 of 3 delivered
andare: failed — 0 of 1 delivered (`andare` refused the request with 422: the project does not exist)
revlocal: retry one with `revlocal publish replay --run 12 --target <TARGET>`
```

```
revlocal publish replay --run <ID> --target <TARGET> --database <PATH>
```

Puts that target's failed actions back in the queue with a fresh attempt budget.
Other targets are untouched — replaying Andare does not re-post the GitHub review.

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
