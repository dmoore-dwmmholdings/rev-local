-- RL-409: a run whose token count nobody measured must not be stored as a run
-- that spent none.
--
-- `run.tokens_in`/`tokens_out` have always been NOT NULL DEFAULT 0, which cannot
-- distinguish "spent nothing" from "nobody counted". §8.3's result.json carries no
-- usage field, so a real engine's runner had no counts to write and wrote zero —
-- and a repo with a two-million-token daily budget never reached it.
--
-- These mirror `budget_ledger.cost_complete`, which has drawn the same distinction
-- for money since 0001.
--
-- DEFAULT 0 means "not known", which is the honest reading of every row written
-- before this column existed: nothing recorded whether those counts were complete,
-- and defaulting them to "measured" would assert something no one checked.
ALTER TABLE run ADD COLUMN tokens_known INTEGER NOT NULL DEFAULT 0;
ALTER TABLE budget_ledger ADD COLUMN tokens_complete INTEGER NOT NULL DEFAULT 0;
