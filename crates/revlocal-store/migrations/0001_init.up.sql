-- 0001_init — the schema of SPEC §5.
--
-- The DDL in SPEC §5 is normative. Where this file differs from what that
-- section printed, the difference is recorded in an ADR and the section was
-- updated in the same commit:
--
--   * run.degraded TEXT — added by RL-103b (ADR 0005). SPEC §8.1 gives
--     EngineOutcome a `degraded: Option<String>` and §12.3 escalates every action
--     on a degraded run to high risk, but the run table had nowhere to keep it.

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
  response_json     TEXT,
  external_ref      TEXT,                    -- issue key, PR review id, page id/url
  error             TEXT,
  created_at        TEXT NOT NULL,
  sent_at           TEXT,
  -- Not an optimisation. This is what makes redelivery safe: at-least-once
  -- delivery with exactly-once effect (§11.6). Dropping it turns a retry into a
  -- duplicate issue in somebody's tracker.
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
  cost_usd          REAL NOT NULL DEFAULT 0,
  UNIQUE (repo_id, day)
);
