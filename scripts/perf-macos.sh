#!/usr/bin/env bash
# Performance check on macOS (the counterpart of perf-windows.ps1): runs a release
# build through scripted sessions and reports how much CPU / memory it uses and how
# smoothly it draws. Needs internet (Home feed, one song played muted) and a display
# that is awake; nothing touches your real settings, keychain or library.
#   scripts/perf-macos.sh [--binary PATH] [--settings settings.json] [--report out.md]
#                         [--no-checks] [--keep] [--video]
# Builds `target/release/music-gpui` first unless --binary is given (e.g. the app
# bundle's Contents/MacOS/cratetune). --settings copies a settings.json into the
# throw-away settings folder (your own look and bar styles). Exits 1 when a check fails
# (see "Checks" at the end); --no-checks only reports. --keep keeps the sessions' logs.
# --video adds music-video sessions (a music video played with the setting off and on:
# Now Playing closed, open, theater mode); a build without videos runs them as plain playback.
# What each number means: docs/TESTING.md, "Performance".
set -euo pipefail

# Keep the display (and so the window's display-link frames) awake throughout.
if [ -z "${PERF_CAFFEINATED:-}" ]; then
  PERF_CAFFEINATED=1 exec caffeinate -d -i -u "$0" "$@"
fi

root=$(cd "$(dirname "$0")/.." && pwd)
binary=""
settings=""
report=""
checks=1
keep=0
video=0
while [ $# -gt 0 ]; do
  case $1 in
    --binary) binary=$2; shift 2 ;;
    --settings) settings=$2; shift 2 ;;
    --report) report=$2; shift 2 ;;
    --no-checks) checks=0; shift ;;
    --keep) keep=1; shift ;;
    --video) video=1; shift ;;
    -h | --help) sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[ "$(uname -s)" = Darwin ] || { echo "macOS only (Windows: scripts/perf-windows.ps1)" >&2; exit 2; }

if [ -z "$binary" ]; then
  (cd "$root" && cargo build --release -p music-gpui)
  binary=$root/target/release/music-gpui
fi
[ -x "$binary" ] || { echo "no build at $binary" >&2; exit 2; }

work=$(mktemp -d "${TMPDIR:-/tmp}/cratetune-perf.XXXXXX")
pid=""
cleanup() {
  [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
  if [ "$keep" = 1 ]; then echo "logs: $work"; else rm -rf "$work"; fi
}
trap cleanup EXIT

now() { perl -MTime::HiRes=time -e 'printf "%.3f\n", time'; }
# `ps -o time=`: [[h:]m]m:ss.hh of CPU time → seconds.
cpu_secs() {
  ps -o time= -p "$1" 2>/dev/null | awk -F: '{ s = 0; for (i = 1; i <= NF; i++) s = s * 60 + $i; print s }'
}
# `footprint`'s "Footprint: 123 MB" (what Activity Monitor shows as Memory) → MB.
footprint_mb() {
  footprint "$1" 2>/dev/null | awk '/Footprint:/ {
    for (i = 1; i < NF; i++) if ($i == "Footprint:") { v = $(i + 1); u = $(i + 2) }
    if (u == "KB") v /= 1024; else if (u == "GB") v *= 1024
    printf "%.0f\n", v; exit }'
}

# Runs one session: a fresh settings folder, MUSIC_TIMINGS on, the given MUSIC_SCRIPT
# (which must end with @quit). Writes its log to $work/NAME.log and one sample per
# second to $work/NAME.samples: seconds since launch, % of one core over the last
# second, footprint MB (every 5 s, else -).
session() { # name script
  local name=$1 cfg="$work/$1" log="$work/$1.log" samples="$work/$1.samples"
  mkdir "$cfg"
  [ -n "$settings" ] && cp "$settings" "$cfg/settings.json"
  MUSIC_CONFIG_DIR=$cfg MUSIC_SECRET_STORE=file MUSIC_VOLUME=0.0 MUSIC_TRAY=0 \
    MUSIC_ON_TOP=1 MUSIC_SMOOTH_WHEEL=1 MUSIC_TIMINGS=1 MUSIC_SCRIPT=$2 \
    "$binary" >"$log" 2>&1 &
  pid=$!
  local start prev_t="" prev_c="" t c n=0 mem
  start=$(now)
  : >"$samples"
  while kill -0 "$pid" 2>/dev/null; do
    sleep 1
    c=$(cpu_secs "$pid") || break
    [ -n "$c" ] || break
    t=$(awk -v a="$(now)" -v b="$start" 'BEGIN { print a - b }')
    n=$((n + 1))
    mem=-
    [ $((n % 5)) = 0 ] && mem=$(footprint_mb "$pid") && [ -n "$mem" ] || mem=${mem:--}
    if [ -n "$prev_t" ]; then
      awk -v t="$t" -v pt="$prev_t" -v c="$c" -v pc="$prev_c" -v m="$mem" \
        'BEGIN { printf "%.2f %.2f %s\n", t, 100 * (c - pc) / (t - pt), m }' >>"$samples"
    fi
    prev_t=$t prev_c=$c
    # A hung session is a failure, not an endless run.
    if awk -v t="$t" 'BEGIN { exit !(t > 120) }'; then kill "$pid" 2>/dev/null || true; fi
  done
  wait "$pid" 2>/dev/null || true
  pid=""
}

# The lines between the first `script @FROM` marker and the next `script @TO` one.
phase() { # log from to
  awk -v from="script @$2" -v to="script @$3" '
    on && index($0, to) { exit }
    on { print }
    !on && index($0, from) { on = 1 }' "$1"
}

# `bench N draws: …, p50 B, p95 C` → one "B C" line per bench.
benches() {
  sed -n 's/.*bench [0-9]* draws: avg [0-9.]* ms, p50 \([0-9.]*\), p95 \([0-9.]*\).*/\1 \2/p' "$1"
}

# Renders per second of one view (`app` or `bar`) from stdin: the median over the
# busy seconds (>= 5 renders), and the median of their interval p95s → "FPS P95".
rate() { # view
  sed -n "s/.*music-gpui: $1 \([0-9]*\) renders\/s\(, interval p50 [0-9.]* \/ p95 \([0-9.]*\)\)\{0,1\}.*/\1 \3/p" |
    awk '$1 >= 5 { f[n] = $1; if ($2 != "") p[m++] = $2; n++ }
    function median(a, k,   i, j, x) {
      for (i = 1; i < k; i++) { x = a[i]; for (j = i - 1; j >= 0 && a[j] > x; j--) a[j + 1] = a[j]; a[j + 1] = x }
      return a[int((k - 1) / 2)]
    }
    END { if (n) printf "%s %s\n", median(f, n), (m ? median(p, m) : "") }'
}

# Average % of one core and the largest footprint between seconds FROM and TO → "CORE MB".
usage() { # samples from to
  awk -v from="$2" -v to="$3" '$1 >= from && $1 < to {
      s += $2; n++; if ($3 != "-" && $3 + 0 > m) m = $3 + 0 }
    END { if (n) printf "%.1f %s\n", s / n, (m ? m : "n/a") }' "$1"
}

# The main display's refresh rate (where the window opens), from
# `system_profiler`'s "_spdisplays_resolution" : "1470 x 956 @ 60.00Hz".
refresh=$(system_profiler SPDisplaysDataType -json 2>/dev/null | awk -F'"' '
  /"_spdisplays_resolution"/ { hz = $4; sub(/.*@ */, "", hz); sub(/ *Hz.*/, "", hz); if (hz + 0 > best) best = hz + 0 }
  /"spdisplays_main"/ && /spdisplays_yes/ && hz != "" { main = hz + 0 }
  END { v = main ? main : best; printf "%d\n", (v ? v + 0.5 : 60) }')
chip=$(sysctl -n machdep.cpu.brand_string 2>/dev/null || echo "?")
cores=$(sysctl -n hw.ncpu 2>/dev/null || echo "?")
macos=$(sw_vers -productVersion 2>/dev/null || echo "?")

wheel() { # start notches lines
  awk -v s="$1" -v n="$2" -v l="$3" 'BEGIN {
    for (i = 0; i < n; i++) printf "%s%.1f:@wheel=700;450;%d", (i ? "," : ""), s + i / 10, l }'
}

echo "1/3 idle, draw cost and scrolling on Home..."
# Bench at the top (~3 s), 30 wheel notches down and 10 back up (each glides, as
# MUSIC_SMOOTH_WHEEL=1 does on Windows), bench again, then sit idle.
# `@titlebar-status` only prints, marking the end of the scrolling.
session home "8:@bench=150,$(wheel 14 30 -3),$(wheel 17.2 10 3),19.5:@titlebar-status,21:@bench=150,40:@quit"
top=$(benches "$work/home.log" | sed -n 1p)
deep=$(benches "$work/home.log" | sed -n 2p)
scroll=$(phase "$work/home.log" wheel titlebar-status | rate app)
idle=$(usage "$work/home.samples" 29 39)

echo "2/3 playing a song (muted) with the Equalizer bar..."
session playing "4:@bar-style=equalizer,5:@search=daft punk,10:@play=0,14:@nav=home,32:@bar-style=standard,33:@quit"
bar=$(phase "$work/playing.log" nav bar-style | rate bar)
playing=$(usage "$work/playing.samples" 18 31)
played=$(grep -c "script @play" "$work/playing.log" || true)

echo "3/3 Now Playing open while playing..."
session now-playing "5:@search=daft punk,10:@play=0,14:@open,30:@quit"
np=$(phase "$work/now-playing.log" open quit | rate app)
np_usage=$(usage "$work/now-playing.samples" 18 29)

# Music videos (`--video`): "Instant Crush" (official video, 480p H.264), played muted;
# CPU and memory over seconds 26-45, once the stream, the Home feed and the radio have loaded.
mv=a5uQMwRMHcs
if [ "$video" = 1 ]; then
  echo "video 1/5 music video playing, setting off, Now Playing open..."
  session mv-off "5:@play-id=$mv,9:@open,46:@quit"
  echo "video 2/5 setting on, Now Playing closed..."
  session mv-closed "3:@video=on,5:@play-id=$mv,46:@quit"
  echo "video 3/5 setting on, Now Playing open..."
  session mv-np "3:@video=on,5:@play-id=$mv,9:@open,44:@video-status,46:@quit"
  echo "video 4/5 setting on, theater mode..."
  session mv-theater "3:@video=on,5:@play-id=$mv,9:@open,10:@theater,44:@video-status,46:@quit"
  echo "video 5/5 setting off, theater mode (artwork)..."
  session mv-theater-art "5:@play-id=$mv,9:@open,10:@theater,46:@quit"
fi

# Fields of "A B" values, "n/a" when missing.
f() { printf '%s\n' "$1" | awk -v i="$2" '{ v = $i } END { print (v == "" ? "n/a" : v) }'; }
out=$(cat <<EOF
# CrateTune performance ($(date '+%Y-%m-%d %H:%M'))

$chip, $cores cores, macOS $macos; display $refresh Hz
Build: $binary${settings:+; settings from $settings}

| Measure | Result |
|---|---|
| Idle on Home (nothing playing): CPU | $(f "$idle" 1) % of one core |
| Idle on Home: memory (footprint) | $(f "$idle" 2) MB |
| Full window draw, top of Home | p50 $(f "$top" 1) ms, p95 $(f "$top" 2) ms |
| Full window draw, scrolled down Home | p50 $(f "$deep" 1) ms, p95 $(f "$deep" 2) ms |
| Scrolling Home (wheel glide) | $(f "$scroll" 1) fps (display $refresh Hz), interval p95 $(f "$scroll" 2) ms |
| Playing, Equalizer bar, Home: bar | $(f "$bar" 1) fps (by design ~22), interval p95 $(f "$bar" 2) ms |
| Playing, Home: CPU / memory | $(f "$playing" 1) % of one core, $(f "$playing" 2) MB |
| Now Playing open while playing | $(f "$np" 1) fps, $(f "$np_usage" 1) % of one core, $(f "$np_usage" 2) MB |
EOF
)
if [ "$video" = 1 ]; then
  vrow() { # label session
    local u r
    u=$(usage "$work/$2.samples" 26 45)
    r=$(phase "$work/$2.log" play-id quit | rate app)
    echo "| $1 | $(f "$u" 1) % of one core, $(f "$u" 2) MB, $(f "$r" 1) renders/s (n/a: under 5) |"
  }
  out="$out
$(vrow "Music video, setting off, Now Playing open" mv-off)
$(vrow "Music video, setting on, Now Playing closed" mv-closed)
$(vrow "Music video, setting on, Now Playing open" mv-np)
$(vrow "Music video, setting on, theater mode" mv-theater)
$(vrow "Music video, setting off, theater mode (artwork)" mv-theater-art)"
  for s in mv-np mv-theater; do
    v=$(grep -o "player Ready, shown [0-9]* dropped [0-9]* repositions [0-9]*" "$work/$s.log" | tail -1 || true)
    [ -n "$v" ] && out="$out
| $s: video frames | $v |"
  done
fi

# Checks, the same as on Windows, plus an absolute draw budget: an M-series Mac draws
# Home in ~4 ms; 6 ms leaves room on a 180 Hz display (5.5 ms a frame, minus what the
# compositor needs) for slower Macs and still catches a doubling.
check() { # ok name
  if [ "$1" = 1 ]; then echo "- pass - $2"; else echo "- FAIL - $2"; fi
}
is() { awk "BEGIN { exit !($1) }" && echo 1 || echo 0; }
t50=$(f "$top" 1) d50=$(f "$deep" 1) fps=$(f "$scroll" 1) core=$(f "$idle" 1)
num() { [ "$1" != n/a ]; }
checks_out=$(
  check "$( [ -n "$top" ] && [ -n "$deep" ] && echo 1 || echo 0)" "Home loaded and benchmarked twice"
  check "$(num "$fps" && is "$fps >= 0.9 * ($refresh < 120 ? $refresh : 120)" || echo 0)" \
    "Scrolling >= 90% of min(display, 120 Hz)"
  check "$(num "$t50" && num "$d50" && is "$d50 <= 1.5 * $t50" || echo 0)" \
    "Scrolled-down draw p50 <= 1.5x top-of-Home p50"
  check "$(num "$t50" && num "$d50" && is "$t50 <= 6 && $d50 <= 6" || echo 0)" \
    "Home draw p50 <= 6 ms (180 Hz budget minus headroom)"
  check "$(num "$core" && is "$core <= 5" || echo 0)" "Idle CPU <= 5% of one core"
  check "$( [ "${played:-0}" -gt 0 ] && [ -n "$playing" ] && echo 1 || echo 0)" \
    "Playback session ran (network)"
)
out="$out

## Checks

$checks_out"
printf '%s\n' "$out"
[ -n "$report" ] && printf '%s\n' "$out" >"$report"
if [ "$checks" = 1 ] && printf '%s\n' "$checks_out" | grep -q FAIL; then exit 1; fi
exit 0
