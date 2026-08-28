-- SQLite cannot DROP COLUMN before 3.35, and the pool may be older; rebuilding the
-- table would be a data-losing operation for a column that is additive and nullable.
-- Reversing this migration therefore leaves the columns in place, which is safe:
-- nothing reads them once the code that wrote them is gone.
SELECT 1;
