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

# An ROI by default, because the window's own bounds are what never settle.
#
# This file previously said "no ROI by default, and that is a decision" — and the
# reasoning was wrong in a way worth recording, because the evidence for it was
# real. Deriving a crop from `tauri.conf.json` DOES overflow: a 1100x760 window
# captures as a smaller surface, so that ROI failed rather than clipped. The
# lesson was "measure the surface", and what got written down instead was "do not
# crop".
#
# The cost of that was REVL-88: a gate that settled about one run in four, cause
# unknown, for weeks. Three consecutive best-effort captures finally showed it —
# 1096x758, then 1100x760, then 1100x760. **The captured surface changes size
# between frames.** framewatch settles by comparing consecutive frames, and two
# frames of different sizes can never match, so a window whose bounds are still
# moving never settles no matter how long it is given. That is why a 90-second
# timeout failed exactly as fast as a 40-second one.
#
# Cropping to a region safely inside the smallest observed surface makes the
# comparison immune to the outer bounds. Measured, not derived: 4/6 without,
# 12/12 with, across two screens in one sitting. The region is sized against the
# SMALLEST surface seen (1094x756), not the configured window — that is the
# measurement the first attempt skipped.
#
# `REVLOCAL_ROI` overrides it; `REVLOCAL_ROI=""` disables cropping entirely, which
# is how the old behaviour is reproduced if this ever needs re-testing.
ROI="${REVLOCAL_ROI-4,4,1086,748}"

# How many times one screen may be attempted before the gate fails.
#
# Three, not "until it works": a screen that needs more than three goes has a
# problem worth knowing about, and an unbounded retry is a gate that cannot fail.
ATTEMPTS="${REVLOCAL_ATTEMPTS:-3}"

SCREENS=("$@")
if [[ ${#SCREENS[@]} -eq 0 ]]; then
  echo "usage: $0 <screen> [<screen> ...] | all" >&2
  exit 2
fi
if [[ "${SCREENS[0]}" == "all" ]]; then
  SCREENS=(dashboard repository:1 repository:3 run:1 findings approvals settings
           onboarding:check onboarding:add_repo onboarding:pick_engine
           onboarding:pick_autonomy onboarding:first_review)
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
  want_id="${screen#*:}"
  [[ "$want_id" == "$screen" ]] && want_id=0

  # One selector, routed by screen. `repository:3` and `run:1` read the same way
  # and the app takes them on different variables, so the split happens here
  # rather than making the caller remember which is which.
  want_repo=0; want_run=0; want_step=""
  case "$want_screen" in
    repository) want_repo="$want_id" ;;
    run)        want_run="$want_id" ;;
    # `onboarding:pick_autonomy` photographs one step of the flow. RL-1103's
    # scripted flow capture is the real answer; until it exists, one shot per
    # step beats one shot of a wizard nobody clicked through.
    onboarding) want_step="$want_id"; want_screen="dashboard" ;;
  esac

  # Bounded retries, and the count is printed when one is used.
  #
  # Not a way of passing a gate that failed: every attempt still has to produce a
  # settled frame, and running out of attempts still fails. It is here because
  # even with the ROI a launch occasionally loses the race — 20 of 21 in one
  # sitting — and re-running the whole sweep by hand is how somebody ends up
  # re-running until green, which is the habit this gate exists to prevent.
  #
  # §18: a retry that said nothing would hide a screen that needs three goes from
  # the person who could fix it.
  attempt=0
  settled=0
  while (( attempt < ATTEMPTS )); do
    attempt=$(( attempt + 1 ))

    if REVLOCAL_DB="$FIXTURE_DB" REVLOCAL_SCREEN="$want_screen" REVLOCAL_REPO="$want_repo" REVLOCAL_RUN="$want_run" REVLOCAL_ONBOARDING="$want_step" framewatch shot \
         --launch "$BIN" \
         --title "rev-local" \
         --out-file "$out" \
         "${roi_args[@]}" \
         --timeout "$TIMEOUT"
    then
      if [[ -s "$out" ]]; then
        settled=1
        break
      fi
      # framewatch exited 0 and wrote nothing. Belt and braces, because an empty
      # PNG is exactly the "silently captured nothing" case.
      echo "gui-verify: $screen produced an empty file (attempt $attempt)" >&2
    else
      echo "gui-verify: $screen never settled within ${TIMEOUT}s (attempt $attempt)" >&2
    fi
  done

  if (( settled == 1 )); then
    if (( attempt > 1 )); then
      echo "gui-verify: $screen -> $out (settled on attempt $attempt of $ATTEMPTS)"
    else
      echo "gui-verify: $screen -> $out"
    fi
  else
    echo "gui-verify: $screen never settled in $ATTEMPTS attempts" >&2
    failed=1
  fi
done

exit "$failed"
