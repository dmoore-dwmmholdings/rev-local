#!/usr/bin/env bash
# Build the deterministic database the GUI captures render (RL-1102, §16.4).
#
# Every number here is fixed on purpose. A capture is only comparable to the last
# one if the data behind it is identical, and "today's budget" is the obvious way
# for that to stop being true — so the ledger day is computed but the figures are
# not.

set -euo pipefail

DB="${1:?usage: gui-fixture.sh <path/to.db>}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLI="$ROOT/target/debug/revlocal"

[[ -x "$CLI" ]] || { echo "gui-fixture: build the CLI first (cargo build -p revlocal-cli)" >&2; exit 1; }

rm -f "$DB"
mkdir -p "$(dirname "$DB")"
work="$(dirname "$DB")/repos"
rm -rf "$work"

for name in acme widgets; do
  mkdir -p "$work/$name"
  git -C "$work/$name" init -q -b main .
  git -C "$work/$name" config user.email fixture@rev-local.invalid
  git -C "$work/$name" config user.name "GUI fixture"
  echo "fn main() {}" > "$work/$name/main.rs"
  git -C "$work/$name" add main.rs
  git -C "$work/$name" commit -q -m "add a main"
done

"$CLI" db migrate --database "$DB" >/dev/null
"$CLI" repo add "$work/acme"    --kind git --name acme    --database "$DB" >/dev/null
"$CLI" repo add "$work/widgets" --kind git --name widgets --autonomy auto --database "$DB" >/dev/null
# An SVN repository, because §15's repository screen has to be captured in both
# vocabularies (REVL-92) — a git-only fixture cannot show that "watched paths"
# and "watched branches" are different screens rather than different words.
# A URL, not a working copy: nothing here invokes svn, and a capture must not
# depend on a binary that is absent on most machines.
"$CLI" repo add "svn://svn.example.invalid/legacy" --kind svn --name legacy --database "$DB" >/dev/null
# Hooks installed on one repository so the trigger indicators are not all off in
# the capture. The indicator reads the disk, so this has to be a real install.
"$CLI" hooks install --repo "$work/acme" --name acme >/dev/null 2>&1 || true

"$CLI" watch --once --database "$DB" >/dev/null 2>&1 || true

# One measured repository and one that is not, so every capture exercises §18's
# distinction between a total and a lower bound.
sqlite3 "$DB" "
INSERT INTO run (change_id,attempt,status,engine,depth,trigger,created_at,finished_at,verdict,tokens_in,tokens_out,tokens_known)
  VALUES (1,1,'done','claude','standard','poll','2026-01-01T01:00:00Z','2026-01-01T01:04:00Z','request_changes',180000,9000,1);
INSERT INTO run (change_id,attempt,status,engine,depth,trigger,created_at)
  VALUES (2,1,'queued','claude','standard','poll','2026-01-01T02:00:00Z');
INSERT INTO budget_ledger (repo_id,day,runs,tokens_in,tokens_out,cost_usd,cost_complete,tokens_complete)
  VALUES (1,date('now','localtime'),37,180000,9000,4.2,1,1),
         (2,date('now','localtime'),12,640000,31000,9.9,1,0);
"
# Findings and one queued action, so the findings and approvals screens have
# something to render. A capture of an empty table proves the window opened and
# nothing else — and "no findings" is exactly what a broken query looks like.
sqlite3 "$DB" "
INSERT INTO finding
  (run_id,fingerprint,severity,category,confidence,file,line_start,line_end,title,body,state,created_at)
  VALUES
  (1,'fp-sql','critical','security',0.9,'src/db.rs',41,44,'SQL injection in find_user',
   'The name parameter is interpolated straight into the query.','open','2026-01-01T01:03:00Z'),
  (1,'fp-panic','high','correctness',0.8,'src/engine.rs',88,88,'unwrap on a parsed header',
   'A malformed header aborts the run rather than failing it.','open','2026-01-01T01:03:01Z'),
  (1,'fp-alloc','medium','perf',0.6,'src/scan.rs',12,30,'allocates per line in the hot loop',
   'The buffer is rebuilt for every line of the diff.','open','2026-01-01T01:03:02Z'),
  (1,'fp-cover','low','tests',0.7,'src/retry.rs',5,5,'the retry path is untested',
   'Nothing exercises the branch that gives up.','suppressed','2026-01-01T01:03:03Z');

INSERT INTO publish_action
  (run_id,finding_id,target,capability,risk,idempotency_key,payload_json,status,attempts,created_at)
  VALUES
  (1,1,'andare','create_issue','high','fixture-andare-fp-sql',
   '{\"title\":\"SQL injection in find_user\",\"body\":\"The name parameter is interpolated straight into the query.\"}',
   'awaiting_approval',0,'2026-01-01T01:05:00Z');
"

echo "gui-fixture: $DB"
