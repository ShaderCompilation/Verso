#!/usr/bin/env bash
# Signs a packaged release's latest.json with minisign, then checks the
# signature and every archive with the release keys built into Verso
# (crates/verso-update/keys/). Asks for the key's password.
#
#   scripts/sign-release.sh            the version in Cargo.toml
#   scripts/sign-release.sh 0.3.0      another packaged version
#
# VERSO_SIGNING_KEY picks the secret key (default ~/.minisign/verso-main.key),
# for example the backup key. See docs/setup.md#releasing.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
VERSION="${1:-$(cargo pkgid -p verso | sed 's/.*[#@]//')}"
OUT="target/package/$VERSION"
KEY="${VERSO_SIGNING_KEY:-$HOME/.minisign/verso-main.key}"
[ -f "$OUT/latest.json" ] || { echo "sign-release.sh: no $OUT/latest.json; run scripts/package.sh first" >&2; exit 1; }
[ -f "$KEY" ] || { echo "sign-release.sh: no secret key at $KEY (docs/setup.md#release-keys)" >&2; exit 1; }

cargo build --release --quiet -p verso-update --bin verso-release
TOOL=target/release/verso-release
# The signed comment names the version, so the manifest can't be relabelled.
COMMENT="$("$TOOL" trusted-comment --manifest "$OUT/latest.json")"
rm -f "$OUT/latest.json.minisig"
minisign -S -s "$KEY" -m "$OUT/latest.json" -x "$OUT/latest.json.minisig" -t "$COMMENT"
"$TOOL" verify --release "$OUT"
