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

  queued 1 run(s)
  reviewed acme a1b2c3d — comment (3 finding(s), 0 action(s), claude)
```

Each tick discovers, queues and reviews: a run left over from an earlier tick is
executed even when this tick found nothing new to discover.

Discovery is persistent: every change is recorded with its skip reason, and the
cursor advances past skipped changes, so a second pass over a quiet repository
finds nothing rather than rediscovering the same commits forever.

A tick that reviewed nothing says why rather than printing nothing — "the kill
switch is engaged", "over today's budget", "that repository is disabled". A
`watch` that silently reviewed nothing would be indistinguishable from one whose
repositories are quiet.

## Leaving the app running

The desktop app runs the same pass on a timer. Turn **Autopilot** on at the top of
the dashboard; it then checks every enabled repository once a minute, reviews what
it finds, and delivers whatever is ready to go — no button presses.

The panel says what the last pass did in one sentence, and lists anything that did
*not* happen and why.

### It only reviews while it is running

There is no background service (§4.2): the daemon runs inside the app, so a
machine that reboots stops reviewing until somebody opens it again. **Settings →
Starting up → "Start rev-local when I log in"** arranges that. It is off until you
ask for it, and the switch reads the filesystem rather than a remembered
preference — a login item's failure mode is silently not being there.

### What goes where

| Output | Configuration needed | Waits for approval? |
|---|---|---|
| Local report | none | **no** |
| Andare issue | the project key | under every mode except `auto` |
| GitHub issue | a recognisable GitHub `remote_url` | under every mode except `auto` |

Filing into somebody else's system is high risk (§12.3), so `auto_low_ask_high` —
the default — holds every tracker issue for you. Choose `auto` on the dashboard if
you want those filed unattended too.

A **local report is not** high risk: it is a file on your own disk, which nobody
else sees and you can delete. It is written under every mode, which is what makes
it the output that works with nothing set up.

The GitHub remote is read from the repository itself when it is added, and
backfilled on the next pass for repositories added before that. A remote on a host
that is not recognisably GitHub is refused rather than guessed at — every forge
uses the same URL shapes, and a wrong guess files your findings against whatever
`owner/name` exists on github.com.

### Local reports

Every repository writes its findings to disk as markdown, one file per finding:

```
~/.local/share/rev-local/reports/<repository>/<fingerprint>.md
```

This target needs no configuration and is on by default, so a machine with no
tracker configured still produces something you — or another agent — can read.
The filename is the finding's fingerprint, so re-reviewing a change that still has
the same problem rewrites one file rather than accumulating duplicates. Drop
`report` from a repository's `targets` to turn it off.

`revlocal watch` runs the identical pass from a terminal and writes the same local
reports; `revlocal publish` is what delivers to a tracker there.

### What it reviews

`review_commits` and `review_prs` decide which kinds of change are eligible. Both
default to on: a watched local repository may never have a pull request, and one
that reviewed neither would review nothing at all. A commit already inside an open
pull request is still only reviewed once.

Turning one off is honoured, and a change skipped for it says so:

```
skipped: review_commits is off for this repository
```

### Stopping, and starting again

The kill switch — on every screen and in the tray — pauses in the database *and*
cancels whatever is running, so an engine mid-review is terminated rather than
left to finish. Releasing it is the **Resume** button on the paused banner.
Anything that was already told to stop stays stopped; resuming is for new work.

### Housekeeping it does on its own

- **Approvals expire.** An action nobody answered within `approval_ttl_hours` (72)
  is rejected with the reason `expired`, which is deliberately not the same as
  somebody declining it. Set it to `0` to wait indefinitely.
- **Old runs are cleared.** Finished runs and their transcripts past
  `transcript_retention_days` (30) are removed, at most once a day. `0` keeps
  everything.
- **A repository whose checkout has gone** is reported by name and costs nothing:
  no discovery, no queueing, and no engine. The other repositories are unaffected.

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
revlocal: reviewing with the mock engine, which spends nothing and invents its
findings. Pass --engine claude or --engine codex for a real review.
...
```

**`--engine` defaults to `mock`, and that default is deliberate.** This command
takes a *path* rather than a configured repository, so there is no stored engine
choice to honour — and a command that started spending your tokens because you
typed a directory name would be the wrong way for a default to be wrong. Pass
`--engine claude` or `--engine codex` for a real review. `revlocal watch` and the
desktop app use each repository's own configured engine and need no flag.

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
