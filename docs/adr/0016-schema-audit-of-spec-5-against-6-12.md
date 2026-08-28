# 16. Schema audit of SPEC §5 against §§6–12

Date: 2026-08-27
Status: accepted
Item: RL-1304

## Context

Three amendments to §5 had already been made, each found the same way — while
implementing something else, one at a time:

| Amendment | Item | Required by |
|---|---|---|
| `run.degraded` | RL-103b | §8.1, §12.3 |
| `budget_ledger.cost_complete` | RL-109c | §8.1, D10 |
| `repo.github_transport` | RL-307 | §6.3 |

Each cost a migration, a `cargo sqlx prepare`, a SPEC edit and an ADR **after** the
surrounding code was written. §5's DDL was drafted before §§6–12 settled, so the
remaining gaps were a known quantity of unknown size. Finding them one at a time
was the expensive way, and it got more expensive with every new caller of the store.

## What the audit found

Five columns, added in one migration (`0004`). §5 is updated in the same commit.

**`run.truncated` and `run.omitted_files_json`** — §18 is explicit: *"Wherever the
system truncates, samples, or drops, it records the fact **on the run** and shows it
in the UI. A review that saw 60% of the diff must never look like a review that saw
all of it."* `ChangeContext` carried this in memory, where it died with the process.
The list is stored **in full** because §9.4 says truncation must never silently hide
a file — a count would not satisfy that.

**`run.verdict` and `run.summary`** — neither is derivable after the fact. The
verdict is a *historical fact*: what was posted. Recomputing it from findings would
change retroactively as findings are suppressed or superseded, so a run that
requested changes would silently become one that approved and the audit trail would
disagree with what GitHub shows. The summary is the engine's own prose and exists
nowhere else once retention prunes the transcript at 30 days (§5.1).

**`publish_action.next_attempt_at`** — §11.6 specifies exponential backoff, and
`attempts` alone cannot say *when* the next attempt is due. Without it every pending
action becomes due the instant the process restarts, defeating backoff at exactly
the moment it matters: a restart often follows the burst of failures that caused it.

`Run::is_consistent` now also rejects a truncated run with an empty omitted list.
Claiming something was dropped without saying what is worse than not claiming it —
the UI would show a truncation warning with nothing behind it.

## Also found: a config gap, not a schema gap

§7.3 requires the webhook listener to be **off by default with explicit opt-in per
repo**, and to validate signatures against a **per-repo secret**. `RepoConfig` had
neither field. Added `webhook_enabled` (default `false`) and `webhook_secret_ref` —
a *keychain reference*, never the secret, since §13.1 says secrets are never in this
file. §13.2's document is updated to match, and `RL-107`'s defaults test parses that
document, so the two cannot drift.

## Deliberately not added

Recording these so the next audit does not re-litigate them:

- **An approval's decider and decision time.** §12.4 says approvals are "always
  audited", and the `audit` table already records actor, time and detail. A second
  copy on `publish_action` would be a place for the two to disagree.
- **The approval expiry deadline.** Derivable: `created_at + approval_ttl_hours`.
  Storing it would freeze a config value that a user may change.
- **The engine's resolved version per run.** Genuinely useful for explaining a
  finding that no longer reproduces, and genuinely *not required* by any section.
  Left out because the audit's job is to close gaps the spec opens, not to
  speculate. If `RL-1203` wants it, that is a one-column migration with a reason.

## Consequences

- Whoever writes the truncation logic (`RL-309`) and the publish queue (`RL-702`)
  finds the columns already there. That was the point of doing this now.
- Four migrations exist; the down path reverses all four, and the test asserts it.
