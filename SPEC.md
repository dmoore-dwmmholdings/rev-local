# rev-local — Specification v1.0

**Status:** authoritative build spec. This repository is implemented end-to-end by
working the milestone list in §17 in order, running the acceptance tests after each
one, and not advancing until they pass.

**Date:** 2026-08-27

---

## 1. Purpose

`rev-local` is a cross-platform desktop application that performs **autonomous code
review of every change landing in a repository**, using **locally installed AI coding
CLIs** (Claude Code and OpenAI Codex) as the review engines, and **publishes the
results into the team's existing systems of record** — GitHub, Andare (issue tracker),
and Trama (documentation wiki) — over MCP.

You point it at a repository. It watches. Every new commit, pull request, or
Subversion revision gets reviewed by a real coding agent with real repository
context, and the outcome shows up where the team already works: as a PR review on
GitHub, as an issue in Andare, as a written review page in Trama.

### 1.1 Design principles

1. **Local-first.** Source never leaves the machine except through the publish
   targets the user explicitly configured. The review engines are the user's own
   already-authenticated CLIs; `rev-local` stores no model credentials.
2. **Nothing is lost.** Every review run, every finding, every publish attempt and
   its response is recorded in a local SQLite database. Runs are resumable and
   replayable.
3. **The remote is never surprised.** Every outbound write is gated by an autonomy
   mode, is idempotent, is attributed, and is recorded in an audit log with a
   receipt.
4. **Degrade, don't stall.** A missing `svn` binary, an offline MCP server, an
   exhausted budget, or a 400-file diff each produce a *degraded but complete*
   outcome, never a hang and never a silent drop.
5. **Headless-equivalent.** Everything the GUI can do, the `revlocal` CLI can do.
   This is a hard architectural requirement — it is what makes the product
   testable end to end without a display.

### 1.2 Non-goals (v1)

- Hosting a shared team service. `rev-local` is single-user, on one machine.
- Authoring fixes / opening PRs with patches. v1 reviews; it does not write code.
- Replacing CI. `rev-local` may *report* a check status, but is not a build system.
- Supporting Mercurial, Perforce, or Azure DevOps.
- Model API access without a CLI. If `claude`/`codex` are not installed, the app
  reports a missing prerequisite; it does not fall back to raw HTTP.

---

## 2. Decisions of record

These were settled with the product owner and are **not open for reinterpretation**
during implementation.

| # | Decision | Value |
|---|---|---|
| D1 | Application shell | **Tauri v2**, Rust core, React + TypeScript UI |
| D2 | Triggers | **All four**: interval polling, local git hooks, GitHub webhooks via tunnel, manual/backfill |
| D3 | Engines | **Claude Code and Codex both supported; selected per repository** (not run in parallel by default) |
| D4 | Autonomy | **Fully autonomous with a global kill switch**, *plus* a selectable **`auto-low / ask-high`** mode modeled on Claude Code / Codex permission modes |
| D5 | Andare | **Issue & work tracker.** Findings become issues; review status moves the linked ticket. Trama receives the written review. |
| D6 | SVN unit | **Per-revision review, plus a synthesized branch-level "pseudo-PR" review when a branch is reintegrated to trunk** |
| D7 | Storage | **SQLite** — repos, runs, findings, publish receipts, full audit log |
| D8 | Default review scope | **Correctness, security, repo-convention/architecture drift, test coverage of the change** |
| D9 | Platforms | **macOS, Windows, Linux** — all three from day one. Engines authenticate via the **user's existing CLI logins**; the app stores no API keys. |
| D10 | Cost control | **Per-repo token/run budgets + global concurrency cap**, with skip rules and pause-on-exhaustion (never silent drop) |
| D11 | Build method | **Numbered milestones with executable acceptance tests** and offline git+svn fixture repositories |
| D12 | Name | `rev-local`; Rust workspace `rev-local`, CLI binary **`revlocal`**, desktop app **rev-local** |

---

## 3. Glossary

- **Change** — the atomic thing being reviewed. Exactly one of: a git commit, a
  GitHub pull request (at a specific head SHA), an SVN revision, or an SVN
  pseudo-PR (a synthesized branch merge diff).
- **Review run** — one execution of the pipeline against one Change. Has a status
  lifecycle and produces zero or more Findings.
- **Finding** — one reviewer observation: file, line, severity, category, claim,
  failure scenario, optional suggested fix.
- **Engine** — a local AI CLI that performs the review (`claude`, `codex`).
- **Publish target** — an outbound system of record (GitHub, Andare, Trama).
- **Capability** — an abstract publish operation (`post_review`, `create_issue`,
  `set_status`, `upsert_doc`, `set_check`) that a target maps onto concrete tools.
- **Autonomy mode** — how much a run may do without a human: `off`, `dry_run`,
  `auto_low_ask_high`, `auto`.
- **Risk class** — `low` or `high`, computed per publish *action* (§12.3). Decides
  whether `auto_low_ask_high` posts or queues.

---

## 4. Architecture

### 4.1 Repository layout

```
rev-local/
├── Cargo.toml                  # workspace
├── crates/
│   ├── revlocal-core/          # domain types, config, errors, risk model. No I/O.
│   ├── revlocal-store/         # SQLite (sqlx), migrations, repositories, audit log
│   ├── revlocal-vcs/           # VcsAdapter trait + git, github, svn implementations
│   ├── revlocal-engine/        # Engine trait + claude, codex, mock implementations
│   ├── revlocal-mcp/           # MCP client (stdio + streamable HTTP), tool discovery
│   ├── revlocal-publish/       # PublishTarget trait + github, andare, trama, generic
│   ├── revlocal-daemon/        # scheduler, trigger sources, run orchestrator, budgets
│   ├── revlocal-cli/           # `revlocal` binary — full headless surface
│   └── revlocal-tauri/         # Tauri v2 shell; thin — commands delegate to daemon
│       └── ui/                 # React + TS + Vite
├── fixtures/                   # offline git & svn fixture generators (§16.2)
├── docs/
│   └── adr/                    # one ADR per non-obvious decision made during build
└── SPEC.md                     # this file
```

**Rule:** `revlocal-core` has no I/O dependencies (no tokio, no sqlx, no reqwest).
Every other crate may depend on it. This keeps the domain model unit-testable and
is enforced by an acceptance test in M1.

### 4.2 Process model

```
┌──────────────────────────────────────────────────────────────────┐
│ Tauri shell (src-tauri)          │  revlocal CLI (revlocal-cli)  │
│  · window, tray, notifications   │  · same commands, no window   │
│  · IPC commands → Daemon         │  · direct → Daemon            │
└────────────────┬─────────────────┴───────────────┬───────────────┘
                 └───────────────┬─────────────────┘
                        ┌────────▼─────────┐
                        │ Daemon (in-proc) │  tokio runtime
                        │  · Scheduler     │
                        │  · TriggerBus    │
                        │  · RunQueue      │
                        │  · BudgetGuard   │
                        │  · EventBus ─────┼──► UI (Tauri events) / CLI (stdout)
                        └────────┬─────────┘
        ┌────────────┬───────────┼────────────┬──────────────┐
   ┌────▼────┐  ┌────▼────┐ ┌────▼─────┐ ┌────▼────┐  ┌──────▼──────┐
   │  VCS    │  │ Engine  │ │  Store   │ │ Publish │  │   MCP       │
   │ adapters│  │ runners │ │ (SQLite) │ │ targets │  │  client     │
   └─────────┘  └─────────┘ └──────────┘ └─────────┘  └─────────────┘
```

The Daemon runs **in-process** inside both the Tauri app and the CLI. There is no
separate background service in v1; the app must be running to review. (A future
`revlocal serve` daemon mode is explicitly deferred but the crate boundary must not
foreclose it.)

### 4.3 Concurrency model

- One tokio multi-threaded runtime.
- The **RunQueue** is the only thing that spawns engine processes. It enforces
  `global.max_concurrent_runs` (default 2) with a semaphore.
- Trigger sources are independent tasks that push `ChangeDetected` events onto the
  TriggerBus; they never execute reviews themselves.
- Publishing happens on a separate bounded queue with its own concurrency of 4 and
  per-target rate limiting, so a slow MCP server cannot block reviewing.
- All long-running work is cancellable via `CancellationToken`; the global kill
  switch (§12.1) cancels every token and drains the queues.

---

## 5. Data model

SQLite via `sqlx` with compile-time-checked queries and versioned migrations in
`crates/revlocal-store/migrations/`. WAL mode on. `foreign_keys = ON`.

```sql
-- 0001_init.sql

CREATE TABLE repo (
  id                INTEGER PRIMARY KEY,
  name              TEXT NOT NULL UNIQUE,
  kind              TEXT NOT NULL CHECK (kind IN ('git','github','svn')),
  local_path        TEXT,                    -- working copy / clone / mirror
  remote_url        TEXT,                    -- origin, GitHub URL, or svn root URL
  default_branch    TEXT,                    -- git: main; svn: trunk path
  engine            TEXT NOT NULL DEFAULT 'claude'
                      CHECK (engine IN ('claude','codex','mock')),
  autonomy          TEXT NOT NULL DEFAULT 'auto_low_ask_high'
                      CHECK (autonomy IN ('off','dry_run','auto_low_ask_high','auto')),
  enabled           INTEGER NOT NULL DEFAULT 1,
  config_json       TEXT NOT NULL DEFAULT '{}',   -- RepoConfig (§13.2)
  github_transport  TEXT                          -- §6.3 ladder result; NULL = not probed
                      CHECK (github_transport IN ('mcp','gh_cli','unauthenticated')),
  created_at        TEXT NOT NULL,
  updated_at        TEXT NOT NULL
);

CREATE TABLE cursor (                        -- what we have already seen
  repo_id           INTEGER NOT NULL REFERENCES repo(id) ON DELETE CASCADE,
  scope             TEXT NOT NULL,           -- 'commits:<branch>' | 'prs' | 'svn:<path>'
  value             TEXT NOT NULL,           -- sha | pr updated_at | revision number
  updated_at        TEXT NOT NULL,
  PRIMARY KEY (repo_id, scope)
);

CREATE TABLE change (
  id                INTEGER PRIMARY KEY,
  repo_id           INTEGER NOT NULL REFERENCES repo(id) ON DELETE CASCADE,
  kind              TEXT NOT NULL CHECK (kind IN ('commit','pr','svn_rev','svn_pseudo_pr')),
  external_id       TEXT NOT NULL,           -- sha | pr#:headsha | r1234 | branch@r1234
  title             TEXT,
  author_name       TEXT,
  author_email      TEXT,
  authored_at       TEXT,
  branch            TEXT,
  base_ref          TEXT,                    -- pr base sha / svn merge base
  head_ref          TEXT,
  url               TEXT,                    -- web URL if known
  diff_stat_json    TEXT NOT NULL DEFAULT '{}',  -- {files,insertions,deletions}
  detected_at       TEXT NOT NULL,
  UNIQUE (repo_id, kind, external_id)
);

CREATE TABLE run (
  id                INTEGER PRIMARY KEY,
  change_id         INTEGER NOT NULL REFERENCES change(id) ON DELETE CASCADE,
  attempt           INTEGER NOT NULL DEFAULT 1,
  status            TEXT NOT NULL CHECK (status IN
                      ('queued','preparing','reviewing','synthesizing','publishing',
                       'awaiting_approval','done','failed','skipped','cancelled')),
  engine            TEXT NOT NULL,
  depth             TEXT NOT NULL CHECK (depth IN ('summary','standard','deep')),
  trigger           TEXT NOT NULL CHECK (trigger IN ('poll','hook','webhook','manual','backfill','retry')),
  skip_reason       TEXT,
  error             TEXT,
  degraded          TEXT,                    -- why output was salvaged (§8.2); NULL = clean
  truncated         INTEGER NOT NULL DEFAULT 0,  -- §9.4/§18: the diff was reduced
  omitted_files_json TEXT,                   -- §9.4: the omitted list, in full
  verdict           TEXT                     -- §10.2; the verdict as posted, not recomputed
                      CHECK (verdict IN ('approve','comment','request_changes')),
  summary           TEXT,                    -- §8.3 engine summary, <= 1200 chars
  tokens_in         INTEGER NOT NULL DEFAULT 0,
  tokens_out        INTEGER NOT NULL DEFAULT 0,
  cost_usd          REAL,
  started_at        TEXT,
  finished_at       TEXT,
  transcript_path   TEXT,                    -- raw engine stdout, on disk
  created_at        TEXT NOT NULL,
  UNIQUE (change_id, attempt)
);

CREATE TABLE finding (
  id                INTEGER PRIMARY KEY,
  run_id            INTEGER NOT NULL REFERENCES run(id) ON DELETE CASCADE,
  fingerprint       TEXT NOT NULL,           -- stable dedupe key (§10.3)
  severity          TEXT NOT NULL CHECK (severity IN ('critical','high','medium','low','info')),
  category          TEXT NOT NULL,           -- correctness|security|convention|tests|perf|other
  confidence        REAL NOT NULL DEFAULT 0.5,
  file              TEXT,
  line_start        INTEGER,
  line_end          INTEGER,
  title             TEXT NOT NULL,           -- <= 80 chars
  body              TEXT NOT NULL,           -- markdown: claim
  failure_scenario  TEXT,                    -- concrete inputs -> wrong behaviour
  suggested_fix     TEXT,
  state             TEXT NOT NULL DEFAULT 'open'
                      CHECK (state IN ('open','published','suppressed','superseded','resolved')),
  created_at        TEXT NOT NULL
);
CREATE INDEX idx_finding_fp ON finding(fingerprint);

CREATE TABLE suppression (                   -- user says "never tell me this again"
  id                INTEGER PRIMARY KEY,
  repo_id           INTEGER REFERENCES repo(id) ON DELETE CASCADE,
  fingerprint       TEXT,
  glob              TEXT,                    -- or path-glob based suppression
  reason            TEXT,
  created_at        TEXT NOT NULL
);

CREATE TABLE publish_action (
  id                INTEGER PRIMARY KEY,
  run_id            INTEGER NOT NULL REFERENCES run(id) ON DELETE CASCADE,
  finding_id        INTEGER REFERENCES finding(id) ON DELETE SET NULL,
  target            TEXT NOT NULL,           -- 'github' | 'andare' | 'trama' | custom
  capability        TEXT NOT NULL,           -- post_review|create_issue|set_status|upsert_doc|set_check|comment
  risk              TEXT NOT NULL CHECK (risk IN ('low','high')),
  idempotency_key   TEXT NOT NULL,
  payload_json      TEXT NOT NULL,
  status            TEXT NOT NULL CHECK (status IN
                      ('pending','awaiting_approval','approved','rejected','sent','failed','skipped_dry_run')),
  attempts          INTEGER NOT NULL DEFAULT 0,
  next_attempt_at   TEXT,                    -- §11.6 backoff; survives a restart
  response_json     TEXT,
  external_ref      TEXT,                    -- issue key, PR review id, page id/url
  error             TEXT,
  created_at        TEXT NOT NULL,
  sent_at           TEXT,
  UNIQUE (target, idempotency_key)
);

CREATE TABLE audit (
  id                INTEGER PRIMARY KEY,
  at                TEXT NOT NULL,
  actor             TEXT NOT NULL,           -- 'daemon' | 'user' | 'engine:claude'
  kind              TEXT NOT NULL,           -- event name
  repo_id           INTEGER,
  run_id            INTEGER,
  detail_json       TEXT NOT NULL
);
CREATE INDEX idx_audit_run ON audit(run_id);
CREATE INDEX idx_audit_at  ON audit(at);

CREATE TABLE budget_ledger (
  id                INTEGER PRIMARY KEY,
  repo_id           INTEGER NOT NULL REFERENCES repo(id) ON DELETE CASCADE,
  day               TEXT NOT NULL,           -- YYYY-MM-DD local
  runs              INTEGER NOT NULL DEFAULT 0,
  tokens_in         INTEGER NOT NULL DEFAULT 0,
  tokens_out        INTEGER NOT NULL DEFAULT 0,
  cost_usd          REAL NOT NULL DEFAULT 0,   -- sum of the costs actually reported
  cost_complete     INTEGER NOT NULL DEFAULT 1, -- 0 if any run reported no cost (§18)
  UNIQUE (repo_id, day)
);
```

> **Implementation note:** this DDL is normative. If you must deviate (a column
> type, an added index, a split table), do it — but record why in `docs/adr/` and
> update this section in the same commit, so the spec never drifts from the code.

### 5.1 Retention

- `run.transcript_path` files older than `global.transcript_retention_days`
  (default 30) are pruned on startup.
- `run`/`finding` rows are never auto-deleted in v1. `revlocal db vacuum --before <date>`
  is the manual escape hatch.

---

## 6. VCS layer

### 6.1 The trait

```rust
#[async_trait]
pub trait VcsAdapter: Send + Sync {
    fn kind(&self) -> RepoKind;

    /// Cheap liveness/config check. Never mutates.
    async fn probe(&self, repo: &Repo) -> Result<ProbeReport>;

    /// Everything new since `cursor`, oldest-first, bounded by `limit`.
    async fn discover(&self, repo: &Repo, cursor: Option<&Cursor>, limit: usize)
        -> Result<Vec<DetectedChange>>;

    /// Full reviewable context for one change.
    async fn materialize(&self, repo: &Repo, change: &Change, into: &Path)
        -> Result<ChangeContext>;

    /// Install/verify local trigger integration (git hooks). No-op for others.
    async fn install_hooks(&self, repo: &Repo, mode: HookMode) -> Result<HookReport>;
}

pub struct ChangeContext {
    pub worktree: PathBuf,       // checked-out state AT the change
    pub diff_unified: String,    // full unified diff, base..head
    pub diff_files: Vec<FileDiff>,
    pub message: String,         // commit message / PR body / svn log msg
    pub parents: Vec<String>,
    pub stat: DiffStat,
    pub truncated: bool,         // true if diff exceeded limits and was reduced
}
```

`materialize` **must not mutate the user's working copy.** Git uses
`git worktree add --detach` into a scratch dir (or `git archive` when the repo is a
bare mirror); SVN uses `svn export` at the revision. Scratch dirs live under
`{data_dir}/scratch/{run_id}/` and are removed when the run terminates, unless
`global.keep_scratch_on_failure` is set.

### 6.2 Git adapter (`kind = 'git'`)

- Backed by shelling out to the `git` binary. Do **not** use libgit2/gix in v1;
  shelling out matches user config (credential helpers, submodules, LFS) exactly.
  All invocations go through one `git()` helper that sets
  `GIT_TERMINAL_PROMPT=0`, `GIT_ASKPASS=echo`, and a per-call timeout.
- **Discovery:** `git fetch --all --prune`, then
  `git rev-list --reverse --first-parent {cursor}..{branch}` for each watched
  branch (`repo.config.branches`, default: the default branch + globs).
  Merge commits are reviewed as a single change with `--first-parent` diff unless
  `review_merge_commits = false` (default `false`; merges are skipped and their
  constituent commits reviewed instead — record `skip_reason = 'merge_commit'`).
- **Cursor:** last reviewed SHA per branch. On force-push (cursor SHA no longer an
  ancestor), record an audit event `history_rewritten`, reset the cursor to the
  merge-base, and re-discover forward.

### 6.3 GitHub adapter (`kind = 'github'`)

Superset of git. Adds pull-request discovery and PR-aware publishing.

- Access is via, in priority order: (a) the configured **GitHub MCP server**,
  (b) the **`gh` CLI** if authenticated, (c) unauthenticated REST for public repos
  (read-only, discovery of PRs only). The selected transport is reported by
  `revlocal doctor` and stored on the repo row.
- **Discovery:** open PRs targeting watched branches, ordered by `updated_at`.
  A PR is a new Change whenever its head SHA changes. `external_id = "{number}:{head_sha}"`
  so a re-push is a distinct Change; the previous run's findings become
  `superseded` if their fingerprints don't recur.
- Drafts are skipped by default (`review_draft_prs = false`).
- Both PR review **and** commit review can be enabled; when both are on, commits
  already covered by an open PR are skipped with `skip_reason = 'covered_by_pr'`.

### 6.4 SVN adapter (`kind = 'svn'`)

- Shells out to `svn`/`svnlook`. Requires `svn` on PATH; `revlocal doctor` reports
  its absence as a blocking prerequisite for SVN repos only.
- **Discovery (per-revision):**
  `svn log --xml -r {cursor+1}:HEAD --limit N -v {root}` over watched paths
  (default `trunk` plus `branches/*` if `watch_branches = true`).
  Each revision is a `svn_rev` Change; `external_id = "r{num}"`.
- **Materialize:** `svn diff -c {rev}` for the diff, `svn export -r {rev}` for the
  tree. Binary and property-only changes are summarized, not diffed.
- **Pseudo-PR (D6):** when a revision's log message or `svn:mergeinfo` property
  change indicates a **reintegration/merge of a branch into trunk**, emit an
  *additional* Change of kind `svn_pseudo_pr` with
  `external_id = "{branch}@r{rev}"`, whose diff is the **whole branch-vs-trunk
  diff** (`svn diff {trunk_url}@{fork_rev} {branch_url}@{rev}`), not just the merge
  revision. Detection heuristics, in order:
  1. `svn:mergeinfo` on the target path gained ranges from a branch path **and**
     the gain is a reintegration rather than one of the three other merge styles
     that also move mergeinfo (ADR 0031): the source path is not the watched
     trunk (else it is a sync merge), the revision changes file content and not
     only `svn:mergeinfo` (else it is `--record-only`, which marks revisions as
     deliberately *not* merged), and the gained range reaches the branch's
     last-changed revision (else it is a cherry-pick);
  2. log message matches `merge_detect_regex` (default
     `(?i)\b(merge|reintegrat\w+)\b.*\b(branches?/[\w./-]+)`);
  3. the revision touches ≥ `pseudo_pr_min_files` (default 5) files and its
     message names a branch path that exists.
  Findings from the constituent per-revision reviews are attached as prior context
  to the pseudo-PR review, and the pseudo-PR review is marked as the
  **authoritative** review for the merge (per-revision reviews on the branch are
  demoted to `info` in publishing to avoid double-filing).

---

## 7. Trigger layer

All four sources produce the same event:

```rust
pub struct TriggerEvent {
    pub repo_id: RepoId,
    pub source: TriggerSource,   // Poll | Hook | Webhook | Manual | Backfill
    pub hint: Option<String>,    // sha / pr number / revision, if known
    pub received_at: DateTime<Utc>,
}
```

The TriggerBus **coalesces**: events for the same repo within
`global.coalesce_window_ms` (default 1500) collapse into one discovery pass. A
trigger never reviews directly — it schedules discovery, discovery creates Changes,
Changes enqueue Runs. This one-way flow is required so that four trigger sources
firing at once cannot produce four duplicate reviews.

### 7.1 Poll

Per-repo `poll_interval_secs` (default 120; minimum enforced 30). Jittered ±10% to
avoid thundering herd. Backs off exponentially to 30 min after consecutive failures
and reports repo health as `degraded` in the UI.

### 7.2 Local git hooks

`revlocal hooks install --repo <name> [--mode reference|bare-mirror]`

- **reference mode** (default): writes `post-commit`, `post-merge`, and
  `post-checkout` hooks into `{local_path}/.git/hooks/` that POST to the app's
  loopback trigger endpoint (`http://127.0.0.1:{trigger_port}/trigger`) with a
  per-repo shared secret. Existing hooks are **not clobbered**: if a hook exists,
  rev-local appends a clearly delimited block, and `hooks uninstall` removes exactly
  that block.
- **bare-mirror mode**: for reviewing *pushes*, the user configures a bare mirror
  (`git clone --mirror`) that developers push to, and rev-local installs a
  `post-receive` hook there. This is the only way to see every pushed ref including
  deletions.
- Hooks must be non-blocking: they fire-and-forget with a 2s timeout and always
  `exit 0`. **A developer's commit must never fail because rev-local is down.**
  This is an acceptance test.

### 7.3 GitHub webhooks via tunnel

- App runs an axum listener on `127.0.0.1:{webhook_port}`, validating
  `X-Hub-Signature-256` against a per-repo secret.
- Public exposure is via a **pluggable tunnel provider**: `cloudflared`, `ngrok`,
  or `manual` (user supplies their own URL). The app shells out to the tunnel
  binary if present, captures the assigned public URL, and offers a one-click
  "register webhook" that uses the GitHub transport from §6.3.
- Handled events: `push`, `pull_request` (opened/synchronize/reopened/ready_for_review),
  `pull_request_review_comment` (for reply threading only, v1.1).
- The listener is **off by default** and requires explicit opt-in per repo. Binding
  and tunnel state are shown in the UI with the live public URL.

### 7.4 Manual & backfill

- `revlocal review --repo R --rev <sha|r1234|pr:123>` — one change, now.
- `revlocal backfill --repo R --since <date|sha|rev> [--limit N] [--dry-run]`
  — enumerates historical changes, enqueues them at low priority behind live work,
  respects budgets, and is resumable (it advances a distinct `backfill:` cursor).

---

## 8. Engine layer

### 8.1 The trait

```rust
#[async_trait]
pub trait Engine: Send + Sync {
    fn id(&self) -> EngineId;                        // claude | codex | mock
    async fn probe(&self) -> Result<EngineProbe>;    // binary present? version? authed?
    async fn run(&self, task: EngineTask, cancel: CancellationToken)
        -> Result<EngineOutcome>;
}

pub struct EngineTask {
    pub cwd: PathBuf,             // the materialized worktree (read-only intent)
    pub out_dir: PathBuf,         // the ONLY writable path; findings land here
    pub prompt: String,           // rendered from a template (§9.2)
    pub attachments: Vec<PathBuf>,// diff file, prior findings, repo conventions
    pub timeout: Duration,
    pub depth: Depth,
}

pub struct EngineOutcome {
    pub findings: Vec<RawFinding>,
    pub summary: String,          // markdown, <= 1200 chars
    pub verdict: Verdict,         // Approve | Comment | RequestChanges
    pub usage: Usage,             // tokens in/out, cost if reported
    pub transcript: String,       // raw stdout for the archive
    pub degraded: Option<String>, // set when output had to be salvaged
}
```

### 8.2 The output contract (engine-agnostic)

This is the single most important interop detail. **Do not depend on a specific
CLI's structured-output flag.** Instead:

1. The runner creates `out_dir` and passes it in the environment as `REVLOCAL_OUT`.
2. The prompt instructs the engine, in imperative terms, to write its result to
   `$REVLOCAL_OUT/result.json` conforming to the schema in §8.3, and to write
   nothing else to that directory.
3. After the process exits, the runner reads `result.json`.
4. **Fallback ladder** if the file is missing or invalid:
   a. parse the last fenced ` ```json ` block in stdout;
   b. parse the whole stdout as JSON;
   c. run a **repair pass** — re-invoke the engine with a short prompt containing
      the malformed text and asking only for corrected JSON (max 1 repair, counted
      against budget);
   d. fail the run with `error = 'engine_output_unparseable'`, preserving the
      transcript. Never guess findings.
   Any use of (a)–(c) sets `degraded`.

### 8.3 `result.json` schema

```jsonc
{
  "schema_version": 1,
  "verdict": "approve" | "comment" | "request_changes",
  "summary": "markdown, <= 1200 chars, what changed and the headline judgement",
  "findings": [
    {
      "severity": "critical|high|medium|low|info",
      "category": "correctness|security|convention|tests|perf|other",
      "confidence": 0.0,
      "file": "path/relative/to/repo/root.rs",
      "line_start": 42,
      "line_end": 47,
      "title": "<= 80 chars, the claim alone",
      "body": "markdown: what is wrong and why",
      "failure_scenario": "concrete inputs/state -> wrong output or crash",
      "suggested_fix": "optional markdown or diff"
    }
  ],
  "coverage_notes": "optional: what you could not review and why"
}
```

Validated with `jsonschema` against `crates/revlocal-engine/schema/result.v1.json`.
Findings failing validation are dropped individually with an audit event; the run
still succeeds if at least the envelope parsed.

### 8.4 Invocation profiles

Invocations are **config-driven templates**, not hardcoded, because CLI flags drift.
Defaults ship as:

```toml
[engines.claude]
bin = "claude"
args = ["-p", "{prompt_file_content}", "--output-format", "json",
        "--permission-mode", "acceptEdits",
        "--add-dir", "{out_dir}"]
version_args = ["--version"]
stdin_prompt = false

[engines.codex]
bin = "codex"
args = ["exec", "--json", "--sandbox", "workspace-write",
        "--cd", "{cwd}", "{prompt_file_content}"]
version_args = ["--version"]
stdin_prompt = false
```

Placeholders: `{cwd}`, `{out_dir}`, `{prompt_file}`, `{prompt_file_content}`,
`{timeout_secs}`. If a template references `{prompt_file_content}` the prompt is
passed as an argv string; if it references `{prompt_file}` a temp file path is
passed instead; if `stdin_prompt = true` the prompt goes on stdin.

`revlocal doctor` **probes** each configured engine: runs `version_args`, then runs
a 20-token smoke task against a tiny fixture and verifies `result.json` appears.
Its output tells the user exactly which engine is usable and why not, and is the
first thing the UI shows on a fresh install.

### 8.5 Sandboxing and safety

- The engine process is spawned with `cwd` = the materialized worktree, which is a
  **scratch copy**, never the user's checkout.
- Environment is inherited (so CLI logins work) minus a denylist
  (`GITHUB_TOKEN`, `GH_TOKEN`, `*_API_KEY`, `*_SECRET`, `*_PASSWORD`) unless
  `engines.<id>.pass_env` explicitly allows a name. Rationale: the review engine
  has no business acting on remotes; only rev-local's publish layer does.
- Hard wall-clock timeout per run (`depth`-scaled: summary 3 min, standard 10 min,
  deep 25 min). On timeout: SIGTERM, 5s grace, SIGKILL; run fails with
  `engine_timeout`; transcript retained.
- The process is killed on cancellation and on kill-switch.
- On Windows, use a Job Object so child processes die with the parent; on Unix,
  spawn in a new process group and signal the group. This is an acceptance test.

---

## 9. Review pipeline

### 9.1 Stages

```
detect → dedupe/skip → materialize → assemble context → select depth
      → engine review → parse & validate → normalize & dedupe findings
      → risk-classify → publish plan → (approve gate) → publish → record
```

Each stage transition writes `run.status` and emits an event. A crash mid-run
leaves a recoverable row: on startup, runs in a non-terminal state older than
`stale_run_minutes` (10) are marked `failed` with `error = 'interrupted'` and
re-enqueued once.

### 9.2 Context assembly

The prompt is rendered from a template (`crates/revlocal-engine/prompts/review.md.hbs`)
with these sections, in order:

1. **Role & output contract** — write `$REVLOCAL_OUT/result.json`, schema inline.
2. **Change metadata** — repo, kind, author, message/PR body, branch, URL.
3. **The diff** — unified, with file list and stat. Truncation rules in §9.4.
4. **Repo conventions** — contents of `CLAUDE.md`, `AGENTS.md`, `CONTRIBUTING.md`,
   `.editorconfig`, and any paths in `repo.config.convention_files`, truncated to
   `max_convention_bytes` (default 24 KB total). This is what powers the
   convention/architecture-drift scope (D8).
5. **Review scope** — the enabled categories, stated as explicit instructions, with
   per-category guidance (correctness / security / convention / tests).
6. **Prior context** — for PRs and pseudo-PRs: findings from earlier runs on the
   same change (so the engine can note "still unfixed"); suppressed fingerprints
   listed as "do not report these".
7. **Rules of engagement** — report only defects you can name a concrete failure
   for; no style nits unless the repo's own conventions state them; every finding
   needs a file and line; prefer 0 findings over speculative ones; you may read any
   file in `cwd` to verify, and you must verify before reporting.

The rendered prompt is stored alongside the transcript for reproducibility.

### 9.3 Depth selection (tiered)

| Depth | Trigger condition | Engine budget |
|---|---|---|
| `summary` | diff > `deep_file_limit` (default 150 files) or > 20k changed lines, or change is doc/lockfile-only | 3 min, no verification pass |
| `standard` | default | 10 min |
| `deep` | any of: touches `repo.config.sensitive_globs`; PR labelled per `deep_labels`; security-relevant file patterns (`auth`, `crypto`, `payment`, `*.sql`, CI configs); or ≥1 `critical`/`high` finding in `standard` | 25 min, adds a self-verification instruction requiring the engine to attempt to refute each finding before reporting |

### 9.4 Skip and truncation rules

Skip (record `skip_reason`, no engine spend):
- change touches only paths matching `repo.config.ignore_globs`
  (default: `**/node_modules/**`, `**/vendor/**`, `**/*.lock`, `**/dist/**`,
  `**/*.min.*`, `**/target/**`, generated-file markers);
- diff is empty after ignore filtering;
- author matches `ignore_authors` (bots: `dependabot[bot]`, `renovate[bot]`, …);
- merge commit with `review_merge_commits = false`;
- commit already covered by an open PR (§6.3);
- change already has a `done` run with the same content hash.

Truncate (set `context.truncated`, tell the engine explicitly what was omitted):
- per-file diff hunks beyond `max_file_diff_bytes` (default 64 KB) are replaced by
  a stat line;
- total diff beyond `max_total_diff_bytes` (default 512 KB) keeps whole files in
  descending "interest" order (source before tests before config before data) until
  the budget is spent.

Truncation must never silently hide a file: the omitted file list is always included
in full.

### 9.5 Finding normalization

- Clamp `severity` to the allowed set; unknown → `medium`. A finding whose *only*
  schema violation is its severity is salvaged rather than dropped; one with any
  other violation is still dropped per §8.3.
- A finding whose `file` isn't in the change's file set is **retained, never
  dropped**. When `allow_out_of_diff_findings = false` (the default outside `deep`,
  where it is `true`) it is forced to `severity <= medium` and published inline as
  `info` (GitHub can't anchor it). Amended by ADR 0021; the earlier wording said
  "drop ... unless", which contradicted both §18 and this section's own next clause.
- Findings matching an active `suppression` are marked `suppressed` and never reach
  the publish plan. They are recorded, not discarded — see ADR 0021.
- Compute `fingerprint` (§10.3) and mark duplicates of an already-`published`
  fingerprint on the same PR as `superseded` instead of re-filing.

---

## 10. Findings, risk, and dedupe

### 10.1 Severity semantics

| Severity | Meaning | Default publish behaviour |
|---|---|---|
| `critical` | Data loss, RCE, auth bypass, corruption. | Issue + PR review `REQUEST_CHANGES` + failing check |
| `high` | Wrong behaviour a user will hit; serious vuln. | Issue + inline PR comment + review `REQUEST_CHANGES` |
| `medium` | Real defect, narrower blast radius. | Inline PR comment; issue only if `andare_min_severity` is `medium` or lower |
| `low` | Minor correctness/convention issue. | Inline PR comment only |
| `info` | Observation, no action implied. | Summary body only |

### 10.2 Verdict mapping

`request_changes` if any `critical`/`high` survives; `comment` if any
`medium`/`low`; `approve` otherwise. The app **never** submits a GitHub `APPROVE`
review by default (`allow_approve = false`) — an AI approving code is a stronger
claim than the product should make unattended. It posts a `COMMENT` review saying
"no blocking findings". `allow_approve = true` is available per repo, opt-in.

### 10.3 Fingerprint

```
fingerprint = sha256(
    repo.name || '\x00' ||
    normalize_path(file) || '\x00' ||
    category || '\x00' ||
    normalized_title
)[0..16]
```
where `normalized_title` is lowercased, whitespace-collapsed, digits replaced with
`#`, and identifiers longer than 3 chars kept verbatim. Deliberately **line-number
independent**, so the same defect surviving a rebase dedupes correctly.

---

## 11. Publish layer

### 11.1 Capability abstraction

```rust
#[async_trait]
pub trait PublishTarget: Send + Sync {
    fn id(&self) -> &str;                              // "github" | "andare" | "trama"
    async fn discover(&self) -> Result<CapabilitySet>; // what can this target do?
    async fn execute(&self, action: &PublishAction) -> Result<PublishReceipt>;
    async fn health(&self) -> Result<TargetHealth>;
}

pub enum Capability {
    PostReview,     // threaded review with inline comments (GitHub)
    Comment,        // a single comment on a change
    CreateIssue,    // file a work item (Andare)
    SetStatus,      // move a work item's state (Andare)
    SetCheck,       // pass/fail/pending check on a commit (GitHub)
    UpsertDoc,      // create-or-update a document (Trama)
    LinkDocToIssue, // cross-link (Trama <-> Andare)
}
```

### 11.2 MCP client (`revlocal-mcp`)

- Speaks MCP over **stdio** (spawned server process) and **streamable HTTP**.
- Config is a familiar `mcpServers` map (§13.1) so users can paste in what they
  already have for Claude Code / Codex.
- On connect: `initialize`, then `tools/list`. The discovered tool list is cached
  with its input schemas and shown in the UI ("Andare: 14 tools, 5 capabilities
  mapped").
- **Capability mapping** is table-driven, with a built-in profile per known target
  and a generic fallback:

```toml
[targets.andare]
mcp_server = "andare"
[targets.andare.map.create_issue]
tool_candidates = ["create_issue", "create_work_item", "issue_create", "create_ticket"]
args = { title = "{finding.title}", body = "{finding.body_md}",
         project = "{repo.config.andare_project}", labels = ["rev-local","{finding.category}"] }
[targets.andare.map.set_status]
tool_candidates = ["update_issue", "set_issue_status", "transition_issue"]
args = { id = "{issue_ref}", status = "{status}" }
```

At startup the mapper resolves each `tool_candidates` list against the *actual*
discovered tool names and validates the rendered args against the tool's JSON
Schema. Unresolvable capabilities are reported as **unmapped**, not attempted; the
UI shows exactly which capability failed to bind and to which server, and offers a
manual override (pick tool + map fields). **This is why the Andare integration does
not require knowing Andare's tool names at build time.**

### 11.3 GitHub target

Capabilities: `PostReview`, `Comment`, `SetCheck`.
Transport: GitHub MCP server if configured, else `gh` CLI, else read-only.

- **PR review**: one review per run, body = summary + findings table, with inline
  comments anchored to `(file, line_start..line_end)` on the PR's head SHA. If an
  inline anchor fails (line not in diff), that comment is demoted to the review body
  under "Findings outside the diff".
- **Commit review** (non-PR): a commit comment.
- **Check**: `rev-local/review` check run — `success` on approve/comment,
  `failure` on request_changes when `block_on_findings = true` (default `false`),
  else `neutral`. Always `in_progress` while the run is active.
- Idempotency: `idempotency_key = "gh:{repo}:{pr}:{head_sha}:{run_kind}"`. On
  re-run for the same head SHA, **edit** the existing review body rather than
  posting a second one.

### 11.4 Andare target (issue tracker)

Capabilities: `CreateIssue`, `SetStatus`, `Comment`.

- Findings of severity ≥ `andare_min_severity` (default `high`) become issues.
- Issue body includes: the claim, the failure scenario, the code excerpt, a link to
  the change (PR URL / commit / `r1234`), the Trama review page link, and a
  `rev-local-fingerprint: <fp>` trailer used for idempotent re-filing.
- Before filing, the target **searches** for an existing open issue carrying the same
  fingerprint trailer (via a `search`/`list` capability if mapped); if found, it
  comments on it rather than duplicating.
- `SetStatus` moves the ticket referenced by the change: if the commit message / PR
  title / SVN log message contains a work-item key matching
  `repo.config.andare_key_regex` (default `[A-Z][A-Z0-9]+-\d+`), the run reports
  review outcome onto that ticket — a comment always, and a status transition when
  `andare_transition_on` maps the verdict to a state name.

### 11.5 Trama target (documentation)

Capabilities: `UpsertDoc`, `Comment`, `LinkDocToIssue`.
Uses the tools this MCP server actually exposes: `list_spaces`, `get_page_tree`,
`search_pages`, `get_page`, `create_page`, `update_page`, `publish_page`,
`comment_on_page`, `link_to_issue`, `list_backlinks`.

- **Page identity:** one page per Change, titled
  `Review: {repo} {short_id} — {truncated change title}`, placed under a parent page
  `Code Reviews / {repo}` in the space `repo.config.trama_space`.
- **Critical constraint (from the server's own guidance): `update_page` REPLACES the
  body.** Therefore the target must `get_page` first, merge, and send the whole
  document back. Never send a fragment. This is an acceptance test with a mock MCP
  server.
- Pages link with `[[wikilinks]]`: each review page links `[[{repo} Review Index]]`
  and any Andare issue via `link_to_issue`.
- A per-repo rolling **`{repo} Review Index`** page is upserted with the last N
  reviews (default 50) as a table. Because updates replace bodies, the index is
  regenerated from SQLite each time — SQLite is the source of truth, Trama is a
  projection. This makes it safe to lose or hand-edit.
- `publish_page` is called only when `trama_publish = true` (default `true` for
  `auto` autonomy, `false` for `auto_low_ask_high` — an unpublished draft is a
  low-risk action, publishing is high-risk).

### 11.6 Delivery guarantees

- Every action is written to `publish_action` **before** it is attempted.
- Retries: 5 attempts, exponential backoff with jitter (1s → 60s cap), only on
  transport/5xx/rate-limit errors. 4xx is terminal.
- `UNIQUE(target, idempotency_key)` makes double-publish structurally impossible.
- Partial failure is normal and reported: a run can be `done` with GitHub posted,
  Andare failed. The UI shows per-target status per run and offers "retry target".

---

## 12. Autonomy, risk, and the kill switch

### 12.1 Kill switch

- A single global toggle in the tray/menu bar and `revlocal pause` / `revlocal resume`.
- When engaged: cancels every in-flight engine process, drains the run queue to
  `cancelled`, holds the publish queue (does not lose it), stops all triggers,
  and displays a persistent banner. Survives restart (persisted in settings).
- `revlocal kill --hard` additionally kills orphaned engine processes by scanning
  the recorded PIDs.

### 12.2 Modes

| Mode | Reviews run? | Low-risk publishes | High-risk publishes |
|---|---|---|---|
| `off` | no | — | — |
| `dry_run` | yes | recorded as `skipped_dry_run`, rendered in UI | same |
| `auto_low_ask_high` | yes | sent immediately | queued to Approvals inbox |
| `auto` | yes | sent | sent |

Mode is per repo, with a global ceiling: the effective mode is
`min(global_mode, repo_mode)` under the ordering `off < dry_run < auto_low_ask_high < auto`.

### 12.3 Risk classification (per action, not per run)

**Low risk** — additive, easily reversible, low blast radius:
- a comment on a PR/commit;
- a PR review with verdict `comment`;
- an *unpublished* Trama draft page;
- a `neutral`/`success` check.

**High risk** — blocks people, notifies broadly, or creates work:
- PR review with `REQUEST_CHANGES` or `APPROVE`;
- a `failure` check run;
- creating an Andare issue;
- transitioning an Andare issue's status;
- `publish_page` on Trama;
- **any** action whose target/capability pair is newly mapped and has never
  succeeded before (first-use is always high risk — this is deliberate: the first
  time rev-local ever writes to a system, a human sees it).

Additionally, any action is escalated to high risk if the run is `degraded`, if the
finding's `confidence < 0.6`, or if the repo has posted > `burst_threshold`
(default 10) actions in the last hour.

### 12.4 Approvals inbox

Queued actions appear in an inbox showing exactly the payload that would be sent,
rendered as the target would render it. Actions: **Approve**, **Approve all for
this run**, **Reject**, **Reject and suppress this finding**, **Edit body then
approve**. Approvals expire after `approval_ttl_hours` (default 72) → `rejected`
with reason `expired`, always audited.

---

## 13. Configuration

### 13.1 Global config — `{config_dir}/rev-local/config.toml`

```toml
[global]
mode = "auto_low_ask_high"       # off | dry_run | auto_low_ask_high | auto
max_concurrent_runs = 2
coalesce_window_ms = 1500
trigger_port = 41791             # loopback hook receiver
webhook_port = 0                 # 0 = disabled
transcript_retention_days = 30
keep_scratch_on_failure = true
stale_run_minutes = 10
max_attempts = 3                      # §9.1: recovery gives up after this many
approval_ttl_hours = 72
burst_threshold = 10

[budgets]
daily_tokens_per_repo = 2000000
daily_runs_per_repo = 200
daily_cost_usd_per_repo = 0      # 0 = unlimited
on_exhausted = "pause"           # pause | queue | skip   (never silently drop)

[engines.claude] # §8.4
[engines.codex]  # §8.4

[mcpServers.github]
type = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]

[mcpServers.andare]
type = "stdio"
command = "andare-mcp"
args = []

[mcpServers.trama]
type = "http"
url = "https://trama.example.com/mcp"

[targets.github]  # §11.3
[targets.andare]  # §11.4
[targets.trama]   # §11.5
```

Secrets are **never** in this file. Tokens for MCP servers come from the OS keychain
(`keyring` crate) or from the server's own auth; the config may reference
`{{keychain:name}}` placeholders which are resolved at connect time and never logged.

### 13.2 Per-repo config (stored in `repo.config_json`)

```jsonc
{
  "branches": ["main", "release/*"],
  "review_prs": true,
  "review_commits": false,
  "review_draft_prs": false,
  "review_merge_commits": false,
  "watch_branches": true,               // svn: branches/* as well as trunk
  "poll_interval_secs": 120,
  "scope": ["correctness", "security", "convention", "tests"],
  "engine": "claude",
  "autonomy": "auto_low_ask_high",
  "ignore_globs": ["**/node_modules/**", "**/vendor/**", "**/*.lock",
                   "**/dist/**", "**/*.min.*", "**/target/**"],
  "ignore_authors": ["dependabot[bot]", "renovate[bot]"],
  "sensitive_globs": ["**/auth/**", "**/crypto/**", "**/*.sql", ".github/workflows/**"],
  "deep_file_limit": 150,               // §9.3
  "deep_labels": [],                    // §9.3
  "convention_files": ["CLAUDE.md", "AGENTS.md", "CONTRIBUTING.md"],
  "targets": ["github", "andare", "trama"],
  "andare_project": "PLAT",
  "andare_min_severity": "high",
  "andare_key_regex": "[A-Z][A-Z0-9]+-\\d+",
  "trama_space": "ENG",
  "trama_publish": false,
  "max_convention_bytes": 24576,        // §9.2
  "max_file_diff_bytes": 65536,         // §9.4
  "max_total_diff_bytes": 524288,       // §9.4
  "webhook_enabled": false,             // §7.3: off by default, explicit opt-in per repo
  "webhook_secret_ref": null,           // keychain reference, never the secret itself
  "block_on_findings": false,
  "allow_approve": false,
  "merge_detect_regex": "(?i)\\b(merge|reintegrat\\w+)\\b.*\\b(branches?/[\\w./-]+)",
  "pseudo_pr_min_files": 5,             // §6.4 heuristic 3
  "andare_transition_on": {}            // §11.4; empty = never move a ticket
}
```

Optional in-repo override: `.rev-local.toml` at the repo root, merged over the
stored config. **A repository must not be able to grant itself more authority than
it was given** — `.rev-local.toml` is committed inside the repository under review,
so anyone who can open a pull request can propose changing it.

The rule is implemented as an **allowlist**, not a denylist (ADR 0007). Only these
keys may be set in-repo, and each may only narrow:

| Key | Merge semantics |
|---|---|
| `scope` | intersection — may drop a review dimension, never add one |
| `ignore_globs` | union — may add ignores, never remove the operator's |
| `ignore_authors` | union |
| `sensitive_globs` | union — may force deeper review, never shallower |
| `convention_files` | union |

Every other key is refused with a typed error naming it, including `autonomy`,
`targets`, `engine`, and — though they are not authority by name — `trama_publish`
and `allow_approve`, which change the risk class of an action (§12.3, §10.2).
Refusal is per key: one forbidden key does not discard the rest of the file.

---

## 14. CLI surface (`revlocal`)

Every command supports `--json` for machine-readable output. This surface **is** the
acceptance-test API.

```
revlocal doctor                     # prerequisites, engines, MCP targets, capabilities
revlocal repo add <path|url> --kind git|github|svn [--name N] [--engine E]
revlocal repo list | show <name> | remove <name> | set <name> key=value...
revlocal watch [--repo N]           # run the daemon in the foreground
revlocal review --repo N --rev <ref> [--depth D] [--dry-run]
revlocal backfill --repo N --since <ref|date> [--limit K]
revlocal runs list [--repo N] [--status S] | show <run_id> | retry <run_id>
revlocal findings list [--repo N] [--severity S] | suppress <fingerprint>
revlocal approvals list | approve <id|--run R|--all> | reject <id> [--suppress]
revlocal publish retry <action_id> | replay --run R --target T
revlocal hooks install|uninstall --repo N [--mode reference|bare-mirror]
revlocal webhook start|stop|status [--tunnel cloudflared|ngrok|manual]
revlocal targets list | test <target> | map <target> <capability> --tool T
revlocal pause | resume | kill --hard
revlocal budget show [--repo N] | reset --repo N
revlocal db migrate | vacuum --before <date> | export --format json
```

---

## 15. Desktop UI

React + TypeScript, Tauri v2 IPC. Six screens. Keep it plain and dense; this is an
operations console, not a marketing surface.

1. **Dashboard** — repo cards (health, last run, queue depth, today's budget bar),
   global mode selector, kill switch, live activity feed from the EventBus.
2. **Repository** — config editor, watched branches/paths, trigger status
   (poll/hooks/webhook each with a live indicator), recent runs, per-repo budget.
3. **Run detail** — timeline of stages with durations, the rendered prompt, the raw
   transcript (collapsible), the diff with findings anchored inline, per-target
   publish status with retry buttons.
4. **Findings** — cross-repo table, filter by severity/category/state, suppress,
   jump to run, "file to Andare" manual action.
5. **Approvals** — the inbox from §12.4, rendered exactly as the target would render.
6. **Settings** — engines (with `doctor` output inline), MCP servers with discovered
   tool lists, capability mapping table with manual override UI, budgets, retention.

Non-negotiable UI behaviours:
- Every destructive or outbound action names its target explicitly ("Post review to
  github.com/acme/api PR #412").
- The kill switch is reachable from every screen and from the tray.
- Live updates come from Tauri events, not polling the DB.
- The app must be fully usable while a review is running (no modal blocking).

---

## 16. Testing

### 16.1 Levels

- **Unit** (`cargo test`, per crate): domain logic, risk classification, fingerprint,
  truncation, config merge, capability mapping resolution.
- **Integration** (`cargo test -p revlocal-* --test '*'`): against fixture repos and
  a mock MCP server, using `Engine = mock`. **No network. No real model calls.**
  This is the tier that runs constantly.
- **Engine-live** (`cargo test --features engine-live -- --ignored`): actually
  invokes `claude` and `codex` against a fixture with a planted bug and asserts a
  finding is produced. Run manually or at a periodic checkpoint, never in the fast
  inner loop.
- **UI** (`vitest` + Testing Library) for component logic, and **visual
  verification** (§16.4) against the built Tauri binary for the launch → add repo →
  dry-run review path.

### 16.2 Fixtures (`fixtures/`)

A script `fixtures/build.sh` (+ `build.ps1`) that constructs, offline:

- `fixtures/out/git-basic/` — a git repo with 12 commits including: a clean commit,
  a commit with a **planted off-by-one bug** in `src/pager.rs`, a commit with a
  **planted SQL injection** in `src/db.rs`, a lockfile-only commit, a merge commit,
  a 200-file commit (for truncation), and a bot-authored commit.
- `fixtures/out/git-bare/` — a bare mirror of the above, for `post-receive` tests.
- `fixtures/out/svn-basic/` — created with `svnadmin create`, accessed via `file://`,
  with `trunk`, `branches/feature-x`, revisions mirroring the git ones, plus a
  **reintegration revision** whose log message and `svn:mergeinfo` trigger pseudo-PR
  detection.
- `fixtures/mock-mcp/` — a Node script implementing an MCP stdio server with
  configurable tool names, deliberate quirks (a tool that requires read-before-write
  like Trama's `update_page`), and a request journal the tests assert against.
- `fixtures/mock-engine/` — a script that behaves like a CLI engine: honours
  `REVLOCAL_OUT`, can be told (via env) to emit valid JSON, malformed JSON, no file,
  or to hang past the timeout — one fixture per branch of the §8.2 fallback ladder.

SVN tests are skipped with a clear message (not a failure) when `svn` is absent, but
CI for the project must install it.

### 16.3 Cross-platform

The integration suite runs on all three platforms in CI. Windows-specific
acceptance: path handling (`\` vs `/` in diff paths and globs), process-group kill
via Job Objects, hook scripts written with the right line endings and a `.cmd`
shim, and SQLite file locking under WAL on a non-NTFS-mounted path.

### 16.4 GUI verification by capture

The desktop UI is verified visually, not by assertion alone. A window capture tool
waits for a window to *settle* and writes a deterministic PNG, which is what makes
it possible to confirm the UI actually renders instead of assuming it does. The
tool is an implementation detail of the developer's environment; what this section
specifies is the two capture modes and what each must guarantee.

**Single-screen capture.** Launch the app on one route with fixture data, wait for
the window to settle, and write `artifacts/gui/<screen>.png` clipped to the app's
own chrome so captures are comparable between machines.

It must exit **non-zero when the window never appears**. A missing or blank UI has
to fail the gate — a screen that silently captures nothing is worse than no
capture, because it looks like a pass. Best-effort settling is for debugging and
must never be used in a gate.

**Scripted flow capture.** Drive a sequence of steps and capture one settled frame
per step, with each frame captioned by the step that produced it. The caption must
come from the driver rather than be written afterwards, so captions cannot drift
from actions. Flows to cover: `onboarding`, `add-repo-to-review`,
`approve-queued-action`.

**Where the gate runs.** Capture is a local developer gate, not a CI gate — see
ADR 0032. CI runs the `vitest` layer, compiles the shell, and smoke-tests that it
starts; visual verification stays where a real desktop session exists.

**How the capture is used.** Capture is only half the gate. The PNG is then *read*
and checked against the screen's stated checklist (§15). Perceptual pixel diffing
against golden images is explicitly **not** the primary gate — it is too brittle
across platforms, fonts and DPI. Goldens may be added later as a regression signal
with a tolerance, but the load-bearing check is that someone looked at the
screenshot and confirmed the required elements are present.

Headless CI viability (Xvfb/Wayland on Linux, screen-recording consent on macOS) is
unresolved. Until it is decided, GUI gates are local-only and CI runs the `vitest`
layer.

---

## 17. Milestones

Work these **in order**. Each has an exit gate that is a command to run. Do not
begin milestone N+1 until N's gate passes. Record any deviation as an ADR.

| M | Title | Deliverable | Exit gate (must pass) |
|---|---|---|---|
| **M0** | Skeleton | Cargo workspace, all 8 crates, `revlocal --version`, CI config for 3 OSes, rustfmt+clippy clean | `cargo build --workspace && cargo clippy --workspace -- -D warnings && ./target/debug/revlocal --version` |
| **M1** | Core domain | `revlocal-core` types (§3, §5), risk model (§12.3), fingerprint (§10.3), config load/merge (§13) | `cargo test -p revlocal-core` ≥ 25 tests incl. risk-matrix table test; **plus** a test asserting `revlocal-core`'s dep tree contains no `tokio`/`sqlx`/`reqwest` |
| **M2** | Store | SQLite migrations (§5), repositories, audit log, budget ledger | `cargo test -p revlocal-store`: migrate up/down, CRUD, `UNIQUE` idempotency violation surfaces as a typed error, WAL concurrency test with 2 writers |
| **M3** | Fixtures | `fixtures/build.sh` + `.ps1`, mock MCP server, mock engine | `./fixtures/build.sh && test -d fixtures/out/git-basic && test -d fixtures/out/svn-basic` (svn portion skips cleanly if `svn` absent) |
| **M4** | Git adapter | discover/materialize/hooks for `kind=git` | integration test: discovers exactly 12 changes from `git-basic`, skips the lockfile + bot + merge commits with correct `skip_reason`, materializes the off-by-one commit into a scratch worktree with a non-empty diff, and **leaves the fixture repo's working tree byte-identical** (asserted via `git status --porcelain` + tree hash) |
| **M5** | Engine layer | `Engine` trait, claude/codex/mock runners, §8.2 fallback ladder, timeouts, process-group kill | test matrix over mock-engine modes: valid → parsed; malformed → repaired; missing file → fenced-block fallback; hang → killed within timeout+2s **and no orphan process remains** (assert by PID) |
| **M6** | Pipeline | full detect→review→findings flow with mock engine, depth selection, truncation, normalization, dedupe | end-to-end test: `revlocal review --repo git-basic --rev <planted-bug-sha> --json` yields a run in `done` with ≥1 finding; the 200-file commit yields `depth=summary` and `truncated=true` with the full omitted-file list present in the prompt |
| **M7** | MCP client | stdio + HTTP transports, `tools/list` discovery, capability mapping + validation, manual override | test against `fixtures/mock-mcp`: resolves candidates to real names, reports an unmapped capability instead of guessing, validates args against tool schema and **refuses** a mismatched payload |
| **M8** | Publish: GitHub | review/comment/check via `gh` and MCP transports, idempotency, inline-anchor demotion | test with a mocked transport: second publish for the same head SHA **edits** rather than duplicates; an unanchorable comment lands in the body section |
| **M9** | Publish: Andare + Trama | issue filing with fingerprint trailer + dedupe search, status transition from work-item key, Trama read-before-write upsert, index page regeneration, `link_to_issue` | mock-MCP journal assertions: `update_page` is always preceded by `get_page` for the same page and carries the **full** merged body; a re-run for the same fingerprint produces a comment, not a second issue |
| **M10** | Autonomy | modes, risk classification wiring, approvals inbox, kill switch, budgets | tests: `dry_run` performs zero MCP writes; `auto_low_ask_high` sends a comment but queues an issue; **first-use of a capability is always queued**; kill switch cancels a running mock engine within 3s and leaves the publish queue intact; budget exhaustion pauses and later resumes without losing changes |
| **M11** | SVN adapter | per-revision discovery/materialize, pseudo-PR synthesis (§6.4) | integration test on `svn-basic`: N revisions discovered in order; the reintegration revision produces **both** a `svn_rev` and a `svn_pseudo_pr` change; the pseudo-PR diff equals the branch-vs-trunk diff, not the merge revision's; per-revision branch findings are demoted in the pseudo-PR's publish plan |
| **M12** | Triggers | poll loop w/ backoff+jitter, loopback hook receiver + hook installer, webhook listener + signature verification + tunnel adapters, coalescing | tests: hook script exits 0 in < 2s with the receiver **down**; an existing user hook survives install/uninstall byte-identically; four simultaneous triggers for one repo produce exactly one discovery pass; a bad webhook signature is rejected |
| **M13** | Desktop UI | six screens (§15), Tauri commands, live events, tray + kill switch, **visual verification harness** (§16.4) | `vitest` green; single-screen capture writes a non-empty settled PNG per screen and exits 0; each PNG is read and confirmed against the screen's §15 checklist; the `add-repo-to-review` flow produces one captioned frame per step; kill switch visible in all six captures |
| **M14** | Live engines | `--features engine-live` suite, `revlocal doctor` polish, packaging for 3 OSes | `cargo test --features engine-live -- --ignored` finds the planted SQL-injection with **both** `claude` and `codex`; `revlocal doctor --json` reports every prerequisite with actionable remediation text; installers build on all three platforms |

---

## 18. Cross-cutting requirements

- **Errors:** one `thiserror` enum per crate, `anyhow` only at the binary edge. Every
  user-visible error carries a remediation sentence. No `unwrap()` outside tests —
  enforced by clippy config.
- **Logging:** `tracing` with a JSON layer to `{data_dir}/logs/`, span per run with
  `run_id`/`repo`/`change` fields. **Redaction layer** scrubs anything matching
  token/secret patterns before it reaches a sink; there is a unit test that feeds a
  fake token through the logger and asserts it does not appear in the output.
- **Time:** all timestamps UTC RFC-3339 in the DB; local time only at render.
- **Paths:** `camino` for UTF-8 paths; every VCS-relative path normalized to `/`
  separators internally.
- **Determinism:** given the same change and the same engine output, the pipeline
  produces the same findings, fingerprints, and publish plan. There is a test.
- **No silent caps.** Wherever the system truncates, samples, or drops, it records
  the fact on the run and shows it in the UI. A review that saw 60% of the diff must
  never look like a review that saw all of it.
