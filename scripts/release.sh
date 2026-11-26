#!/usr/bin/env bash
# Cuts and publishes Verso releases (paired with .github/workflows/release.yml).
#
#   scripts/release.sh tag                     tag & push v<Cargo.toml version> to trigger CI
#   scripts/release.sh publish [VERSION]       download CI draft, sign locally, verify & publish
#   scripts/release.sh publish-local [VERSION] verify local target/package/<V>/ & publish directly
#
# Signing always stays local via scripts/sign-release.sh (~/.minisign/).
set -euo pipefail
cd "$(dirname "$0")/.."

die() { echo "release.sh: $*" >&2; exit 1; }
crate_version() { cargo pkgid -p verso | sed 's/.*[#@]//'; }
need_gh() {
    command -v gh >/dev/null 2>&1 || die "gh CLI is required (https://cli.github.com/)"
    gh auth status >/dev/null 2>&1 || die "gh is not logged in; run 'gh auth login'"
}

cmd_tag() {
    local version tag branch
    version="$(crate_version)"
    tag="v$version"
    branch="$(git rev-parse --abbrev-ref HEAD)"
    [ "$branch" = "master" ] || die "switch to 'master' before tagging (on '$branch')"
    [ -z "$(git status --porcelain --ignore-submodules=none)" ] || die "uncommitted changes; commit first"
    [ -s "dist/release-notes/$version.txt" ] || die "write dist/release-notes/$version.txt first"
    git rev-parse -q --verify "refs/tags/$tag" >/dev/null && die "tag $tag already exists locally"
    git ls-remote --exit-code --tags origin "refs/tags/$tag" >/dev/null 2>&1 && die "tag $tag already exists on origin"
    git tag -a "$tag" -m "Verso $version"
    git push origin master
    git push origin "$tag"
    echo "==> pushed $tag; GitHub Actions is building the draft release"
    echo "    watch:   gh run watch"
    echo "    publish: scripts/release.sh publish $version"
}

cmd_publish() {
    need_gh
    local version="${1:-$(crate_version)}" tag out draft
    tag="v$version"
    out="target/package/$version"
    draft="$(gh release view "$tag" --json isDraft -q .isDraft 2>/dev/null || true)"
    [ -n "$draft" ] || die "no GitHub release found for $tag yet (check: gh run list --workflow=release.yml)"
    [ "$draft" = "true" ] || die "release $tag is already published"
    echo "==> downloading draft release $tag into $out"
    rm -rf "$out" && mkdir -p "$out"
    gh release download "$tag" --dir "$out"
    [ -f "$out/latest.json" ] || die "downloaded draft has no latest.json"
    echo "==> signing and verifying $version"
    scripts/sign-release.sh "$version"
    echo "==> uploading signature and publishing $tag as a regular release"
    gh release upload "$tag" "$out/latest.json.minisig" --clobber
    gh release edit "$tag" --draft=false --prerelease=false --latest
    echo "==> published $tag: $(gh release view "$tag" --json url -q .url)"
}

cmd_publish_local() {
    need_gh
    local version="${1:-$(crate_version)}" tag out notes
    tag="v$version"
    out="target/package/$version"
    notes="dist/release-notes/$version.txt"
    [ -f "$out/latest.json" ] || die "no $out/latest.json; run scripts/package.sh first"
    [ -s "$notes" ] || die "missing release notes: $notes"
    gh release view "$tag" >/dev/null 2>&1 && die "GitHub release $tag already exists"
    if [ ! -f "$out/latest.json.minisig" ]; then
        scripts/sign-release.sh "$version"
    else
        cargo build --release --quiet -p verso-update --bin verso-release
        target/release/verso-release verify --release "$out"
    fi
    git rev-parse -q --verify "refs/tags/$tag" >/dev/null || git tag -a "$tag" -m "Verso $version"
    git push origin master
    echo "==> creating and publishing $tag from $out"
    # The GitHub page also links the install guide; the notes file is shown on the Kindle.
    local body; body="$(mktemp)"
    { cat "$notes"; printf '\nInstall guide: https://verso-reader.app/install\n'; } > "$body"
    gh release create "$tag" "$out"/* \
        --target "$(git rev-parse "$tag^{commit}")" \
        --title "Verso $version" \
        --notes-file "$body" \
        --prerelease=false \
        --latest
    rm -f "$body"
    git fetch origin tag "$tag" >/dev/null 2>&1 || true
    echo "==> published $tag: $(gh release view "$tag" --json url -q .url)"
}

case "${1:-}" in
tag) shift; [ $# -eq 0 ] || die "usage: $0 tag"; cmd_tag ;;
publish) shift; [ $# -le 1 ] || die "usage: $0 publish [VERSION]"; cmd_publish "${1:-}" ;;
publish-local) shift; [ $# -le 1 ] || die "usage: $0 publish-local [VERSION]"; cmd_publish_local "${1:-}" ;;
*) echo "usage: $0 tag | publish [VERSION] | publish-local [VERSION]" >&2; exit 2 ;;
esac
