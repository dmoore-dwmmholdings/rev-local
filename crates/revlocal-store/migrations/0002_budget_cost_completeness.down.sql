-- SQLite has supported DROP COLUMN since 3.35; sqlx bundles a newer one.
ALTER TABLE budget_ledger DROP COLUMN cost_complete;
