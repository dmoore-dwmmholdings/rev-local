-- Dropping these loses the distinction between a measured zero and an unmeasured
-- run, which is the whole point of 0007. SQLite supports DROP COLUMN since 3.35.
ALTER TABLE budget_ledger DROP COLUMN tokens_complete;
ALTER TABLE run DROP COLUMN tokens_known;
