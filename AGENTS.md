# rev-local

Autonomous local code review for git, GitHub, and Subversion, powered by locally
installed Claude Code and Codex CLIs, publishing to GitHub / Andare / Trama over MCP.

## If you are an agent working on this repository

1. [`BUILD_PROMPT.md`](BUILD_PROMPT.md) — the loop instruction. Start here.
2. [`SPEC.md`](SPEC.md) — the authoritative build spec. Read only the sections your
   current work item references.
3. [`docs/backlog/BACKLOG.md`](docs/backlog/BACKLOG.md) — the 107-item work breakdown
   (`backlog.json` is the machine-readable form; Andare is the tracker once imported).
4. [`docs/BUILD_LOOP.md`](docs/BUILD_LOOP.md) — the full operating procedure.
5. `docs/STATE.md` — where the build currently is. If it doesn't exist, you are at
   `RL-101`; create it from the template in BUILD_LOOP.md §1.

Do not start coding before reading 1, 2 and 3.

## Ground rules

- One work item per iteration. Dependencies closed before an item is eligible.
- A gate has passed only if you ran it this iteration and saw exit 0.
- UI changes are verified with Framewatch captures you actually look at.
- Inner-loop tests never touch the network and never spend model tokens.
- Never mutate a repository under review — always work in a scratch worktree/export.
- `cargo clippy --workspace --all-targets -- -D warnings` is part of "it works".
