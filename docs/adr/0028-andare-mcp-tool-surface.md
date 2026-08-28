# 28. Andare's real MCP tool surface, and what it means for §11.4

Date: 2026-08-28

## Status

Accepted

## Context

SPEC §11.4 files findings into Andare as issues. §11.2 is explicit that tool names
are discovered rather than assumed, but the *default* candidate lists and argument
templates still have to be written against something. RL-606 connected to a live
Andare MCP server and captured `tools/list`.

## Decision

### The surface

27 tools. The ones §11.4 and §11.5 need:

| Capability | Tool | Required args | Notable optional args |
|---|---|---|---|
| file a finding | `create_issue` | `project`, `summary` | `description`, `type`, `priority`, `storyPoints`, `epic`, `parent`, `assignee`, `sprint` |
| transition | `set_issue_status` | `key`, `status` | — |
| comment | `comment_on_issue` | `key`, `body` | — |
| dedupe search | `search_issues` | — | `aql`, `project`, `limit` |
| read one | `get_issue` | `key` | — |
| edit fields | `update_issue` | `key` | `summary`, `description`, `labels`, `priority`, `epic`, `sprint`, `assignee`, dates, `components` |
| link | `link_issues` | `key`, `target` | `type` (`blocks`, `blocked_by`, `relates`, `duplicates`) |
| estimate | `set_story_points` | `key`, `points` | — |
| record a review | `submit_review` | `key`, `verdict`, `summary` | `body` |

The rest — sprints, ceremonies, cancellations, task claiming, attachments read —
are workspace-management tools rev-local has no reason to call.

### SPEC §11.2's example argument names do not match Andare

The spec's illustrative template is:

```toml
args = { title = "{finding.title}", body = "{finding.body_md}", project = "..." }
```

Andare's `create_issue` takes **`summary`**, not `title`, and **`description`**,
not `body`. `project` is correct. The example in §11.2 is illustrative rather than
normative, so the spec is not wrong — but anything copied from it verbatim would
fail.

**It would fail at `targets list`, not at publish time**, because RL-604 validates
rendered arguments against the tool's own schema before calling. This is the first
real-world instance of that check earning itself.

### Every capability §11.4 needs is expressible

`create_issue`, `set_status`, `comment` and `search` all bind with no manual
override required. Two constraints carry forward from ADR 0002 and are confirmed:

- **`create_issue` has no `labels` argument.** Labels cost a second call to
  `update_issue`, whose `labels` argument *replaces* the list rather than adding to
  it — so a label set is read-modify-write, not append.
- **There is no attachment-creation tool.** `list_attachments` and
  `read_attachment` exist; nothing writes one. A finding that wants to carry a
  patch or a diff file must inline it in `description` or link out. This is a
  genuine capability gap rather than a naming problem, so no manual override can
  fix it.

### Built-in candidates

`capability::builtin_target("andare")` carries the real names first and the
plausible alternatives after, so a differently-named server still resolves:

```
create_issue  -> ["create_issue", "create_work_item", "issue_create", "create_ticket"]
set_status    -> ["set_issue_status", "update_issue", "transition_issue"]
comment       -> ["comment_on_issue", "add_comment", "create_comment"]
search        -> ["search_issues", "search", "find_issues"]
```

`set_status` lists `set_issue_status` before `update_issue` deliberately: Andare
has both, `update_issue` does not accept a status, and candidate order is priority
order.

## Consequences

- RL-705 can be written against these names without a discovery round trip during
  development, while still resolving whatever the user's server reports.
- The attachment gap needs a decision in RL-705: inline, link out, or don't offer
  it. Recorded rather than resolved here.
- `submit_review` exists, so a review verdict is expressible if §11.4 ever wants
  one. Not currently in scope.
