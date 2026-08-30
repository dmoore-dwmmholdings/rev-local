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
echo "gui-fixture: $DB"
