#!/usr/bin/env bash
# Runs a command for CI and, if it fails, reports compiler diagnostics and the end
# of the output as GitHub annotations (shown on the run page and in the checks API,
# so a failure can be read without downloading logs).
#   scripts/ci-run.sh cargo clippy --workspace --all-targets -- -D warnings
set -uo pipefail
log=$(mktemp)
"$@" 2>&1 | tee "$log"
status=${PIPESTATUS[0]}
if [ "$status" -ne 0 ]; then
  # rustc / clippy diagnostics:   --> path/to/file.rs:12:5  under "error: message"
  awk '
    /^(error|warning)(\[[^]]*\])?: / { msg = $0; next }
    /^ *--> [^ ]+:[0-9]+:[0-9]+/ && msg != "" {
      split($2, loc, ":")
      sub(/^(error|warning)(\[[^]]*\])?: /, "", msg)
      printf "::error file=%s,line=%s,col=%s::%s\n", loc[1], loc[2], loc[3], msg
      msg = ""
    }
  ' "$log" | sort -u | head -40
  # Anything else (build script panics, test failures): the last lines, as one annotation.
  tail -n 60 "$log" | sed -e 's/%/%25/g' -e 's/\r//g' | awk 'BEGIN { ORS = "%0A" } { print }' \
    | sed -e 's/^/::error title=Output tail ('"$1"')::/' && echo
fi
exit "$status"
