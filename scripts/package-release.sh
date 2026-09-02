#!/bin/sh
# Build + package `zcode` release archives locally — the same shape
# `.github/workflows/release.yml` produces (a staged dir with the binary,
# README.md and CHANGELOG.md, tarred/zipped, plus a .sha256), but runnable on
# your own machine with no tag push and no CI wait.
#
# Usage:
#   ./scripts/package-release.sh                      # DEFAULT_TARGETS below
#   ./scripts/package-release.sh aarch64-apple-darwin  # one or more explicit targets
#
# Env:
#   ZCODE_RELEASE_VERSION   archive-name version (default: [workspace.package]
#                            version in Cargo.toml — no "v" prefix, matches
#                            what scripts/release.sh reads/writes)
#
# The no-args default always targets both platforms — mac (Intel + Apple
# Silicon) and Linux/Ubuntu x86_64 — regardless of which one you're running
# on. Whichever of those isn't native to the host needs `cross` (Docker) on
# PATH; this script uses it automatically when present. Without it, that
# target is skipped with a note rather than failing the whole run — you still
# get archives for what *is* native, e.g. plain `cargo` on a Mac with no
# Docker installed gets you both Darwin arches and skips Linux. Getting every
# target unconditionally, with no local Docker dependency, is what
# .github/workflows/release.yml is for.

set -eu

REPO_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$REPO_ROOT"

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

current_version() {
    awk '/^\[workspace\.package\]/{f=1;next} /^\[/{f=0} f && /^version = /{
        gsub(/version = "|"/, ""); print; exit
    }' "$REPO_ROOT/Cargo.toml"
}

VERSION=${ZCODE_RELEASE_VERSION:-$(current_version)}
HOST_OS=$(uname -s)
HOST_ARCH=$(uname -m)
# Normalise uname's arm64 (macOS) to the arch component rustc target triples use.
case "$HOST_ARCH" in arm64) HOST_ARCH=aarch64 ;; esac

# mac (both arches) + linux/ubuntu x86_64 — see the header comment on how a
# non-native one of these is built (cross) or skipped (no cross, no Docker).
DEFAULT_TARGETS="aarch64-apple-darwin x86_64-apple-darwin x86_64-unknown-linux-gnu"

is_native() {
    case "$HOST_OS" in
        Darwin) case "$1" in *-apple-darwin) return 0 ;; *) return 1 ;; esac ;;
        Linux) case "$1" in "$HOST_ARCH"-unknown-linux-*) return 0 ;; *) return 1 ;; esac ;;
        *) return 1 ;;
    esac
}

checksum() {
    # sha256sum (coreutils) first: present on Linux and in Git-for-Windows'
    # bash, absent on macOS; shasum is the reverse. Matches the same
    # fallback order in .github/workflows/release.yml, where hardcoding
    # shasum broke the Windows leg (`shasum: command not found`).
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi
}

if [ $# -gt 0 ]; then
    TARGETS=$*
else
    TARGETS=$DEFAULT_TARGETS
fi

OUT_DIR="$REPO_ROOT/dist/out"
mkdir -p "$OUT_DIR"

BUILT=""
SKIPPED=""
for target in $TARGETS; do
    step "$target"

    BUILDER=cargo
    if ! is_native "$target"; then
        if command -v cross >/dev/null 2>&1; then
            warn "$target is not native to $HOST_OS/$HOST_ARCH — building with 'cross' (Docker)"
            BUILDER=cross
        else
            warn "$target is not native to $HOST_OS/$HOST_ARCH and 'cross' is not on PATH — skipping"
            warn "  install: cargo install cross --git https://github.com/cross-rs/cross"
            warn "  or build it on a matching runner via .github/workflows/release.yml"
            SKIPPED="$SKIPPED $target"
            continue
        fi
    fi

    if command -v rustup >/dev/null 2>&1; then
        rustup target add "$target"
    fi

    "$BUILDER" build --release --locked -p zcode --target "$target"

    bin="target/$target/release/zcode"
    case "$target" in *windows*) bin="$bin.exe" ;; esac
    [ -f "$bin" ] || die "expected binary not found at $bin"

    name="zcode-$VERSION-$target"
    stage="$REPO_ROOT/dist/$name"
    rm -rf "$stage"
    mkdir -p "$stage"
    case "$target" in
        *windows*) cp "$bin" "$stage/zcode.exe" ;;
        *) cp "$bin" "$stage/zcode" ;;
    esac
    cp README.md CHANGELOG.md "$stage/"

    case "$target" in
        *windows*)
            archive="$name.zip"
            if command -v 7z >/dev/null 2>&1; then
                (cd "$REPO_ROOT/dist" && 7z a -bd "$OUT_DIR/$archive" "$name" >/dev/null)
            elif command -v zip >/dev/null 2>&1; then
                (cd "$REPO_ROOT/dist" && zip -qr "$OUT_DIR/$archive" "$name")
            else
                die "packaging $target needs '7z' or 'zip' on PATH"
            fi
            ;;
        *)
            archive="$name.tar.gz"
            (cd "$REPO_ROOT/dist" && tar czf "$OUT_DIR/$archive" "$name")
            ;;
    esac
    rm -rf "$stage"
    (cd "$OUT_DIR" && checksum "$archive" > "$archive.sha256")

    ok "$OUT_DIR/$archive"
    BUILT="$BUILT $archive"
done

[ -n "$BUILT" ] || die "nothing was built"

say ""
say "${BOLD}Built:${RESET}"
for f in $BUILT; do
    say "  $OUT_DIR/$f"
done
if [ -n "$SKIPPED" ]; then
    say ""
    say "${BOLD}Skipped${RESET} (no 'cross' on PATH for a non-native target):"
    for t in $SKIPPED; do
        say "  $t"
    done
fi
say ""
say "Attach these to an existing tag's GitHub release with:"
say "    gh release upload v$VERSION $OUT_DIR/* --clobber"
