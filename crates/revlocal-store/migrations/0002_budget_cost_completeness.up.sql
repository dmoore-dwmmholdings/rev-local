-- 0002 — record whether a day's accumulated cost accounts for everything.
--
-- SPEC §5 gives budget_ledger `cost_usd REAL NOT NULL DEFAULT 0`, but an engine
-- need not report a price (§8.1 types EngineOutcome's cost as optional). Folding
-- an unknown cost into that column as 0 makes an unmeasured day indistinguishable
-- from a free one, and decision D10 plus §18 say a budget must never look like it
-- has headroom it has not been shown to have.
--
-- cost_usd therefore accumulates only the costs that were actually reported, and
-- this flag says whether anything was missing. See ADR 0010.

ALTER TABLE budget_ledger
  ADD COLUMN cost_complete INTEGER NOT NULL DEFAULT 1;
