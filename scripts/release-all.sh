#!/usr/bin/env bash
# Releases the version in Cargo.toml in one go: scripts/release.sh tag, wait for
# the Release workflow, then scripts/release.sh publish (asks for the signing
# key's password).
#
# Safe to rerun after a failed build, a wrong password or Ctrl-C: a tag that is
# already on origin isn't pushed again.
set -euo pipefail
cd "$(dirname "$0")/.."
trap 'exit 130' INT # gh may swallow Ctrl-C; don't go on to the next step

die() { echo "release-all.sh: $*" >&2; exit 1; }

# Fail now rather than after the build.
command -v gh >/dev/null || die "gh CLI is required (https://cli.github.com/)"
gh auth status >/dev/null 2>&1 || die "gh is not logged in; run 'gh auth login'"
command -v minisign >/dev/null || die "minisign is required to sign the release"
key="${VERSO_SIGNING_KEY:-$HOME/.minisign/verso-main.key}"
[ -f "$key" ] || die "no secret key at $key"

version="$(cargo pkgid -p verso | sed 's/.*[#@]//')"
tag="v$version"
[ "$(gh release view "$tag" --json isDraft -q .isDraft 2>/dev/null || true)" != "false" ] ||
    die "$tag is already published; bump the version first"

# The commit an (annotated) tag on origin points at; empty if there is no tag.
tag_commit() { git ls-remote origin "refs/tags/$tag^{}" | cut -f1; }

commit="$(tag_commit)"
if [ -n "$commit" ]; then
    echo "==> $tag is already on origin; resuming"
else
    scripts/release.sh tag
    commit="$(tag_commit)"
    [ -n "$commit" ] || die "$tag is not on origin after tagging"
fi

# GitHub takes a few seconds to create the run.
run=""
for _ in {1..24}; do
    run="$(gh run list --workflow=release.yml --branch "$tag" --commit "$commit" -L 1 \
        --json databaseId -q '.[0].databaseId // empty' || true)"
    [ -z "$run" ] || break
    sleep 5
done
[ -n "$run" ] || die "no Release run appeared for $tag (check: gh run list --workflow=release.yml)"

# gh run watch can also stop on a network error, so ask for the result itself.
echo "==> waiting for build $run"
while :; do
    gh run watch "$run" --compact --interval 10 || true
    result="$(gh run view "$run" --json status,conclusion -q '.status + " " + .conclusion' || true)"
    case "$result" in
    "completed success") break ;;
    "completed "*)
        gh run view "$run" --log-failed | tail -n 40 || true
        die "build ended with '${result#completed }'; to retry it: gh run rerun $run --failed, then $0" ;;
    esac
    sleep 10
done

printf '\a' # the password prompt is next
echo "==> build finished; publishing $tag (commit ${commit:0:7})"
[ "$commit" = "$(git rev-parse HEAD)" ] ||
    echo "    note: HEAD is not the tagged commit; Ctrl-C if you meant to release newer work"
scripts/release.sh publish "$version"
