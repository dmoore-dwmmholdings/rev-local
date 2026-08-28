# 0023 — `--json` owns stdout, and a missing rev is not a broken repository

- Status: accepted
- Date: 2026-08-27
- Item: RL-506b (REVL-117)
- Supersedes: none

## Context

RL-506a left two things: nothing implemented `VcsAdapter`, and the CLI had no
`review`. Both are assembly rather than new behaviour, but three choices inside them
are decisions.

## Decision 1 — under `--json`, exactly one document reaches stdout

A `--json` flag whose stdout carries a progress line is not machine-readable. A caller
piping to `jq` gets a parse error and reasonably concludes their own pipeline is
broken. So under `--json` the report is the only thing written to stdout, and
everything informational goes to stderr, where a pipe does not see it.

The notice that the review used the mock engine is a live example: it is genuinely
useful to a person and fatal to a parser.

`review_json_sends_informational_output_to_stderr_only` asserts both halves — absent
from `--json` stdout **and** present on the human path's stderr. Without the second
assertion the first would pass just as well if the message had been deleted, which
would test nothing.

## Decision 2 — two renderers, not one renderer with a flag

`render_human` formats for a person; the JSON path serializes `ReviewReport` and does
not touch it. One shared path would eventually round a number or trim a string "for
readability" and silently break the byte-stability RL-506a's criterion depends on.

## Decision 3 — a missing rev is `NoSuchChange`, not `CommandFailed`

A rev that does not resolve is the caller naming something that is not there. A caller
branches on the variant, and collapsing it into a generic command failure makes "no
such commit" indistinguishable from "your repository is broken" — different messages,
different remediation, different UI.

The error mapping is deliberately **not** a blanket `From<GitError>`: `VcsError` names
the repository in most variants and `GitError` cannot know one. §18 requires a
user-visible error to say what to do, and "`git rev-parse` failed" without naming the
repository is not actionable for a user with eleven of them.

## The negative probe that found a broken feature

Three probes were run. The third — deleting the `NoSuchChange` mapping — **changed no
test**. Per RL-506a's rule, I checked which kind of miss it was before moving on.

It was both, in the worst combination:

- The CLI test asserted only that the error "names the rev". Git's own stderr contains
  the sha, so it passed whether the mapping existed or not.
- **The mapping never fired anyway.** Its three patterns — `unknown revision`,
  `bad revision`, `not a valid object name` — were guessed. `git worktree add`
  actually says `invalid reference`.

So a feature that had a test, a doc comment and a passing suite did nothing at all.
The only reason it surfaced is that the probe was run and its silence was
investigated rather than assumed benign.

Fixed by reading git's real output, adding `invalid reference`, and making both tests
discriminate: the CLI now requires the classified wording and rejects a leaked `git
worktree add`, and `git_adapter_a_missing_rev_is_no_such_change_not_a_command_failure`
asserts the variant directly. Re-probing now fails both.

**This is the fourth time in M6 that guessing a tool's output text has produced a
silently dead code path** (RL-505's validator violations, RL-506a's
`GIT_ALLOW_PROTOCOL` wording, RL-506a's fixture file paths, and this). The rule is now
as a standing rule: never write a string match against another tool's output without
running the tool and reading what it says.

## Consequences

- `GitAdapter` adds **no VCS logic**. A second place deciding what a change is would
  eventually disagree with the first.
- Each branch keeps its own cursor in `discover`; a shared one would let a commit on a
  quiet branch be skipped by activity on a busy one.
- `probe` collects **every** problem, not the first: telling a user their repo is
  broken one reason at a time turns one fix into three round trips. It also reports a
  repo whose `branches` patterns match nothing, which would otherwise sit there
  silently never being reviewed.
- An unparseable `config_json` falls back to §13.2's defaults rather than failing the
  review. Reviewing with the defaults beats reviewing nothing; the layer that owns the
  repo row surfaces the parse warnings.
- `install_hooks` **refuses** `Install`/`Uninstall` rather than no-opping. A user who
  runs `install`, sees no error and is not protected is worse off than one told it is
  not built yet. `Verify` honestly reports "not installed".
- `revlocal review` takes a path rather than a configured repo, so it works against
  anything without registration — which is what makes it a debugging tool. The repo
  *name* (not path) reaches the fingerprint, so moving a checkout does not make every
  finding look new.
