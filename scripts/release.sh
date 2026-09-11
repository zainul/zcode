#!/bin/sh
# Cut a zcode release: bump the version, roll the changelog, tag it, and
# publish the GitHub release with binaries attached.
#
# Three ways to do it, from most manual to most automatic:
#
#   ./scripts/release.sh bump minor           # bump + changelog, commit locally
#   ./scripts/release.sh bump 0.4.1           # or an explicit version
#   ./scripts/release.sh bump patch --push    # ...and push the current branch
#
#   ./scripts/release.sh tag                  # on main, after the bump merged:
#                                              # create the vX.Y.Z tag
#   ./scripts/release.sh tag --push            # ...and push it (+ gh release, if present)
#
#   ./scripts/release.sh publish minor        # bump + push main + tag + push tag
#                                              # + gh release + local binaries, all in one run
#
# `bump` and `tag` stay separate on purpose — a version-bump commit can go
# through review like anything else, and a tag should point at what actually
# landed on main, not at a pre-merge commit on a feature branch. `publish` is
# the express lane for when you don't want that gap: it requires you to
# already be on `main` with a clean tree, and pushes straight to it — there is
# no review step in between. Use `bump`/`tag` separately if you want one.
#
# `publish` also builds local release archives (scripts/package-release.sh)
# and uploads them to the release with `gh release upload`, so the binaries
# are attached immediately instead of waiting on
# .github/workflows/release.yml's CI matrix (which still runs on the pushed
# tag and re-uploads/clobbers with its own cross-platform set — harmless
# either way, see that workflow's `release` job).
#
# Environment:
#   RELEASE_COMMIT_TRAILER   extra line(s) appended to the bump commit message
#                             (e.g. a Co-Authored-By trailer)

set -eu

REPO_ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$REPO_ROOT"

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
Cut a zcode release.

Usage:
  ./scripts/release.sh bump <major|minor|patch|X.Y.Z> [options]
  ./scripts/release.sh tag [options]
  ./scripts/release.sh publish <major|minor|patch|X.Y.Z> [options]

`bump` options:
  --skip-ci    Skip the `make ci` gate before bumping (not recommended)
  --push       Push the current branch after committing
  --yes        Don't prompt for confirmation
  --dry-run    Print what would change without touching any files

`tag` options:
  --push       Push the tag (and create a GitHub release, if `gh` is on PATH)
  --yes        Don't prompt for confirmation

`publish` options (bump + push main + tag + push tag + gh release + binaries):
  --skip-ci        Skip the `make ci` gate before bumping (not recommended)
  --skip-package   Don't build/upload local binaries — leave that to CI
  --targets "..."  Space-separated target triples, forwarded to
                    package-release.sh (default: its own DEFAULT_TARGETS)
  --yes            Don't prompt for confirmation

  -h, --help   Show this message
USAGE
}

CURRENT_VERSION_FILE="$REPO_ROOT/Cargo.toml"

current_version() {
    awk '/^\[workspace\.package\]/{f=1;next} /^\[/{f=0} f && /^version = /{
        gsub(/version = "|"/, ""); print; exit
    }' "$CURRENT_VERSION_FILE"
}

is_semver() {
    case "$1" in
        [0-9]*.[0-9]*.[0-9]*) return 0 ;;
        *) return 1 ;;
    esac
}

bump_version() {
    old=$1; kind=$2
    if is_semver "$kind"; then
        printf '%s\n' "$kind"
        return
    fi
    printf '%s\n' "$old" | awk -F. -v kind="$kind" -v OFS=. '{
        major=$1; minor=$2; patch=$3
        if (kind == "major") { major=major+1; minor=0; patch=0 }
        else if (kind == "minor") { minor=minor+1; patch=0 }
        else if (kind == "patch") { patch=patch+1 }
        else { print "unknown bump kind: " kind > "/dev/stderr"; exit 1 }
        print major, minor, patch
    }'
}

confirm() {
    [ "$YES" -eq 1 ] && return 0
    if [ ! -t 0 ]; then
        die "not an interactive terminal — re-run with --yes to proceed"
    fi
    printf '%s [y/N] ' "$1"
    read -r reply
    case "$reply" in [Yy]*) return 0 ;; *) return 1 ;; esac
}

require_clean_tree() {
    [ -z "$(git status --porcelain)" ] || die "working tree is not clean — commit or stash first"
}

# --------------------------------------------------------------- shared steps
# Bumps every internal crate version pin OLD -> NEW, rolls CHANGELOG.md, and
# commits. Expects OLD, NEW, SKIP_CI to be set by the caller.
run_bump_steps() {
    if [ "$SKIP_CI" -eq 0 ]; then
        step "Running make ci (this is the last chance to catch a break before it ships)"
        make ci
        ok "make ci passed"
    else
        warn "skipping make ci — you asked for it"
    fi

    step "Bumping every internal crate version pin"
    # Every "version = \"$OLD\"" in a workspace Cargo.toml is either
    # [workspace.package] itself or a path-dependency's version requirement
    # (e.g. `domain = { path = "../domain", version = "0.2.0" }`). Cargo checks
    # that requirement against the local package even though it resolves via
    # `path`, so leaving it behind breaks the build the moment the two diverge.
    find . -name Cargo.toml -not -path '*/target/*' -print0 \
        | xargs -0 sed -i.bak "s/version = \"$OLD\"/version = \"$NEW\"/g"
    find . -name 'Cargo.toml.bak' -not -path '*/target/*' -delete

    step "Refreshing Cargo.lock"
    cargo check --workspace --quiet

    step "Rolling CHANGELOG.md"
    awk -v new="$NEW" -v date="$(date +%F)" '
        /^## \[Unreleased\]$/ {
            print
            print ""
            print "## [" new "] - " date
            next
        }
        { print }
    ' CHANGELOG.md > CHANGELOG.md.new
    mv CHANGELOG.md.new CHANGELOG.md

    git add Cargo.lock CHANGELOG.md $(find . -name Cargo.toml -not -path '*/target/*')

    MSG="chore(release): v$NEW"
    if [ -n "${RELEASE_COMMIT_TRAILER:-}" ]; then
        MSG="$MSG

$RELEASE_COMMIT_TRAILER"
    fi
    git commit -q -m "$MSG"
    ok "committed chore(release): v$NEW"
}

# Validates the tag doesn't already exist and CHANGELOG.md has a matching
# section. Expects VERSION, TAG; sets NOTES for create_tag/push_tag_and_release.
check_tag_prereqs() {
    git rev-parse "$TAG" >/dev/null 2>&1 && die "tag $TAG already exists"
    grep -q "^## \[$VERSION\]" CHANGELOG.md \
        || die "CHANGELOG.md has no '## [$VERSION]' section — run 'bump' and merge it first"

    NOTES=$("$REPO_ROOT/scripts/changelog-notes.sh" "$VERSION")
}

# Creates the annotated tag. Expects TAG, NOTES (from check_tag_prereqs).
create_tag() {
    step "Tagging $TAG at $(git rev-parse --short HEAD) on $(git branch --show-current)"
    printf '%s\n\n%s\n' "$TAG" "$NOTES" | git tag -a "$TAG" -F -
    ok "created $TAG"
}

# Pushes the tag and creates/updates the GitHub release. Expects TAG, NOTES.
push_tag_and_release() {
    step "Pushing $TAG"
    git push origin "$TAG"
    ok "pushed"

    if command -v gh >/dev/null 2>&1; then
        step "Creating the GitHub release"
        if gh release view "$TAG" >/dev/null 2>&1; then
            printf '%s\n' "$NOTES" | gh release edit "$TAG" --title "$TAG" --notes-file -
        else
            printf '%s\n' "$NOTES" | gh release create "$TAG" --title "$TAG" --notes-file -
        fi
        ok "release published"
    else
        REMOTE_URL=$(git remote get-url origin 2>/dev/null || true)
        SLUG=$(printf '%s' "$REMOTE_URL" | sed -E 's#.*[:/]([^/]+/[^/]+?)(\.git)?$#\1#')
        say ""
        warn "gh is not installed — the tag is pushed but no GitHub release exists yet."
        if [ -n "$SLUG" ]; then
            say "  Create one at: https://github.com/$SLUG/releases/new?tag=$TAG"
        fi
    fi
}

# ---------------------------------------------------------------------- bump
cmd_bump() {
    KIND=${1:-}
    [ -n "$KIND" ] || { usage; die "bump needs major, minor, patch, or an explicit X.Y.Z"; }
    shift

    SKIP_CI=0; PUSH=0; YES=0; DRY_RUN=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --skip-ci) SKIP_CI=1 ;;
            --push) PUSH=1 ;;
            --yes) YES=1 ;;
            --dry-run) DRY_RUN=1 ;;
            -h|--help) usage; exit 0 ;;
            *) die "unknown option: $1" ;;
        esac
        shift
    done

    require_clean_tree
    OLD=$(current_version)
    NEW=$(bump_version "$OLD" "$KIND")
    is_semver "$NEW" || die "'$NEW' is not a valid X.Y.Z version"
    [ "$OLD" != "$NEW" ] || die "new version ($NEW) is the same as the current one"

    BRANCH=$(git branch --show-current)

    UNRELEASED=$(awk '/^## \[Unreleased\]$/{f=1;next} /^## \[/{f=0} f' CHANGELOG.md)
    [ -n "$(printf '%s' "$UNRELEASED" | tr -d '[:space:]')" ] \
        || die "CHANGELOG.md's [Unreleased] section is empty — nothing to release"

    step "Releasing $OLD -> $NEW on branch '$BRANCH'"

    if [ "$DRY_RUN" -eq 1 ]; then
        say ""
        say "Would bump every internal path-dependency version pin $OLD -> $NEW,"
        say "move [Unreleased] into a new '## [$NEW] - $(date +%F)' section, and commit:"
        say ""
        printf '%s\n' "$UNRELEASED" | sed 's/^/    /'
        ok "dry run — nothing changed"
        return 0
    fi

    confirm "Bump $OLD -> $NEW and commit on '$BRANCH'?" || die "aborted"

    run_bump_steps

    if [ "$PUSH" -eq 1 ]; then
        step "Pushing $BRANCH"
        git push -u origin "$BRANCH"
        ok "pushed"
    else
        say ""
        say "Not pushed. When you're ready:"
        say "    git push -u origin $BRANCH"
    fi

    say ""
    say "${BOLD}Next${RESET}: once v$NEW is on main (merged, or pushed directly),"
    say "run  ${BOLD}./scripts/release.sh tag${RESET}  on main to cut and push the git tag."
}

# ----------------------------------------------------------------------- tag
cmd_tag() {
    PUSH=0; YES=0
    while [ $# -gt 0 ]; do
        case "$1" in
            --push) PUSH=1 ;;
            --yes) YES=1 ;;
            -h|--help) usage; exit 0 ;;
            *) die "unknown option: $1" ;;
        esac
        shift
    done

    require_clean_tree
    VERSION=$(current_version)
    TAG="v$VERSION"

    check_tag_prereqs
    confirm "Create annotated tag $TAG?" || die "aborted"
    create_tag

    if [ "$PUSH" -eq 0 ]; then
        say ""
        say "Not pushed. When you're ready:"
        say "    git push origin $TAG"
        return 0
    fi

    push_tag_and_release
}

# ------------------------------------------------------------------- publish
cmd_publish() {
    KIND=${1:-}
    [ -n "$KIND" ] || { usage; die "publish needs major, minor, patch, or an explicit X.Y.Z"; }
    shift

    SKIP_CI=0; YES=0; SKIP_PACKAGE=0; TARGETS=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --skip-ci) SKIP_CI=1 ;;
            --skip-package) SKIP_PACKAGE=1 ;;
            --targets) TARGETS=${2:-}; [ -n "$TARGETS" ] || die "--targets needs a value"; shift ;;
            --yes) YES=1 ;;
            -h|--help) usage; exit 0 ;;
            *) die "unknown option: $1" ;;
        esac
        shift
    done

    require_clean_tree
    BRANCH=$(git branch --show-current)
    [ "$BRANCH" = "main" ] \
        || die "publish must run on 'main' (currently on '$BRANCH') — checkout main first, or use bump/tag separately for a reviewed release"

    OLD=$(current_version)
    NEW=$(bump_version "$OLD" "$KIND")
    is_semver "$NEW" || die "'$NEW' is not a valid X.Y.Z version"
    [ "$OLD" != "$NEW" ] || die "new version ($NEW) is the same as the current one"

    UNRELEASED=$(awk '/^## \[Unreleased\]$/{f=1;next} /^## \[/{f=0} f' CHANGELOG.md)
    [ -n "$(printf '%s' "$UNRELEASED" | tr -d '[:space:]')" ] \
        || die "CHANGELOG.md's [Unreleased] section is empty — nothing to release"

    command -v gh >/dev/null 2>&1 || warn "gh is not on PATH — the tag will be pushed but no GitHub release or binary upload will happen"

    step "Publishing $OLD -> v$NEW on main: bump, push, tag, push, GitHub release, binaries"
    confirm "Bump to $NEW, push main, tag v$NEW, push the tag, and publish the release with binaries?" \
        || die "aborted"

    run_bump_steps

    step "Pushing main"
    git push origin main
    ok "pushed main"

    VERSION=$NEW
    TAG="v$VERSION"
    check_tag_prereqs
    create_tag
    push_tag_and_release

    if [ "$SKIP_PACKAGE" -eq 1 ]; then
        warn "skipping local packaging — .github/workflows/release.yml will still build and attach binaries via CI"
    elif ! command -v gh >/dev/null 2>&1; then
        warn "skipping local packaging — 'gh' is required to upload binaries to the release"
    else
        step "Building local release archives"
        if [ -n "$TARGETS" ]; then
            # shellcheck disable=SC2086 # intentional word-splitting of a space-separated target list
            ZCODE_RELEASE_VERSION="$VERSION" "$REPO_ROOT/scripts/package-release.sh" $TARGETS
        else
            ZCODE_RELEASE_VERSION="$VERSION" "$REPO_ROOT/scripts/package-release.sh"
        fi

        step "Uploading local archives to $TAG"
        gh release upload "$TAG" "$REPO_ROOT/dist/out"/* --clobber
        ok "uploaded local archives"
    fi

    say ""
    ok "v$VERSION published"
    if command -v gh >/dev/null 2>&1; then
        gh release view "$TAG" --web >/dev/null 2>&1 || true
        say "  $(gh release view "$TAG" --json url -q .url 2>/dev/null || echo "$TAG")"
    fi
}

# ---------------------------------------------------------------------- main
[ $# -ge 1 ] || { usage; exit 1; }
CMD=$1; shift
case "$CMD" in
    bump) cmd_bump "$@" ;;
    tag) cmd_tag "$@" ;;
    publish) cmd_publish "$@" ;;
    -h|--help) usage ;;
    *) usage; die "unknown command: $CMD" ;;
esac
