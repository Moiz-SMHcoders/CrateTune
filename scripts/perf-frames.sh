#!/usr/bin/env bash
# Frame-time regression check, offline (CI step "perf: frame-time"; macOS and Windows).
# Opens a release build on Home filled from the recorded InnerTube responses
# (`MUSIC_FIXTURES`: no network, generated artwork), times full window draws at the
# top (`@bench`), scrolls through a long feed (~120 shelves) to its end and times
# them again. Fails when
#   * a draw at the end costs more than RATIO (1.5) x one at the top: a frame must
#     not get more expensive the further a page is scrolled (sections far outside
#     the viewport aren't built, `components/cull.rs`); runner speed cancels out;
#   * either p50 is above MAX_MS: a ceiling for slow CI runners (docs/TESTING.md).
#
#   scripts/perf-frames.sh [--binary PATH] [--max-ms MS] [--ratio R] [--keep]
#
# Builds `target/release/music-gpui` first unless --binary is given. Needs a desktop
# session (a window is opened; draws are timed without presenting). Settings go to a
# throw-away folder; nothing touches yours. MUSIC_NO_CULL=1 in the environment builds
# every section (as before culling): the check must then fail. --keep keeps the log.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
binary=""
max_ms=${PERF_MAX_MS:-25}
gap=${PERF_GAP_MS:-1}
ratio=1.5
keep=0
while [ $# -gt 0 ]; do
  case $1 in
    --binary) binary=$2; shift 2 ;;
    --max-ms) max_ms=$2; shift 2 ;;
    --ratio) ratio=$2; shift 2 ;;
    --gap) gap=$2; shift 2 ;;
    --keep) keep=1; shift ;;
    -h | --help) sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*) exe=music-gpui.exe; windows=1 ;;
  *) exe=music-gpui; windows=0 ;;
esac
if [ -z "$binary" ]; then
  (cd "$root" && cargo build --release -p music-gpui)
  binary=$root/target/release/$exe
fi
[ -x "$binary" ] || { echo "no build at $binary" >&2; exit 2; }

# Paths handed to the (native) app: Windows form under Git Bash.
native() { if [ "$windows" = 1 ]; then cygpath -w "$1"; else printf '%s\n' "$1"; fi; }

work=$(mktemp -d "${TMPDIR:-/tmp}/cratetune-frames.XXXXXX")
pid=""
cleanup() {
  [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
  if [ "$keep" = 1 ]; then echo "log: $work/app.log"; else rm -rf "$work"; fi
}
trap cleanup EXIT
mkdir "$work/config"

# Timeline (seconds after launch; items run in order, a late one just runs late).
# Each bench draws 60 frames to warm up, then the 300 that count, GAP (1) ms apart:
# with draws back to back the UI thread stays busy, so the system keeps it on a fast
# core at a steady clock (16 ms apart, macOS moves it between efficiency and
# performance cores and the times jump 3x between runs). The top bench waits for
# Home and its artwork; then 60 scroll steps of 1500 px, each followed by two forced draws, so the feed
# keeps loading (and layout catching up) even where the system sends the window no
# frames; the deep bench runs at the end of the feed (2 + 40 pages, ~120 shelves).
script="6:@bench=60:$gap,6.1:@bench=300:$gap"
t=9
for _ in $(seq 1 60); do
  script="$script,$t:@scroll=640;420;-1500,$(awk "BEGIN { print $t + 0.1 }"):@bench=2"
  t=$(awk "BEGIN { print $t + 0.25 }")
done
script="$script,26:@bench=60:$gap,26.1:@bench=300:$gap,32:@quit"

echo "perf-frames: $binary"
(
  export MUSIC_CONFIG_DIR MUSIC_FIXTURES
  MUSIC_CONFIG_DIR=$(native "$work/config")
  MUSIC_FIXTURES=$(native "$root/crates/music-core/tests/fixtures/innertube")
  MUSIC_SECRET_STORE=file MUSIC_VOLUME=0.0 MUSIC_TRAY=0 MUSIC_ON_TOP=1 \
    MUSIC_NO_TRANSITIONS=1 MUSIC_SCRIPT=$script exec "$binary"
) >"$work/app.log" 2>&1 &
pid=$!
# A hung app is a failure, not a stuck CI job.
for _ in $(seq 1 120); do
  kill -0 "$pid" 2>/dev/null || break
  sleep 1
done
if kill -0 "$pid" 2>/dev/null; then
  kill "$pid" 2>/dev/null || true
  echo "the app didn't quit within 120 s" >&2
fi
wait "$pid" 2>/dev/null || true
pid=""

# `bench 300 draws: avg A ms, p50 B, p95 C, max D …` → "B C"
benches=$(sed -n 's/.*bench 300 draws: avg [0-9.]* ms, p50 \([0-9.]*\), p95 \([0-9.]*\).*/\1 \2/p' "$work/app.log")
top=$(printf '%s\n' "$benches" | sed -n 1p)
deep=$(printf '%s\n' "$benches" | sed -n 2p)
pages=$(sed -n 's/.*fixture feed page \([0-9]*\)\/\([0-9]*\).*/\1 \2/p' "$work/app.log" | tail -n 1)
# The feed asks for its first continuation once Home is drawn: before the top bench?
loaded=$(awk '/fixture feed page 1\// { print 1; exit } /bench 300 draws/ { print 0; exit }' "$work/app.log")
cull=on
[ -n "${MUSIC_NO_CULL:-}" ] && [ "${MUSIC_NO_CULL}" != 0 ] && cull="OFF (MUSIC_NO_CULL)"

report=$(awk -v top="$top" -v deep="$deep" -v pages="$pages" -v ratio="$ratio" \
  -v max="$max_ms" -v cull="$cull" -v loaded="$loaded" '
function na(v) { return v == "" ? "n/a" : v }
# Prints the check; returns 1 if it failed.
function check(ok, name) { printf "- %s - %s\n", (ok ? "pass" : "FAIL"), name; return !ok }
BEGIN {
  split(top, t, " "); split(deep, d, " "); split(pages, p, " ")
  printf "| Full window draw (offline Home, culling %s) | p50 | p95 |\n|---|---|---|\n", cull
  printf "| top of Home | %s ms | %s ms |\n", na(t[1]), na(t[2])
  printf "| end of the feed | %s ms | %s ms |\n\n", na(d[1]), na(d[2])
  fail = 0
  fail += check(loaded == 1, "Home was showing the feed when the top bench started")
  fail += check(p[1] != "" && p[1] == p[2], \
    sprintf("feed loaded to its end (%s of %s pages)", na(p[1]), na(p[2])))
  ok = check(t[1] != "" && d[1] != "", "both benchmarks ran")
  fail += ok
  if (!ok) {
    fail += check(d[1] <= ratio * t[1], \
      sprintf("end-of-feed draw p50 <= %s x top p50 (%.2fx)", ratio, d[1] / t[1]))
    fail += check(t[1] <= max && d[1] <= max, sprintf("draw p50 <= %s ms", max))
  }
  exit (fail > 0 ? 1 : 0)
}') && status=0 || status=1
printf '%s\n' "$report"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  { echo "### perf: frame-time ($(uname -s))"; echo; printf '%s\n' "$report"; } >>"$GITHUB_STEP_SUMMARY"
fi
if [ "$status" != 0 ]; then
  echo "---- app log (tail) ----"
  tail -n 40 "$work/app.log"
  if [ -n "${GITHUB_ACTIONS:-}" ]; then
    printf '%s\n' "$report" | grep FAIL | sed 's/^- FAIL - /::error title=perf\: frame-time::/'
  fi
fi
exit "$status"
