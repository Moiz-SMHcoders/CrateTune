# Sourced by the packaging scripts: reads packaging/app.env (KEY=VALUE, values may
# contain spaces, no shell expansion) into shell variables, plus APP_VERSION from Cargo.
# shellcheck shell=bash
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
while IFS= read -r line || [[ -n "$line" ]]; do
  [[ "$line" =~ ^[[:space:]]*(#|$) ]] && continue
  key="${line%%=*}"
  value="${line#*=}"
  value="${value%\"}"; value="${value#\"}"
  printf -v "$key" '%s' "$value"
done < "$ROOT/packaging/app.env"
APP_VERSION="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
APP_EXECUTABLE=music-gpui
case "$(uname -m)" in
  arm64|aarch64) APP_ARCH=arm64 ;;
  x86_64|amd64) APP_ARCH=x86_64 ;;
  *) APP_ARCH="$(uname -m)" ;;
esac
# AppStream / Flatpak id, Flathub's rule for GitHub projects: io.github.<owner>.<id>, lower
# case, '-' → '_' (io.github.moiz_smhcoders.cratetune).
APPSTREAM_ID="io.github.$(printf '%s' "${UPDATE_REPO%%/*}" | tr 'A-Z-' 'a-z_').$APP_ID"
export APP_NAME APP_ID BUNDLE_ID APP_SUMMARY UPDATE_REPO APP_VERSION APP_EXECUTABLE APP_ARCH ROOT APPSTREAM_ID
