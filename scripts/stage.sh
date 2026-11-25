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

# Verify Kindle ELF ABI compatibility before staging:
# - interpreter is /lib/ld-linux-armhf.so.3
# - max required GLIBC version <= 2.20 (i.MX armhf Kindles like Oasis 3)
# - DT_REL + DT_RELSZ == DT_JMPREL (contiguous .rel.dyn/.rel.plt for glibc <= 2.23, bug #14341)
# - PT_ARM_EXIDX is present
check_kindle_elf() {
    python3 -I - "$@" <<'PY'
import re, subprocess, sys

for path in sys.argv[1:]:
    out = subprocess.check_output(["readelf", "-lW", "-dW", "-VW", path], text=True)
    if "/lib/ld-linux-armhf.so.3" not in out:
        sys.exit(f"stage.sh: {path}: expected /lib/ld-linux-armhf.so.3 interpreter")
    if "ARM_EXIDX" not in out:
        sys.exit(f"stage.sh: {path}: missing PT_ARM_EXIDX program header")
    ver_section = out.split("Version needs section", 1)[-1]
    bad_vers = [
        v for v in re.findall(r"Name:\s+GLIBC_(\d+)\.(\d+)(?:\.(\d+))?", ver_section)
        if (int(v[0]), int(v[1]), int(v[2] or 0)) > (2, 20, 0)
    ]
    if bad_vers:
        names = ", ".join(f"GLIBC_{a}.{b}" + (f".{c}" if c else "") for a, b, c in bad_vers)
        sys.exit(f"stage.sh: {path}: requires glibc newer than 2.20: {names}")
    rel = re.search(r"\(REL\)\s+0x([0-9a-f]+)", out)
    relsz = re.search(r"\(RELSZ\)\s+(\d+)\s+\(bytes\)", out)
    jmprel = re.search(r"\(JMPREL\)\s+0x([0-9a-f]+)", out)
    if rel and relsz and jmprel:
        rel_end = int(rel.group(1), 16) + int(relsz.group(1), 10)
        jmp_start = int(jmprel.group(1), 16)
        if rel_end != jmp_start:
            sys.exit(
                f"stage.sh: {path}: DT_REL+DT_RELSZ (0x{rel_end:x}) != DT_JMPREL (0x{jmp_start:x}); "
                "non-contiguous relocations crash glibc <= 2.23 (see .cargo/arm-glibc-relplt.ld)"
            )
PY
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
check_kindle_elf "$KINDLE/verso" "$KINDLE/verso-mdns"
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
