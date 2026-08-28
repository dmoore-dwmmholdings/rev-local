DROP TABLE IF EXISTS setting;
-- `run.engine_pid` is left in place: SQLite before 3.35 cannot DROP COLUMN, and
-- rebuilding `run` to remove one nullable column would risk data for no gain.
