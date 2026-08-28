# 0018 — The prompt is a template, and the template escapes by default

- Status: accepted
- Date: 2026-08-27
- Item: RL-502 (REVL-47)
- Supersedes: none

## Context

SPEC §9.2 fixes seven prompt sections and their order, names the template path
(`crates/revlocal-engine/prompts/review.md.hbs`), and caps repo conventions at
`max_convention_bytes` (24 KB). The prompt's *wording* is an implementation choice, provided
the section order and the §8.2 output contract survive.

Two things had to be decided.

## Decision 1 — a real template, compiled in

Handlebars, at the path §9.2 names, `include_str!`-ed into the binary.

A template rather than Rust string-building because prompt wording is the thing most
likely to be tuned, and tuning it should not require a rebuild of the reviewer.

Compiled in rather than read from disk because a packaged desktop app has no
`crates/` directory beside the binary. A runtime read would pass every test in this
repository and fail on every install — the worst possible place to find out.

## Decision 2 — every interpolation is a triple-stache

Handlebars HTML-escapes `{{value}}`. **An escaped diff is a corrupted diff.** `<`
becomes `&lt;`, `&&` becomes `&amp;&amp;`, and the engine reviews plausible-looking
code that nobody wrote — then reports findings citing lines that do not exist. There
is no error, no warning, and nothing in the transcript that looks wrong.

So `{{{...}}}` everywhere, and `prompt_a_diff_is_not_html_escaped` guards it against
someone tidying the braces later. Flipping one triple-stache to a double fails that
test and only that test — verified this iteration.

The template's own comment tripped the adjacent version of this. `{{! ... }}` ends at
the **first** `}}`, so a comment explaining triple-stache syntax terminated inside its
own example and spilled the remainder of itself into every rendered prompt. The
escaping test caught it, which is the only reason it is written up here rather than
shipped. The template now uses `{{!-- --}}`, which tolerates `}}`.

## Decision 3 — where the §9.x byte limits live

`max_convention_bytes` (24 KB), `max_file_diff_bytes` (64 KB) and
`max_total_diff_bytes` (512 KB) are named with defaults in §9.2 and §9.4 prose and
were absent from both config documents — the fourth instance of this shape, after
`degraded`, `webhook_enabled` and `ignore_globs`.

All three go in `RepoConfig` (§13.2), not `[global]`: a monorepo with a 200 KB
`CONTRIBUTING.md` and a small service repo want different answers, and every other
review-shaping knob is already per-repo. SPEC §13.2's document is updated to match.

All three are added now rather than one at a time. `max_convention_bytes` is the one
this item needs; RL-504 needs the other two and is next. Adding one and rediscovering
the gap twice more is the mistake RL-1304 already recorded.

## Consequences

- Convention truncation is **stated inside the prompt** ("showing the first N bytes
  of M"), per §18. An engine shown two thirds of a style guide must not be left to
  treat it as the whole one — it would report the missing third as unwritten.
- The budget is shared across convention files and spent in §9.2's listed order, so a
  long `CONTRIBUTING.md` cannot crowd out the `CLAUDE.md` listed before it.
- Truncation cuts on a char boundary. Slicing a `String` by byte index mid-character
  panics, and an em dash in a contributing guide is entirely ordinary.
- A repository that states no conventions gets an explicit "do not invent any".
  Without it, every convention-scope (D8) finding is the engine's own taste reported
  as the repository's rule.
- `render_to` writes `prompt.md` beside the transcript (§9.2). This collides by name
  with `revlocal_engine::PROMPT_FILE`, which `CliEngine::run_with_repair` already
  writes; RL-503 wires the two together so the assembled prompt is the one sent,
  rather than leaving two writers of one filename.
