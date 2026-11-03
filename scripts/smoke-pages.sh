#!/usr/bin/env bash
# Opens every Settings section and the main pages once, offline (`MUSIC_FIXTURES`),
# and fails if the app panics or doesn't quit cleanly. Catches crashes that only
# happen while a page renders (e.g. reading an entity that is being updated), which
# unit tests don't draw.
#
#   scripts/smoke-pages.sh [--binary PATH]
#
# Builds `target/release/music-gpui` first unless --binary is given. Needs a desktop
# session (CI: the runners' display, Xvfb on Linux). Settings live in a throw-away
# folder; nothing touches yours.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
binary=""
while [ $# -gt 0 ]; do
  case "$1" in
    --binary) binary=$2; shift 2 ;;
    -h | --help) sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
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

native() { if [ "$windows" = 1 ]; then cygpath -w "$1"; else printf '%s\n' "$1"; fi; }

work=$(mktemp -d "${TMPDIR:-/tmp}/cratetune-smoke.XXXXXX")
trap 'rm -rf "$work"' EXIT
mkdir "$work/config"

routes="home explore charts moods new-releases history library:songs library:albums
  library:artists library:playlists downloads stats search:daft%20punk together"
sections="appearance playback modules eq lyrics content network privacy storage account
  integrations sponsorblock together spotify ai-filter cast devices friends backup about
  shortcuts"
script="" t=4
for r in $routes; do script="$script$t:@nav=$r,"; t=$((t + 2)); done
for s in $sections; do script="$script$t:@nav=settings:$s,"; t=$((t + 2)); done
# Friend groups and device sync switched on: their sections and the Together page's card
# draw their full content too.
for a in @friends=on @friends-share=on @device-sync=on @nav=settings:friends @nav=settings:devices \
  @nav=together; do script="$script$t:$a,"; t=$((t + 2)); done
script="$script$t:@quit"
limit=$((t + 60))

status=0
(
  export MUSIC_CONFIG_DIR MUSIC_FIXTURES
  MUSIC_CONFIG_DIR=$(native "$work/config")
  MUSIC_FIXTURES=$(native "$root/crates/music-core/tests/fixtures/innertube")
  MUSIC_SECRET_STORE=file MUSIC_VOLUME=0.0 MUSIC_TRAY=0 MUSIC_NO_TRANSITIONS=1 \
    MUSIC_SCRIPT=$script exec "$binary"
) >"$work/app.log" 2>&1 &
pid=$!
for _ in $(seq 1 "$limit"); do
  kill -0 "$pid" 2>/dev/null || break
  sleep 1
done
if kill -0 "$pid" 2>/dev/null; then
  kill "$pid" 2>/dev/null || true
  echo "FAIL: the app didn't quit within $limit s" >&2
  status=1
fi
wait "$pid" 2>/dev/null || status=$?

navs=$(grep -c 'script @nav' "$work/app.log" || true)
if grep -qE 'panicked|fatal runtime error' "$work/app.log"; then
  echo "FAIL: the app panicked:" >&2
  grep -E -A3 'panicked|fatal runtime error' "$work/app.log" >&2
  status=1
elif [ "$status" != 0 ]; then
  echo "FAIL: exit status $status" >&2
  tail -20 "$work/app.log" >&2
fi
echo "smoke: $navs pages opened, exit status $status"
exit "$status"
