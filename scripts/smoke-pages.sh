#!/usr/bin/env bash
# Opens every Settings section and the main pages once, offline (`MUSIC_FIXTURES`),
# then a few page tabs (opened, switched by script and by their keys, moved, closed),
# Now Playing's designs, the mini player and artist / album links (ids looked up), and
# fails if the app panics, doesn't quit cleanly or a link opens the wrong page. Catches crashes that only
# happen while a page renders (e.g. reading an entity that is being updated), which
# unit tests don't draw. Also switches every Now Playing design's and mini player
# layout's elements on and off at the smallest window and fails if parts overlap or
# leave the window. Then a Listen together session on this computer (no relay): the
# "Start listening together" sheet, a Friends session with the listeners chip, its
# panel and mode picker, a reaction, the mode switch's confirmation and every mode, the
# page and the shared queue in session, and leaving; fails unless each mode was reached.
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
    -h | --help) sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
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
# Every design's elements (`np_elements.rs`) at the smallest window (`MUSIC_ON_TOP` floats
# the window, so a tiling window manager lets it be resized): all of them on, a few
# switched off again, each time the parts on screen checked for overlaps and overflow
# (`@np-bounds`); the customize panel; back to the defaults.
script="$script$t:@resize=860;540,"; t=$((t + 2))
for l in split centered lyrics immersive card minimal editorial queue turntable; do
  script="$script$t:@layout=$l,$t:@np-elements=$l:all,$t:@np-bounds,"; t=$((t + 2))
  script="$script$t:@np-elements=$l:like=off;seek_bar=off;times=off;volume=off,"; t=$((t + 1))
  script="$script$t:@np-bounds,"; t=$((t + 2))
done
script="$script$t:@np-portrait=auto,$t:@layout=turntable,$t:@np-customize,"; t=$((t + 2))
script="$script$t:@np-elements=turntable:shuffle=off,$t:@np-customize=off,"; t=$((t + 1))
script="$script$t:@np-elements-reset=all,$t:@np-elements-status,"; t=$((t + 1))
script="$script$t:@close,$t:@mini-queue=on,$t:@mini=on,"; t=$((t + 3))
script="$script$t:@mini-skin=card,"; t=$((t + 3))
for k in bar compact card lyrics turntable; do
  script="$script$t:@mini-skin=$k,"; t=$((t + 3))
  script="$script$t:@mini-elements=$k:all,"; t=$((t + 3))
  script="$script$t:@np-bounds,$t:@mini-menu,"; t=$((t + 2))
  script="$script$t:@mini-elements=$k:none,"; t=$((t + 3))
done
script="$script$t:@mini-elements-reset=all,"; t=$((t + 3))
script="$script$t:@mini=off,"; t=$((t + 1))
# Artist / album links (`components/entity_links.rs`), as the player bar's: a song without
# ids looks them up (the fixtures' `next_song.json`), then a search result without them
# (the artist search), each page opened and checked by `@links-status` below.
script="$script$t:@play-bare=khnokW3Mw24:Get Lucky:Daft Punk & Julian Casablancas,"; t=$((t + 2))
script="$script$t:@go-artist=1,"; t=$((t + 3))
script="$script$t:@links-status,$t:@go-album,"; t=$((t + 3))
script="$script$t:@links-status,$t:@nav=search:daft%20punk,"; t=$((t + 3))
script="$script$t:@play-bare=0,"; t=$((t + 2))
script="$script$t:@go-artist=0,"; t=$((t + 3))
script="$script$t:@links-status,"; t=$((t + 1))
# Listen together (`integrations/together_ui.rs`, `pages/together.rs`): the sheet, then a
# session on this computer in each mode (chip, panel, picker, reaction, confirmation,
# page, queue panel with who queued what), the sheet once more over a session, leaving.
script="$script$t:@together-network=direct,$t:@together-sheet,"; t=$((t + 2))
script="$script$t:@close,$t:@nav=search:daft%20punk,"; t=$((t + 2))
script="$script$t:@play=0,"; t=$((t + 2))
script="$script$t:@together-start=friends,"; t=$((t + 2))
script="$script$t:@together-panel=on,"; t=$((t + 1))
script="$script$t:@together-mode-picker=on,"; t=$((t + 1))
script="$script$t:@together-react=heart,$t:@together-react=party,"; t=$((t + 1))
script="$script$t:@together-panel=off,$t:@enqueue=1,$t:@queue=on,"; t=$((t + 2))
script="$script$t:@nav=together,"; t=$((t + 2))
script="$script$t:@together-status,$t:@together-confirm-mode=dj,"; t=$((t + 2))
script="$script$t:@close,$t:@together-mode=dj,"; t=$((t + 2))
script="$script$t:@together-status,$t:@together-mode=host,"; t=$((t + 2))
script="$script$t:@together-status,$t:@together-panel=on,"; t=$((t + 2))
script="$script$t:@together-panel=off,$t:@together-sheet,"; t=$((t + 2))
script="$script$t:@close,$t:@queue=off,$t:@together-leave,"; t=$((t + 2))
script="$script$t:@together-status,$t:@nav=together,"; t=$((t + 2))
script="$script$t:@quit"
limit=$((t + 60))

status=0
(
  export MUSIC_CONFIG_DIR MUSIC_FIXTURES
  MUSIC_CONFIG_DIR=$(native "$work/config")
  MUSIC_FIXTURES=$(native "$root/crates/music-core/tests/fixtures/innertube")
  MUSIC_SECRET_STORE=file MUSIC_VOLUME=0.0 MUSIC_TRAY=0 MUSIC_NO_TRANSITIONS=1 \
    MUSIC_DEMO_CLOCK=1 MUSIC_ON_TOP=1 MUSIC_SCRIPT=$script exec "$binary"
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
elif grep -q 'bounds .*FAIL' "$work/app.log"; then
  echo "FAIL: Now Playing / mini player parts overlap or leave the window:" >&2
  grep 'bounds .*FAIL' "$work/app.log" >&2
  status=1
elif [ "$(grep -c 'np bounds .*: ok' "$work/app.log" || true)" != 18 ] ||
  [ "$(grep -c 'mini bounds .*: ok' "$work/app.log" || true)" != 5 ]; then
  echo "FAIL: expected 18 Now Playing and 5 mini player bounds checks:" >&2
  grep 'bounds ' "$work/app.log" >&2 || true
  status=1
elif [ "$tab_lines" != 2 ]; then
  echo "FAIL: expected 2 page tab status lines, got $tab_lines" >&2
  status=1
elif ! tail -1 <<<"$(grep 'music-gpui: page tabs:' "$work/app.log")" | grep -q 'count=1 .*\*explore\['; then
  # ⌘2 / Ctrl+2 with one tab left went on to Explore; ⌘W / Ctrl+W kept the last tab.
  echo "FAIL: the keys didn't pass on with one tab" >&2
  status=1
fi
# Listen together: every mode reached, then idle again.
modes=$(grep -o 'together   mode Some([A-Za-z]*)' "$work/app.log" | sed 's/.*Some(//; s/)//' | tr '\n' ' ')
if [ "$status" = 0 ] && [ "$modes" != "Friends Dj Host " ]; then
  echo "FAIL: Listen together modes reached: '$modes' (expected Friends Dj Host)" >&2
  status=1
elif [ "$status" = 0 ] && ! grep -q 'music-gpui: together idle' "$work/app.log"; then
  echo "FAIL: the Listen together session didn't end" >&2
  status=1
fi
# The three link pages, in order: Julian Casablancas, Random Access Memories, Daft Punk.
links=$(grep 'music-gpui: links:' "$work/app.log" | sed 's/.* route=//' | cut -d'|' -f1 | tr '\n' ' ')
if [ "$status" = 0 ] && [ "$links" != "artist:UCWhpbdGLnR8XtXLr0yl2nYA album:MPREb_K8qWMWVqXGi artist:UCRr1xG_2WIDs18a6cIiCxeA " ]; then
  echo "FAIL: the artist / album links opened: $links" >&2
  status=1
fi
grep 'music-gpui: page tabs:' "$work/app.log" || true
grep -E 'bounds ' "$work/app.log" || true
grep 'music-gpui: links:' "$work/app.log" || true
grep -E 'together (hosting|idle)|together   mode' "$work/app.log" || true
echo "smoke: $navs pages opened, exit status $status"
exit "$status"
