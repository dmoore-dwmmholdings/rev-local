-- 0004 — the state §§7-12 require persisted that §5's DDL had no column for.
--
-- Found by auditing §5 against §§6-12 in one pass (RL-1304, ADR 0016) rather than
-- one at a time while implementing, which is how the previous three were found.
-- Each column below is justified by a specific sentence elsewhere in the spec, and
-- each is NOT derivable from what is already stored.

-- §9.4 truncation, and §18: "Wherever the system truncates, samples, or drops, it
-- records the fact ON THE RUN and shows it in the UI. A review that saw 60% of the
-- diff must never look like a review that saw all of it."
--
-- ChangeContext carries this in memory; without these two columns it dies with the
-- process and the UI has nothing to show. omitted_files_json is the list IN FULL,
-- because §9.4 says truncation must never silently hide a file.
ALTER TABLE run ADD COLUMN truncated INTEGER NOT NULL DEFAULT 0;
ALTER TABLE run ADD COLUMN omitted_files_json TEXT;

-- §8.3 and §10.2: the engine's summary and the verdict the review reached.
--
-- Neither is derivable after the fact. The summary is the engine's own prose and
-- exists nowhere else once the transcript is pruned (§5.1 prunes transcripts after
-- 30 days). The verdict is a HISTORICAL FACT — what was posted — and recomputing it
-- from findings would change retroactively as findings are suppressed or
-- superseded, so a run that requested changes would silently become one that
-- approved.
ALTER TABLE run ADD COLUMN verdict TEXT
  CHECK (verdict IN ('approve','comment','request_changes'));
ALTER TABLE run ADD COLUMN summary TEXT;

-- §11.6: "Retries: 5 attempts, exponential backoff with jitter (1s -> 60s cap)".
--
-- `attempts` alone cannot express WHEN the next attempt is due. Without this, every
-- pending action becomes due the moment the process restarts, and the backoff that
-- exists to stop rev-local hammering a rate-limited target is defeated by exactly
-- the event most likely to follow a burst of failures.
ALTER TABLE publish_action ADD COLUMN next_attempt_at TEXT;
