-- RL-803: what the approver actually saw.
--
-- SPEC §12.4 shows the approver "exactly the payload that would be sent", and the
-- acceptance criterion is that an edit after approval is impossible. A digest of
-- the reviewed payload makes that checkable rather than merely intended: the queue
-- re-computes it at dispatch and refuses an action whose payload has moved.
--
-- Nullable, because most actions are never approved by anyone — a low-risk action
-- under `auto` goes straight out, and NULL means "nobody approved this", which is
-- distinguishable from "approved, and here is what they saw".
ALTER TABLE publish_action ADD COLUMN approved_payload_digest TEXT;

-- Why an approval ended. `expired` is the one §12.4 names explicitly, and it must
-- be distinguishable from a human saying no: one is a timeout nobody looked at,
-- the other is a decision.
ALTER TABLE publish_action ADD COLUMN decision_reason TEXT;
