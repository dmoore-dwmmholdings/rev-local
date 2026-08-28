# 7. The in-repo config overlay is an allowlist

Date: 2026-08-27
Status: accepted
Item: RL-107b

## Context

SPEC §13.2 permits an in-repo `.rev-local.toml`:

> merged over the stored config (repo-local wins for scope/ignores, never for
> autonomy or targets — a repository must not be able to grant itself more
> authority)

This is a security boundary. `.rev-local.toml` is committed *inside the repository
under review*, so anyone who can open a pull request can propose changing it. If a
repo can raise its own autonomy, an attacker's first PR turns off the human in the
loop and the second one does whatever it likes.

The sentence contains two things: a rule (the parenthetical) and an example of the
rule (the clause before it). They do not have the same scope.

## Decision

**The overlay is an allowlist of five keys**, not a denylist of two:

```
scope, ignore_globs, ignore_authors, sensitive_globs, convention_files
```

Everything else in `RepoConfig` is refused with a typed
`ConfigError::FieldNotPermitted` naming the key.

The denylist reading — refuse `autonomy` and `targets`, permit the rest — fails in
the direction that costs something. A field added to `RepoConfig` next year is
*granted* to every repository by default, silently, by nobody's decision. An
allowlist fails the other way: a new field is refused until somebody looks at it,
and the refusal is visible because the user is told which key was ignored and why.

The test that matters here enumerates `RepoConfig`'s fields **from the type** and
asserts every non-permitted one is refused, so adding a field cannot leave the
policy stale.

### Two fields §13.2 does not name are treated as authority

`trama_publish` and `allow_approve` are refused. Neither appears in §13.2's
sentence, and both are authority in effect:

- `trama_publish` turns a low-risk draft into a high-risk `publish_page` (§12.3).
- `allow_approve` gates whether the app will ever submit a GitHub `APPROVE` review
  — which §10.2 calls "a stronger claim than the product should make unattended".

A repository able to set either can escalate what rev-local does on its behalf,
which is the thing the rule exists to prevent. Reading the sentence literally would
leave that open.

### Narrowing is per-field, not "repo-local wins"

"Repo-local wins" would let an overlay *remove* an operator's entry, which is a
widening however the field is labelled. Un-ignoring `**/vendor/**` makes rev-local
review and file issues on code it was told to leave alone. So:

- `scope` — **intersection**. A repo may drop a dimension, not add one.
- `ignore_globs`, `ignore_authors` — **union**. Add, never remove.
- `sensitive_globs` — **union**. Adding forces deeper review (§9.3); removing would
  make review shallower.
- `convention_files` — **union**. Reading more of a repo's conventions grants
  nothing.

The realistic mistake this protects against is not an attack: it is someone writing
`.rev-local.toml` listing only their own additions, and silently un-ignoring
everything the operator configured.

### Refusal is per key, and even the safe direction is refused

One forbidden key does not discard the file. Otherwise a single bad line costs the
user every legitimate setting, and the sane response becomes deleting the file.

`autonomy = "off"` is refused too, though it only ever reduces authority. Autonomy
is the operator's setting; a repo silently reviewing nothing because someone
committed that line is a failure the operator would have to debug from the outside.

## Spec change

SPEC §13.2's sentence is updated to state the rule as implemented, since it now
describes something narrower than the code does. The rule's intent is unchanged —
this widens what the intent protects.

## Consequences

- Adding a field to `RepoConfig` requires deciding whether a repository may set it.
  The default is no, and `every_repo_config_field_outside_the_allowlist_is_refused`
  fails until the decision is made explicitly.
- `RL-1205` (first-run onboarding) and `RL-1206` (operator docs) should say that
  `.rev-local.toml` can only narrow, so users do not write files expecting
  otherwise.
