#!/usr/bin/env bash
# Puts each release component's files in target/package/stage/<id>/, laid out
# as they are installed under the component's folder (dist/components.json).
# Builds nothing: run it after the builds in scripts/package.sh or
# scripts/deploy.sh, which both use it.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
STAGE=target/package/stage
KINDLE=target/armv7-unknown-linux-musleabihf/release
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
# - static ARMv7 hard-float ELF32 (no PT_INTERP, no PT_DYNAMIC)
# - PT_ARM_EXIDX is present
# - ARM attributes: v7, VFP register args, no VFPv4/VFPv5/Fused-MAC/NEONv2/IDIV (Cortex-A9 safe)
check_kindle_elf() {
    python3 -I - "$@" <<'PY'
import os, re, subprocess, sys

# readelf localizes its output, so parse it in the C locale.
env = {**os.environ, "LC_ALL": "C"}
env.pop("LANGUAGE", None)

for path in sys.argv[1:]:
    out = subprocess.check_output(["readelf", "-h", "-lW", "-A", path], text=True, env=env)
    if not re.search(r"^\s*Class:\s+ELF32\b", out, re.MULTILINE):
        sys.exit(f"stage.sh: {path}: expected ELF32 binary")
    if not re.search(r"^\s*Machine:\s+ARM\b", out, re.MULTILINE):
        sys.exit(f"stage.sh: {path}: expected ARM machine")
    flags = re.search(r"^\s*Flags:\s*(.*)$", out, re.MULTILINE)
    if not flags or "hard-float ABI" not in flags.group(1):
        sys.exit(f"stage.sh: {path}: expected hard-float ABI in ELF flags")

    phdrs = set(re.findall(r"^\s+([A-Z0-9_]+)\s+0x[0-9a-f]+\s+0x[0-9a-f]+", out, re.MULTILINE))
    if "INTERP" in phdrs:
        sys.exit(f"stage.sh: {path}: unexpected PT_INTERP (must be static)")
    if "DYNAMIC" in phdrs:
        sys.exit(f"stage.sh: {path}: unexpected PT_DYNAMIC (must be static)")
    if "ARM_EXIDX" not in phdrs:
        sys.exit(f"stage.sh: {path}: missing PT_ARM_EXIDX program header")

    if not re.search(r"^\s*Tag_CPU_arch:\s*v7\b", out, re.MULTILINE):
        sys.exit(f"stage.sh: {path}: expected Tag_CPU_arch: v7")
    if not re.search(r"^\s*Tag_ABI_VFP_args:\s*VFP registers\b", out, re.MULTILINE):
        sys.exit(f"stage.sh: {path}: expected Tag_ABI_VFP_args: VFP registers")
    fp = re.search(r"^\s*Tag_FP_arch:\s*(.*)$", out, re.MULTILINE)
    if fp and ("VFPv4" in fp.group(1) or "VFPv5" in fp.group(1)):
        sys.exit(f"stage.sh: {path}: unsupported Tag_FP_arch for Cortex-A9: {fp.group(1).strip()}")
    simd = re.search(r"^\s*Tag_Advanced_SIMD_arch:\s*(.*)$", out, re.MULTILINE)
    if simd and ("Fused-MAC" in simd.group(1) or "NEONv2" in simd.group(1)):
        sys.exit(f"stage.sh: {path}: unsupported Tag_Advanced_SIMD_arch for Cortex-A9: {simd.group(1).strip()}")
    div = re.search(r"^\s*Tag_DIV_use:\s*(.*)$", out, re.MULTILINE)
    if div and "Allowed" in div.group(1):
        sys.exit(f"stage.sh: {path}: unsupported Tag_DIV_use for Cortex-A9: {div.group(1).strip()}")
PY
}

if [ "${1:-}" = "--check-elf" ]; then
    shift
    [ "$#" -gt 0 ] || { echo "usage: $0 --check-elf PATH..." >&2; exit 2; }
    check_kindle_elf "$@"
    exit 0
fi

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
