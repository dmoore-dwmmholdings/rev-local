# 2. Andare MCP tool mapping

Date: 2026-08-27

## Status

Accepted

## Context

§11.4's Andare publish target has to bind to Andare's real tool names. SPEC §11.2
is explicit that those names are discovered from the running server rather than
assumed at build time, so the surface was observed against a live server before
anything was written against it.

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

Type mapping is 1:1 — every item type rev-local files (`epic`, `feature`, `story`,
`task`, `spike`) exists natively in Andare. No degradation was required.

Milestone is carried as a label (`M0` … `M14`, `all`), since Andare has no
milestone field. Sprints exist but are a scheduling construct, not a scope one.

## Consequences

- Labels cost one extra `update_issue` per item: `create_issue` has no label
  argument, so a label set is always a second call.
- A description trailer is the idempotency key. §11.5's dedupe rule means a
  re-publish must `search_issues` for the trailer before creating anything, or the
  same finding files a second issue.
- `set_issue_status` and `comment_on_issue` are both available, which is what
  RL-706 needs to report an outcome onto a linked work item without filing
  anything new.
