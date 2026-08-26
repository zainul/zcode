#!/bin/sh
# Uninstall zcode.
#
#   ./scripts/uninstall.sh                # remove the binary
#   ./scripts/uninstall.sh --prefix ~/bin # remove from a specific directory
#   ./scripts/uninstall.sh --yes          # do not ask for confirmation
#   ./scripts/uninstall.sh --help
#
# Project data (.zcode/ directories and zcode.json files) is left alone: it
# lives inside your projects and this script will not go hunting through your
# filesystem for it. The paths are printed at the end so you can remove them.

set -eu

BIN_NAME=zcode
PREFIX=${ZCODE_INSTALL_DIR:-}
ASSUME_YES=0

if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
    BOLD=$(printf '\033[1m'); RED=$(printf '\033[31m')
    GREEN=$(printf '\033[32m'); YELLOW=$(printf '\033[33m'); RESET=$(printf '\033[0m')
else
    BOLD=''; RED=''; GREEN=''; YELLOW=''; RESET=''
fi

say()  { printf '%s\n' "$*"; }
step() { printf '%s==>%s %s\n' "$BOLD" "$RESET" "$*"; }
ok()   { printf '%s  ok%s %s\n' "$GREEN" "$RESET" "$*"; }
warn() { printf '%s warn%s %s\n' "$YELLOW" "$RESET" "$*" >&2; }
die()  { printf '%serror%s %s\n' "$RED" "$RESET" "$*" >&2; exit 1; }

usage() {
    cat <<'USAGE'
Uninstall zcode.

Usage: ./scripts/uninstall.sh [OPTIONS]

Options:
  --prefix <DIR>   Only remove the copy in <DIR>
  -y, --yes        Do not prompt for confirmation
  -h, --help       Show this message

Without --prefix, every zcode on your PATH plus the usual install
locations are offered for removal.
USAGE
}

while [ $# -gt 0 ]; do
    case "$1" in
        --prefix) [ $# -ge 2 ] || die "--prefix needs a directory"; PREFIX=$2; shift 2 ;;
        --prefix=*) PREFIX=${1#--prefix=}; shift ;;
        -y|--yes) ASSUME_YES=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) die "unknown option: $1 (try --help)" ;;
    esac
done

case "$(uname -s 2>/dev/null || echo unknown)" in
    MINGW*|MSYS*|CYGWIN*) BIN_NAME=zcode.exe ;;
esac
case "$PREFIX" in "~"/*) PREFIX="$HOME/${PREFIX#\~/}" ;; esac

# ------------------------------------------------------- find the installs
FOUND=''
add_candidate() {
    [ -f "$1" ] || return 0
    case " $FOUND " in *" $1 "*) return 0 ;; esac
    FOUND="$FOUND $1"
}

if [ -n "$PREFIX" ]; then
    add_candidate "$PREFIX/$BIN_NAME"
else
    # Whatever the shell would actually run, first.
    if command -v "$BIN_NAME" >/dev/null 2>&1; then
        add_candidate "$(command -v "$BIN_NAME")"
    fi
    for dir in /usr/local/bin "$HOME/.local/bin" "$HOME/bin" /opt/homebrew/bin "$HOME/.cargo/bin"; do
        add_candidate "$dir/$BIN_NAME"
    done
fi

if [ -z "$FOUND" ]; then
    say "No zcode installation found."
    say "If you installed somewhere unusual, point at it:"
    say "    ./scripts/uninstall.sh --prefix /path/to/dir"
    exit 0
fi

step "Found:"
for f in $FOUND; do
    version=$("$f" version 2>/dev/null || echo "unreadable")
    say "  $f  ($version)"
done

if [ "$ASSUME_YES" -eq 0 ]; then
    # Non-interactive callers (CI, pipes) must opt in explicitly rather than
    # being silently cancelled or hanging on a prompt nobody can answer.
    if [ ! -t 0 ]; then
        die "not running interactively — re-run with --yes to confirm removal"
    fi
    printf 'Remove %s file(s)? [y/N] ' "$(echo "$FOUND" | wc -w | tr -d ' ')"
    read -r reply || reply=n
    case "$reply" in
        y|Y|yes|YES) ;;
        *) say "Cancelled."; exit 0 ;;
    esac
fi

# ------------------------------------------------------------------ remove
removed=0
for f in $FOUND; do
    dir=$(dirname "$f")
    if [ -w "$dir" ]; then
        rm -f "$f" && { ok "removed $f"; removed=$((removed + 1)); }
    else
        warn "cannot remove $f — $dir is not writable"
        say  "    sudo rm $f"
    fi
done

# ------------------------------------------------------------- what remains
say ""
if [ "$removed" -gt 0 ]; then
    step "Uninstalled."
else
    step "Nothing was removed."
fi

cat <<EOF

${BOLD}Left in place${RESET} (project data — remove it yourself if you want it gone)

  ./zcode.json, ./zcode.toml   per-project configuration
  ./.zcode/sessions/           saved transcripts
  ./.zcode/reports/            run telemetry
  ./.zcode/skills/             your skill notes

  Find them with:
      find . -maxdepth 3 -name '.zcode' -o -maxdepth 3 -name 'zcode.json'

${BOLD}Also check${RESET}

  Your API key export (ZCODE_*_API_KEY) and any PATH line added at install
  time may still be in your shell startup file.
EOF
