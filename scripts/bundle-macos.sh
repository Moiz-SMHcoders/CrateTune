#!/usr/bin/env bash
# Builds "<APP_NAME>.app", a DMG and the update archive from the release binary.
#
#   scripts/bundle-macos.sh [--no-build] [--no-dmg] [--no-zip] [--target <triple>]
#
# Output (target/bundle/):
# * <APP_NAME>.app
# * <APP_ID>-<version>-macos-<arch>.dmg      first installs (drag to Applications)
# * <APP_ID>-<version>-macos-<arch>.app.zip  the in-app updater's package
#   (`ditto -c -k --keepParent`, keeps the signature; see music_core::updater)
# Name, bundle id and summary come from packaging/app.env, the version from Cargo.toml.
# Signing: ad-hoc (`codesign -s -`) unless MACOS_SIGN_IDENTITY is set (a "Developer ID
# Application: …" identity; then the hardened runtime and packaging/macos/entitlements.plist
# are used — notarize the DMG afterwards with `xcrun notarytool`, needs full Xcode).
set -euo pipefail
source "$(dirname "$0")/lib/app-env.sh"
cd "$ROOT"

BUILD=1 DMG=1 ZIP=1 TARGET=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --no-build) BUILD=0 ;;
    --no-dmg) DMG=0 ;;
    --no-zip) ZIP=0 ;;
    --target) TARGET="$2"; shift ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
  shift
done

if [[ -n "$TARGET" ]]; then
  BIN_DIR="target/$TARGET/release"
  case "$TARGET" in aarch64-*) APP_ARCH=arm64 ;; x86_64-*) APP_ARCH=x86_64 ;; esac
else
  BIN_DIR="target/release"
fi
if [[ $BUILD == 1 ]]; then
  cargo build --release -p music-gpui ${TARGET:+--target "$TARGET"}
fi
BIN="$BIN_DIR/$APP_EXECUTABLE"
[[ -x "$BIN" ]] || { echo "missing $BIN (build first)" >&2; exit 1; }

OUT="target/bundle"
APP="$OUT/$APP_NAME.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/$APP_EXECUTABLE"
cp packaging/macos/AppIcon.icns "$APP/Contents/Resources/AppIcon.icns"
printf 'APPL????' > "$APP/Contents/PkgInfo"

# LSEnvironment: the space-efficient allocator (see crates/music-gpui/src/memory.rs);
# set here, a launch from Finder needs no re-exec.
YEAR="$(date +%Y)"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleDisplayName</key><string>${APP_NAME}</string>
  <key>CFBundleName</key><string>${APP_NAME}</string>
  <key>CFBundleExecutable</key><string>${APP_EXECUTABLE}</string>
  <key>CFBundleIdentifier</key><string>${BUNDLE_ID}</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${APP_VERSION}</string>
  <key>CFBundleVersion</key><string>${APP_VERSION}</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.music</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
  <key>NSMicrophoneUsageDescription</key><string>${APP_NAME} listens for about 10 seconds when you ask it to recognize a song. Only an audio fingerprint is sent, never the recording.</string>
  <key>NSHumanReadableCopyright</key><string>© ${YEAR} ${APP_NAME} contributors · GPL-3.0</string>
  <key>CFBundleGetInfoString</key><string>${APP_SUMMARY}</string>
  <key>LSEnvironment</key><dict><key>MallocSpaceEfficient</key><string>1</string></dict>
</dict>
</plist>
PLIST
plutil -lint "$APP/Contents/Info.plist" >/dev/null

if [[ -n "${MACOS_SIGN_IDENTITY:-}" ]]; then
  codesign --force --options runtime --timestamp \
    --entitlements packaging/macos/entitlements.plist \
    --sign "$MACOS_SIGN_IDENTITY" "$APP"
else
  # Ad-hoc: runs locally; Gatekeeper on other Macs asks to confirm (right-click → Open).
  codesign --force --sign - "$APP"
fi
codesign --verify --strict "$APP"
echo "built $APP ($APP_VERSION, $APP_ARCH)"

if [[ $ZIP == 1 ]]; then
  # The updater unpacks this with `ditto -x -k` next to the installed bundle and
  # checks `codesign --verify --strict`, the bundle id and CFBundleShortVersionString.
  ZIP_PATH="$OUT/${APP_ID}-${APP_VERSION}-macos-${APP_ARCH}.app.zip"
  rm -f "$ZIP_PATH"
  # No extended attributes / resource forks: only local metadata (provenance); the
  # signature lives in the files.
  ditto -c -k --keepParent --norsrc --noextattr "$APP" "$ZIP_PATH"
  echo "built $ZIP_PATH"
fi

if [[ $DMG == 1 ]]; then
  STAGE="$(mktemp -d)"
  trap 'rm -rf "$STAGE"' EXIT
  cp -R "$APP" "$STAGE/"
  ln -s /Applications "$STAGE/Applications"
  DMG_PATH="$OUT/${APP_ID}-${APP_VERSION}-macos-${APP_ARCH}.dmg"
  rm -f "$DMG_PATH"
  hdiutil create -quiet -volname "$APP_NAME" -srcfolder "$STAGE" -ov -format UDZO "$DMG_PATH"
  if [[ -n "${MACOS_SIGN_IDENTITY:-}" ]]; then
    codesign --force --sign "$MACOS_SIGN_IDENTITY" "$DMG_PATH"
  fi
  echo "built $DMG_PATH"
fi
