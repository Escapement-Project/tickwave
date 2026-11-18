#!/usr/bin/env bash
# Prints CHANGELOG.md's section for <version>, the release's notes, without its heading. Fails
# when there is none, so no release goes out without notes.
#
# usage: notes.sh <version>
set -euo pipefail
[ $# = 1 ] || { echo "usage: notes.sh <version>" >&2; exit 1; }
cd "$(dirname "$0")/../.."
notes=$(awk -v h="## v$1" '
  $0 == h || index($0, h " ") == 1 { on = 1; next }
  on && /^## / { exit }
  on { print }' CHANGELOG.md | sed -e '/./,$!d')
[ -n "$(printf '%s' "$notes" | tr -d '[:space:]')" ] || {
  echo "notes.sh: CHANGELOG.md has no section for v$1" >&2
  exit 1
}
printf '%s\n' "$notes"
