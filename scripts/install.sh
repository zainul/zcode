#!/bin/sh
# Install or update zcode.
#
# Detects your platform, builds the release binary from source, and installs it
# somewhere on your PATH. POSIX sh: works with bash, zsh, dash and ash.
#
# Re-running it updates an existing installation **in place** — the new binary
# replaces the old one wherever it already lives, so you never end up with two
# copies shadowing each other on PATH.
#
#   ./scripts/install.sh                  # install, or update an existing install
#   ./scripts/install.sh --prefix ~/bin   # choose the destination
#   ./scripts/install.sh --no-build       # install an existing target/release build
#   ./scripts/install.sh --help
#
# Environment:
#   ZCODE_INSTALL_DIR   same as --prefix

set -eu

BIN_NAME=zcode
REPO_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
PREFIX=${ZCODE_INSTALL_DIR:-}
DO_BUILD=1

# ---------------------------------------------------------------- presentation
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
Install or update zcode — a lean terminal coding agent.

Usage: ./scripts/install.sh [OPTIONS]

Options:
  --prefix <DIR>   Install into <DIR> instead of the default
  --no-build       Skip `cargo build`; install the existing target/release binary
  -h, --help       Show this message

Updating:
  Just run it again. An existing zcode is replaced where it already lives,
  and the old and new build stamps are shown so you can confirm it took.

Destination when nothing is installed yet:
  /usr/local/bin   if it exists and is writable
  ~/.local/bin     otherwise (created if needed)
USAGE
}

while [ $# -gt 0 ]; do
    case "$1" in
        --prefix) [ $# -ge 2 ] || die "--prefix needs a directory"; PREFIX=$2; shift 2 ;;
        --prefix=*) PREFIX=${1#--prefix=}; shift ;;
        --no-build) DO_BUILD=0; shift ;;
        -h|--help) usage; exit 0 ;;
        *) die "unknown option: $1 (try --help)" ;;
    esac
done

# ------------------------------------------------------------------- platform
OS=$(uname -s 2>/dev/null || echo unknown)
ARCH=$(uname -m 2>/dev/null || echo unknown)

case "$OS" in
    Linux)  PLATFORM=linux ;;
    Darwin) PLATFORM=macos ;;
    FreeBSD|OpenBSD|NetBSD) PLATFORM=bsd ;;
    MINGW*|MSYS*|CYGWIN*)
        PLATFORM=windows
        BIN_NAME=zcode.exe
        warn "Windows detected. This script works under Git Bash, MSYS2 and WSL."
        warn "The TUI is untested on Windows; the headless 'zcode run' is fine."
        ;;
    *) PLATFORM=unknown; warn "unrecognised OS '$OS' — continuing anyway" ;;
esac

case "$ARCH" in
    x86_64|amd64)  ARCH_LABEL=x86_64 ;;
    arm64|aarch64) ARCH_LABEL=arm64 ;;
    *) ARCH_LABEL=$ARCH; warn "unrecognised architecture '$ARCH' — continuing anyway" ;;
esac

step "Platform: $PLATFORM ($ARCH_LABEL)"

# ----------------------------------------------------------------- toolchain
if [ "$DO_BUILD" -eq 1 ]; then
    if ! command -v cargo >/dev/null 2>&1; then
        say ""
        die "cargo not found. Install Rust first:

    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

then re-run this script. (zcode pins its own toolchain, so any recent
rustup installation is enough.)"
    fi
    ok "cargo $(cargo --version 2>/dev/null | cut -d' ' -f2)"
fi

# ------------------------------------------------------------------ destination
# An existing install is upgraded where it already sits. Installing to a
# different directory instead would leave the old binary on PATH, and whichever
# came first would keep winning — the confusing failure this avoids is
# "I updated but the new subcommand still is not there".
EXISTING=''
if command -v "$BIN_NAME" >/dev/null 2>&1; then
    EXISTING=$(command -v "$BIN_NAME")
fi

if [ -z "$PREFIX" ]; then
    if [ -n "$EXISTING" ]; then
        PREFIX=$(dirname "$EXISTING")
        step "Updating the existing installation in $PREFIX"
    elif [ -d /usr/local/bin ] && [ -w /usr/local/bin ]; then
        PREFIX=/usr/local/bin
    else
        PREFIX="$HOME/.local/bin"
    fi
fi
# Expand a leading ~ so --prefix ~/bin works even when quoted.
case "$PREFIX" in "~"/*) PREFIX="$HOME/${PREFIX#\~/}" ;; esac

step "Destination: $PREFIX"

# --------------------------------------------------------------------- build
SRC="$REPO_ROOT/target/release/$BIN_NAME"
if [ "$DO_BUILD" -eq 1 ]; then
    step "Building (this takes a few minutes the first time)"
    ( cd "$REPO_ROOT" && cargo build --release ) || die "build failed"
fi
[ -f "$SRC" ] || die "binary not found at $SRC — run without --no-build"
ok "built $(du -h "$SRC" | cut -f1 | tr -d ' ')"

# ------------------------------------------------------------------- install
if ! mkdir -p "$PREFIX" 2>/dev/null; then
    die "cannot create $PREFIX — choose another with --prefix, or re-run with sudo"
fi
if [ ! -w "$PREFIX" ]; then
    die "$PREFIX is not writable.
Re-run with sudo, or install somewhere you own:

    ./scripts/install.sh --prefix \"\$HOME/.local/bin\""
fi

DEST="$PREFIX/$BIN_NAME"
OLD_VERSION=''
if [ -e "$DEST" ]; then
    OLD_VERSION=$("$DEST" version 2>/dev/null || echo "unknown version")
fi

# Replace via a temp file + rename so an interrupted copy cannot leave a
# half-written binary in place of a working one.
cp "$SRC" "$DEST.tmp" && chmod 755 "$DEST.tmp" && mv "$DEST.tmp" "$DEST"

NEW_VERSION=$("$DEST" version 2>/dev/null || echo "unknown version")
if [ -n "$OLD_VERSION" ]; then
    say "  was: $OLD_VERSION"
    say "  now: $NEW_VERSION"
    if [ "$OLD_VERSION" = "$NEW_VERSION" ]; then
        warn "the build stamp did not change — you may have re-installed the same build"
    fi
    ok "updated $DEST"
else
    ok "installed $DEST"
fi

# Any other copy earlier on PATH would keep shadowing the one just installed.
OTHERS=$(command -v -a "$BIN_NAME" 2>/dev/null | sort -u | grep -v "^$DEST$" || true)
if [ -n "$OTHERS" ]; then
    say ""
    warn "other copies of $BIN_NAME are also on your PATH:"
    for other in $OTHERS; do say "    $other"; done
    say "  Whichever comes first in PATH wins. Remove the extras with:"
    say "      ./scripts/uninstall.sh"
    say "  then re-run this script."
fi

# ---------------------------------------------------------------------- PATH
on_path=0
case ":${PATH}:" in *":$PREFIX:"*) on_path=1 ;; esac

if [ "$on_path" -eq 0 ]; then
    case "$(basename "${SHELL:-sh}")" in
        zsh)  RC="$HOME/.zshrc" ;;
        bash) if [ "$PLATFORM" = macos ]; then RC="$HOME/.bash_profile"; else RC="$HOME/.bashrc"; fi ;;
        fish) RC="$HOME/.config/fish/config.fish" ;;
        *)    RC="your shell's startup file" ;;
    esac
    say ""
    warn "$PREFIX is not on your PATH."
    if [ "$RC" = "$HOME/.config/fish/config.fish" ]; then
        say "  Add it with:"
        say "      fish_add_path $PREFIX"
    else
        say "  Add it with:"
        say "      echo 'export PATH=\"$PREFIX:\$PATH\"' >> $RC"
        say "      . $RC"
    fi
else
    ok "$PREFIX is on your PATH"
fi

# -------------------------------------------------------------------- verify
say ""
if [ "$on_path" -eq 1 ] && command -v "$BIN_NAME" >/dev/null 2>&1; then
    step "Verifying"
    "$BIN_NAME" version
else
    step "Verifying"
    "$DEST" version
fi

cat <<EOF

${BOLD}Next steps${RESET}

  1. Create a config in your project:
       echo '{ "provider": "openrouter", "model": "anthropic/claude-sonnet-4.5" }' > zcode.json

  2. Export your API key (never stored in the config file):
       export ZCODE_OPENROUTER_API_KEY=sk-or-v1-...

  3. Run a task:
       zcode run "add doc comments to the public functions in src/main.rs"

  Guide:     docs/guide/README.md
  Uninstall: ./scripts/uninstall.sh
EOF
