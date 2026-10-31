#!/usr/bin/env bash
# Signs packaging/player-fixes.json (writes packaging/player-fixes.json.minisig):
#
#   scripts/sign-player-fixes.sh            # sign (after editing the manifest)
#   scripts/sign-player-fixes.sh --verify   # check the committed pair against the app's key
#   scripts/sign-player-fixes.sh --keygen   # make a new key pair (rotating the key: see below)
#
# The secret key lives OUTSIDE the repository: $PLAYER_FIXES_KEY, default
# ~/.config/cratetune/player-fixes.key (minisign format, no password, mode 0600).
# Never commit it. The public key is compiled into the app
# (crates/music-core/src/youtube/fixes.rs, PUBLIC_KEY); a new key only reaches
# users with an app update, so rotate only if the old one leaked.
#
# The app fetches the pair from raw.githubusercontent.com/<UPDATE_REPO>/main/packaging/
# (and the relay mirror), uses it only if the signature verifies and its `serial` is
# higher than what it has. Bump `serial` with every change. Data only — see
# docs/RELEASING.md for the fields. Stock minisign verifies it too:
#   minisign -Vm packaging/player-fixes.json -P <public key>
set -euo pipefail
cd "$(dirname "$0")/.."

KEY="${PLAYER_FIXES_KEY:-$HOME/.config/cratetune/player-fixes.key}"
MANIFEST=packaging/player-fixes.json
tool() { cargo run --quiet -p music-core --example player_fixes -- "$@"; }

case "${1:-}" in
  --keygen) tool keygen "$KEY" ;;
  --verify) tool verify "$MANIFEST" ;;
  "")
    if [[ ! -f "$KEY" ]]; then
      echo "no signing key at $KEY (set PLAYER_FIXES_KEY, or --keygen for a new one)" >&2
      exit 1
    fi
    tool sign "$KEY" "$MANIFEST"
    tool verify "$MANIFEST"
    ;;
  *) sed -n '2,18p' "$0"; exit 2 ;;
esac
