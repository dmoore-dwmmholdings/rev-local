# Prompt: import the rev-local backlog into Andare

Run this **once**, before starting the build loop, in a session where the Andare MCP
server is connected. It is idempotent — re-running it will not duplicate items.

---

## Task

Import every item in `docs/backlog/backlog.json` into the Andare project for rev-local,
preserving the hierarchy and dependencies, and write the resulting Andare keys back
into `backlog.json`.

## Step 1 — discover, do not assume

You do **not** know Andare's tool names. Find them:

1. List the MCP tools available to you. Identify the Andare server.
2. Read each candidate tool's input schema before calling it.
3. Build a mapping table for these operations and write it to
   `docs/adr/0002-andare-tool-mapping.md`:

   | Operation | Andare tool | Required args | Notes |
   |---|---|---|---|
   | create item | | | |
   | set parent / link child | | | |
   | set issue type (epic/story/spike/task) | | | |
   | add label | | | |
   | set priority | | | |
   | set estimate | | | |
   | link dependency (blocks / blocked-by) | | | |
   | comment | | | |
   | search / list by field | | | |
   | transition status | | | |

4. If an operation has no corresponding tool, record it as **unavailable** and say how
   you will degrade (e.g. dependencies expressed as a line in the description rather
   than a real link). Do not invent a tool name and do not call a tool whose schema you
   have not read.

## Step 2 — check for a prior import

Search Andare for an existing item whose description contains `rev-local-item: RL-`.
If any exist, this is a re-run: update those items in place rather than creating new
ones, and only create items whose `RL-` id is absent.

## Step 3 — create, parents first

Create in this order so every parent exists before its children:

1. All 13 epics (`type: epic`).
2. All features and spikes.
3. All stories and tasks, each parented to its epic.

For each item, set:

- **Title** — `[RL-xxx] <title>`
- **Type** — map `epic|feature|story|task|spike` onto Andare's nearest issue type;
  record the mapping in the ADR.
- **Priority**, **estimate**, **labels** — from the JSON. Always add the label
  `rev-local` plus the milestone label (`M0` … `M14`).
- **Description** — this exact structure:

```markdown
<description from JSON>

## Acceptance criteria
- [ ] …one line per criterion…

## Gate
`<gate command>`

## Spec
SPEC.md <spec_refs, comma separated>

## Depends on
RL-xxx, RL-yyy      ← omit the section if empty

---
rev-local-item: RL-xxx
```

The `rev-local-item:` trailer is the idempotency key. Every item must carry it.

## Step 4 — dependencies

Once every item exists, add the dependency links from each item's `depends_on`. If
Andare has no dependency link type, leave the `## Depends on` section in the
description and note the degradation in the ADR.

## Step 5 — write back

Update `docs/backlog/backlog.json`, setting `andare_key` on each item to the key
Andare returned. Then regenerate the markdown:

```bash
python3 scripts/gen_backlog.py   # NOTE: teach it to preserve andare_key first
```

`gen_backlog.py` currently regenerates `andare_key: null`. Before running it after an
import, modify it to read any existing `backlog.json` and carry `andare_key` and
`status` forward. Do this as part of the import task — otherwise the first regeneration
silently discards every key.

## Step 6 — report

Print a table: item count created, updated, skipped; every operation you found
unavailable; and the top-level epic keys, so the build loop can be pointed at them.

## Do not

- Do not create anything outside the rev-local project.
- Do not close, transition, or delete any pre-existing Andare item.
- Do not proceed past Step 1 if you cannot identify the Andare server — stop and say so.
