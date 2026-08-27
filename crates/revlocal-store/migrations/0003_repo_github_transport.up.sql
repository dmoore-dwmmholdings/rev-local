-- 0003 — record which transport rev-local uses to reach GitHub.
--
-- SPEC §6.3: "The selected transport is reported by `revlocal doctor` and stored on
-- the repo row." §5's DDL had nowhere to put it. It is a discovered runtime fact
-- rather than user configuration, so it does not belong in `config_json` — mixing
-- the two would make it unclear whether a value was chosen by the user or by the
-- ladder, which is exactly the question a doctor report answers. See ADR 0015.
--
-- NULL means "not probed yet". Absence is distinguishable from `unauthenticated`,
-- which matters: one means nobody has looked, the other means we looked and this is
-- as good as it gets.

ALTER TABLE repo
  ADD COLUMN github_transport TEXT
    CHECK (github_transport IN ('mcp','gh_cli','unauthenticated'));
