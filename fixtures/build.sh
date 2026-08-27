#!/usr/bin/env bash
# Build the offline fixture repositories (SPEC §16.2).
#
# Everything here is deterministic: two consecutive runs produce byte-identical
# commit SHAs. That is not a nicety. Tests reference commits *by role* through
# .manifest.json rather than by hardcoded SHA, and the manifest is only useful if
# the SHAs it names are stable.
#
# Determinism comes from three things, all of which have bitten this script:
#   * fixed author AND committer identity and dates — git hashes both;
#   * GIT_CONFIG_GLOBAL/SYSTEM pointed at /dev/null, so a developer's own
#     git config (commit.gpgsign, core.autocrlf, init.defaultBranch, a hooksPath)
#     cannot change the result;
#   * an explicit initial branch name, since init.defaultBranch differs by version.
#
# Usage: ./fixtures/build.sh [--out DIR]

set -euo pipefail

FIXTURE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT_DIR="${FIXTURE_ROOT}/out"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) OUT_DIR="$2"; shift 2 ;;
    -h|--help) sed -n '2,18p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "build.sh: unknown argument $1" >&2; exit 2 ;;
  esac
done

GIT_BASIC="${OUT_DIR}/git-basic"
GIT_BARE="${OUT_DIR}/git-bare"

# --- determinism ------------------------------------------------------------

# Isolate from whatever the developer has configured.
export GIT_CONFIG_GLOBAL=/dev/null
export GIT_CONFIG_SYSTEM=/dev/null
export GIT_CONFIG_NOSYSTEM=1
export LC_ALL=C
export TZ=UTC

readonly AUTHOR_NAME="Fixture Author"
readonly AUTHOR_EMAIL="fixtures@rev-local.invalid"
readonly BOT_NAME="dependabot[bot]"
readonly BOT_EMAIL="49699333+dependabot[bot]@users.noreply.github.com"

# One fixed instant per commit, so a SHA cannot drift with wall-clock time.
readonly BASE_EPOCH=1735689600   # 2025-01-01T00:00:00Z

# Commit N is BASE_EPOCH + N*60, as both author and committer time.
set_commit_time() {
  local index="$1"
  local stamp=$(( BASE_EPOCH + index * 60 ))
  export GIT_AUTHOR_DATE="${stamp} +0000"
  export GIT_COMMITTER_DATE="${stamp} +0000"
}

# Commit with a fixed identity. `--no-gpg-sign` because a developer with signing
# on by default would otherwise produce different objects.
commit_as() {
  local index="$1" name="$2" email="$3" subject="$4"
  set_commit_time "$index"
  GIT_AUTHOR_NAME="$name" GIT_AUTHOR_EMAIL="$email" \
  GIT_COMMITTER_NAME="$name" GIT_COMMITTER_EMAIL="$email" \
    git commit --quiet --no-gpg-sign -m "$subject"
}

commit_normal() { commit_as "$1" "$AUTHOR_NAME" "$AUTHOR_EMAIL" "$2"; }
commit_bot()    { commit_as "$1" "$BOT_NAME" "$BOT_EMAIL" "$2"; }

# --- manifest ---------------------------------------------------------------
#
# Tests must never hardcode a SHA. They look a commit up by role here, so a
# fixture can gain a commit without every test being rewritten.

MANIFEST_ENTRIES=()

record() {
  local role="$1" subject="$2"
  local sha
  sha="$(git rev-parse HEAD)"
  MANIFEST_ENTRIES+=("    {\"role\": \"${role}\", \"sha\": \"${sha}\", \"subject\": \"${subject}\"}")
}

write_manifest() {
  local path="$1"
  {
    printf '{\n'
    printf '  "fixture": "git-basic",\n'
    printf '  "generator": "fixtures/build.sh",\n'
    printf '  "default_branch": "main",\n'
    printf '  "commits": [\n'
    local index=0
    for entry in "${MANIFEST_ENTRIES[@]}"; do
      if [[ $index -gt 0 ]]; then printf ',\n'; fi
      printf '%s' "$entry"
      index=$(( index + 1 ))
    done
    printf '\n  ]\n'
    printf '}\n'
  } > "$path"
}

# --- git-basic --------------------------------------------------------------

echo "fixtures: building ${GIT_BASIC}"
rm -rf "$GIT_BASIC" "$GIT_BARE"
mkdir -p "$GIT_BASIC"
cd "$GIT_BASIC"

git init --quiet --initial-branch=main .
git config core.autocrlf false
git config core.fileMode true
git config commit.gpgsign false

# 1 — initial scaffold
mkdir -p src
cat > README.md <<'EOF'
# fixture

An offline fixture repository. Every commit here is deliberate; see .manifest.json.
EOF
cat > src/main.rs <<'EOF'
fn main() {
    println!("fixture");
}
EOF
git add -A
commit_normal 1 "Initial commit"
record "initial" "Initial commit"

# 2 — a clean commit, nothing wrong with it
cat > src/util.rs <<'EOF'
/// Clamp `value` into `lo..=hi`.
pub fn clamp(value: i64, lo: i64, hi: i64) -> i64 {
    if value < lo {
        lo
    } else if value > hi {
        hi
    } else {
        value
    }
}
EOF
git add -A
commit_normal 2 "Add a clamp helper"
record "clean" "Add a clamp helper"

# 3 — planted off-by-one. `<=` against len() indexes one past the end.
cat > src/pager.rs <<'EOF'
/// Return the items on `page`, counting from zero.
pub fn page_items(items: &[String], page: usize, per_page: usize) -> Vec<String> {
    let start = page * per_page;
    let mut out = Vec::new();
    // BUG (planted): `<=` walks one past the last index on a full final page.
    for index in start..=(start + per_page) {
        if index <= items.len() {
            out.push(items[index].clone());
        }
    }
    out
}
EOF
git add -A
commit_normal 3 "Add pagination helper"
record "planted_bug_off_by_one" "Add pagination helper"

# 4 — filler
cat >> src/util.rs <<'EOF'

/// Whether `value` is within `lo..=hi`.
pub fn in_range(value: i64, lo: i64, hi: i64) -> bool {
    value >= lo && value <= hi
}
EOF
git add -A
commit_normal 4 "Add in_range helper"
record "filler" "Add in_range helper"

# 5 — planted SQL injection: user input concatenated into a query.
cat > src/db.rs <<'EOF'
/// Look a user up by name.
pub fn find_user(conn: &Connection, name: &str) -> Result<Vec<Row>, Error> {
    // BUG (planted): `name` is interpolated straight into the SQL.
    let sql = format!("SELECT id, email FROM users WHERE name = '{}'", name);
    conn.query(&sql)
}

pub struct Connection;
pub struct Row;
pub struct Error;

impl Connection {
    pub fn query(&self, _sql: &str) -> Result<Vec<Row>, Error> {
        Ok(Vec::new())
    }
}
EOF
git add -A
commit_normal 5 "Add user lookup"
record "planted_bug_sql_injection" "Add user lookup"

# 6 — lockfile only. The skip rules must not review this (SPEC §9.4).
cat > Cargo.lock <<'EOF'
# This file is automatically @generated by Cargo.
version = 4

[[package]]
name = "fixture"
version = "0.1.0"
EOF
git add -A
commit_normal 6 "Update Cargo.lock"
record "lockfile_only" "Update Cargo.lock"

# 7 — bot-authored. Skipped by ignore_authors (SPEC §13.2).
mkdir -p .github
cat > .github/dependabot.yml <<'EOF'
version: 2
updates:
  - package-ecosystem: cargo
    directory: "/"
    schedule:
      interval: weekly
EOF
git add -A
commit_bot 7 "Bump serde from 1.0.0 to 1.0.1"
record "bot" "Bump serde from 1.0.0 to 1.0.1"

# 8 — filler
cat > src/lib.rs <<'EOF'
pub mod db;
pub mod pager;
pub mod util;
EOF
git add -A
commit_normal 8 "Declare modules"
record "filler_modules" "Declare modules"

# 9 — 200 files, for depth selection and truncation (SPEC §9.3, §9.4).
mkdir -p generated
for n in $(seq -w 1 200); do
  printf '/// Generated fixture module %s.\npub const ID_%s: u32 = %s;\n' "$n" "$n" "$((10#$n))" \
    > "generated/mod_${n}.rs"
done
git add -A
commit_normal 9 "Add 200 generated modules"
record "large_200_files" "Add 200 generated modules"

# 10 — work on a branch, so there is something to merge
git checkout --quiet -b feature/pager-tweak
cat >> src/pager.rs <<'EOF'

/// Number of pages needed for `count` items.
pub fn page_count(count: usize, per_page: usize) -> usize {
    count.div_ceil(per_page)
}
EOF
git add -A
commit_normal 10 "Add page_count"
record "branch_work" "Add page_count"

# 11 — merge commit. --no-ff so it is a real merge with two parents; the skip
#      rules must not review it (SPEC §9.4).
git checkout --quiet main
set_commit_time 11
GIT_AUTHOR_NAME="$AUTHOR_NAME" GIT_AUTHOR_EMAIL="$AUTHOR_EMAIL" \
GIT_COMMITTER_NAME="$AUTHOR_NAME" GIT_COMMITTER_EMAIL="$AUTHOR_EMAIL" \
  git merge --quiet --no-ff --no-gpg-sign -m "Merge branch 'feature/pager-tweak'" \
    feature/pager-tweak
record "merge" "Merge branch 'feature/pager-tweak'"

# 12 — a final clean commit
cat >> README.md <<'EOF'

Built by `fixtures/build.sh`. Do not edit by hand.
EOF
git add -A
commit_normal 12 "Note the generator in the README"
record "clean_final" "Note the generator in the README"

write_manifest "${GIT_BASIC}/.manifest.json"

# The manifest is generated, so it must not be a dirty file in the working tree —
# M4's gate asserts `git status --porcelain` is empty after a review.
printf '.manifest.json\n' > .git/info/exclude

COMMIT_COUNT="$(git rev-list --count HEAD)"
if [[ "$COMMIT_COUNT" -ne 12 ]]; then
  echo "fixtures: expected 12 commits on main, got ${COMMIT_COUNT}" >&2
  exit 1
fi

# --- git-bare ---------------------------------------------------------------
#
# A --mirror clone, for post-receive hook tests (SPEC §16.2, §7.2).

echo "fixtures: building ${GIT_BARE}"
git clone --quiet --mirror "$GIT_BASIC" "$GIT_BARE"

echo "fixtures: git-basic has ${COMMIT_COUNT} commits; manifest at ${GIT_BASIC}/.manifest.json"
