# Build state
- current_milestone: M1
- current_item: RL-103b
- item_status: in_progress
- last_gate_command: cargo test -p revlocal-core
- last_gate_result: PASS — exit 0. 13 tests + 3 doctests, 0 failed.
- last_visual: n/a
- next_action: RL-103b (REVL-108) — the domain structs: Repo, Cursor, Change, DiffStat,
    FileDiff, Run, Usage, Finding, Suppression, PublishAction, AuditEntry, BudgetLedgerEntry
- blocked_on: none
- adrs_open: none
- iterations_this_item: 1
- items_closed: [RL-101, RL-102]
- andare_connected: true

## Environment observed (2026-08-27)

| Tool | Status |
|---|---|
| `cargo` / `rustc` | 1.98.0 |
| `clippy`, `rustfmt` | installed this iteration via `rustup component add` |
| `claude` | on PATH |
| `codex` | **absent** — RL-408 and the Codex half of RL-1203 stay blocked |
| `svn` | **absent** — RL-202 and RL-E9 will need it installed |
| `framewatch` | on PATH |
| `git` | on PATH; repo initialised this iteration (it was not a git repo) |

## Known blockers

1. ~~Andare MCP not connected~~ — **resolved.** Andare is connected, the backlog is
   already imported as project `REVL` (RL-101 = `REVL-14`), and Andare is now the
   status source of truth. `RL-606` (discover Andare's tool surface) is unblocked.
2. `codex` is not installed. `RL-408` and the Codex half of `RL-1203` are blocked.
   Everything else proceeds — the mock engine covers the inner loop.
3. `svn` is not installed. Needed from `RL-202` onward; not yet blocking.
4. Framewatch in headless CI is unverified (`RL-1104`). GUI gates are loop-local.

## RL-103 was split

RL-103 covered 21 types across SPEC §3 and §5 — more than one iteration. Split per
BUILD_PROMPT: the enums and newtype ids landed as the first half; the structs are
**REVL-108 / RL-103b**, a subtask of REVL-16. REVL-16 stays `In Progress` and closes
when RL-103b lands, since its criterion "every type in SPEC §3 and §5 is represented"
is not yet true.

Landed: 14 enums (`RepoKind`, `EngineKind`, `AutonomyMode`, `ChangeKind`, `RunStatus`,
`Depth`, `TriggerSource`, `Severity`, `Category`, `FindingState`, `Capability`,
`PublishActionStatus`, `RiskClass`, `Verdict`) and 7 newtype ids, plus `ParseEnumError`
/ `DomainError`. See ADR 0004.

## ci_green_unobserved

`RL-102` acceptance criterion 2 — "all three OS legs green" — **cannot be observed from
this container**: `act` is not installed and there is no GitHub remote configured. What
was observed is that `.github/workflows/ci.yml` parses and asserts its own contract:
`crates/revlocal-cli/tests/ci_workflow.rs` (6 tests) checks the push/PR triggers, the
three-runner matrix with `fail-fast: false`, an `svn` installer per OS plus an
unconditional `svn --version` verification, the four gates in BUILD_LOOP §2 order, the
cargo cache, and the `if: failure()` upload of `artifacts/logs/**` + `artifacts/gui/*.png`.

**The legs stay unverified until the first push to a GitHub remote.** Re-check this
section then; if a leg is red, RL-102 reopens.

## live_engine_notes

None yet — no engine-live suite exists before M5.
