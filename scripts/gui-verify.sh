#!/usr/bin/env bash
# Capture one settled frame of a named screen (RL-1102, SPEC §16.4).
#
# Usage: scripts/gui-verify.sh <screen> [<screen> ...]
#        scripts/gui-verify.sh all
#
# Writes artifacts/gui/<screen>.png and exits non-zero if any screen never
# appeared. ADR 0032: this is a *local* gate. A hosted macOS runner cannot grant
# Screen Recording consent, so CI compiles the shell, smoke-tests that it starts,
# and runs the vitest layer instead.
#
# Why a script rather than a remembered command line: a capture is only
# comparable to the last one if it was taken the same way. The ROI, the settle
# time and the fixture data all have to be identical or the diff is noise.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

BIN="${REVLOCAL_DESKTOP_BIN:-$ROOT/target/debug/revlocal-desktop}"
OUT_DIR="$ROOT/artifacts/gui"
FIXTURE_DB="${REVLOCAL_GUI_DB:-$ROOT/artifacts/gui/fixture.db}"

# Long enough for a cold start with a debug build; the timeout is the bound that
# matters and `--settle-ms` only decides when a *drawn* window counts as done.
SETTLE_MS="${REVLOCAL_SETTLE_MS:-800}"
TIMEOUT="${REVLOCAL_GUI_TIMEOUT:-40}"

# No ROI by default, and that is a decision rather than an omission.
#
# §16.4 asks for a crop that clips host chrome so captures are comparable. The
# obvious implementation — crop from the window size in `tauri.conf.json` — is
# wrong, and wrong in a way that cost an afternoon: the *captured surface* is not
# the configured size. A 1100x760 window captures as 1094x756 here, so an ROI
# derived from the config overflows by six pixels and framewatch fails rather than
# clipping. That produced a gate which passed about one run in three, and a flaky
# gate is worse than none because it teaches people to re-run until green.
#
# framewatch already captures the *window*, not the screen, so the only thing an
# ROI would remove is the title bar — and a title bar that is identical between
# two runs does not hurt comparability at all. Reliability beats a cosmetic crop.
#
# `REVLOCAL_ROI` opts back in for anybody who measures their own surface first:
#   framewatch shot ... --out-file /tmp/probe.png   # then read its dimensions
ROI="${REVLOCAL_ROI:-}"

SCREENS=("$@")
if [[ ${#SCREENS[@]} -eq 0 ]]; then
  echo "usage: $0 <screen> [<screen> ...] | all" >&2
  exit 2
fi
if [[ "${SCREENS[0]}" == "all" ]]; then
  SCREENS=(dashboard repository:1 repository:3 findings approvals run)
fi

if ! command -v framewatch >/dev/null 2>&1; then
  # Refuse rather than skip. A capture gate that passes when the tool is absent
  # is indistinguishable from one that captured a blank screen, which is the
  # failure §16.4 exists to prevent.
  echo "gui-verify: framewatch is not on PATH; nothing was captured" >&2
  exit 1
fi
if [[ ! -x "$BIN" ]]; then
  echo "gui-verify: $BIN is not built" >&2
  echo "  try: cargo build -p revlocal-tauri --features desktop --bin revlocal-desktop" >&2
  exit 1
fi

mkdir -p "$OUT_DIR"

# Fixture data, rebuilt each run so a capture never depends on what the last one
# left behind. Determinism is the whole point of comparing two PNGs.
if [[ ! -f "$FIXTURE_DB" ]]; then
  echo "gui-verify: building fixture data at $FIXTURE_DB"
  "$ROOT/scripts/gui-fixture.sh" "$FIXTURE_DB"
fi

failed=0
for screen in "${SCREENS[@]}"; do
  out="$OUT_DIR/${screen//:/-}.png"
  rm -f "$out"

  # `--settle-best-effort` is deliberately NOT passed. §16.4: a screen that never
  # settles must fail the gate, and writing "the latest frame anyway" is how a
  # half-painted window passes as a rendered one.
  # `${a[@]}` on an EMPTY array is an "unbound variable" error under `set -u`
  # in bash 3.2, which is what macOS ships and therefore what runs here. The
  # array is seeded with a harmless flag rather than left empty, because the
  # alternatives — dropping `set -u`, or requiring bash 5 — both cost more than
  # this line. REVL-29 was the same trap in the fixture builder.
  roi_args=(--settle-ms "$SETTLE_MS")
  [[ -n "$ROI" ]] && roi_args+=(--roi "$ROI")

  # The screen name has to reach the app, not just name the output file. Without
  # this every capture was the dashboard under a different filename — which looks
  # exactly like a working gate until somebody opens two PNGs and finds the same
  # picture. `initial_screen` reads this on mount.
  # `repository:3` captures screen `repository` with repo 3 selected. §15's
  # repository screen is about *a* repository, and REVL-92 wants it captured in
  # both vocabularies — a git repo and an SVN one are different screens, not
  # different words, and only a second capture shows that.
  want_screen="${screen%%:*}"
  want_repo="${screen#*:}"
  [[ "$want_repo" == "$screen" ]] && want_repo=0

  if REVLOCAL_DB="$FIXTURE_DB" REVLOCAL_SCREEN="$want_screen" REVLOCAL_REPO="$want_repo" framewatch shot \
       --launch "$BIN" \
       --title "rev-local" \
       --out-file "$out" \
       "${roi_args[@]}" \
       --timeout "$TIMEOUT"
  then
    if [[ -s "$out" ]]; then
      echo "gui-verify: $screen -> $out"
    else
      # framewatch exited 0 and wrote nothing. Belt and braces, because an empty
      # PNG is exactly the "silently captured nothing" case.
      echo "gui-verify: $screen produced an empty file" >&2
      failed=1
    fi
  else
    echo "gui-verify: $screen never settled within ${TIMEOUT}s" >&2
    failed=1
  fi
done

exit "$failed"
