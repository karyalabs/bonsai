#!/usr/bin/env bash
# Prints the GitHub release notes for a Bonsai version: its section of
# CHANGELOG.md followed by the download note.
#
#   script/release_notes.sh 0.20.0      the notes for 0.20.0
#   script/release_notes.sh --check     fails unless CHANGELOG.md has a
#                                       section for the Cargo version
#   script/release_notes.sh --versions  every version CHANGELOG.md covers
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CHANGELOG="${BONSAI_CHANGELOG:-$ROOT_DIR/CHANGELOG.md}"

cargo_version() {
  awk -F'"' '/^\[workspace.package\]/ { s = 1; next } /^\[/ { s = 0 } s && /^version *=/ { print $2; exit }' "$ROOT_DIR/Cargo.toml"
}

# The lines under "## <version> …", up to the next "## " heading, with
# leading and trailing blank lines removed.
section() {
  awk -v version="$1" '
    /^## / { inside = ($2 == version); next }
    inside { line[++n] = $0; if (NF) { if (!first) first = n; last = n } }
    END { for (i = first; first && i <= last; i++) print line[i] }
  ' "$CHANGELOG"
}

case "${1:-}" in
  --check)
    version="$(cargo_version)"
    if [[ -z "$(section "$version")" ]]; then
      echo "CHANGELOG.md has no notes for $version. Add a \"## $version (date)\" section saying what's new; it becomes the release notes." >&2
      exit 1
    fi
    echo "CHANGELOG.md has notes for $version"
    ;;
  --versions)
    awk '/^## [0-9]/ { print $2 }' "$CHANGELOG"
    ;;
  "" | -h | --help)
    sed -n '2,9p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    [[ -n "${1:-}" ]]
    ;;
  *)
    notes="$(section "$1")"
    if [[ -z "$notes" ]]; then
      echo "CHANGELOG.md has no notes for $1" >&2
      exit 1
    fi
    printf '%s\n\n---\n\n' "$notes"
    cat <<NOTE
Download Bonsai.dmg, open it and drag Bonsai to Applications. Bonsai needs
macOS 14 or later. It is signed with Developer ID and notarized by Apple, and
Bonsai.release.plist records the build's version, signature and checksums.
NOTE
    ;;
esac
