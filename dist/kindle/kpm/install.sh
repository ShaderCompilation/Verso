#!/bin/sh
# KPM install hook (called with no args on fresh install, or "upgrade" on
# upgrade). Runs relative to the unpacked KPM package folder.
#
# Copies every file listed in files.txt from ./payload/ into the Kindle's
# storage root, writing verso/installed.json last and ensuring
# verso/update/hand-install is present so verso.sh clears stale OTA state.
# Pre-existing user data under verso/data/ and keys under verso/ssh/ are left
# untouched.
#
# For tests only: VERSO_STORAGE_ROOT replaces /mnt/us.
set -eu

ROOT="${VERSO_STORAGE_ROOT:-/mnt/us}"
PKG_DIR="$(cd "$(dirname "$0")" && pwd)"
MODE="${1:-install}"
LIST="${PKG_DIR}/files.txt"
PAYLOAD="${PKG_DIR}/payload"

if [ ! -f "${LIST}" ] || [ ! -d "${PAYLOAD}" ]; then
    echo "verso install: package payload is incomplete" >&2
    exit 1
fi

is_safe_rel() {
    case "$1" in
        "" | /* | .. | ../* | */.. | */../* | . | ./* | */. | */./*) return 1 ;;
    esac
    return 0
}

install_one() {
    rel="$1"
    if ! is_safe_rel "${rel}"; then
        echo "verso install: refusing unsafe path: ${rel}" >&2
        exit 1
    fi
    src="${PAYLOAD}/${rel}"
    dst="${ROOT}/${rel}"
    if [ ! -f "${src}" ]; then
        echo "verso install: missing payload file: ${rel}" >&2
        exit 1
    fi
    mkdir -p "${dst%/*}"
    cp -f "${src}" "${dst}.kpm-tmp"
    if [ -x "${src}" ]; then
        chmod 755 "${dst}.kpm-tmp" 2>/dev/null || true
    else
        chmod 644 "${dst}.kpm-tmp" 2>/dev/null || true
    fi
    mv -f "${dst}.kpm-tmp" "${dst}"
}

if [ "${MODE}" = "upgrade" ]; then
    echo "Upgrading Verso in ${ROOT}"
else
    echo "Installing Verso into ${ROOT}"
fi

HAS_INSTALLED_JSON=0
while IFS= read -r rel; do
    [ -n "${rel}" ] || continue
    if [ "${rel}" = "verso/installed.json" ]; then
        HAS_INSTALLED_JSON=1
        continue
    fi
    install_one "${rel}"
done <"${LIST}"

# Ensure hand-install marker is present so verso.sh resets any stale OTA state.
if [ ! -f "${ROOT}/verso/update/hand-install" ]; then
    mkdir -p "${ROOT}/verso/update"
    : >"${ROOT}/verso/update/hand-install"
fi

# Write installed.json last so it never describes a partial install.
if [ "${HAS_INSTALLED_JSON}" = 1 ]; then
    install_one "verso/installed.json"
fi

echo "Verso installed."
exit 0
