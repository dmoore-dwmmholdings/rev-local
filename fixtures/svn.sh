#!/usr/bin/env bash
# Build the Subversion fixture (SPEC §16.2, §6.4). Sourced by build.sh.
#
# Structured so pseudo-PR detection (§6.4) can be tested through EACH heuristic
# independently. The reintegration revision carries BOTH a matching log message
# and a real `svn:mergeinfo` change, and there is a second reintegration that
# carries mergeinfo with a log message that deliberately does NOT match
# `merge_detect_regex` — so heuristic 1 can be tested without heuristic 2
# quietly rescuing it. A fixture where every signal fires at once cannot tell you
# which signal your code is actually using.

set -euo pipefail

# The repository's current HEAD revision, as a bare number.
#
# NOT `svn info --show-item revision`: that arrived in Subversion 1.9, and the
# Windows runner installs win32svn 1.8.15, where it is `invalid option`. `--xml`
# has been there since 1.3 and is stable output, which is the other reason to
# prefer it — ADR 0023's rule about parsing another tool's human output.
head_revision() {
  local url="$1"
  svn "${svn_opts[@]}" info --xml "$url" \
    | tr '>' '>\n' \
    | grep -m 1 'revision="' \
    | sed -e 's/.*revision="//' -e 's/".*//'
}

# Build the svn fixture into $1. Assumes `svn` and `svnadmin` exist; build.sh
# checks that and writes the skip manifest when they do not.
build_svn_fixture() {
  local out_dir="$1"

  # Resolve to a path svn can open, whatever form the caller passed.
  #
  # `build.ps1` invokes this through Git-for-Windows bash, and the argument that
  # arrives is not the one PowerShell sent: CI observed `C:\Users\...` reaching
  # here as `c/Users/...` — drive letter lowercased, colon and leading slash both
  # gone. That produced `file://c/Users/...`, which names a HOST called `c`, and
  # svn reported E180001 from inside a PowerShell stack trace.
  #
  # Rather than guess which layer mangled it, normalise here, where the path is
  # about to be used. `pwd -W` is Git-bash's own answer for "this path in Windows
  # terms" and gives `C:/Users/...`; everywhere else it is unsupported and plain
  # `pwd` is already correct. The directory has to exist to `cd` into it, so this
  # creates it first — which build.sh did a moment later anyway.
  mkdir -p "$out_dir"
  out_dir="$(cd "$out_dir" && { pwd -W 2>/dev/null || pwd; })"

  local repo="${out_dir}/svn-basic"
  # Windows needs three slashes and the drive letter inside the path:
  # `file://D:/x` names a HOST called `D:`, which is why this presented as a
  # silent non-zero exit rather than an error anyone could read. POSIX paths start
  # with `/` and take the two-slash form.
  local repo_url
  case "$repo" in
    [A-Za-z]:/*) repo_url="file:///${repo}" ;;
    *)           repo_url="file://${repo}" ;;
  esac
  local wc="${out_dir}/.svn-wc"

  rm -rf "$repo" "$wc"

  svnadmin create "$repo"

  # Property changes on revision 0 are needed for the fixture's own log edits and
  # are harmless here; without this hook svn refuses them.
  cat > "${repo}/hooks/pre-revprop-change" <<'HOOK'
#!/bin/sh
exit 0
HOOK
  chmod +x "${repo}/hooks/pre-revprop-change"

  local svn_opts=(--non-interactive --no-auth-cache)
  local commit_author="fixtures"

  _svn() { svn "${svn_opts[@]}" "$@"; }

  # r1 — standard layout
  _svn mkdir -m "Create standard layout" \
    "${repo_url}/trunk" "${repo_url}/branches" "${repo_url}/tags" >/dev/null

  _svn checkout "${repo_url}/trunk" "$wc" >/dev/null
  pushd "$wc" >/dev/null

  # r2 — initial files, mirroring the git fixture's shape
  mkdir -p src
  cat > README.md <<'EOF'
# svn fixture

An offline Subversion fixture. See .manifest.json for revision roles.
EOF
  cat > src/main.rs <<'EOF'
fn main() {
    println!("fixture");
}
EOF
  _svn add --quiet README.md src
  _svn commit --quiet -m "Initial import" >/dev/null

  # r3 — a clean commit
  cat > src/util.rs <<'EOF'
/// Clamp `value` into `lo..=hi`.
pub fn clamp(value: i64, lo: i64, hi: i64) -> i64 {
    if value < lo { lo } else if value > hi { hi } else { value }
}
EOF
  _svn add --quiet src/util.rs
  _svn commit --quiet -m "Add a clamp helper" >/dev/null

  # r4 — planted off-by-one, same bug as the git fixture
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
  _svn add --quiet src/pager.rs
  _svn commit --quiet -m "Add pagination helper" >/dev/null

  # r5 — planted SQL injection
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
EOF
  _svn add --quiet src/db.rs
  _svn commit --quiet -m "Add user lookup" >/dev/null

  # r6 — lockfile only
  cat > Cargo.lock <<'EOF'
# This file is automatically @generated by Cargo.
version = 4
EOF
  _svn add --quiet Cargo.lock
  _svn commit --quiet -m "Update Cargo.lock" >/dev/null

  popd >/dev/null

  # The fork point is the repository HEAD at the moment of the copy — §6.4's
  # pseudo-PR diff is `trunk@fork_rev` vs `branch@rev`, so this must be the
  # revision the branch was copied FROM, not the working copy's own revision
  # (which lags behind after committing a child path).
  local fork_rev
  fork_rev="$(head_revision "${repo_url}")"

  # r7 — branch created by copy. This revision is the fork point.
  _svn copy -m "Create branches/feature-x from trunk" \
    "${repo_url}/trunk" "${repo_url}/branches/feature-x" >/dev/null

  local branch_wc="${out_dir}/.svn-branch-wc"
  rm -rf "$branch_wc"
  _svn checkout "${repo_url}/branches/feature-x" "$branch_wc" >/dev/null
  pushd "$branch_wc" >/dev/null

  # r8, r9 — work on the branch
  cat >> src/pager.rs <<'EOF'

/// Number of pages needed for `count` items.
pub fn page_count(count: usize, per_page: usize) -> usize {
    count.div_ceil(per_page)
}
EOF
  _svn commit --quiet -m "Add page_count on the branch" >/dev/null

  cat > src/paging_notes.md <<'EOF'
Paging behaviour is documented here.
EOF
  _svn add --quiet src/paging_notes.md
  _svn commit --quiet -m "Document paging on the branch" >/dev/null

  popd >/dev/null

  # r10 — reintegration into trunk. Both heuristics fire: `svn merge` records
  # svn:mergeinfo, and the message matches merge_detect_regex.
  pushd "$wc" >/dev/null
  _svn update --quiet >/dev/null
  _svn merge --quiet "${repo_url}/branches/feature-x" . >/dev/null
  _svn commit --quiet -m "Merge branches/feature-x into trunk" >/dev/null
  popd >/dev/null

  # r11 — a SECOND reintegration whose log message deliberately does NOT match
  # merge_detect_regex, so heuristic 1 (svn:mergeinfo) can be tested on its own.
  # A fixture where every signal fires at once cannot tell you which signal the
  # code is actually using.
  local branch2_wc="${out_dir}/.svn-branch2-wc"
  _svn copy -m "Create branches/feature-y from trunk" \
    "${repo_url}/trunk" "${repo_url}/branches/feature-y" >/dev/null
  rm -rf "$branch2_wc"
  _svn checkout "${repo_url}/branches/feature-y" "$branch2_wc" >/dev/null
  pushd "$branch2_wc" >/dev/null
  cat > src/quiet.rs <<'EOF'
/// Added on feature-y.
pub const QUIET: bool = true;
EOF
  _svn add --quiet src/quiet.rs
  _svn commit --quiet -m "Add quiet flag" >/dev/null
  popd >/dev/null

  pushd "$wc" >/dev/null
  _svn update --quiet >/dev/null
  _svn merge --quiet "${repo_url}/branches/feature-y" . >/dev/null
  _svn commit --quiet -m "Sync work from the y line" >/dev/null
  popd >/dev/null

  local head_rev
  head_rev="$(head_revision "${repo_url}")"

  # The manifest below names revisions by number. If the repository did not end
  # up the shape this script thinks it did, those numbers are wrong and every
  # test that looks a role up would silently read the wrong revision — so this
  # fails loudly instead of writing a manifest that lies.
  if [[ "$head_rev" -ne 13 ]]; then
    echo "fixtures: expected svn-basic to end at r13, got r${head_rev}" >&2
    return 1
  fi
  if [[ "$fork_rev" -ne 6 ]]; then
    echo "fixtures: expected the feature-x fork point at r6, got r${fork_rev}" >&2
    return 1
  fi

  # --- verify the fixture is what it claims ---------------------------------
  #
  # `svn merge` recording mergeinfo is the entire point of r10 and r13. If it
  # silently did not, pseudo-PR heuristic 1 would be untestable and the failure
  # would look like the adapter's fault, so it is checked here.
  local mergeinfo
  mergeinfo="$(svn "${svn_opts[@]}" propget svn:mergeinfo "${repo_url}/trunk" 2>/dev/null || true)"
  if [[ "$mergeinfo" != *"/branches/feature-x"* ]]; then
    echo "fixtures: svn:mergeinfo on trunk does not mention feature-x; got: ${mergeinfo:-<empty>}" >&2
    return 1
  fi
  if [[ "$mergeinfo" != *"/branches/feature-y"* ]]; then
    echo "fixtures: svn:mergeinfo on trunk does not mention feature-y; got: ${mergeinfo}" >&2
    return 1
  fi

  # --- manifest --------------------------------------------------------------

  cat > "${out_dir}/svn-basic/.manifest.json" <<EOF
{
  "fixture": "svn-basic",
  "generator": "fixtures/svn.sh",
  "skipped": false,
  "repo_url": "${repo_url}",
  "trunk": "/trunk",
  "head_revision": ${head_rev},
  "fork_revision": ${fork_rev},
  "revisions": [
    {"role": "layout", "rev": 1, "subject": "Create standard layout"},
    {"role": "initial", "rev": 2, "subject": "Initial import"},
    {"role": "clean", "rev": 3, "subject": "Add a clamp helper"},
    {"role": "planted_bug_off_by_one", "rev": 4, "subject": "Add pagination helper"},
    {"role": "planted_bug_sql_injection", "rev": 5, "subject": "Add user lookup"},
    {"role": "lockfile_only", "rev": 6, "subject": "Update Cargo.lock"},
    {"role": "branch_created", "rev": 7, "subject": "Create branches/feature-x from trunk"},
    {"role": "branch_work", "rev": 8, "subject": "Add page_count on the branch"},
    {"role": "branch_work_2", "rev": 9, "subject": "Document paging on the branch"},
    {"role": "reintegration_rev", "rev": 10, "subject": "Merge branches/feature-x into trunk"},
    {"role": "branch2_created", "rev": 11, "subject": "Create branches/feature-y from trunk"},
    {"role": "branch2_work", "rev": 12, "subject": "Add quiet flag"},
    {"role": "reintegration_rev_mergeinfo_only", "rev": 13, "subject": "Sync work from the y line"}
  ],
  "pseudo_pr": {
    "branch": "/branches/feature-x",
    "fork_revision": ${fork_rev},
    "reintegration_revision": 10,
    "detected_by": ["mergeinfo", "log_message"]
  },
  "pseudo_pr_mergeinfo_only": {
    "branch": "/branches/feature-y",
    "reintegration_revision": 13,
    "detected_by": ["mergeinfo"],
    "note": "the log message deliberately does not match merge_detect_regex, so heuristic 1 can be tested alone"
  }
}
EOF

  rm -rf "$wc" "$branch_wc" "$branch2_wc"
  echo "fixtures: svn-basic built to r${head_rev}; manifest at ${out_dir}/svn-basic/.manifest.json"
}

# Write the manifest that records a skipped build, so a test can tell "svn was
# absent" from "the generator failed". SPEC §18: no silent caps — a fixture that
# was not built must say so rather than simply not existing.
write_svn_skip_manifest() {
  local out_dir="$1" reason="$2"
  mkdir -p "${out_dir}/svn-basic"
  cat > "${out_dir}/svn-basic/.manifest.json" <<EOF
{
  "fixture": "svn-basic",
  "generator": "fixtures/svn.sh",
  "skipped": true,
  "reason": "${reason}",
  "revisions": [],
  "remediation": "install Subversion (apt-get install subversion / brew install subversion / choco install svn) and re-run fixtures/build.sh"
}
EOF
}
