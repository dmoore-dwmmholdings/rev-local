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

# Identity, timing and the commit sequence all come from
# fixtures/content/git-basic/steps.json, so build.ps1 reads the same values rather
# than repeating them. Commit N is base_epoch + N*seconds_per_step, as both author
# and committer time, so a SHA cannot drift with wall-clock time.
set_commit_time() {
  local index="$1"
  local stamp=$(( BASE_EPOCH + index * SECONDS_PER_STEP ))
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
#
# Driven from fixtures/content/git-basic/steps.json. The file bodies live under
# fixtures/content/ and are COPIED, not written inline here, so this script and
# build.ps1 apply identical bytes rather than each carrying its own copy of every
# file. Two hand-maintained generators that must agree byte-for-byte will not stay
# in agreement; this leaves only the git invocations to keep in step.

CONTENT_DIR="${FIXTURE_ROOT}/content/git-basic"
STEPS_FILE="${CONTENT_DIR}/steps.json"

# steps.json is read with node, which is already required by the mock engine and
# mock MCP fixtures. `jq` is not, and adding a second dependency to read one file
# would make the fixture harder to build than the thing it tests.
steps_field() {
  node -e '
    const fs = require("node:fs");
    const data = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    const path = process.argv[2].split(".");
    let value = data;
    for (const key of path) { value = value?.[key]; }
    process.stdout.write(String(value ?? ""));
  ' "$STEPS_FILE" "$1"
}

# Emit one `|`-separated line per step: kind|index|dir|role|subject|author|name|count|into
#
# The delimiter is load-bearing twice over, and both constraints are bash 3.2 --
# the only bash macOS ships, and the one `Command::new("bash")` finds:
#
#   * NOT a control byte. bash 3.2 does not word-split on $'\001' at all: every
#     field lands in $kind and the read loop sees one unknown step kind. That is
#     what broke every fixture-dependent test in the workspace.
#   * NOT whitespace. Tab and space are IFS *whitespace*, which POSIX collapses --
#     `a<tab><tab>b` yields two fields, not three -- so an empty `dir` would shift
#     `into` out of existence. Empty fields are normal here, so the delimiter has
#     to be a non-whitespace character that 3.2 splits on.
#
# No field may contain `|`, a tab or a newline. A subject that did would shift
# every later field one position left, which reads as a wrong fixture rather than
# a broken one -- so this refuses to emit it at all.
steps_lines() {
  node -e '
    const fs = require("node:fs");
    const data = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    const NAMES = ["kind", "index", "dir", "role", "subject", "author", "name", "count", "into"];
    for (const step of data.steps) {
      const fields = [
        step.kind,
        step.index ?? "",
        step.dir ?? "",
        step.role ?? "",
        step.subject ?? "",
        step.author ?? "human",
        step.name ?? step.branch ?? "",
        step.count ?? "",
        step.into ?? "",
      ].map(String);
      fields.forEach((value, i) => {
        if (/[|\t\r\n]/.test(value)) {
          process.stderr.write(
            `fixtures: step ${step.kind}: field ${NAMES[i]} contains "|", a tab or a newline: ` +
            JSON.stringify(value) + "\n"
          );
          process.exit(1);
        }
      });
      process.stdout.write(fields.join("|") + "\n");
    }
  ' "$STEPS_FILE"
}

echo "fixtures: building ${GIT_BASIC}"
rm -rf "$GIT_BASIC" "$GIT_BARE"
mkdir -p "$GIT_BASIC"
cd "$GIT_BASIC"

DEFAULT_BRANCH="$(steps_field default_branch)"
BASE_EPOCH="$(steps_field base_epoch)"
SECONDS_PER_STEP="$(steps_field seconds_per_step)"
AUTHOR_NAME="$(steps_field author.name)"
AUTHOR_EMAIL="$(steps_field author.email)"
BOT_NAME="$(steps_field bot.name)"
BOT_EMAIL="$(steps_field bot.email)"

git init --quiet --initial-branch="$DEFAULT_BRANCH" .
git config core.autocrlf false
git config core.fileMode true
git config commit.gpgsign false

while IFS='|' read -r kind index dir role subject who name count into; do
  case "$kind" in
    commit)
      # Copy the step's whole tree over the working tree. Full snapshots rather
      # than patches: a snapshot cannot half-apply, and it is what makes the two
      # drivers trivially identical.
      cp -R "${CONTENT_DIR}/${dir}/." .
      git add -A
      if [[ "$who" == "bot" ]]; then
        commit_as "$index" "$BOT_NAME" "$BOT_EMAIL" "$subject"
      else
        commit_as "$index" "$AUTHOR_NAME" "$AUTHOR_EMAIL" "$subject"
      fi
      record "$role" "$subject"
      ;;

    generate)
      # 200 files, for depth selection AND truncation (SPEC §9.3, §9.4).
      #
      # Each file is deliberately verbose. The two-line version this replaced made a
      # 44 KB diff against a 512 KB default budget, so §9.4's caps could never fire:
      # the step claimed to exercise truncation and only ever exercised depth, and
      # every unit test passed because each lowered the budget to suit itself. The
      # M6 exit gate caught it. See REVL-118 and ADR 0025.
      #
      # ASCII only, and no locale-dependent formatting: build.ps1 must produce these
      # bytes exactly, and `fixture_parity` compares them.
      mkdir -p "$into"
      for n in $(seq -w 1 "$count"); do
        {
          printf '/// Generated fixture module %s.\n' "$n"
          printf '///\n'
          printf '/// Deliberately verbose: 200 of these must exceed max_total_diff_bytes\n'
          printf '/// (512 KB) so that SPEC 9.4 truncation runs at its DEFAULT settings.\n'
          printf 'pub const ID_%s: u32 = %s;\n' "$n" "$((10#$n))"
          printf '\n'
          for k in $(seq -w 1 40); do
            printf 'pub fn value_%s_%s(input: u32) -> u32 { input.wrapping_add(%s).wrapping_mul(3) }\n' \
              "$n" "$k" "$((10#$k))"
          done
        } > "${into}/mod_${n}.rs"
      done
      git add -A
      commit_as "$index" "$AUTHOR_NAME" "$AUTHOR_EMAIL" "$subject"
      record "$role" "$subject"
      ;;

    branch)
      git checkout --quiet -b "$name"
      ;;

    checkout)
      git checkout --quiet "$name"
      ;;

    merge)
      # --no-ff so it is a real merge with two parents; a fast-forward would have
      # one and M4's merge skip rule would never fire.
      set_commit_time "$index"
      GIT_AUTHOR_NAME="$AUTHOR_NAME" GIT_AUTHOR_EMAIL="$AUTHOR_EMAIL" \
      GIT_COMMITTER_NAME="$AUTHOR_NAME" GIT_COMMITTER_EMAIL="$AUTHOR_EMAIL" \
        git merge --quiet --no-ff --no-gpg-sign -m "$subject" "$name"
      record "$role" "$subject"
      ;;

    *)
      echo "fixtures: unknown step kind ${kind}" >&2
      exit 1
      ;;
  esac
done < <(steps_lines)

write_manifest "${GIT_BASIC}/.manifest.json"

# The manifest is generated, so it must not be a dirty file in the working tree —
# M4's gate asserts `git status --porcelain` is empty after a review.
printf '.manifest.json\n' > .git/info/exclude

COMMIT_COUNT="$(git rev-list --count HEAD)"
if [[ "$COMMIT_COUNT" -ne 12 ]]; then
  echo "fixtures: expected 12 commits on ${DEFAULT_BRANCH}, got ${COMMIT_COUNT}" >&2
  exit 1
fi

# --- git-bare ---------------------------------------------------------------
#
# A --mirror clone, for post-receive hook tests (SPEC §16.2, §7.2).

echo "fixtures: building ${GIT_BARE}"
git clone --quiet --mirror "$GIT_BASIC" "$GIT_BARE"

echo "fixtures: git-basic has ${COMMIT_COUNT} commits; manifest at ${GIT_BASIC}/.manifest.json"

# --- svn-basic --------------------------------------------------------------
#
# The SVN half is optional at build time. `svn` is not installed everywhere the
# inner loop runs, and M3's gate says the svn portion must skip cleanly when it
# is absent. Skipping writes a manifest that SAYS it skipped, so a test can tell
# "svn was absent" from "the generator failed" — SPEC §18's no-silent-caps rule
# applies to fixtures too.

# shellcheck source=fixtures/svn.sh
source "${FIXTURE_ROOT}/svn.sh"

if command -v svn >/dev/null 2>&1 && command -v svnadmin >/dev/null 2>&1; then
  echo "fixtures: building ${OUT_DIR}/svn-basic"
  build_svn_fixture "$OUT_DIR"
else
  echo "fixtures: SKIPPING svn-basic — svn/svnadmin not on PATH."
  echo "fixtures:   SVN tests will skip. Install Subversion and re-run to enable them."
  write_svn_skip_manifest "$OUT_DIR" "svn and svnadmin are not on PATH"
fi
