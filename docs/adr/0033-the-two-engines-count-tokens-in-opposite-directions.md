# 0033 — The two engines count tokens in opposite directions

**Status:** accepted
**Context:** SPEC §8.1, §8.2, §8.4; RL-408, RL-409

## What was investigated

`codex` 0.151.0, non-interactively, against the questions RL-408 asked: the exec
invocation, whether the sandbox blocks writing to `out_dir` outside `--cd`, what
`--json` actually emits, and whether SPEC §8.4's default template is right.

Everything below was observed by running the CLI, not read from documentation.

## Finding 1 — the two CLIs report cache tokens with opposite meanings

This is the important one, and it is a trap in both directions.

**Claude reports additive buckets.** From a captured one-sentence prompt:

```
input_tokens                2
cache_creation_input_tokens 8453
cache_read_input_tokens     10143
```

`input_tokens` is the *non-cached remainder*. Reading it alone records 2 tokens for
a call that processed 18,598 — a 99.99% undercount, and a token ceiling that never
fires.

**Codex reports an inclusive total with a breakdown.**

```
input_tokens        35945
cached_input_tokens 28160
```

`cached_input_tokens` is part of the 35,945, not additional to it. Summing them
double-counts 28,160 tokens.

**So one extractor per engine, never a shared one.** A single function cannot be
right for both, and being wrong is silent in either direction: one budget never
fires, the other fires early.

Two pieces of evidence for the Codex reading, because one number would be a guess.
`reasoning_output_tokens` (69) sits inside `output_tokens` (230) — a subset by
definition, which establishes the convention. And across two runs with very
different prompt sizes, `input_tokens` exceeded `cached_input_tokens` both times
while tracking total context; additive buckets would not behave that way.

## Finding 2 — the sandbox does not block `out_dir` outside `--cd`

The spike anticipated this and asked for a fix if it were true. It is not, in this
version: with `--sandbox workspace-write --cd <work>`, Codex wrote to a path under
`/tmp` **and** to one under `$HOME`, both outside the working root.

So `out_dir` stays where it is. Moving it inside `cwd` would have meant a review
writing into the tree it is reviewing, which §6.1 exists to prevent — the fix would
have been worse than the problem, and it is fortunate it was not needed.

Worth re-testing when Codex updates: this is permissive behaviour, and permissive
behaviour is the kind that gets tightened.

## Finding 3 — `--json` is JSONL, not a document

Claude's `--output-format json` emits one JSON object. Codex's `--json` emits a
stream of events: `thread.started`, `turn.started`, `item.started`,
`item.completed`, `turn.completed`.

Usage lives on `turn.completed`, and a session can have several. **The last one
wins** — it carries the cumulative figure, and taking the first reports the opening
turn as though it were the run.

A non-JSON line in the stream is skipped rather than fatal. JSONL in practice picks
up banners and progress lines, and losing a run's counts to one would record a
measured run as unmeasured.

## Finding 4 — Codex reports no price

There is no `total_cost_usd` equivalent. `cost_usd` is `None`, not a computed
figure: ADR 0010 keeps an unmeasured cost from reading as a free one, and
arithmetic over rates hard-coded in this crate would go stale silently the next
time pricing moved.

Claude does report one, exactly, and that is used as-is.

## Finding 5 — SPEC §8.4's template was already right

```toml
args = ["exec", "--json", "--sandbox", "workspace-write", "--cd", "{cwd}", "{prompt_file_content}"]
```

Every flag exists and behaves as the template assumes. No change needed, which is
worth recording: a spike that changes nothing has still established that the thing
it examined is correct, and that is not the same as not having looked.

Two flags are worth knowing about for later and are deliberately not adopted now:
`--output-schema <FILE>` constrains the model's final response to a JSON Schema,
which may be a better route to §8.3's `result.json` than the §8.2 ladder; and
`-o/--output-last-message <FILE>` writes the final message straight to a file.
Both would change the output contract, so they belong to their own decision.

## Consequence

`revlocal-engine`'s `usage` module gains `from_codex_jsonl` beside
`from_claude_json`, each tested against a captured real payload. Both engines are
now `Measured`, so `revlocal doctor` no longer warns that a token budget is
advisory for either.
