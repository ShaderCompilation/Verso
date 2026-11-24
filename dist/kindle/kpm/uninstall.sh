#!/bin/sh
# KPM uninstall hook (called with no args on full uninstall, or "upgrade"
# before the new version's install.sh upgrade). Runs relative to the unpacked
# KPM package folder.
#
# Removes Verso's installed app, launcher, KUAL and scriptlet files while
# preserving user data under verso/data/ (books state, highlights, settings,
# dictionaries) and verso/ssh/. Never removes the unpacked KPM package folder
# itself (./); KPM deletes it after this script returns 0.
#
# For tests only: VERSO_STORAGE_ROOT replaces /mnt/us.
set -eu

ROOT="${VERSO_STORAGE_ROOT:-/mnt/us}"
PKG_DIR="$(cd "$(dirname "$0")" && pwd)"
MODE="${1:-}"
LIST="${PKG_DIR}/files.txt"
PAYLOAD="${PKG_DIR}/payload"

is_safe_rel() {
    case "$1" in
        "" | /* | .. | ../* | */.. | */../* | . | ./* | */. | */./*) return 1 ;;
    esac
    return 0
}

prune_parents() {
    dir="$1"
    while [ -n "${dir}" ] &&
        [ "${dir}" != "${ROOT}" ] &&
        [ "${dir}" != "${ROOT}/documents" ] &&
        [ "${dir}" != "${ROOT}/extensions" ]; do
        rmdir "${dir}" 2>/dev/null || break
        next="${dir%/*}"
        [ "${next}" != "${dir}" ] || break
        dir="${next}"
    done
}

if [ "${MODE}" = "upgrade" ]; then
    echo "Preparing Verso upgrade in ${ROOT}"
else
    echo "Uninstalling Verso from ${ROOT} (preserving verso/data/)"
fi

if [ -f "${LIST}" ]; then
    while IFS= read -r rel; do
        [ -n "${rel}" ] || continue
        is_safe_rel "${rel}" || continue
        dst="${ROOT}/${rel}"
        if [ "${rel}" = "documents/Verso.sh" ]; then
            if [ -f "${dst}" ]; then
                if grep -q '^# Name: Verso$' "${dst}" 2>/dev/null ||
                    { [ -f "${PAYLOAD}/${rel}" ] && cmp -s "${dst}" "${PAYLOAD}/${rel}" 2>/dev/null; }; then
                    rm -f "${dst}"
                fi
            fi
        else
            rm -f "${dst}"
        fi
        prune_parents "${dst%/*}"
    done <"${LIST}"
fi

if [ "${MODE}" != "upgrade" ]; then
    if [ ! -e "${ROOT}/documents/Verso.sh" ]; then
        rm -rf "${ROOT}/documents/Verso.sh.sdr"
    fi
    rm -f "${ROOT}/verso/installed.json" "${ROOT}/verso/verso.log" "${ROOT}/verso/verso.log.old"
    rm -rf "${ROOT}/verso/update" "${ROOT}/verso/cache"
    rmdir "${ROOT}/extensions/verso" 2>/dev/null || true
    rmdir "${ROOT}/verso/data" 2>/dev/null || true
    rmdir "${ROOT}/verso" 2>/dev/null || true
fi

exit 0
