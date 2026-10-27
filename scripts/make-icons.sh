#!/usr/bin/env bash
# Renders packaging/icons/app-icon.svg into the platform icon files (committed, so
# builds don't need these tools):
#   packaging/macos/AppIcon.icns       (iconutil)
#   packaging/windows/app.ico          (16–256 px)
#   packaging/linux/icons/<n>x<n>.png  (hicolor sizes) and packaging/icons/app-icon-512.png
# Needs rsvg-convert (librsvg) and, on macOS, iconutil; ImageMagick (`magick`) for the .ico.
set -euo pipefail
cd "$(dirname "$0")/.."
SVG=packaging/icons/app-icon.svg
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

render() { rsvg-convert -w "$1" -h "$1" "$SVG" -o "$2"; }

# macOS .icns
mkdir -p packaging/macos "$TMP/AppIcon.iconset"
for size in 16 32 128 256 512; do
  render "$size" "$TMP/AppIcon.iconset/icon_${size}x${size}.png"
  render $((size * 2)) "$TMP/AppIcon.iconset/icon_${size}x${size}@2x.png"
done
if command -v iconutil >/dev/null; then
  iconutil -c icns "$TMP/AppIcon.iconset" -o packaging/macos/AppIcon.icns
else
  echo "iconutil not found (macOS only): skipping AppIcon.icns" >&2
fi

# Linux hicolor PNGs
mkdir -p packaging/linux/icons
for size in 32 48 64 128 256 512; do
  render "$size" "packaging/linux/icons/${size}x${size}.png"
done
cp packaging/linux/icons/512x512.png packaging/icons/app-icon-512.png

# Windows .ico
mkdir -p packaging/windows
for size in 16 24 32 48 64 128 256; do render "$size" "$TMP/ico-$size.png"; done
magick "$TMP"/ico-16.png "$TMP"/ico-24.png "$TMP"/ico-32.png "$TMP"/ico-48.png \
  "$TMP"/ico-64.png "$TMP"/ico-128.png "$TMP"/ico-256.png packaging/windows/app.ico
echo "icons written"
