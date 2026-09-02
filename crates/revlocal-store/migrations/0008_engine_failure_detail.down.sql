-- Dropping this loses the only stored account of why a run failed beyond its
-- code. SQLite supports DROP COLUMN since 3.35.
ALTER TABLE run DROP COLUMN error_detail;
