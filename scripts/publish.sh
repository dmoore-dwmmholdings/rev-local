#!/usr/bin/env bash
# Publish the current `master` tree to the public repository.
#
# The public repository carries the product. It does not carry how the product
# gets built: the files listed in PRIVATE below stay on `master` and are never
# pushed, and the public branch is an orphan history so they have never been in a
# commit that left this machine.
#
# This works by building a tree with git's plumbing rather than by checking
# anything out, so `master`, the index and the working tree are never touched. It
# appends one commit to `public` per publish — no force-push, no rewriting what is
# already out there.
#
# Usage: ./scripts/publish.sh "commit subject" ["body"]

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# Everything that describes the build process rather than the product.
PRIVATE=(
  AGENTS.md
  BUILD_PROMPT.md
  docs/BUILD_LOOP.md
  docs/STATE.md
  docs/backlog
  scripts
)

SUBJECT="${1:-}"
BODY="${2:-}"
if [[ -z "$SUBJECT" ]]; then
  echo "usage: $0 \"commit subject\" [\"body\"]" >&2
  exit 1
fi

if [[ -n "$(git status --porcelain)" ]]; then
  echo "publish: the working tree is dirty; commit to master first" >&2
  exit 1
fi

# A scratch index, so the real one is untouched.
INDEX="$(mktemp -t revlocal-publish-index)"
rm -f "$INDEX"
export GIT_INDEX_FILE="$INDEX"
trap 'rm -f "$INDEX"' EXIT

git read-tree HEAD
for path in "${PRIVATE[@]}"; do
  git rm --cached -q -r --ignore-unmatch "$path"
done

TREE="$(git write-tree)"

# Refuse to publish if anything private survived. A grep over the tree listing is
# cheap; discovering it in a public repository is not.
LEAKED="$(git ls-tree -r --name-only "$TREE" \
  | grep -E '^(AGENTS\.md|BUILD_PROMPT\.md|docs/BUILD_LOOP\.md|docs/STATE\.md|docs/backlog/|scripts/)' || true)"
if [[ -n "$LEAKED" ]]; then
  echo "publish: refusing — private paths are still in the tree:" >&2
  echo "$LEAKED" >&2
  exit 1
fi

# Path exclusion is not enough: a published file can *mention* the process. ADR
# 0002 shipped once describing the private backlog import, because the first guard
# only looked at filenames. This one reads the tree's contents.
MARKERS='BUILD_LOOP|BUILD_PROMPT|AGENTS\.md is|docs/STATE|docs/backlog|gen_backlog|build loop|autonomous agent|implementing agent'
MENTIONS=""
while read -r file; do
  case "$file" in
    Cargo.lock|ui/*|src-tauri/target/*) continue ;;
  esac
  hit="$(git show "$TREE:$file" 2>/dev/null | grep -nE "$MARKERS" || true)"
  if [[ -n "$hit" ]]; then
    MENTIONS="$MENTIONS
$file: $hit"
  fi
done < <(git ls-tree -r --name-only "$TREE")

if [[ -n "$MENTIONS" ]]; then
  echo "publish: refusing — published files mention the build process:" >&2
  echo "$MENTIONS" >&2
  exit 1
fi

PARENT=""
if git rev-parse --verify --quiet public >/dev/null; then
  PARENT="-p public"
fi

MESSAGE="$SUBJECT"
if [[ -n "$BODY" ]]; then
  MESSAGE="$SUBJECT

$BODY"
fi

# shellcheck disable=SC2086
COMMIT="$(printf '%s\n' "$MESSAGE" | git commit-tree "$TREE" $PARENT)"
git branch -f public "$COMMIT"

unset GIT_INDEX_FILE

echo "publish: public -> $COMMIT"
echo "publish: $(git ls-tree -r --name-only public | wc -l | tr -d ' ') files"
git push origin public:main
