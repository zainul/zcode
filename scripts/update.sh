#!/bin/sh
# Update an existing zcode installation.
#
# A thin wrapper around install.sh, which already replaces an existing install
# in place. It exists so "how do I update?" has an obvious answer.
#
#   ./scripts/update.sh              # rebuild and replace the installed binary
#   ./scripts/update.sh --prefix DIR # update a specific copy
#
# It does not touch your git checkout: run `git pull` first if you want the
# latest source.

set -eu

DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

if ! command -v zcode >/dev/null 2>&1; then
    printf 'zcode is not installed yet — installing it now.\n\n'
fi

exec "$DIR/install.sh" "$@"
