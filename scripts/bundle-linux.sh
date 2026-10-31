#!/usr/bin/env bash
# Linux packages from the release binary (run on Linux; not tested on macOS):
#
#   scripts/bundle-linux.sh [--no-build] [--appimage]
#
# * target/bundle/<APP_ID>-<version>-linux-<arch>.tar.gz — a relocatable tree:
#     bin/<APP_ID>, share/applications/<APP_ID>.desktop, share/icons/hicolor/*/apps/<APP_ID>.png,
#     share/metainfo/, LICENSE, plus install.sh (copies it into ~/.local or a prefix).
# * --appimage: also target/bundle/<APP_ID>-<version>-<arch>.AppImage, built with
#   appimagetool (https://github.com/AppImage/appimagetool; set APPIMAGETOOL=path or
#   have it on PATH).
#
# Runtime needs (not bundled): a Vulkan driver, libxkbcommon, Wayland or X11 (libxcb),
# fontconfig/freetype, ALSA (libasound2) and D-Bus (MPRIS media keys, Secret Service).
set -euo pipefail
source "$(dirname "$0")/lib/app-env.sh"
cd "$ROOT"

BUILD=1 APPIMAGE=0
for arg in "$@"; do
  case "$arg" in
    --no-build) BUILD=0 ;;
    --appimage) APPIMAGE=1 ;;
    *) echo "unknown option $arg" >&2; exit 2 ;;
  esac
done
[[ $BUILD == 1 ]] && cargo build --release -p music-gpui
ARCH="$(uname -m)"
OUT=target/bundle
NAME="${APP_ID}-${APP_VERSION}-linux-${ARCH}"
TREE="$OUT/$NAME"
rm -rf "$TREE"
mkdir -p "$TREE/bin" "$TREE/share/applications" "$TREE/share/metainfo"
install -m 755 "target/release/$APP_EXECUTABLE" "$TREE/bin/$APP_ID"
sed -e "s|@APP_NAME@|$APP_NAME|g" -e "s|@APP_ID@|$APP_ID|g" -e "s|@APP_SUMMARY@|$APP_SUMMARY|g" \
  packaging/linux/app.desktop.in > "$TREE/share/applications/$APP_ID.desktop"
for png in packaging/linux/icons/*.png; do
  size="$(basename "$png" .png)"
  mkdir -p "$TREE/share/icons/hicolor/$size/apps"
  cp "$png" "$TREE/share/icons/hicolor/$size/apps/$APP_ID.png"
done
# AppStream metadata: packaging/linux/app.metainfo.xml.in, shared with the Flatpak (same
# component id, so software centres see one app).
RELEASE_DATE="${RELEASE_DATE:-$(date -u +%Y-%m-%d)}"
sed -e "s|@METAINFO_ID@|$APPSTREAM_ID|g" -e "s|@DESKTOP_ID@|$APP_ID.desktop|g" \
  -e "s|@APP_NAME@|$APP_NAME|g" -e "s|@APP_SUMMARY@|$APP_SUMMARY|g" -e "s|@APP_ID@|$APP_ID|g" \
  -e "s|@UPDATE_REPO@|$UPDATE_REPO|g" -e "s|@APP_VERSION@|$APP_VERSION|g" \
  -e "s|@RELEASE_DATE@|$RELEASE_DATE|g" \
  packaging/linux/app.metainfo.xml.in | sed '/^<!--$/,/^-->$/d' \
  > "$TREE/share/metainfo/$APPSTREAM_ID.metainfo.xml"
cp LICENSE "$TREE/"
cat > "$TREE/install.sh" <<SH
#!/bin/sh
# Installs into \${PREFIX:-\$HOME/.local}.
set -e
PREFIX="\${PREFIX:-\$HOME/.local}"
cd "\$(dirname "\$0")"
mkdir -p "\$PREFIX"
cp -R bin share "\$PREFIX/"
command -v update-desktop-database >/dev/null && update-desktop-database "\$PREFIX/share/applications" || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q "\$PREFIX/share/icons/hicolor" || true
echo "installed to \$PREFIX (make sure \$PREFIX/bin is on PATH)"
SH
chmod +x "$TREE/install.sh"
tar -C "$OUT" -czf "$OUT/$NAME.tar.gz" "$NAME"
echo "built $OUT/$NAME.tar.gz"

if [[ $APPIMAGE == 1 ]]; then
  TOOL="${APPIMAGETOOL:-$(command -v appimagetool || true)}"
  [[ -n "$TOOL" ]] || { echo "appimagetool not found" >&2; exit 1; }
  APPDIR="$OUT/$APP_ID.AppDir"
  rm -rf "$APPDIR"
  mkdir -p "$APPDIR/usr"
  cp -R "$TREE/bin" "$TREE/share" "$APPDIR/usr/"
  cp "$TREE/share/applications/$APP_ID.desktop" "$APPDIR/"
  cp packaging/linux/icons/256x256.png "$APPDIR/$APP_ID.png"
  cat > "$APPDIR/AppRun" <<SH
#!/bin/sh
HERE="\$(dirname "\$(readlink -f "\$0")")"
exec "\$HERE/usr/bin/$APP_ID" "\$@"
SH
  chmod +x "$APPDIR/AppRun"
  ARCH="$ARCH" "$TOOL" "$APPDIR" "$OUT/${APP_ID}-${APP_VERSION}-${ARCH}.AppImage"
  echo "built $OUT/${APP_ID}-${APP_VERSION}-${ARCH}.AppImage"
fi
