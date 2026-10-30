#!/usr/bin/env bash
# Frame-time regression check, offline (CI step "perf: frame-time"; macOS and Windows).
# Opens a release build on Home filled from the recorded InnerTube responses
# (`MUSIC_FIXTURES`: no network, generated artwork), times full window draws near the
# top (`@bench`, one screen down), scrolls through a long feed (~60 shelves) to its end
# and times them again. Fails when
#   * a draw at the end costs more than RATIO (1.5) x one near the top: a frame must
#     not get more expensive the further a page is scrolled (sections far outside
#     the viewport aren't built, `components/cull.rs`); runner speed cancels out;
#   * either p50 is above MAX_MS (25): a ceiling for gross slowdowns (docs/TESTING.md 6.3).
#
#   scripts/perf-frames.sh [--binary PATH] [--max-ms MS] [--ratio R] [--gap MS]
#                          [--attempts N] [--keep]
#
# Builds `target/release/music-gpui` first unless --binary is given. Needs a desktop
# session (a window is opened; draws are timed without presenting). Settings go to a
# throw-away folder; nothing touches yours. MUSIC_NO_CULL=1 in the environment builds
# every section (as before culling): the check must then fail. A failed run is
# repeated (--attempts, default 3); it fails if all fail. --keep keeps the logs.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
binary=""
max_ms=${PERF_MAX_MS:-25}
gap=${PERF_GAP_MS:-1}
ratio=1.5
keep=0
attempts=3
while [ $# -gt 0 ]; do
  case $1 in
    --binary) binary=$2; shift 2 ;;
    --max-ms) max_ms=$2; shift 2 ;;
    --ratio) ratio=$2; shift 2 ;;
    --gap) gap=$2; shift 2 ;;
    --attempts) attempts=$2; shift 2 ;;
    --keep) keep=1; shift ;;
    -h | --help) sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
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
  if [ "$keep" = 1 ]; then echo "logs: $work"; else rm -rf "$work"; fi
}
trap cleanup EXIT

# Timeline (seconds after launch; items run in order, a late one just runs late).
# Each bench draws 60 frames to warm up, then the 300 that count, GAP (1) ms apart:
# with draws back to back the UI thread stays busy, so the system keeps it on a fast
# core at a steady clock (16 ms apart, macOS moves it between efficiency and
# performance cores and the times jump 3x between runs). The first bench runs one
# screen down, after Home, its artwork and the feed page that scrolling asks for have
# arrived: there the window shows shelves only, as at the end (at the very top the
# title and chips take a third of it, and the end drew 1.2-1.4x slower than that on
# the CI runners just for having more shelves in view). Then 40 scroll steps of 1500 px, each followed by two forced draws, so the
# feed keeps loading (and layout catching up) even where the system sends the window
# no frames; the deep bench runs at the end of the feed (2 + 20 pages, ~60 shelves).
script="5:@scroll=640;420;-700,5.1:@bench=2,8:@bench=60:$gap,8.1:@bench=300:$gap"
t=11
for _ in $(seq 1 40); do
  script="$script,$t:@scroll=640;420;-1500,$(awk "BEGIN { print $t + 0.1 }"):@bench=2"
  t=$(awk "BEGIN { print $t + 0.25 }")
done
script="$script,28:@bench=60:$gap,28.1:@bench=300:$gap,34:@quit"

# One run of the app, logging to $work/app-N.log.
run() { # n
  local log="$work/app-$1.log"
  rm -rf "$work/config" && mkdir "$work/config"
  (
    export MUSIC_CONFIG_DIR MUSIC_FIXTURES
    MUSIC_CONFIG_DIR=$(native "$work/config")
    MUSIC_FIXTURES=$(native "$root/crates/music-core/tests/fixtures/innertube")
    MUSIC_SECRET_STORE=file MUSIC_VOLUME=0.0 MUSIC_TRAY=0 MUSIC_ON_TOP=1 \
      MUSIC_NO_TRANSITIONS=1 MUSIC_SCRIPT=$script exec "$binary"
  ) >"$log" 2>&1 &
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
}

# Checks the log of run N; prints the report and returns 1 if a check failed.
evaluate() { # n
  local log="$work/app-$1.log" benches top deep pages loaded cull
  # `bench 300 draws: avg A ms, p50 B, p95 C, max D …` → "B C"
  benches=$(sed -n 's/.*bench 300 draws: avg [0-9.]* ms, p50 \([0-9.]*\), p95 \([0-9.]*\).*/\1 \2/p' "$log")
  top=$(printf '%s\n' "$benches" | sed -n 1p)
  deep=$(printf '%s\n' "$benches" | sed -n 2p)
  pages=$(sed -n 's/.*fixture feed page \([0-9]*\)\/\([0-9]*\).*/\1 \2/p' "$log" | tail -n 1)
  # The feed asks for its first continuation once Home is drawn: before the first bench?
  loaded=$(awk '/fixture feed page 1\// { print 1; exit } /bench 300 draws/ { print 0; exit }' "$log")
  cull=on
  [ -n "${MUSIC_NO_CULL:-}" ] && [ "${MUSIC_NO_CULL}" != 0 ] && cull="OFF (MUSIC_NO_CULL)"
  awk -v top="$top" -v deep="$deep" -v pages="$pages" -v ratio="$ratio" \
    -v max="$max_ms" -v cull="$cull" -v loaded="$loaded" -v n="$1" '
  function na(v) { return v == "" ? "n/a" : v }
  # Prints the check; returns 1 if it failed.
  function check(ok, name) { printf "- %s - %s\n", (ok ? "pass" : "FAIL"), name; return !ok }
  BEGIN {
    split(top, t, " "); split(deep, d, " "); split(pages, p, " ")
    printf "| Run %s: full window draw (offline Home, culling %s) | p50 | p95 |\n|---|---|---|\n", n, cull
    printf "| near the top (one screen down) | %s ms | %s ms |\n", na(t[1]), na(t[2])
    printf "| end of the feed | %s ms | %s ms |\n\n", na(d[1]), na(d[2])
    fail = check(loaded == 1, "Home was showing the feed when the first bench started")
    fail += check(p[1] != "" && p[1] == p[2], \
      sprintf("feed loaded to its end (%s of %s pages)", na(p[1]), na(p[2])))
    missing = check(t[1] != "" && d[1] != "", "both benchmarks ran")
    fail += missing
    if (!missing) {
      fail += check(d[1] <= ratio * t[1], \
        sprintf("end-of-feed draw p50 <= %s x near-top p50 (%.2fx)", ratio, d[1] / t[1]))
      fail += check(t[1] <= max && d[1] <= max, sprintf("draw p50 <= %s ms", max))
    }
    exit (fail > 0 ? 1 : 0)
  }'
}

echo "perf-frames: $binary"
# A shared runner (or a busy desktop) can be slow for a while: a failed run is repeated
# (ATTEMPTS, 3) and the check fails only if every run fails; a regression fails them all.
status=1
for n in $(seq 1 "$attempts"); do
  run "$n"
  report=$(evaluate "$n") && status=0 || status=1
  printf '%s\n\n' "$report"
  if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    { echo "### perf: frame-time ($(uname -s))"; echo; printf '%s\n\n' "$report"; } >>"$GITHUB_STEP_SUMMARY"
  fi
  [ "$status" = 0 ] && break
  echo "---- run $n: app log (tail) ----"
  tail -n 30 "$work/app-$n.log"
  echo
done
if [ "$status" != 0 ] && [ -n "${GITHUB_ACTIONS:-}" ]; then
  printf '%s\n' "$report" | grep FAIL | sed 's/^- FAIL - /::error title=perf\: frame-time::/'
fi
exit "$status"
