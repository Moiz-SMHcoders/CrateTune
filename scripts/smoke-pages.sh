#!/usr/bin/env bash
# Opens every Settings section and the main pages once, offline (`MUSIC_FIXTURES`),
# then a few page tabs (opened, switched by script and by their keys, moved, closed),
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
# Page tabs (`page_tabs.rs`): new / background tabs, switching (also by ⌘ / Ctrl keys,
# which pass on to Home / Explore / Library once one tab is left), reordering, closing.
case "$(uname -s)" in Darwin) cmd=cmd ;; *) cmd=ctrl ;; esac
tabs="@tab-new=explore @tab-open=charts @tab-open=library:albums @tab=3 @tab=next
  @tab-new=settings:shortcuts @key=$cmd-1 @key=ctrl-tab @tab-move=1:4 @tab=prev
  @nav=search:daft%20punk @key=$cmd-9 @key=$cmd-w @tab-close=1 @tab-status
  @tab-duplicate @tab-new=history @tab=2 @tab-close @tab-close @tab-close @key=$cmd-w
  @key=$cmd-2 @key=$cmd-w @tab-status"
for a in $tabs; do script="$script$t:$a,"; t=$((t + 2)); done
# Friend groups and device sync switched on: their sections and the Together page's card
# draw their full content too.
for a in @friends=on @friends-share=on @device-sync=on @nav=settings:friends @nav=settings:devices \
  @nav=together; do script="$script$t:$a,"; t=$((t + 2)); done
# Now Playing in every design (the stacked split, the turntable's ring of bars) and the
# mini player with "Up next", over the search results (the demo clock plays them).
script="$script$t:@nav=search:daft%20punk,"; t=$((t + 3))
script="$script$t:@play=0,"; t=$((t + 2))
script="$script$t:@np-viz=on,$t:@np-click-empty=on,"; t=$((t + 1))
script="$script$t:@open,"; t=$((t + 2))
for l in split centered lyrics immersive card minimal editorial queue turntable; do
  script="$script$t:@layout=$l,"; t=$((t + 2))
done
script="$script$t:@np-portrait=always,$t:@layout=split,"; t=$((t + 2))
script="$script$t:@close,$t:@mini-queue=on,$t:@mini=on,"; t=$((t + 3))
script="$script$t:@mini-skin=card,"; t=$((t + 3))
script="$script$t:@mini=off,"; t=$((t + 1))
script="$script$t:@quit"
limit=$((t + 60))

status=0
(
  export MUSIC_CONFIG_DIR MUSIC_FIXTURES
  MUSIC_CONFIG_DIR=$(native "$work/config")
  MUSIC_FIXTURES=$(native "$root/crates/music-core/tests/fixtures/innertube")
  MUSIC_SECRET_STORE=file MUSIC_VOLUME=0.0 MUSIC_TRAY=0 MUSIC_NO_TRANSITIONS=1 \
    MUSIC_DEMO_CLOCK=1 MUSIC_SCRIPT=$script exec "$binary"
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
tab_lines=$(grep -c 'music-gpui: page tabs:' "$work/app.log" || true)
if grep -qE 'panicked|fatal runtime error' "$work/app.log"; then
  echo "FAIL: the app panicked:" >&2
  grep -E -A3 'panicked|fatal runtime error' "$work/app.log" >&2
  status=1
elif [ "$status" != 0 ]; then
  echo "FAIL: exit status $status" >&2
  tail -20 "$work/app.log" >&2
elif [ "$tab_lines" != 2 ]; then
  echo "FAIL: expected 2 page tab status lines, got $tab_lines" >&2
  status=1
elif ! tail -1 <<<"$(grep 'music-gpui: page tabs:' "$work/app.log")" | grep -q 'count=1 .*\*explore\['; then
  # ⌘2 / Ctrl+2 with one tab left went on to Explore; ⌘W / Ctrl+W kept the last tab.
  echo "FAIL: the keys didn't pass on with one tab" >&2
  status=1
fi
grep 'music-gpui: page tabs:' "$work/app.log" || true
echo "smoke: $navs pages opened, exit status $status"
exit "$status"
