#!/usr/bin/env bash
# Builds the Russian word-form pack (ru.vwf) from the kept OpenCorpora export
# with the host's vwf-build, then runs the quality list against it. Works
# offline.
#
# Output: target/wordforms/ru.vwf. Does nothing when the source, the builder
# and this script are unchanged.
#
#   scripts/build-wordforms.sh                    build if needed (suffix rules)
#   scripts/build-wordforms.sh --encoding sets    build with lemma sets instead, for measuring
#   scripts/build-wordforms.sh --force            rebuild even if up to date
#   scripts/build-wordforms.sh --check            only run the quality list on the current pack
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
SOURCE=assets/wordforms/opencorpora/dict.opcorpora.xml.bz2
CAPTURED=2024-04-23
CHECK=assets/wordforms/ru-check.tsv
OUT=target/wordforms
PACK="$OUT/ru.vwf"
ENCODING=rules
FORCE=0
CHECK_ONLY=0
while [ $# -gt 0 ]; do
    case "$1" in
    --encoding) ENCODING="$2"; shift 2 ;;
    --force) FORCE=1; shift ;;
    --check) CHECK_ONLY=1; shift ;;
    *) echo "usage: $0 [--encoding rules|sets] [--force] [--check]" >&2; exit 2 ;;
    esac
done

# The host build, never cross-compiled: the pack is plain data.
cargo build --release --quiet -p verso-wordforms --bin vwf-build
BUILDER=target/release/vwf-build

if [ "$CHECK_ONLY" = 1 ]; then
    exec "$BUILDER" --pack "$PACK" --check "$CHECK"
fi

STAMP="$(cat "$SOURCE" "$BUILDER" "$0" <(echo "$ENCODING") | sha256sum | cut -d' ' -f1)"
if [ "$FORCE" = 0 ] && [ -f "$PACK" ] && [ "$(cat "$OUT/.stamp" 2>/dev/null)" = "$STAMP" ]; then
    echo "==> word forms are up to date ($PACK)"
    exit 0
fi

echo "==> building $PACK ($ENCODING)"
mkdir -p "$OUT"
rm -f "$OUT/.stamp"
bzip2 -dc "$SOURCE" | "$BUILDER" --out "$PACK" --encoding "$ENCODING" --date "$CAPTURED" --check "$CHECK"
echo "$STAMP" >"$OUT/.stamp"
