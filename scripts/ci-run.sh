#!/usr/bin/env bash
# Runs a command for CI and, if it fails, reports what went wrong as GitHub
# annotations (shown on the run page and in the checks API, so a failure can be
# read without downloading logs):
#   * a `-D warnings` lint run is repeated with `-W warnings`, so every crate is
#     checked and all diagnostics are listed (not just those of the first crate
#     that failed);
#   * the end of the output, for build-script panics and failing tests.
#   scripts/ci-run.sh cargo clippy --workspace --all-targets -- -D warnings
set -uo pipefail
log=$(mktemp)
"$@" 2>&1 | tee "$log"
status=${PIPESTATUS[0]}
[ "$status" -eq 0 ] && exit 0

strip() { sed -E 's/\x1b\[[0-9;]*[A-Za-z]//g; s/\r//g' "$1"; }
# One annotation per call (GitHub shows at most 10 error annotations per step).
annotate() { # title file
  sed -e 's/%/%25/g' "$2" | awk 'BEGIN { ORS = "%0A" } { print }' |
    sed -e "s|^|::error title=$1::|"
  echo
}

args=("$@")
if [[ " ${args[*]} " == *" -D warnings"* ]]; then
  relaxed=()
  for a in "${args[@]}"; do [ "$a" = "-D" ] && a="-W"; relaxed+=("$a"); done
  # `cargo clippy … -- -W warnings`: put --message-format before the `--`.
  short=()
  for a in "${relaxed[@]}"; do
    [ "$a" = "--" ] && short+=("--message-format=short")
    short+=("$a")
  done
  diag=$(mktemp)
  CARGO_TERM_COLOR=never "${short[@]}" 2>&1 |
    grep -E '^[^ ]+:[0-9]+:[0-9]+: (warning|error)' | sort -u > "$diag"
  echo "---- all diagnostics ($(wc -l < "$diag")) ----"
  cat "$diag"
  [ -s "$diag" ] && annotate "Diagnostics ($1 ${2:-})" "$diag"
fi
tail_file=$(mktemp)
strip "$log" | tail -n 60 > "$tail_file"
annotate "Output tail ($1 ${2:-})" "$tail_file"
exit "$status"
