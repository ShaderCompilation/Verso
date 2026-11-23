#!/usr/bin/env bash
# Builds a Verso release in target/package/<version>/: one archive per
# component, latest.json (the manifest) and verso-<version>.zip (the
# first install). Sign it afterwards with scripts/sign-release.sh.
#
#   scripts/package.sh                   package the version in Cargo.toml
#   scripts/package.sh --allow-dirty     package uncommitted changes, for testing
#   scripts/package.sh --notes FILE      release notes from FILE instead of
#                                        dist/release-notes/<version>.txt
#
# VERSO_RELEASED=YYYY-MM-DD sets the release date (default: today, UTC), for
# rebuilding an earlier release byte for byte.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
die() {
    echo "package.sh: $*" >&2
    exit 1
}

ALLOW_DIRTY=0
NOTES=""
while [ $# -gt 0 ]; do
    case "$1" in
    --allow-dirty) ALLOW_DIRTY=1; shift ;;
    --notes) NOTES="$2"; shift 2 ;;
    *) echo "usage: $0 [--allow-dirty] [--notes FILE]" >&2; exit 2 ;;
    esac
done

VERSION="$(cargo pkgid -p verso | sed 's/.*[#@]//')"
NOTES="${NOTES:-dist/release-notes/$VERSION.txt}"
[ -f "$NOTES" ] || die "write the release notes for $VERSION in $NOTES first"
if [ "$ALLOW_DIRTY" = 0 ] && [ -n "$(git status --porcelain --ignore-submodules=none)" ]; then
    die "there are uncommitted changes; commit them, or use --allow-dirty for a test package"
fi
RELEASED="${VERSO_RELEASED:-$(date -u +%F)}"

echo "==> building"
cargo kindle-build
scripts/build-ssh.sh
scripts/build-wordforms.sh
# The packaging tool runs on this computer.
cargo build --release --quiet -p verso-update --bin verso-release
scripts/stage.sh

OUT="target/package/$VERSION"
echo "==> packaging Verso $VERSION ($RELEASED) in $OUT"
rm -rf "$OUT"
target/release/verso-release pack --components dist/components.json --stage target/package/stage \
    --out "$OUT" --version "$VERSION" --released "$RELEASED" --notes "$NOTES"
ls -l "$OUT"
echo "==> next: scripts/sign-release.sh, then publish"
