#!/bin/sh
# KPM launch hook (`kpm launch verso` / `;kpm launch verso`).
# Invokes /mnt/us/verso/verso.sh in a detached session, waiting for
# SH_Integration to unload first when launched through it so verso.sh never
# races popAppHistory or pauses awesome mid-transition.
#
# For tests only: VERSO_STORAGE_ROOT replaces /mnt/us,
# VERSO_LAUNCHER_BIN adds fake Kindle commands to PATH and
# VERSO_LAUNCHER_SHELL replaces /bin/sh.

export PATH="/usr/sbin:/usr/bin:/sbin:/bin:${PATH}"
if [ -n "${VERSO_LAUNCHER_BIN:-}" ]; then
    PATH="${VERSO_LAUNCHER_BIN}:${PATH}"
fi

ROOT="${VERSO_STORAGE_ROOT:-/mnt/us}"
LAUNCHER="${ROOT}/verso/verso.sh"
[ -f "${LAUNCHER}" ] || exit 1

SH="${VERSO_LAUNCHER_SHELL:-/bin/sh}"

# shellcheck disable=SC2086 # SH may be "busybox sh" in tests
setsid ${SH} -c '
    if [ -n "${VERSO_LAUNCHER_BIN:-}" ] && [ -x "${VERSO_LAUNCHER_BIN}/pidof" ]; then
        pidof() { "${VERSO_LAUNCHER_BIN}/pidof" "$@"; }
    fi
    waited=0
    i=0
    while [ "${i}" -lt 50 ]; do
        active="$(lipc-get-prop com.lab126.appmgrd activeApp 2>/dev/null || true)"
        if ! pidof sh_integration_launcher >/dev/null 2>&1 &&
            [ "${active}" != "com.notmarek.shell_integration.launcher" ]; then
            break
        fi
        waited=1
        usleep 100000 2>/dev/null || sleep 0.1 2>/dev/null || sleep 1
        i=$((i + 1))
    done
    if [ "${waited}" = 1 ]; then
        usleep 200000 2>/dev/null || sleep 0.2 2>/dev/null || sleep 1
    fi
    exec ${VERSO_LAUNCHER_SHELL:-/bin/sh} "$1"
' verso-launch "${LAUNCHER}" </dev/null >/dev/null 2>&1 &
exit 0
