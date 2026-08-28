# 2. Andare MCP tool mapping

Date: 2026-08-27

## Status

Accepted

## Context

`docs/backlog/IMPORT.md` requires the Andare MCP tool surface be discovered, not
assumed, before importing the 107-item backlog.

## Decision

Mapping observed from the connected `andare` MCP server:

| Operation | Andare tool | Required args | Notes |
|---|---|---|---|
| create item | `create_issue` | `project`, `summary` | Also takes `type`, `priority`, `storyPoints`, `epic`, `description`, `assignee`, `sprint`, `parent` |
| set parent / link child | `create_issue` (`epic`) / `update_issue` (`epic`) | `key`, `epic` | `parent` is reserved for subtasks; `epic` files an item under a larger one |
| set issue type | `create_issue` (`type`) | `type` | Native types: initiative, epic, feature, story, bug, spike, chore, task, subtask |
| add label | `update_issue` | `key`, `labels` | REPLACES the list. `create_issue` has no label arg, so labels need a second call |
| set priority | `create_issue` / `update_issue` | `priority` | highest \| high \| medium \| low \| lowest |
| set estimate | `create_issue` (`storyPoints`) / `set_story_points` | `key`, `points` | Project scale: 0,1,2,3,5,8,13,21,34,55,89 |
| link dependency | `link_issues` | `key`, `target`, `type` | `blocked_by` used; links are symmetric and drive the critical path |
| comment | `comment_on_issue` | — | Available; not used by the import |
| search / list by field | `search_issues` | `aql` | AQL, e.g. `text ~ "rev-local-item: RL-101"` |
| transition status | `set_issue_status` | — | Available; not used by the import |

Type mapping is 1:1 — every backlog type (`epic`, `feature`, `story`, `task`,
`spike`) exists natively in Andare. No degradation was required.

Milestone is carried as a label (`M0` … `M14`, `all`), since Andare has no
milestone field. Sprints exist but are a scheduling construct, not a scope one.

## Consequences

- Labels cost one extra `update_issue` per item.
- The `rev-local-item: RL-xxx` description trailer remains the idempotency key;
  re-import must `search_issues` on it before creating.
- `andare_key` is now populated in `backlog.json`, and `gen_backlog.py` carries
  `andare_key`/`status` forward across regeneration.
