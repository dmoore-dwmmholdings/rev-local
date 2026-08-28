-- Reverse of 0001_init.
--
-- Dropped children-first so the drops hold even with foreign_keys = ON, which is
-- how the pool always opens. Indexes go with their tables.

DROP TABLE IF EXISTS budget_ledger;
DROP TABLE IF EXISTS audit;
DROP TABLE IF EXISTS publish_action;
DROP TABLE IF EXISTS suppression;
DROP TABLE IF EXISTS finding;
DROP TABLE IF EXISTS run;
DROP TABLE IF EXISTS change;
DROP TABLE IF EXISTS cursor;
DROP TABLE IF EXISTS repo;
