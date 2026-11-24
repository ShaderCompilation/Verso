#!/usr/bin/env bash
# Puts each release component's files in target/package/stage/<id>/, laid out
# as they are installed under the component's folder (dist/components.json).
# Builds nothing: run it after the builds in scripts/package.sh or
# scripts/deploy.sh, which both use it.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
STAGE=target/package/stage
KINDLE=target/armv7-unknown-linux-gnueabihf/release
SSH=target/kindle-ssh/bin
CRE=third_party/crengine/cr3gui/data
WF=assets/wordforms/opencorpora

# copy COMPONENT SOURCE [NAME]: one file or folder into the component,
# optionally renamed.
copy() {
    if [ ! -e "$2" ]; then
        echo "stage.sh: $2 is missing; build it first (scripts/package.sh or scripts/deploy.sh do)" >&2
        exit 1
    fi
    local dst="$STAGE/$1/${3:-$(basename "$2")}"
    mkdir -p "$(dirname "$dst")"
    cp -R "$2" "$dst"
}

rm -rf "$STAGE"
copy app "$KINDLE/verso"
copy app LICENSE
copy app NOTICE.md
copy launcher dist/kindle/verso/verso.sh
copy launcher dist/kindle/verso/tools/report.sh tools/report.sh
copy launcher scripts/collect-diagnostics.sh tools/collect-diagnostics.sh
copy kual dist/kindle/extensions/verso/config.xml
copy kual dist/kindle/extensions/verso/menu.json
copy scriptlet dist/kindle/documents/Verso.sh
copy tools "$KINDLE/verso-mdns"
for f in dropbear sftp-server LICENSE-dropbear LICENCE-openssh; do
    copy tools "$SSH/$f"
done
for f in epub.css html5.css hyph; do
    copy crengine-data "$CRE/$f"
done
for f in Literata-Regular.ttf Literata-Italic.ttf Literata-SemiBold.ttf Literata-SemiBoldItalic.ttf Literata-LICENSE.txt; do
    copy fonts "assets/fonts/$f"
done
copy wordforms-ru target/wordforms/ru.vwf
copy wordforms-ru "$WF/ATTRIBUTION.txt" OpenCorpora-ATTRIBUTION.txt
copy wordforms-ru "$WF/LICENSE-CC-BY-SA-3.0.txt" OpenCorpora-LICENSE.txt
echo "==> staged $(ls "$STAGE" | wc -l) components in $STAGE"
