#!/usr/bin/env bash
# Capture a multi-step flow, one captioned frame per step (RL-1103, SPEC §16.4).
#
# Usage: scripts/gui-flow.sh <flow>
#        scripts/gui-flow.sh onboarding
#        scripts/gui-flow.sh add-repo-to-review
#
# Prints the session directory. It contains `timeline.jsonl`, `session.json`, a
# `README_FOR_AGENT.md` framewatch writes for whatever reads it next, and
# `frames/NNNNNN_<label>.png` — one settled frame per step, captioned with the
# step that produced it.
#
# ADR 0032: a *local* gate, like gui-verify.sh. A hosted macOS runner cannot grant
# Screen Recording consent.
#
# # Why one file drives both the caption and the app
#
# §16.4 asks for labels that come "from the driver via --labels-file, so captions
# and actions cannot drift apart". A driver that advanced the app through one
# channel and captioned through another would eventually label a frame with the
# step before or after the one it shows — and a caption that is confidently wrong
# is worse than no caption.
#
# So there is one file. `framewatch watch --labels-file` tails it for captions,
# and the app reads its last line to decide what to show (`REVLOCAL_FLOW_FILE`,
# `flow_step`). One write does both, so they cannot disagree.
#
# # Why the app is launched once
#
# `framewatch watch` follows a window; when that window closes the capture stream
# ends with "Failed to find any displays or windows to capture" and the session
# waits forever for frames that cannot arrive. Tested, not assumed. So a flow is
# one long-lived window that changes what it shows — which is also what a person
# doing these steps would see.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

BIN="${REVLOCAL_DESKTOP_BIN:-$ROOT/target/debug/revlocal-desktop}"
OUT_DIR="$ROOT/artifacts/flow"
FIXTURE_DB="${REVLOCAL_GUI_DB:-$ROOT/artifacts/gui/fixture.db}"

SETTLE_MS="${REVLOCAL_SETTLE_MS:-800}"
# Per step, not for the session: a flow of five steps needs five settles.
STEP_SECS="${REVLOCAL_FLOW_STEP_SECS:-7}"
# Inside the smallest observed surface — see gui-verify.sh for why this matters.
ROI="${REVLOCAL_ROI-4,4,1086,748}"

# Uncaptioned frames the session is allowed before the steps begin.
#
# Two, measured: `initial` when watch attaches, and one `settled` when the window
# finishes its first paint. `--frames` is a hard stop, so a budget that counted
# only the steps ran out one short and the last step never got a frame. Counting
# captioned frames at the end is what actually decides whether the session is
# complete — this only has to be generous enough not to cut it off.
FRAME_SLACK="${REVLOCAL_FLOW_SLACK:-3}"

# The session always terminates, which §16.4 asks for explicitly.
#
# `--frames` alone does not give that: it is a hard *stop*, not a deadline, so a
# session that asked for one more frame than the app happened to produce waited
# for it forever. That is exactly what happened — every step captured correctly
# and the script hung on the last `wait`. `--duration` is the bound that makes
# termination a property rather than a hope; `--frames` stays as the upper bound
# so a flow that finishes early does not sit out its clock.
DURATION=""

# Computed once the step list is known, below.
FLOW="${1:-}"
case "$FLOW" in
  onboarding)
    # §15's guided path, which is the flow RL-1205 built.
    STEPS=(check add_repo pick_engine pick_autonomy first_review)
    ;;
  add-repo-to-review)
    # The flow named in RL-1103: set a repository up, then look at what came out
    # of reviewing it. The fixture database already holds the run and the queued
    # action, so these steps are the screens somebody would visit in order.
    STEPS=(check add_repo pick_autonomy dashboard run:1 findings approvals)
    ;;
  *)
    echo "usage: $0 <onboarding|add-repo-to-review>" >&2
    exit 2
    ;;
esac

# One interval per step, plus the two uncaptioned startup frames, plus a margin
# for the last step to settle before the clock stops.
DURATION=$(( (${#STEPS[@]} + 3) * STEP_SECS ))

if ! command -v framewatch >/dev/null 2>&1; then
  # Refuse rather than skip. A flow gate that passes when the tool is absent is
  # indistinguishable from one that captured nothing, which is the failure §16.4
  # exists to prevent.
  echo "gui-flow: framewatch is not on PATH; nothing was captured" >&2
  exit 1
fi
if [[ ! -x "$BIN" ]]; then
  echo "gui-flow: $BIN is not built" >&2
  echo "  try: cargo build -p revlocal-tauri --features desktop --bin revlocal-desktop" >&2
  exit 1
fi

if [[ ! -f "$FIXTURE_DB" ]]; then
  echo "gui-flow: building fixture data at $FIXTURE_DB"
  "$ROOT/scripts/gui-fixture.sh" "$FIXTURE_DB"
fi

mkdir -p "$OUT_DIR"
LABELS="$(mktemp "${TMPDIR:-/tmp}/revlocal-flow.XXXXXX")"

app=""
watch=""
cleanup() {
  [[ -n "$watch" ]] && kill "$watch" 2>/dev/null || true
  [[ -n "$app" ]] && kill "$app" 2>/dev/null || true
  rm -f "$LABELS"
}
trap cleanup EXIT

# Deliberately empty. The app opens on its default view, `watch` captures that as
# its own uncaptioned `initial` frame, and then every step gets a mark of its own.
#
# The alternative — pre-writing step one so the window opens on it — was tried and
# is worse: a line written before `watch` attaches is a line it never tails, so
# the first frame came out captioned `initial` anyway. Writing it a second time
# after attaching was worse still, because the extra mark landed on a frame that
# had already been captured and pushed every later caption off by one. One write
# per step, and the frame count allows for the initial one.
: > "$LABELS"

REVLOCAL_DB="$FIXTURE_DB" REVLOCAL_FLOW_FILE="$LABELS" "$BIN" >/dev/null 2>&1 &
app=$!

# `${a[@]}` on an EMPTY array is an "unbound variable" error under `set -u` in
# bash 3.2, which is what macOS ships. REVL-29 and gui-verify.sh, twice before.
roi_args=(--settle-ms "$SETTLE_MS")
[[ -n "$ROI" ]] && roi_args+=(--roi "$ROI")

framewatch watch \
  --title "rev-local" \
  --labels-file "$LABELS" \
  --out "$OUT_DIR" \
  --frames "$(( ${#STEPS[@]} + FRAME_SLACK ))" \
  --duration "$DURATION" \
  --wait 30 \
  "${roi_args[@]}" > "$OUT_DIR/watch.log" 2>&1 &
watch=$!

# A full step interval before the first label, not a token pause.
#
# A mark captions the *next* frame, so it has to arrive when the window is
# already still — otherwise it is consumed by the initial settle and every later
# caption slides by one. Two runs were lost to a 3-second wait here.
sleep "$STEP_SECS"

# One label per step, paced so each has time to settle before the next arrives.
for step in "${STEPS[@]}"; do
  echo "$step" >> "$LABELS"
  sleep "$STEP_SECS"
done

# Bounded: `--frames` stops the session at one frame per step, so this waits for
# a capture that is already guaranteed to end.
wait "$watch" 2>/dev/null || true
watch=""

session="$(ls -1dt "$OUT_DIR"/*_rev-local 2>/dev/null | head -1 || true)"
if [[ -z "$session" || ! -d "$session" ]]; then
  echo "gui-flow: no session was written; see $OUT_DIR/watch.log" >&2
  exit 1
fi

# Captioned frames only. The `initial` frame is the app's landing state and is
# not one of the steps, so counting it would let a short session look complete.
frames=$(find "$session/frames" -name '*_settled_*.png' 2>/dev/null | wc -l | tr -d ' ')
if (( frames < ${#STEPS[@]} )); then
  # §18: say what is missing rather than printing a session that looks complete.
  echo "gui-flow: $frames frame(s) for ${#STEPS[@]} step(s) — the session is short" >&2
  echo "$session"
  exit 1
fi

echo "gui-flow: $FLOW — $frames frame(s)"
echo "$session"
