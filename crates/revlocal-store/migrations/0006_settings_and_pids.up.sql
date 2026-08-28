-- RL-804: the kill switch has to survive a restart, and `kill --hard` has to know
-- which processes were ours.
--
-- SPEC §12.1: "Survives restart (persisted in settings)" and "kills orphaned
-- engine processes by scanning the recorded PIDs". Neither had anywhere to live.

-- A small key/value store for operator state that is not configuration.
--
-- Deliberately separate from config.toml: config is what the user wrote, and this
-- is what rev-local was told to do at runtime. ADR 0015 draws the same line for
-- the GitHub transport, and for the same reason — a report has to be able to say
-- which of the two it is looking at.
CREATE TABLE setting (
  key        TEXT PRIMARY KEY,
  value      TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

-- The engine process a run spawned, recorded while it is alive.
--
-- Nullable and cleared on completion: a non-NULL pid on a run that is no longer
-- active is exactly the orphan `kill --hard` is looking for. Storing it means a
-- process that outlived the daemon can still be found after a restart, which is
-- the case that matters — a crash is how orphans happen in the first place.
ALTER TABLE run ADD COLUMN engine_pid INTEGER;
