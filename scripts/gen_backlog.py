#!/usr/bin/env python3
"""Generate the rev-local delivery backlog.

Single source of truth for the work breakdown. Emits:
  docs/backlog/backlog.json  - machine-readable, importable into Andare
  docs/backlog/BACKLOG.md    - human-readable rendering

Regenerate with: python3 scripts/gen_backlog.py
Never hand-edit the outputs; edit this file and re-run.
"""
import json, pathlib, textwrap

ITEMS = []

def add(id, type, title, *, parent=None, milestone=None, labels=(), priority="P2",
        estimate=None, depends_on=(), spec=(), desc="", ac=(), gate=None, notes=None):
    ac = list(ac)
    if type == "epic" and not ac:
        ac = ["Every child item is closed with an observed passing gate",
              "The milestone exit criteria in SPEC §17 are met",
              "`cargo clippy --workspace --all-targets -- -D warnings` is clean"]
    ITEMS.append({
        "id": id, "type": type, "title": title, "parent": parent,
        "milestone": milestone, "labels": list(labels), "priority": priority,
        "estimate": estimate, "depends_on": list(depends_on),
        "spec_refs": list(spec), "description": textwrap.dedent(desc).strip(),
        "acceptance_criteria": ac, "gate": gate, "notes": notes,
        "andare_key": None, "status": "todo",
    })

# ─────────────────────────────────────────────────────────────────────────────
# EPIC 1 — Foundation
# ─────────────────────────────────────────────────────────────────────────────
add("RL-E1", "epic", "Foundation: workspace, domain model, persistence", milestone="M0-M2",
    priority="P0", labels=["foundation"], spec=["§4", "§5", "§13"], desc="""
    Everything downstream depends on this epic. It establishes the Cargo workspace,
    the I/O-free domain core, and the SQLite store. Nothing here talks to a network,
    a VCS, or a model.
    """,
    ac=["All of RL-1xx closed", "cargo build/clippy/test green on macOS, Windows, Linux"])

add("RL-101", "story", "Scaffold Cargo workspace with all eight crates", parent="RL-E1",
    milestone="M0", priority="P0", estimate=3, labels=["rust", "scaffold"], spec=["§4.1"],
    desc="""
    Create the workspace exactly as laid out in SPEC §4.1: revlocal-core, -store, -vcs,
    -engine, -mcp, -publish, -daemon, -cli, plus src-tauri/ and ui/ placeholders.
    Workspace-level [workspace.dependencies] for shared crates so versions stay aligned.
    Set rust-version, edition 2021, and a rustfmt.toml + clippy.toml (deny unwrap outside tests).
    """,
    ac=["`cargo build --workspace` succeeds",
        "`cargo clippy --workspace --all-targets -- -D warnings` is clean",
        "`revlocal --version` prints a version",
        "clippy.toml disallows `unwrap`/`expect` in non-test code"],
    gate="cargo build --workspace && cargo clippy --workspace --all-targets -- -D warnings && ./target/debug/revlocal --version")

add("RL-102", "story", "CI matrix for macOS, Windows and Linux", parent="RL-E1",
    milestone="M0", priority="P0", estimate=3, labels=["ci"], spec=["§16.3"],
    depends_on=["RL-101"],
    desc="""
    GitHub Actions workflow running fmt, clippy, build and test on all three OSes.
    Install `svn` on every runner (M11 needs it). Cache cargo registry and target dir.
    Upload framewatch artifacts (M13) and test logs on failure.
    """,
    ac=["Workflow runs on push and PR",
        "All three OS legs green on an empty workspace",
        "`svn --version` succeeds on every runner",
        "Failure uploads logs + any artifacts/gui/*.png"],
    gate="act -j test 2>/dev/null || echo 'verify in CI'")

add("RL-103", "story", "Domain types in revlocal-core", parent="RL-E1", milestone="M1",
    priority="P0", estimate=5, labels=["rust", "domain"], spec=["§3", "§5"],
    depends_on=["RL-101"],
    desc="""
    Repo, RepoKind, Cursor, Change, ChangeKind, Run, RunStatus, Depth, TriggerSource,
    Finding, Severity, Category, FindingState, PublishAction, Capability, Verdict,
    AutonomyMode, RiskClass, Usage, DiffStat, FileDiff. Serde on everything.
    Newtype IDs (RepoId, RunId, ChangeId) — never bare i64 across an API boundary.
    """,
    ac=["Every type in SPEC §3 and §5 is represented",
        "Serde round-trip property test for each enum",
        "Newtype IDs are not interchangeable (compile-fail test via trybuild)"],
    gate="cargo test -p revlocal-core")

add("RL-104", "story", "Enforce that revlocal-core has no I/O dependencies", parent="RL-E1",
    milestone="M1", priority="P0", estimate=2, labels=["rust", "architecture"], spec=["§4.1"],
    depends_on=["RL-103"],
    desc="""
    The domain core must stay unit-testable and fast. Add a test that parses
    `cargo metadata` and fails if revlocal-core's transitive dependency tree contains
    tokio, sqlx, reqwest, hyper, or rusqlite.
    """,
    ac=["Test fails loudly if someone adds tokio to revlocal-core",
        "Test names the offending crate and the path that pulled it in"],
    gate="cargo test -p revlocal-core --test no_io_deps")

add("RL-105", "story", "Risk classification model", parent="RL-E1", milestone="M1",
    priority="P0", estimate=5, labels=["rust", "domain", "safety"], spec=["§12.3"],
    depends_on=["RL-103"],
    desc="""
    Pure function: (action kind, target, capability, run state, finding, repo history)
    -> RiskClass. Implements SPEC §12.3 including the escalation rules: degraded run,
    confidence < 0.6, burst threshold, and — critically — first-ever use of a
    (target, capability) pair is ALWAYS high risk.
    """,
    ac=["Table-driven test covering every row of the §12.3 matrix",
        "First-use escalation has its own dedicated test",
        "Escalation rules compose (a low-risk comment on a degraded run is high)",
        "Function is total: no panics, no unwraps, exhaustive matches"],
    gate="cargo test -p revlocal-core risk::")

add("RL-106", "story", "Finding fingerprint algorithm", parent="RL-E1", milestone="M1",
    priority="P0", estimate=3, labels=["rust", "domain"], spec=["§10.3"],
    depends_on=["RL-103"],
    desc="""
    Implement the line-number-independent fingerprint from SPEC §10.3: sha256 over
    repo, normalized path, category and normalized title, truncated to 16 hex chars.
    Title normalization: lowercase, collapse whitespace, digits -> '#', keep
    identifiers > 3 chars verbatim.
    """,
    ac=["Same defect at a different line yields the same fingerprint",
        "Same title in a different file yields a different fingerprint",
        "Windows-style backslash paths normalize to the same value as POSIX",
        "Golden test vectors committed so the algorithm cannot drift silently"],
    gate="cargo test -p revlocal-core fingerprint")

add("RL-107", "story", "Configuration load, merge and validation", parent="RL-E1",
    milestone="M1", priority="P1", estimate=5, labels=["rust", "config"], spec=["§13"],
    depends_on=["RL-103"],
    desc="""
    Global TOML config + per-repo JSON config + optional in-repo .rev-local.toml,
    merged with the precedence in SPEC §13.2. Enforce the security rule: an in-repo
    file may narrow scope/ignores but may NEVER widen autonomy or add publish targets.
    Resolve {{keychain:name}} placeholders lazily, never eagerly, never into logs.
    """,
    ac=["Merge precedence test for every field class",
        "In-repo config attempting to set autonomy=auto is rejected with a typed error",
        "In-repo config attempting to add a target is rejected",
        "Unknown keys produce a warning, not a hard failure",
        "Defaults match SPEC §13 exactly (test asserts the default document)"],
    gate="cargo test -p revlocal-core config::")

add("RL-108", "story", "SQLite schema and migrations", parent="RL-E1", milestone="M2",
    priority="P0", estimate=5, labels=["rust", "storage"], spec=["§5"],
    depends_on=["RL-103"],
    desc="""
    sqlx migrations implementing SPEC §5 DDL verbatim. WAL mode, foreign_keys=ON,
    busy_timeout set. Include the UNIQUE(target, idempotency_key) constraint on
    publish_action — that constraint is a safety feature, not an optimisation.
    """,
    ac=["`revlocal db migrate` creates the schema from empty",
        "Migrations are idempotent when re-run",
        "A down-migration path exists and is tested",
        "PRAGMA journal_mode returns wal"],
    gate="cargo test -p revlocal-store migrations")

add("RL-109", "story", "Store repositories (CRUD) for every entity", parent="RL-E1",
    milestone="M2", priority="P0", estimate=8, labels=["rust", "storage"], spec=["§5"],
    depends_on=["RL-108"],
    desc="""
    Typed repository structs over each table with compile-time-checked queries.
    Cursor read/advance, change upsert-by-(repo,kind,external_id), run state
    transitions, finding insert with fingerprint index, publish_action insert with
    idempotency, audit append, budget ledger increment.
    """,
    ac=["CRUD round-trip test per entity",
        "Inserting a duplicate (target, idempotency_key) returns a typed AlreadyExists error, not a raw sqlx error",
        "Run state transitions reject illegal moves (done -> reviewing) at the type or function level",
        "Cursor advance is atomic"],
    gate="cargo test -p revlocal-store")

add("RL-110", "story", "Audit log and budget ledger", parent="RL-E1", milestone="M2",
    priority="P1", estimate=3, labels=["rust", "storage", "safety"], spec=["§5", "§12"],
    depends_on=["RL-109"],
    desc="""
    Append-only audit log with actor/kind/detail, plus the per-repo-per-day budget
    ledger with atomic increment. Budget increments must be safe under concurrent runs.
    """,
    ac=["Audit rows are never updated or deleted by any store method",
        "Concurrent budget increments from two tasks sum correctly (WAL concurrency test)",
        "Day rollover creates a new ledger row, does not mutate yesterday's"],
    gate="cargo test -p revlocal-store -- --include-ignored budget")

add("RL-111", "task", "Structured logging with secret redaction", parent="RL-E1",
    milestone="M2", priority="P0", estimate=3, labels=["rust", "security"], spec=["§18"],
    depends_on=["RL-101"],
    desc="""
    tracing subscriber with a JSON file layer under {data_dir}/logs/ and a redaction
    layer that scrubs token-shaped strings before they reach any sink.
    """,
    ac=["A fake token pushed through tracing does not appear in the log file",
        "Redaction covers: Bearer tokens, gh[pousr]_*, trama_*/andare_*-style prefixed keys, and values of fields named *token*/*secret*/*password*",
        "Redaction is applied to span fields as well as event fields"],
    gate="cargo test -p revlocal-core redact")

# ─────────────────────────────────────────────────────────────────────────────
# EPIC 2 — Fixtures
# ─────────────────────────────────────────────────────────────────────────────
add("RL-E2", "epic", "Offline test fixtures and mocks", milestone="M3", priority="P0",
    labels=["testing"], spec=["§16.2"], depends_on=["RL-E1"], desc="""
    The inner loop must never touch the network or spend model tokens. This epic builds
    the git repo, the bare mirror, the SVN repository, the mock MCP server and the mock
    engine that every later test depends on.
    """,
    ac=["`./fixtures/build.sh` reproduces every fixture from scratch, deterministically"])

add("RL-201", "story", "Git fixture repository generator", parent="RL-E2", milestone="M3",
    priority="P0", estimate=5, labels=["testing", "fixtures"], spec=["§16.2"],
    desc="""
    fixtures/build.sh creates fixtures/out/git-basic with 12 commits: a clean commit,
    a planted off-by-one in src/pager.rs, a planted SQL injection in src/db.rs, a
    lockfile-only commit, a merge commit, a 200-file commit, a bot-authored commit,
    and normal filler. Fixed author/date/env so SHAs are byte-identical every run.
    """,
    ac=["Two consecutive builds produce identical commit SHAs",
        "A manifest file records sha -> role (planted_bug, lockfile_only, bot, ...) for tests to reference by role, never by hardcoded sha",
        "Also produces fixtures/out/git-bare as a --mirror clone"],
    gate="./fixtures/build.sh && test -f fixtures/out/git-basic/.manifest.json")

add("RL-202", "story", "SVN fixture repository generator", parent="RL-E2", milestone="M3",
    priority="P0", estimate=5, labels=["testing", "fixtures", "svn"], spec=["§16.2", "§6.4"],
    depends_on=["RL-201"],
    desc="""
    svnadmin create + file:// access. trunk plus branches/feature-x, revisions mirroring
    the git fixture, and a reintegration revision carrying both a matching log message
    and a real svn:mergeinfo property change, so pseudo-PR detection can be tested via
    both heuristics independently.
    """,
    ac=["Manifest records revision -> role including reintegration_rev and its fork point",
        "The reintegration revision genuinely changes svn:mergeinfo (verified with svn propget)",
        "Script skips with a clear message and exit 0 when `svn` is absent, and says so in the manifest"],
    gate="./fixtures/build.sh && test -f fixtures/out/svn-basic/.manifest.json")

add("RL-203", "story", "Mock engine binary", parent="RL-E2", milestone="M3", priority="P0",
    estimate=5, labels=["testing", "fixtures"], spec=["§8.2", "§16.2"],
    desc="""
    A script that impersonates a CLI engine and honours REVLOCAL_OUT. Behaviour is
    selected by env var MOCK_ENGINE_MODE so one fixture covers every branch of the
    §8.2 fallback ladder: valid | malformed_json | fenced_only | no_file | hang |
    partial_findings | nonzero_exit | slow_but_ok.
    """,
    ac=["One mode per rung of the fallback ladder",
        "`hang` mode ignores SIGTERM so the SIGKILL path is genuinely exercised",
        "`valid` mode emits a result.json that validates against result.v1.json",
        "Runs on Windows (a .cmd shim or a portable node script)"],
    gate="MOCK_ENGINE_MODE=valid REVLOCAL_OUT=$(mktemp -d) fixtures/mock-engine/run && echo ok")

add("RL-204", "story", "Mock MCP server with a request journal", parent="RL-E2",
    milestone="M3", priority="P0", estimate=5, labels=["testing", "fixtures", "mcp"],
    spec=["§16.2", "§11.2"],
    desc="""
    Node stdio MCP server with configurable tool names, so capability-mapping tests can
    simulate Andare exposing `create_work_item` instead of `create_issue`. Records every
    request to a journal file that tests assert against. Implements a Trama-like
    update_page that FAILS if the caller did not call get_page for that page first.
    """,
    ac=["Tool names and schemas are configurable via a JSON profile",
        "Journal records method, args and ordering",
        "update_page without a preceding get_page returns an error — this is what makes RL-802 testable",
        "Can be told to return rate-limit and 5xx errors on demand for retry tests"],
    gate="node fixtures/mock-mcp/selftest.js")

add("RL-205", "task", "Windows fixture parity (build.ps1)", parent="RL-E2", milestone="M3",
    priority="P1", estimate=3, labels=["testing", "fixtures", "windows"], spec=["§16.3"],
    depends_on=["RL-201", "RL-202"],
    desc="PowerShell equivalent of build.sh producing byte-identical fixtures on Windows.",
    ac=["Same commit SHAs as the bash script",
        "CRLF does not leak into fixture file contents"],
    gate="pwsh fixtures/build.ps1")

# ─────────────────────────────────────────────────────────────────────────────
# EPIC 3 — Git / GitHub
# ─────────────────────────────────────────────────────────────────────────────
add("RL-E3", "epic", "VCS: git and GitHub adapters", milestone="M4", priority="P0",
    labels=["vcs"], spec=["§6.1", "§6.2", "§6.3"], depends_on=["RL-E2"], desc="""
    Discovery, materialization and hook installation for local git repositories and
    GitHub-hosted ones. The absolute constraint: reviewing must never mutate the
    repository under review.
    """)

add("RL-301", "story", "VcsAdapter trait and scratch-worktree lifecycle", parent="RL-E3",
    milestone="M4", priority="P0", estimate=5, labels=["rust", "vcs"], spec=["§6.1"],
    desc="""
    Define the trait from SPEC §6.1 and the scratch directory lifecycle under
    {data_dir}/scratch/{run_id}/, with RAII cleanup honouring keep_scratch_on_failure.
    """,
    ac=["Scratch dir is removed on drop for a successful run",
        "Scratch dir survives a failed run when keep_scratch_on_failure is set",
        "Two concurrent runs on the same repo get isolated scratch dirs"],
    gate="cargo test -p revlocal-vcs scratch")

add("RL-302", "story", "Git command wrapper with timeouts and no interactive prompts",
    parent="RL-E3", milestone="M4", priority="P0", estimate=3, labels=["rust", "vcs"],
    spec=["§6.2"], depends_on=["RL-301"],
    desc="""
    Single choke point for every git invocation. Sets GIT_TERMINAL_PROMPT=0,
    GIT_ASKPASS=echo, per-call timeout, captures stdout/stderr, maps exit codes to
    typed errors. No other code in the workspace may spawn `git` directly.
    """,
    ac=["A repo requiring credentials fails fast instead of hanging",
        "Timeout kills the child and its process group",
        "A grep-based test asserts no other module spawns `git` directly"],
    gate="cargo test -p revlocal-vcs git::cmd")

add("RL-303", "story", "Git commit discovery with cursor advancement", parent="RL-E3",
    milestone="M4", priority="P0", estimate=8, labels=["rust", "vcs"], spec=["§6.2"],
    depends_on=["RL-302"],
    desc="""
    fetch --prune, then rev-list --reverse --first-parent per watched branch, honouring
    branch globs. Emits DetectedChange in oldest-first order and advances the cursor
    only after a change is durably recorded.
    """,
    ac=["Discovers exactly the expected changes from git-basic (by manifest role, not hardcoded shas)",
        "Cursor advance is crash-safe: killing after discovery but before recording re-discovers the same change, and does not duplicate it",
        "Branch globs (release/*) resolve correctly"],
    gate="cargo test -p revlocal-vcs --test git_discover")

add("RL-304", "story", "Force-push and history-rewrite recovery", parent="RL-E3",
    milestone="M4", priority="P1", estimate=5, labels=["rust", "vcs"], spec=["§6.2"],
    depends_on=["RL-303"],
    desc="""
    When the stored cursor SHA is no longer an ancestor of the branch tip, emit a
    `history_rewritten` audit event, reset the cursor to the merge-base, and
    re-discover forward rather than replaying the entire branch.
    """,
    ac=["Test force-pushes the fixture branch and asserts recovery",
        "An audit row is written",
        "Recovery does not re-review commits that survived the rewrite (dedupe by content hash)"],
    gate="cargo test -p revlocal-vcs --test git_force_push")

add("RL-305", "story", "Skip rules: merges, bots, lockfiles, ignored globs", parent="RL-E3",
    milestone="M4", priority="P0", estimate=5, labels=["rust", "vcs"], spec=["§9.4"],
    depends_on=["RL-303"],
    desc="Implement the skip table from SPEC §9.4, recording an explicit skip_reason for each.",
    ac=["Each skip category has a fixture commit and a test asserting the exact skip_reason",
        "A skipped change still creates a `change` row and a `skipped` run — skips are visible, not invisible",
        "Empty-after-filtering diffs are skipped, not sent to an engine"],
    gate="cargo test -p revlocal-vcs skip_rules")

add("RL-306", "story", "Materialize a change without mutating the source repo", parent="RL-E3",
    milestone="M4", priority="P0", estimate=5, labels=["rust", "vcs", "safety"], spec=["§6.1"],
    depends_on=["RL-302", "RL-301"],
    desc="""
    `git worktree add --detach` into scratch (or `git archive` for bare mirrors), plus
    the unified diff and per-file diffs. The user's working tree, index, HEAD and stash
    must be untouched.
    """,
    ac=["After materializing every fixture commit, `git status --porcelain` on the fixture is empty",
        "The fixture's HEAD and tree hash are byte-identical before and after",
        "Worktree is pruned afterwards (`git worktree list` returns to its prior state)",
        "Works against a bare mirror where worktree add is unavailable"],
    gate="cargo test -p revlocal-vcs --test git_materialize_is_readonly")

add("RL-307", "story", "GitHub transport selection: MCP, gh CLI, unauthenticated",
    parent="RL-E3", milestone="M4", priority="P1", estimate=5, labels=["rust", "vcs", "github"],
    spec=["§6.3"], depends_on=["RL-302"],
    desc="""
    Priority ladder from SPEC §6.3. The chosen transport is recorded on the repo row and
    reported by `revlocal doctor`, so a user always knows how the app is reaching GitHub.
    """,
    ac=["Ladder falls through in order and reports which rung it landed on",
        "Unauthenticated mode is read-only and refuses every write with a clear error",
        "doctor output names the transport and, on failure, the remediation"],
    gate="cargo test -p revlocal-vcs github::transport")

add("RL-308", "story", "Pull request discovery keyed by head SHA", parent="RL-E3",
    milestone="M4", priority="P0", estimate=5, labels=["rust", "vcs", "github"], spec=["§6.3"],
    depends_on=["RL-307"],
    desc="""
    Open PRs against watched branches become Changes with external_id "{number}:{head_sha}",
    so a re-push is a new Change. Drafts skipped by default. Commits already covered by an
    open PR are skipped with skip_reason='covered_by_pr'.
    """,
    ac=["Re-push produces a second Change, not a mutated first one",
        "Findings from the prior head SHA whose fingerprints do not recur become `superseded`",
        "Draft PRs are skipped unless review_draft_prs is enabled",
        "covered_by_pr skip is exercised with both commit and PR review enabled"],
    gate="cargo test -p revlocal-vcs --test github_pr_discover")

# ─────────────────────────────────────────────────────────────────────────────
# EPIC 4 — Engines
# ─────────────────────────────────────────────────────────────────────────────
add("RL-E4", "epic", "Review engines: Claude Code and Codex runners", milestone="M5",
    priority="P0", labels=["engine"], spec=["§8"], depends_on=["RL-E2"], desc="""
    Invoke locally installed AI CLIs as review engines, without depending on any
    specific CLI's structured-output flag. The output contract and its fallback ladder
    are the load-bearing design here.
    """)

add("RL-401", "story", "Engine trait, task and outcome types", parent="RL-E4", milestone="M5",
    priority="P0", estimate=3, labels=["rust", "engine"], spec=["§8.1"],
    desc="Implement the trait and structs from SPEC §8.1, plus EngineId and EngineProbe.",
    ac=["Mock engine implements the trait and is usable from tests",
        "Trait is object-safe (Box<dyn Engine> works)"],
    gate="cargo test -p revlocal-engine trait_")

add("RL-402", "story", "result.json schema and validator", parent="RL-E4", milestone="M5",
    priority="P0", estimate=3, labels=["rust", "engine"], spec=["§8.3"],
    depends_on=["RL-401"],
    desc="""
    Commit crates/revlocal-engine/schema/result.v1.json matching SPEC §8.3 and validate
    every engine output against it. A finding that fails validation is dropped
    individually with an audit event; the run still succeeds if the envelope parsed.
    """,
    ac=["Valid document passes",
        "A single malformed finding is dropped, the rest survive, and an audit row records the drop",
        "A malformed envelope fails the run rather than inventing findings",
        "schema_version mismatch is a typed error with an actionable message"],
    gate="cargo test -p revlocal-engine schema")

add("RL-403", "story", "Config-driven invocation templates", parent="RL-E4", milestone="M5",
    priority="P0", estimate=5, labels=["rust", "engine"], spec=["§8.4"],
    depends_on=["RL-401"],
    desc="""
    Render {cwd}, {out_dir}, {prompt_file}, {prompt_file_content}, {timeout_secs} into
    an argv template. Support stdin_prompt. Ship the claude and codex defaults from
    SPEC §8.4 but treat them as defaults, never as assumptions.
    """,
    ac=["All placeholder forms render correctly, including on Windows paths with spaces",
        "An unknown placeholder is a config error at load time, not a runtime surprise",
        "Argv is never shell-interpolated (no shell injection via a prompt or a path)"],
    gate="cargo test -p revlocal-engine template")

add("RL-404", "story", "Output contract with the four-rung fallback ladder", parent="RL-E4",
    milestone="M5", priority="P0", estimate=8, labels=["rust", "engine"], spec=["§8.2"],
    depends_on=["RL-402", "RL-403", "RL-203"],
    desc="""
    Read $REVLOCAL_OUT/result.json. On failure: last fenced json block -> whole stdout
    -> one repair invocation -> fail with engine_output_unparseable. Any fallback sets
    `degraded`, which later escalates publish risk.
    """,
    ac=["One test per mock-engine mode, asserting which rung handled it",
        "The repair pass runs at most once and its tokens are charged to the budget",
        "engine_output_unparseable preserves the transcript and never fabricates findings",
        "`degraded` is set for rungs a, b and c, and unset for rung 0"],
    gate="cargo test -p revlocal-engine --test fallback_ladder")

add("RL-405", "story", "Process supervision: timeout, cancellation, no orphans", parent="RL-E4",
    milestone="M5", priority="P0", estimate=8, labels=["rust", "engine", "platform"],
    spec=["§8.5"], depends_on=["RL-403"],
    desc="""
    Depth-scaled wall-clock timeout, SIGTERM then 5s grace then SIGKILL. Unix: spawn in a
    new process group and signal the group. Windows: Job Object so children die with the
    parent. Cancellation token wired to the kill switch.
    """,
    ac=["Hung mock engine is killed within timeout + 2s",
        "No orphan process survives — asserted by checking the recorded PID and its children",
        "A grandchild process spawned by the engine also dies",
        "Test passes on all three platforms in CI"],
    gate="cargo test -p revlocal-engine --test process_supervision")

add("RL-406", "story", "Environment denylist for engine processes", parent="RL-E4",
    milestone="M5", priority="P0", estimate=3, labels=["rust", "engine", "security"],
    spec=["§8.5"], depends_on=["RL-403"],
    desc="""
    Inherit the environment so CLI logins work, minus GITHUB_TOKEN, GH_TOKEN, *_API_KEY,
    *_SECRET, *_PASSWORD, unless explicitly allowed by engines.<id>.pass_env. The review
    engine has no business acting on remotes.
    """,
    ac=["A GITHUB_TOKEN in the parent env is absent from the child env",
        "pass_env allows a named variable through",
        "Test asserts the child's actual environment, not just the builder's intent"],
    gate="cargo test -p revlocal-engine env_denylist")

add("RL-407", "story", "Claude Code and Codex runner implementations", parent="RL-E4",
    milestone="M5", priority="P0", estimate=5, labels=["rust", "engine"], spec=["§8.4"],
    depends_on=["RL-404", "RL-405", "RL-406"],
    desc="Concrete Engine impls wiring the templates, plus probe() implementing version detection.",
    ac=["probe() reports binary present/absent, version, and whether a smoke task succeeded",
        "Absence of the binary is a clear prerequisite error, never a panic",
        "Both engines are selectable per repo"],
    gate="cargo test -p revlocal-engine runners")

add("RL-408", "spike", "SPIKE: Codex CLI structured-output and sandbox behaviour",
    parent="RL-E4", milestone="M5", priority="P1", estimate=3, labels=["spike", "engine"],
    spec=["§8.2", "§8.4"],
    desc="""
    Timeboxed investigation. `codex` is not currently installed on the build machine.
    Determine: the exact exec invocation and flags for non-interactive use; whether the
    sandbox permits writing to REVLOCAL_OUT when it is outside --cd; what its JSON output
    actually looks like; and whether the default invocation template in SPEC §8.4 is right.
    """,
    ac=["Findings written to docs/adr/ as an ADR",
        "SPEC §8.4 codex defaults updated to match reality",
        "If the sandbox blocks REVLOCAL_OUT, the ADR proposes a concrete fix (e.g. put out_dir inside cwd)"],
    notes="Timebox: 4 hours. Blocked until `codex` is installed on the build machine.")

# ─────────────────────────────────────────────────────────────────────────────
# EPIC 5 — Pipeline
# ─────────────────────────────────────────────────────────────────────────────
add("RL-E5", "epic", "Review pipeline", milestone="M6", priority="P0", labels=["pipeline"],
    spec=["§9", "§10"], depends_on=["RL-E3", "RL-E4"], desc="""
    The stage machine that turns a detected change into normalized, deduplicated,
    risk-classified findings.
    """)

add("RL-501", "story", "Run state machine and crash recovery", parent="RL-E5", milestone="M6",
    priority="P0", estimate=5, labels=["rust", "pipeline"], spec=["§9.1"],
    depends_on=["RL-109"],
    desc="""
    Stage transitions from SPEC §9.1, each persisted and emitted as an event. On startup,
    non-terminal runs older than stale_run_minutes become failed('interrupted') and are
    re-enqueued exactly once.
    """,
    ac=["Illegal transitions are rejected",
        "Simulated crash mid-`reviewing` recovers on next startup",
        "Recovery re-enqueues once, not in a loop (attempt counter is respected)"],
    gate="cargo test -p revlocal-daemon state_machine")

add("RL-502", "story", "Prompt assembly with the seven required sections", parent="RL-E5",
    milestone="M6", priority="P0", estimate=8, labels=["rust", "pipeline", "prompt"],
    spec=["§9.2"], depends_on=["RL-306"],
    desc="""
    Render the review prompt template with the SPEC §9.2 sections in order: role/output
    contract, change metadata, diff, repo conventions, review scope, prior context, rules
    of engagement. Convention files are read from the materialized worktree and capped.
    """,
    ac=["Section order matches §9.2 exactly (snapshot test)",
        "CLAUDE.md/AGENTS.md/CONTRIBUTING.md are picked up when present and their absence is not an error",
        "Convention content is capped at max_convention_bytes with the truncation stated in the prompt",
        "Suppressed fingerprints appear in the prior-context section as do-not-report",
        "The rendered prompt is persisted next to the transcript"],
    gate="cargo test -p revlocal-daemon prompt::")

add("RL-503", "story", "Depth selection", parent="RL-E5", milestone="M6", priority="P1",
    estimate=5, labels=["rust", "pipeline"], spec=["§9.3"], depends_on=["RL-501"],
    desc="Implement the §9.3 tier table, including escalation to deep on a critical/high finding.",
    ac=["200-file fixture commit selects summary",
        "A commit touching a sensitive_glob selects deep",
        "A standard run producing a critical finding triggers a deep re-run exactly once",
        "Timeouts scale with depth"],
    gate="cargo test -p revlocal-daemon depth")

add("RL-504", "story", "Diff truncation that never hides a file", parent="RL-E5",
    milestone="M6", priority="P0", estimate=5, labels=["rust", "pipeline", "safety"],
    spec=["§9.4", "§18"], depends_on=["RL-502"],
    desc="""
    Per-file and total diff caps with interest-ordered retention (source > tests > config
    > data). The omitted-file list is ALWAYS included in full — a review that saw 60% of
    the diff must never look like one that saw all of it.
    """,
    ac=["Truncated run sets context.truncated and the run surfaces it",
        "Every omitted file is named in the prompt even when its content is dropped",
        "Interest ordering is asserted with a mixed-type fixture commit",
        "Binary files are summarized, never emitted as bytes"],
    gate="cargo test -p revlocal-daemon truncation")

add("RL-505", "story", "Finding normalization, suppression and dedupe", parent="RL-E5",
    milestone="M6", priority="P0", estimate=5, labels=["rust", "pipeline"], spec=["§9.5", "§10"],
    depends_on=["RL-106", "RL-402"],
    desc="""
    Clamp severity, handle out-of-diff findings per §9.5, apply suppressions, compute
    fingerprints, and mark repeats of an already-published fingerprint as superseded
    rather than re-filing.
    """,
    ac=["Out-of-diff finding is retained but capped at medium when allow_out_of_diff_findings is false",
        "A suppressed fingerprint never reaches the publish plan",
        "A repeat fingerprint on the same PR is superseded, not duplicated",
        "Unknown severity maps to medium rather than being dropped"],
    gate="cargo test -p revlocal-daemon normalize")

add("RL-506", "story", "End-to-end pipeline with the mock engine", parent="RL-E5",
    milestone="M6", priority="P0", estimate=5, labels=["rust", "pipeline", "e2e"],
    spec=["§9"], depends_on=["RL-501", "RL-502", "RL-503", "RL-504", "RL-505"],
    desc="Wire every stage together and expose it as `revlocal review --repo R --rev X --json`.",
    ac=["Reviewing the planted-bug commit yields a done run with >= 1 finding",
        "The 200-file commit yields depth=summary and truncated=true",
        "Output is stable JSON suitable for test assertions",
        "The whole flow runs with zero network access (asserted by a no-network test wrapper)"],
    gate="cargo test -p revlocal-daemon --test pipeline_e2e")

add("RL-507", "story", "Determinism guarantee", parent="RL-E5", milestone="M6", priority="P1",
    estimate=3, labels=["rust", "pipeline"], spec=["§18"], depends_on=["RL-506"],
    desc="Same change + same engine output => same findings, fingerprints and publish plan.",
    ac=["Running the same review twice with a fixed mock engine yields byte-identical findings and publish plans",
        "No HashMap iteration order leaks into output ordering"],
    gate="cargo test -p revlocal-daemon determinism")

# ─────────────────────────────────────────────────────────────────────────────
# EPIC 6 — MCP
# ─────────────────────────────────────────────────────────────────────────────
add("RL-E6", "epic", "MCP client and capability mapping", milestone="M7", priority="P0",
    labels=["mcp"], spec=["§11.2"], depends_on=["RL-E2"], desc="""
    Speak MCP to arbitrary servers, discover their real tool surface, and bind abstract
    capabilities onto concrete tools — so integrating Andare does not require knowing
    Andare's tool names at build time.
    """)

add("RL-601", "story", "MCP client: stdio transport", parent="RL-E6", milestone="M7",
    priority="P0", estimate=5, labels=["rust", "mcp"], spec=["§11.2"],
    desc="Spawn a server process, initialize, tools/list, tools/call, with lifecycle and error mapping.",
    ac=["Connects to fixtures/mock-mcp and lists tools",
        "Server crash is surfaced as a typed error and the client reconnects on next use",
        "Child process is reaped, not leaked, on drop"],
    gate="cargo test -p revlocal-mcp stdio")

add("RL-602", "story", "MCP client: streamable HTTP transport", parent="RL-E6", milestone="M7",
    priority="P0", estimate=5, labels=["rust", "mcp"], spec=["§11.2"], depends_on=["RL-601"],
    desc="""
    HTTP transport with bearer auth resolved from the keychain at connect time. This is
    the transport Trama uses today, so it must work against a real remote server.
    """,
    ac=["initialize + tools/list succeed against a local HTTP mock",
        "Auth header value never appears in logs or error messages",
        "Non-2xx responses map to typed, actionable errors",
        "Handles both application/json and text/event-stream responses"],
    gate="cargo test -p revlocal-mcp http")

add("RL-603", "story", "Tool discovery cache and health reporting", parent="RL-E6",
    milestone="M7", priority="P1", estimate=3, labels=["rust", "mcp"], spec=["§11.2"],
    depends_on=["RL-601", "RL-602"],
    desc="Cache tools/list with schemas per server; expose health for the UI and for doctor.",
    ac=["Cache invalidates on reconnect",
        "doctor prints 'server: N tools, M capabilities mapped, K unmapped'",
        "An unreachable server degrades that target only — other targets keep working"],
    gate="cargo test -p revlocal-mcp discovery")

add("RL-604", "story", "Capability mapping with candidate resolution and schema validation",
    parent="RL-E6", milestone="M7", priority="P0", estimate=8, labels=["rust", "mcp"],
    spec=["§11.2"], depends_on=["RL-603"],
    desc="""
    Resolve tool_candidates lists against discovered names, render arg templates, and
    validate the rendered args against the tool's own JSON Schema BEFORE calling it.
    An unresolvable capability is reported as unmapped — never guessed at.
    """,
    ac=["Resolves create_issue -> create_work_item when that is what the server exposes",
        "Reports unmapped instead of calling a plausible-looking wrong tool",
        "Refuses to send a payload that fails the tool's input schema, with a message naming the offending field",
        "Mapping state is visible via `revlocal targets list`"],
    gate="cargo test -p revlocal-mcp --test capability_mapping")

add("RL-605", "story", "Manual capability override", parent="RL-E6", milestone="M7",
    priority="P2", estimate=3, labels=["rust", "mcp", "ux"], spec=["§11.2"],
    depends_on=["RL-604"],
    desc="`revlocal targets map <target> <capability> --tool T --arg k=template` persists an override.",
    ac=["Override survives restart",
        "Override is validated against the tool schema at save time, not at first use",
        "`revlocal targets test <target>` performs a dry-run render of every mapped capability"],
    gate="cargo test -p revlocal-mcp override")

add("RL-606", "spike", "SPIKE: discover Andare's real MCP tool surface", parent="RL-E6",
    milestone="M7", priority="P0", estimate=2, labels=["spike", "mcp", "andare"],
    spec=["§11.4"],
    desc="""
    BLOCKING for RL-E7 Andare work. The Andare MCP server is not currently configured on
    the build machine (only `trama` is). Obtain its endpoint and token, connect, run
    tools/list, and record the real tool names and input schemas.
    """,
    ac=["Andare MCP server reachable and `tools/list` captured into docs/adr/",
        "targets.andare tool_candidates in the default config updated to the real names",
        "Confirmed which of create_issue / set_status / comment / search are actually expressible",
        "If a capability cannot be expressed even with manual mapping, escalate per BUILD_LOOP §5"],
    notes="Timebox: 2 hours. Blocked on credentials from the product owner.")

# ─────────────────────────────────────────────────────────────────────────────
# EPIC 7 — Publishing
# ─────────────────────────────────────────────────────────────────────────────
add("RL-E7", "epic", "Publish targets: GitHub, Andare, Trama", milestone="M8-M9",
    priority="P0", labels=["publish"], spec=["§11"], depends_on=["RL-E5", "RL-E6"], desc="""
    Getting review outcomes into the systems of record, idempotently, with receipts.
    """)

add("RL-701", "story", "PublishTarget trait, action queue and receipts", parent="RL-E7",
    milestone="M8", priority="P0", estimate=5, labels=["rust", "publish"], spec=["§11.1", "§11.6"],
    desc="""
    Bounded queue with concurrency 4, per-target rate limiting, and the guarantee that
    every action is persisted BEFORE it is attempted.
    """,
    ac=["A crash between persist and send leaves a pending row that is retried on startup",
        "A slow target cannot block reviewing",
        "Per-target rate limits are honoured"],
    gate="cargo test -p revlocal-publish queue")

add("RL-702", "story", "Idempotency and retry policy", parent="RL-E7", milestone="M8",
    priority="P0", estimate=5, labels=["rust", "publish", "safety"], spec=["§11.6"],
    depends_on=["RL-701"],
    desc="""
    UNIQUE(target, idempotency_key) makes double-publish structurally impossible.
    5 attempts, exponential backoff with jitter capped at 60s, retry only on
    transport/5xx/429. 4xx is terminal.
    """,
    ac=["Replaying the same action is a no-op that returns the original receipt",
        "429 is retried with backoff; 400 is not retried",
        "Attempt count and last error are visible on the action row",
        "Backoff has jitter (two concurrent retries do not align)"],
    gate="cargo test -p revlocal-publish idempotency")

add("RL-703", "story", "GitHub: PR review with inline comments", parent="RL-E7", milestone="M8",
    priority="P0", estimate=8, labels=["rust", "publish", "github"], spec=["§11.3"],
    depends_on=["RL-702", "RL-307"],
    desc="""
    One review per run: body = summary + findings table, inline comments anchored to
    (file, line range) on the head SHA. Unanchorable comments are demoted into a
    "Findings outside the diff" section rather than dropped.
    """,
    ac=["Second publish for the same head SHA edits the existing review, does not post a second",
        "An unanchorable comment appears in the body section",
        "Review body renders correctly with zero findings",
        "APPROVE is never submitted unless allow_approve is explicitly enabled"],
    gate="cargo test -p revlocal-publish --test github_review")

add("RL-704", "story", "GitHub: commit comments and check runs", parent="RL-E7", milestone="M8",
    priority="P1", estimate=5, labels=["rust", "publish", "github"], spec=["§11.3"],
    depends_on=["RL-703"],
    desc="""
    Non-PR commit review posts a commit comment. The rev-local/review check run is
    in_progress during the run and resolves to success/neutral/failure per
    block_on_findings.
    """,
    ac=["Check is set in_progress at run start and always resolved, even on run failure",
        "failure only when block_on_findings is on; neutral otherwise",
        "A crashed run leaves no permanently in_progress check (startup reconciliation)"],
    gate="cargo test -p revlocal-publish github_check")

add("RL-705", "story", "Andare: file findings as issues with fingerprint dedupe",
    parent="RL-E7", milestone="M9", priority="P0", estimate=8, labels=["rust", "publish", "andare"],
    spec=["§11.4"], depends_on=["RL-702", "RL-604", "RL-606"],
    desc="""
    Findings at or above andare_min_severity become issues carrying a
    `rev-local-fingerprint: <fp>` trailer. Before filing, search for an existing open
    issue with that trailer and comment on it instead of duplicating.
    """,
    ac=["Re-running the same review produces a comment on the existing issue, not a second issue",
        "Issue body contains claim, failure scenario, code excerpt, change link and Trama page link",
        "Below-threshold findings do not create issues",
        "If the search capability is unmapped, the target degrades to 'comment only' and says so, rather than duplicating"],
    gate="cargo test -p revlocal-publish --test andare_issues")

add("RL-706", "story", "Andare: report review outcome onto the linked work item",
    parent="RL-E7", milestone="M9", priority="P1", estimate=5, labels=["rust", "publish", "andare"],
    spec=["§11.4"], depends_on=["RL-705"],
    desc="""
    Extract a work-item key from the commit message / PR title / SVN log using
    andare_key_regex, then comment the review outcome on that ticket and optionally
    transition its status per andare_transition_on.
    """,
    ac=["Key extraction handles multiple keys, no key, and a key inside a URL",
        "A comment is always posted; a transition only when configured and valid",
        "An invalid transition is a recorded failure, not a crash",
        "Status transition is classified high risk (see RL-802)"],
    gate="cargo test -p revlocal-publish andare_status")

add("RL-707", "story", "Trama: read-before-write page upsert", parent="RL-E7", milestone="M9",
    priority="P0", estimate=8, labels=["rust", "publish", "trama"], spec=["§11.5"],
    depends_on=["RL-702", "RL-604"],
    desc="""
    Trama's update_page REPLACES the body. Therefore every update must get_page first,
    merge, and send the whole document. Never send a fragment. Page identity, titling
    and parent placement per SPEC §11.5.
    """,
    ac=["Mock-MCP journal shows get_page immediately preceding every update_page for the same page",
        "The update payload contains the full merged body, not a fragment",
        "create_page is used only when the page genuinely does not exist (title collision is handled)",
        "A hand-edited page is not clobbered: human-authored sections outside rev-local's markers survive an update"],
    gate="cargo test -p revlocal-publish --test trama_upsert")

add("RL-708", "story", "Trama: per-repo review index regenerated from SQLite", parent="RL-E7",
    milestone="M9", priority="P1", estimate=5, labels=["rust", "publish", "trama"], spec=["§11.5"],
    depends_on=["RL-707"],
    desc="""
    A rolling '{repo} Review Index' page listing the last N reviews. Because updates
    replace bodies, the index is regenerated from SQLite every time — SQLite is the
    source of truth, Trama is a projection. This makes it safe to lose or hand-edit.
    """,
    ac=["Deleting the index page and re-running restores it completely",
        "Index respects the N-most-recent cap and states the cap on the page",
        "Review pages link to the index with a [[wikilink]] and vice versa"],
    gate="cargo test -p revlocal-publish trama_index")

add("RL-709", "story", "Trama: publish_page gating and issue cross-linking", parent="RL-E7",
    milestone="M9", priority="P2", estimate=3, labels=["rust", "publish", "trama"], spec=["§11.5"],
    depends_on=["RL-707", "RL-705"],
    desc="""
    publish_page only when trama_publish is set; an unpublished draft is a low-risk
    action while publishing is high-risk. link_to_issue cross-links the review page to
    any Andare issue filed from it.
    """,
    ac=["Draft creation is low risk, publish_page is high risk (asserted against the risk model)",
        "link_to_issue is called with the real issue key returned by Andare, never a guessed one",
        "Cross-link failure does not fail the run"],
    gate="cargo test -p revlocal-publish trama_publish_gate")

add("RL-710", "story", "Partial-failure reporting and per-target retry", parent="RL-E7",
    milestone="M9", priority="P1", estimate=3, labels=["rust", "publish", "ux"], spec=["§11.6"],
    depends_on=["RL-701"],
    desc="A run can be done with GitHub posted and Andare failed. Report per-target status and allow retrying one target.",
    ac=["Run detail exposes status per target",
        "`revlocal publish replay --run R --target T` retries only that target",
        "A permanently failed target does not block the run from reaching a terminal state"],
    gate="cargo test -p revlocal-publish partial_failure")

# ─────────────────────────────────────────────────────────────────────────────
# EPIC 8 — Autonomy
# ─────────────────────────────────────────────────────────────────────────────
add("RL-E8", "epic", "Autonomy, risk gating, approvals and budgets", milestone="M10",
    priority="P0", labels=["safety"], spec=["§12"], depends_on=["RL-E7"], desc="""
    The controls that make unattended operation acceptable: modes, the kill switch, the
    approvals inbox, and spend limits.
    """)

add("RL-801", "story", "Autonomy modes and the global ceiling", parent="RL-E8", milestone="M10",
    priority="P0", estimate=5, labels=["rust", "safety"], spec=["§12.2"],
    depends_on=["RL-105", "RL-701"],
    desc="""
    off | dry_run | auto_low_ask_high | auto, per repo, with the effective mode being
    min(global, repo) under that ordering.
    """,
    ac=["dry_run performs zero MCP writes — asserted against the mock-MCP journal being empty of writes",
        "dry_run still records the exact payload that would have been sent, and renders it in the UI",
        "Global off overrides a repo set to auto",
        "Mode changes take effect without restart and are audited"],
    gate="cargo test -p revlocal-daemon --test autonomy_modes")

add("RL-802", "story", "Risk gating wired into the publish queue", parent="RL-E8",
    milestone="M10", priority="P0", estimate=5, labels=["rust", "safety"], spec=["§12.3"],
    depends_on=["RL-801"],
    desc="Every action is risk-classified at enqueue time; auto_low_ask_high sends low and queues high.",
    ac=["A PR comment is sent while an Andare issue from the same run is queued",
        "First use of a (target, capability) pair classifies as high risk, so it is queued under auto_low_ask_high and sent under auto — the test asserts both halves",
        "A degraded run escalates all of its actions to high",
        "Burst threshold escalation triggers after the configured count within an hour"],
    gate="cargo test -p revlocal-daemon --test risk_gating")

add("RL-803", "story", "Approvals inbox", parent="RL-E8", milestone="M10", priority="P0",
    estimate=5, labels=["rust", "ux"], spec=["§12.4"], depends_on=["RL-802"],
    desc="""
    Queued actions with the exact payload rendered as the target would render it.
    Approve / approve-all-for-run / reject / reject-and-suppress / edit-then-approve.
    TTL expiry to rejected('expired'), always audited.
    """,
    ac=["Approve sends exactly the reviewed payload — an edit after approval is impossible",
        "Reject-and-suppress creates a suppression that a later run honours",
        "Expiry is audited and visible, not silent",
        "CLI and UI operate on the same queue"],
    gate="cargo test -p revlocal-daemon --test approvals")

add("RL-804", "story", "Kill switch", parent="RL-E8", milestone="M10", priority="P0",
    estimate=5, labels=["rust", "safety"], spec=["§12.1"], depends_on=["RL-405", "RL-701"],
    desc="""
    Cancel every in-flight engine process, drain the run queue to cancelled, HOLD the
    publish queue without losing it, stop all triggers, persist the paused state across
    restart. `revlocal kill --hard` also reaps orphaned engine PIDs.
    """,
    ac=["A running mock engine is cancelled within 3 seconds",
        "Pending publish actions still exist after a kill and resume, and are sent on resume",
        "Paused state survives a restart",
        "No orphan engine process remains after kill --hard"],
    gate="cargo test -p revlocal-daemon --test kill_switch")

add("RL-805", "story", "Budgets and concurrency caps", parent="RL-E8", milestone="M10",
    priority="P0", estimate=5, labels=["rust", "safety"], spec=["§13.1", "§4.3"],
    depends_on=["RL-110", "RL-501"],
    desc="""
    Per-repo daily token/run/cost ceilings and a global concurrency semaphore.
    on_exhausted = pause | queue | skip — but never silent drop.
    """,
    ac=["Exhausting the token budget pauses the repo and surfaces a reason",
        "Changes detected while paused are still recorded and are reviewed after reset",
        "max_concurrent_runs is genuinely enforced (a test with 5 queued runs observes at most N concurrent engine spawns)",
        "Day rollover resumes automatically"],
    gate="cargo test -p revlocal-daemon --test budgets")

# ─────────────────────────────────────────────────────────────────────────────
# EPIC 9 — SVN
# ─────────────────────────────────────────────────────────────────────────────
add("RL-E9", "epic", "Subversion support", milestone="M11", priority="P0", labels=["vcs", "svn"],
    spec=["§6.4"], depends_on=["RL-E5"], desc="""
    Per-revision review plus the synthesized branch-level pseudo-PR on reintegration —
    the feature that gives SVN users PR-grade review without PRs.
    """)

add("RL-901", "story", "SVN command wrapper and prerequisite detection", parent="RL-E9",
    milestone="M11", priority="P0", estimate=3, labels=["rust", "svn"], spec=["§6.4"],
    desc="""
    Choke point for svn/svnlook invocations with timeouts and non-interactive flags.
    Absence of `svn` is a blocking prerequisite for SVN repos ONLY — git repos keep working.
    """,
    ac=["Missing svn binary produces an actionable doctor message, not a panic",
        "Git repos are unaffected when svn is absent",
        "--non-interactive and --trust-server-cert-failures behaviour is explicit and configurable"],
    gate="cargo test -p revlocal-vcs svn::cmd")

add("RL-902", "story", "SVN revision discovery over watched paths", parent="RL-E9",
    milestone="M11", priority="P0", estimate=5, labels=["rust", "svn"], spec=["§6.4"],
    depends_on=["RL-901", "RL-202"],
    desc="`svn log --xml -r cursor+1:HEAD -v` over trunk and optionally branches/*, oldest first.",
    ac=["Discovers the fixture's revisions in ascending order",
        "Cursor is the revision number and advances only after recording",
        "Path filtering excludes revisions touching only unwatched paths",
        "Handles a revision with no changed paths without failing"],
    gate="cargo test -p revlocal-vcs --test svn_discover")

add("RL-903", "story", "SVN materialization", parent="RL-E9", milestone="M11", priority="P0",
    estimate=5, labels=["rust", "svn"], spec=["§6.4"], depends_on=["RL-902"],
    desc="`svn diff -c REV` plus `svn export -r REV` into scratch. Binary and property-only changes summarized.",
    ac=["Export produces a tree at the correct revision",
        "Property-only changes yield a summary, not an empty diff that looks like nothing happened",
        "Binary files are summarized with size and type",
        "The user's working copy is never touched"],
    gate="cargo test -p revlocal-vcs --test svn_materialize")

add("RL-904", "feature", "SVN pseudo-PR synthesis on branch reintegration", parent="RL-E9",
    milestone="M11", priority="P0", estimate=8, labels=["rust", "svn", "feature"], spec=["§6.4"],
    depends_on=["RL-903"],
    desc="""
    Detect a reintegration via (1) svn:mergeinfo gaining ranges from a branch path,
    (2) the merge_detect_regex on the log message, or (3) file-count plus a named branch
    that exists. Emit an ADDITIONAL svn_pseudo_pr change whose diff is the whole
    branch-vs-trunk diff at the fork point, not the merge revision's diff.
    """,
    ac=["The fixture's reintegration revision produces BOTH a svn_rev and a svn_pseudo_pr change",
        "Each detection heuristic is independently unit-tested with the others disabled",
        "The pseudo-PR diff equals `svn diff trunk@fork branch@rev` and differs from the merge revision's diff",
        "Fork-point determination is tested against a branch with intervening trunk merges",
        "A false positive (a commit merely mentioning the word 'merge') does not create a pseudo-PR"],
    gate="cargo test -p revlocal-vcs --test svn_pseudo_pr")

add("RL-905", "story", "Pseudo-PR authority and demotion of constituent reviews", parent="RL-E9",
    milestone="M11", priority="P1", estimate=5, labels=["rust", "svn", "publish"], spec=["§6.4"],
    depends_on=["RL-904"],
    desc="""
    The pseudo-PR review is authoritative for the merge; findings from per-revision
    branch reviews are attached as prior context and demoted in the publish plan so the
    same defect is not filed twice.
    """,
    ac=["A defect found on both a branch revision and the pseudo-PR is filed once",
        "Per-revision branch findings appear as prior context in the pseudo-PR prompt",
        "Demotion is visible in the publish plan, not silently dropped"],
    gate="cargo test -p revlocal-vcs svn_demotion")

add("RL-906", "spike", "SPIKE: svn:mergeinfo reliability across SVN versions", parent="RL-E9",
    milestone="M11", priority="P2", estimate=2, labels=["spike", "svn"], spec=["§6.4"],
    desc="""
    Timeboxed. Determine how reliable mergeinfo-based detection is across svn 1.8-1.14
    and across merge styles (reintegrate, sync merge, cherry-pick, --record-only).
    Decide whether heuristic ordering in SPEC §6.4 needs revising.
    """,
    ac=["ADR recording which merge styles each heuristic catches",
        "SPEC §6.4 updated if the ordering is wrong",
        "Fixture extended with any merge style found to be mishandled"],
    notes="Timebox: 3 hours.")

# ─────────────────────────────────────────────────────────────────────────────
# EPIC 10 — Triggers
# ─────────────────────────────────────────────────────────────────────────────
add("RL-E10", "epic", "Triggers: poll, git hooks, webhooks, manual", milestone="M12",
    priority="P0", labels=["triggers"], spec=["§7"], depends_on=["RL-E5"], desc="""
    Four ways to learn that something changed, all funnelling through one coalescing bus
    so that four simultaneous signals cannot produce four duplicate reviews.
    """)

add("RL-1001", "story", "TriggerBus with coalescing", parent="RL-E10", milestone="M12",
    priority="P0", estimate=5, labels=["rust", "triggers"], spec=["§7"], depends_on=["RL-501"],
    desc="""
    All sources emit TriggerEvent; events for the same repo within coalesce_window_ms
    collapse into one discovery pass. Triggers never review directly.
    """,
    ac=["Four simultaneous triggers for one repo produce exactly one discovery pass",
        "Triggers for different repos are not coalesced together",
        "An event arriving during a discovery pass schedules exactly one follow-up pass"],
    gate="cargo test -p revlocal-daemon --test trigger_coalescing")

add("RL-1002", "story", "Polling with jitter and exponential backoff", parent="RL-E10",
    milestone="M12", priority="P0", estimate=3, labels=["rust", "triggers"], spec=["§7.1"],
    depends_on=["RL-1001"],
    desc="Per-repo interval (min 30s) with ±10% jitter, backing off to 30 min after consecutive failures, surfacing repo health as degraded.",
    ac=["Interval below the minimum is clamped with a warning",
        "Backoff escalates and then recovers on the first success",
        "Health state is observable via `revlocal repo show --json`"],
    gate="cargo test -p revlocal-daemon poll")

add("RL-1003", "story", "Loopback trigger receiver", parent="RL-E10", milestone="M12",
    priority="P0", estimate=3, labels=["rust", "triggers", "security"], spec=["§7.2"],
    depends_on=["RL-1001"],
    desc="Bind 127.0.0.1:{trigger_port} only, authenticate with a per-repo shared secret, emit a TriggerEvent.",
    ac=["Binds loopback only — a test asserts it is not reachable on a non-loopback interface",
        "A request without the correct secret is rejected",
        "Port conflict produces an actionable error and a suggested alternative port"],
    gate="cargo test -p revlocal-daemon trigger_receiver")

add("RL-1004", "story", "Git hook installer that never breaks a developer's commit",
    parent="RL-E10", milestone="M12", priority="P0", estimate=8,
    labels=["rust", "triggers", "safety"], spec=["§7.2"], depends_on=["RL-1003"],
    desc="""
    Install post-commit/post-merge/post-checkout (reference mode) or post-receive
    (bare-mirror mode). Hooks fire-and-forget with a 2s timeout and ALWAYS exit 0.
    Existing hooks are appended to inside delimited markers, never clobbered.
    """,
    ac=["With the receiver DOWN, `git commit` in the fixture still succeeds in under 2 seconds",
        "An existing user hook is byte-identical after install followed by uninstall",
        "Install is idempotent — running it twice does not duplicate the block",
        "Hooks work on Windows (correct line endings and a .cmd shim where needed)",
        "bare-mirror mode installs post-receive on fixtures/out/git-bare and fires on push"],
    gate="cargo test -p revlocal-daemon --test git_hooks")

add("RL-1005", "story", "GitHub webhook listener with signature verification", parent="RL-E10",
    milestone="M12", priority="P1", estimate=5, labels=["rust", "triggers", "security"],
    spec=["§7.3"], depends_on=["RL-1001"],
    desc="""
    axum listener validating X-Hub-Signature-256 with a per-repo secret. Handles push and
    pull_request events. OFF by default, explicit opt-in per repo.
    """,
    ac=["A bad signature is rejected with 401 and audited",
        "Signature comparison is constant-time",
        "Replayed deliveries (same X-GitHub-Delivery) are ignored",
        "Listener is disabled unless webhook_port is set AND the repo opted in"],
    gate="cargo test -p revlocal-daemon --test webhook")

add("RL-1006", "story", "Tunnel providers and webhook registration", parent="RL-E10",
    milestone="M12", priority="P2", estimate=5, labels=["rust", "triggers"], spec=["§7.3"],
    depends_on=["RL-1005"],
    desc="Pluggable cloudflared | ngrok | manual providers; capture the assigned public URL and offer one-click webhook registration via the GitHub transport.",
    ac=["Missing tunnel binary is reported clearly with install guidance, not a crash",
        "Public URL is captured and displayed",
        "Tunnel death is detected and surfaced; the app does not silently stop receiving events",
        "Registering the webhook is classified high risk (it mutates a GitHub repo's settings)"],
    gate="cargo test -p revlocal-daemon tunnel")

add("RL-1007", "story", "Manual review and resumable backfill", parent="RL-E10", milestone="M12",
    priority="P1", estimate=5, labels=["rust", "triggers", "cli"], spec=["§7.4"],
    depends_on=["RL-1001"],
    desc="`revlocal review --rev` and `revlocal backfill --since`, the latter at low priority behind live work, respecting budgets and resumable via its own cursor.",
    ac=["Backfill yields to live triggers rather than starving them",
        "Interrupting and re-running backfill resumes rather than restarting",
        "--dry-run enumerates without spending engine tokens",
        "--limit is honoured exactly"],
    gate="cargo test -p revlocal-daemon --test backfill")

# ─────────────────────────────────────────────────────────────────────────────
# EPIC 11 — UI + Framewatch
# ─────────────────────────────────────────────────────────────────────────────
add("RL-E11", "epic", "Desktop UI and Framewatch visual verification", milestone="M13",
    priority="P0", labels=["ui"], spec=["§15", "§16.4"], depends_on=["RL-E8"], desc="""
    Six screens plus tray, and — importantly for autonomous building — a Framewatch
    harness so the loop can SEE the GUI it just built instead of assuming it renders.
    """)

add("RL-1101", "story", "Tauri shell, IPC commands and event bridge", parent="RL-E11",
    milestone="M13", priority="P0", estimate=5, labels=["tauri", "ui"], spec=["§4.2", "§15"],
    desc="""
    Tauri v2 app hosting the in-process daemon. Commands delegate to the daemon; the
    EventBus is bridged to Tauri events so the UI updates live rather than polling.
    """,
    ac=["App launches on all three platforms",
        "A daemon event appears in the UI without a poll",
        "Closing the window to tray keeps the daemon running; quitting stops it cleanly",
        "IPC surface is a thin delegation layer with no business logic (asserted by review)"],
    gate="cargo test -p revlocal-tauri ipc")

add("RL-1102", "story", "Framewatch GUI verification harness", parent="RL-E11", milestone="M13",
    priority="P0", estimate=5, labels=["framewatch", "testing", "ui"], spec=["§16.4"],
    depends_on=["RL-1101"],
    desc="""
    scripts/gui-verify.sh (+ .ps1) wrapping `framewatch shot --launch` to capture a
    settled PNG of a named screen into artifacts/gui/<name>.png, deterministically.
    ROI cropping clips host chrome so captures are comparable. This is how the build loop
    verifies the UI actually renders.
    """,
    ac=["`scripts/gui-verify.sh dashboard` writes artifacts/gui/dashboard.png and exits 0",
        "Uses --settle-ms and --timeout so a slow first paint does not produce a blank frame",
        "Exits non-zero when the window never appears — a missing UI must fail the gate, not pass silently",
        "--roi is configured per screen so window chrome is excluded",
        "Documented in docs/GUI_VERIFICATION.md with the exact commands"],
    gate="scripts/gui-verify.sh dashboard && test -s artifacts/gui/dashboard.png")

add("RL-1103", "story", "Framewatch scripted flow capture with marks", parent="RL-E11",
    milestone="M13", priority="P1", estimate=5, labels=["framewatch", "testing", "ui"],
    spec=["§16.4"], depends_on=["RL-1102"],
    desc="""
    For multi-step flows (add repo -> dry-run review -> open run detail -> approvals),
    run `framewatch watch --labels-file` alongside a driver script that writes step labels,
    so each captured frame is captioned with the step that produced it. Produces a session
    directory the loop agent can read frame-by-frame.
    """,
    ac=["A flow session contains one captioned settled frame per step",
        "Labels come from the driver via --labels-file, so captions and actions cannot drift apart",
        "`--until-settled`/`--frames` bound the session so it always terminates",
        "Session directory path is printed for the agent to consume"],
    gate="scripts/gui-flow.sh add-repo-to-review && test -d .framewatch")

add("RL-1104", "spike", "SPIKE: Framewatch in headless CI", parent="RL-E11", milestone="M13",
    priority="P1", estimate=2, labels=["spike", "framewatch", "ci"], spec=["§16.3", "§16.4"],
    desc="""
    Timeboxed. Determine whether framewatch can capture under Xvfb/Wayland on the Linux
    CI runner and what the Windows/macOS runner story is (screen recording permissions on
    macOS in particular). Decide: gate on GUI capture in CI, or make it a local-only
    developer/loop gate with CI running only the vitest layer.
    """,
    ac=["ADR recording the decision and the reasoning",
        "If CI capture is viable, the workflow uploads artifacts/gui/*.png on every run",
        "If not, docs state clearly that GUI gates are local-only and CI covers the rest"],
    notes="Timebox: 3 hours. macOS screen-recording consent is the likely blocker.")

add("RL-1105", "story", "Dashboard screen", parent="RL-E11", milestone="M13", priority="P0",
    estimate=5, labels=["ui", "react"], spec=["§15"], depends_on=["RL-1101", "RL-1102"],
    desc="Repo cards with health, last run, queue depth and today's budget bar; global mode selector; kill switch; live activity feed.",
    ac=["Kill switch is present and functional",
        "Activity feed updates from events, not polling",
        "Budget bar reflects the ledger",
        "Framewatch capture of the dashboard is reviewed and shows all four regions populated with fixture data"],
    gate="npm --prefix ui test -- dashboard && scripts/gui-verify.sh dashboard")

add("RL-1106", "story", "Repository screen", parent="RL-E11", milestone="M13", priority="P1",
    estimate=5, labels=["ui", "react"], spec=["§15"], depends_on=["RL-1105"],
    desc="Config editor, watched branches/paths, per-trigger live status indicators, recent runs, per-repo budget.",
    ac=["Each of the four triggers has an independent live indicator",
        "Config edits validate before save and show the validation error inline",
        "An SVN repo shows path/branch watching rather than git branch semantics",
        "Framewatch capture reviewed for both a git repo and an SVN repo"],
    gate="npm --prefix ui test -- repository && scripts/gui-verify.sh repository")

add("RL-1107", "story", "Run detail screen", parent="RL-E11", milestone="M13", priority="P0",
    estimate=8, labels=["ui", "react"], spec=["§15"], depends_on=["RL-1105"],
    desc="Stage timeline with durations, the rendered prompt, collapsible raw transcript, diff with findings anchored inline, per-target publish status with retry.",
    ac=["Findings anchor to the correct diff lines",
        "Truncated reviews visibly say so, per the no-silent-caps rule",
        "Per-target retry buttons work and reflect state",
        "The transcript is collapsed by default and does not block rendering on a large log",
        "Framewatch capture reviewed for a run with findings and one with none"],
    gate="npm --prefix ui test -- run-detail && scripts/gui-verify.sh run-detail")

add("RL-1108", "story", "Findings screen", parent="RL-E11", milestone="M13", priority="P2",
    estimate=5, labels=["ui", "react"], spec=["§15"], depends_on=["RL-1105"],
    desc="Cross-repo table with severity/category/state filters, suppress, jump to run, manual 'file to Andare'.",
    ac=["Filters compose",
        "Suppress creates a suppression and the row updates immediately",
        "Manual file-to-Andare is risk-gated like any other publish",
        "Framewatch capture reviewed"],
    gate="npm --prefix ui test -- findings && scripts/gui-verify.sh findings")

add("RL-1109", "story", "Approvals screen", parent="RL-E11", milestone="M13", priority="P0",
    estimate=5, labels=["ui", "react", "safety"], spec=["§12.4", "§15"], depends_on=["RL-803", "RL-1105"],
    desc="The inbox rendering each queued payload as the target would render it, with the five actions from SPEC §12.4.",
    ac=["The rendered preview matches what is actually sent (same renderer, asserted by test)",
        "Every action names its target explicitly",
        "Approve-all-for-run is confirmed before executing",
        "Framewatch capture reviewed showing a queued GitHub review and a queued Andare issue side by side"],
    gate="npm --prefix ui test -- approvals && scripts/gui-verify.sh approvals")

add("RL-1110", "story", "Settings screen with doctor output and capability mapping",
    parent="RL-E11", milestone="M13", priority="P1", estimate=5, labels=["ui", "react"],
    spec=["§15"], depends_on=["RL-603", "RL-1105"],
    desc="Engines with inline doctor output, MCP servers with discovered tool lists, the capability mapping table with manual override, budgets and retention.",
    ac=["Unmapped capabilities are called out visibly with a fix affordance",
        "doctor can be re-run from the UI",
        "Secrets are never displayed, only their presence",
        "Framewatch capture reviewed showing an unmapped capability state"],
    gate="npm --prefix ui test -- settings && scripts/gui-verify.sh settings")

add("RL-1111", "story", "Tray, notifications and global kill switch reachability",
    parent="RL-E11", milestone="M13", priority="P1", estimate=3, labels=["ui", "tauri", "safety"],
    spec=["§15"], depends_on=["RL-1105"],
    desc="System tray with status and kill switch; native notification on high-severity findings and on actions awaiting approval.",
    ac=["Kill switch reachable from every screen AND from the tray",
        "Notifications are rate-limited so a backfill cannot spam the user",
        "Tray reflects paused state",
        "Framewatch capture of each screen confirms the kill switch is visible in all six"],
    gate="npm --prefix ui test -- tray && scripts/gui-verify.sh all")

# ─────────────────────────────────────────────────────────────────────────────
# EPIC 12 — CLI, packaging, release
# ─────────────────────────────────────────────────────────────────────────────
add("RL-E12", "epic", "CLI surface, live engines, packaging and release", milestone="M14",
    priority="P0", labels=["release"], spec=["§14", "§16.1"], depends_on=["RL-E11"], desc="""
    The headless surface that makes everything testable, real-engine validation, and
    shippable installers for three platforms.
    """)

add("RL-1201", "story", "Complete CLI surface with --json everywhere", parent="RL-E12",
    milestone="M14", priority="P0", estimate=8, labels=["cli"], spec=["§14"],
    desc="""
    Every command in SPEC §14, each supporting --json. This surface IS the acceptance-test
    API, so it must be complete and stable before M14 closes.
    """,
    ac=["Every command in §14 exists and is exercised by at least one test",
        "--json output is schema-stable and documented",
        "Exit codes are meaningful and documented (0 ok, 1 error, 2 usage, 3 blocked-by-budget, 4 awaiting-approval)",
        "`revlocal --help` is coherent for someone who has not read the spec"],
    gate="cargo test -p revlocal-cli --test cli_surface")

add("RL-1202", "story", "revlocal doctor", parent="RL-E12", milestone="M14", priority="P0",
    estimate=5, labels=["cli", "ux"], spec=["§8.4", "§14"], depends_on=["RL-407", "RL-603"],
    desc="""
    Prerequisites, engine probes (including a real smoke task), MCP target reachability,
    capability mapping status, and platform-specific checks. Every failure carries
    remediation text. This is the first thing a user runs on a fresh install.
    """,
    ac=["Reports each engine as usable/unusable with a reason",
        "Reports each MCP target with tool count and mapped/unmapped capability counts",
        "Missing svn is reported as blocking for SVN repos only",
        "Every failure line includes a concrete next action",
        "--json output is machine-consumable by the build loop"],
    gate="./target/debug/revlocal doctor --json")

add("RL-1203", "story", "Live engine acceptance suite", parent="RL-E12", milestone="M14",
    priority="P0", estimate=5, labels=["testing", "engine"], spec=["§16.1"],
    depends_on=["RL-407", "RL-506"],
    desc="""
    `--features engine-live` tests that actually invoke claude and codex against the
    planted-bug fixtures. Informational before M14, blocking at M14.
    """,
    ac=["Claude Code finds the planted SQL injection in src/db.rs",
        "Codex finds the planted SQL injection in src/db.rs",
        "Both produce schema-valid result.json without hitting the repair rung",
        "The suite skips cleanly with a clear message when a binary is absent",
        "Token usage per run is recorded so the cost of the suite is known"],
    gate="cargo test --features engine-live -- --ignored")

add("RL-1204", "story", "Packaging and installers for macOS, Windows and Linux",
    parent="RL-E12", milestone="M14", priority="P1", estimate=8, labels=["release", "packaging"],
    spec=["§9"], depends_on=["RL-1101"],
    desc="Tauri bundles: .dmg, .msi, .AppImage/.deb. Signing where credentials exist; unsigned with clear docs where they do not.",
    ac=["All three bundles build in CI",
        "The bundled app launches and passes the dashboard Framewatch capture",
        "First-run experience lands on doctor output when prerequisites are missing",
        "Unsigned-build implications are documented rather than hidden"],
    gate="npm --prefix ui run build && cargo tauri build")

add("RL-1205", "story", "First-run onboarding", parent="RL-E12", milestone="M14", priority="P2",
    estimate=5, labels=["ui", "ux"], spec=["§15"], depends_on=["RL-1202", "RL-1110"],
    desc="Guided path: doctor -> add first repo -> pick engine -> choose autonomy -> dry-run first review -> show the result.",
    ac=["A user with no config reaches a completed dry-run review without editing a file",
        "Default autonomy for a newly added repo is never `auto`",
        "Onboarding can be re-run from Settings",
        "Framewatch flow capture of the whole onboarding path is reviewed"],
    gate="scripts/gui-flow.sh onboarding")

add("RL-1206", "task", "README and operator documentation", parent="RL-E12", milestone="M14",
    priority="P2", estimate=3, labels=["docs"], depends_on=["RL-1202"],
    desc="README, docs/GUI_VERIFICATION.md, docs/OPERATIONS.md (what to do when a target fails, how budgets behave, how to recover a stuck run).",
    ac=["README gets a new user to a first review",
        "Operations doc covers: target down, budget exhausted, stuck run, force-push recovery, orphaned check run",
        "Every doc claim is verified against the built binary, not assumed"],
    gate="test -f README.md && test -f docs/OPERATIONS.md")

# ─────────────────────────────────────────────────────────────────────────────
# Cross-cutting
# ─────────────────────────────────────────────────────────────────────────────
add("RL-E13", "epic", "Cross-cutting quality gates", milestone="all", priority="P1",
    labels=["quality"], spec=["§18"], desc="""
    Standing requirements that apply to every other epic and are re-checked at each
    milestone close.
    """)

add("RL-1301", "task", "Error taxonomy and remediation text", parent="RL-E13", priority="P1",
    estimate=3, labels=["rust", "ux"], spec=["§18"],
    desc="One thiserror enum per crate, anyhow only at the binary edge, every user-visible error carrying a remediation sentence.",
    ac=["No anyhow in library crate public APIs",
        "A test enumerates user-visible error variants and asserts each has non-empty remediation text"],
    gate="cargo test --workspace errors::")

add("RL-1302", "task", "No-silent-caps audit", parent="RL-E13", priority="P0", estimate=2,
    labels=["safety", "quality"], spec=["§18"],
    desc="""
    Standing audit: every place the system truncates, samples, caps or drops must record
    it on the run and surface it in the UI and CLI output.
    """,
    ac=["Each cap site has a corresponding recorded field and a test asserting it is set",
        "The UI displays every such field",
        "A grep-based check lists cap sites and fails if one has no recorded counterpart"],
    gate="cargo test --workspace no_silent_caps")

add("RL-1303", "task", "Cross-platform path and process audit", parent="RL-E13", priority="P1",
    estimate=3, labels=["platform", "quality"], spec=["§16.3", "§18"],
    desc="camino UTF-8 paths, internal normalization to '/', Windows process groups, WAL under Windows file locking, hook line endings.",
    ac=["Path normalization tested with Windows-style inputs on every platform",
        "The full integration suite is green on all three OSes in CI",
        "No test is skipped on Windows without an explicit documented reason"],
    gate="cargo test --workspace")

add("RL-1304", "task", "Security review of the publish and secrets paths", parent="RL-E13",
    priority="P0", estimate=3, labels=["security"], spec=["§8.5", "§13.1", "§18"],
    depends_on=["RL-111", "RL-406", "RL-602"],
    desc="Targeted review: no credentials in config or logs, keychain resolution is lazy, engine env denylist holds, webhook signature verification is constant-time, no shell interpolation of untrusted strings.",
    ac=["Each item verified by a test, not by inspection alone",
        "Findings recorded as issues and closed before M14",
        "A deliberately planted secret in a config is not written to any log or transcript"],
    gate="cargo test --workspace security::")

# ─────────────────────────────────────────────────────────────────────────────
out = pathlib.Path(__file__).resolve().parent.parent / "docs" / "backlog"
out.mkdir(parents=True, exist_ok=True)

ids = {i["id"] for i in ITEMS}
for i in ITEMS:
    if i["parent"] and i["parent"] not in ids:
        raise SystemExit(f"{i['id']}: unknown parent {i['parent']}")
    for d in i["depends_on"]:
        if d not in ids:
            raise SystemExit(f"{i['id']}: unknown dependency {d}")

# Carry Andare keys and status forward from any prior generation. Without this,
# regenerating after an import silently discards every key written back by IMPORT.md.
_prev = out / "backlog.json"
if _prev.exists():
    _old = {i["id"]: i for i in json.loads(_prev.read_text())["items"]}
    for i in ITEMS:
        o = _old.get(i["id"])
        if o:
            i["andare_key"] = o.get("andare_key")
            i["status"] = o.get("status", "todo")

(out / "backlog.json").write_text(json.dumps({
    "project": "rev-local",
    "spec": "SPEC.md",
    "generated_by": "scripts/gen_backlog.py",
    "item_count": len(ITEMS),
    "items": ITEMS,
}, indent=2) + "\n")

# ── markdown rendering ──
L = []
L.append("# rev-local — delivery backlog\n")
L.append("> Generated by `scripts/gen_backlog.py`. **Do not hand-edit.**")
L.append("> Edit the generator and re-run. Once imported into Andare, the Andare key is")
L.append("> written back into `backlog.json` and Andare becomes the working source of truth")
L.append("> for *status*; this file remains the source of truth for *scope*.\n")
counts = {}
for i in ITEMS:
    counts[i["type"]] = counts.get(i["type"], 0) + 1
L.append("**%d items** — " % len(ITEMS) + ", ".join(f"{v} {k}" for k, v in sorted(counts.items())) + "\n")
L.append("---\n")

by_parent = {}
for i in ITEMS:
    by_parent.setdefault(i["parent"], []).append(i)

def render(item, depth):
    h = "#" * min(depth + 2, 6)
    L.append(f"{h} `{item['id']}` {item['type'].upper()} — {item['title']}\n")
    meta = [f"**Priority** {item['priority']}"]
    if item["milestone"]: meta.append(f"**Milestone** {item['milestone']}")
    if item["estimate"]: meta.append(f"**Est** {item['estimate']}")
    if item["spec_refs"]: meta.append("**Spec** " + ", ".join(item["spec_refs"]))
    if item["depends_on"]: meta.append("**Depends on** " + ", ".join(f"`{d}`" for d in item["depends_on"]))
    if item["labels"]: meta.append("**Labels** " + ", ".join(item["labels"]))
    L.append(" · ".join(meta) + "\n")
    if item["description"]:
        L.append(item["description"] + "\n")
    if item["acceptance_criteria"]:
        L.append("**Acceptance criteria**\n")
        for a in item["acceptance_criteria"]:
            L.append(f"- [ ] {a}")
        L.append("")
    if item["gate"]:
        L.append(f"**Gate** — `{item['gate']}`\n")
    if item["notes"]:
        L.append(f"> {item['notes']}\n")
    for c in by_parent.get(item["id"], []):
        render(c, depth + 1)

for top in by_parent.get(None, []):
    render(top, 0)
    L.append("---\n")

(out / "BACKLOG.md").write_text("\n".join(L))
print(f"wrote {len(ITEMS)} items")
