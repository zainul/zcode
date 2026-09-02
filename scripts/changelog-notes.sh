#!/bin/sh
# Print CHANGELOG.md's entry for one version to stdout, trimmed of leading
# blank lines. Shared by `scripts/release.sh tag` (which feeds it to
# `gh release create`) and `.github/workflows/release.yml` (which feeds the
# same text to `gh release create`/`gh release edit` after building the
# platform binaries) — one place to read the section out of, so the two never
# drift into rendering a release's notes differently.
#
# Usage: ./scripts/changelog-notes.sh <version>   # e.g. 0.4.1, no leading v

set -eu

REPO_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
VERSION=${1:?usage: changelog-notes.sh <version>}

grep -q "^## \[$VERSION\]" "$REPO_ROOT/CHANGELOG.md" \
    || { echo "changelog-notes.sh: no '## [$VERSION]' section in CHANGELOG.md" >&2; exit 1; }

awk -v ver="$VERSION" '
    BEGIN { p = 0 }
    $0 ~ "^## \\[" ver "\\]" { p = 1; next }
    p && /^## \[/ { exit }
    p { print }
' "$REPO_ROOT/CHANGELOG.md" | sed '/./,$!d'
