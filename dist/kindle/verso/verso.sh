#!/bin/sh
# Verso launcher. Pauses the Kindle UI, runs Verso, and always brings the
# Kindle UI back afterwards, even if Verso crashes or is killed.
#
# Started from KUAL (extensions/verso/menu.json) or by scripts/deploy.sh.
# Installed at /mnt/us/verso/verso.sh.

export PATH="/usr/sbin:/usr/bin:/sbin:/bin:${PATH}"

APP_DIR="/mnt/us/verso"
LOG="${APP_DIR}/verso.log"
PIDFILE="/tmp/verso-launcher.pid"
FB_DUMP="/tmp/verso-fb.dump"
SELF="/tmp/verso-launcher.sh"

# Run from a copy in /tmp. /mnt/us is a FUSE filesystem; if this file were
# replaced there (e.g. by a deploy) while running, the restore step could break.
if [ "$0" != "${SELF}" ]; then
    cp -f "$0" "${SELF}" && exec /bin/sh "${SELF}" "$@"
    # If the copy failed, carry on from the original.
fi

# Keep the log small: start a fresh one when it passes 256 KB.
if [ -f "${LOG}" ] && [ "$(stat -c %s "${LOG}" 2>/dev/null || echo 0)" -gt 262144 ]; then
    mv -f "${LOG}" "${LOG}.old"
fi

log() {
    echo "$(date '+%Y-%m-%d %H:%M:%S') launcher: $*" >>"${LOG}"
}

# Only one Verso at a time.
if pidof verso >/dev/null 2>&1 || { [ -f "${PIDFILE}" ] && kill -0 "$(cat "${PIDFILE}")" 2>/dev/null; }; then
    log "Verso is already running"
    exit 0
fi
# Don't fight another app for the screen, and don't start while asleep:
# pausing and restoring the Kindle UI underneath them would mess up their state.
if pidof reader.lua >/dev/null 2>&1; then
    log "KOReader is running; close it before starting Verso"
    exit 1
fi
POWER_STATE="$(lipc-get-prop com.lab126.powerd state 2>/dev/null)"
if [ -n "${POWER_STATE}" ] && [ "${POWER_STATE}" != "active" ]; then
    log "the Kindle isn't awake (power state: ${POWER_STATE}); not starting"
    exit 1
fi
echo $$ >"${PIDFILE}"

AWESOME_STOPPED=no
STATUS_STOPPED=no
VOLUMD_STOPPED=no
PILLOW_DISABLED=no
VERSO_PID=""

restore() {
    # Nothing may interrupt this.
    trap '' INT TERM HUP
    trap - EXIT
    log "restoring the Kindle UI"
    [ "${VOLUMD_STOPPED}" = yes ] && killall -CONT volumd 2>/dev/null
    [ "${STATUS_STOPPED}" = yes ] && killall -CONT JunoStatusBarDriver 2>/dev/null
    [ "${AWESOME_STOPPED}" = yes ] && killall -CONT awesome 2>/dev/null
    # Put back what was on screen before, so there's no flash of our UI
    # while the home screen loads.
    if [ -f "${FB_DUMP}" ]; then
        cat "${FB_DUMP}" >/dev/fb0 2>/dev/null
        rm -f "${FB_DUMP}"
    fi
    [ "${PILLOW_DISABLED}" = yes ] && lipc-set-prop com.lab126.pillow disableEnablePillow enable
    lipc-set-prop com.lab126.appmgrd start app://com.lab126.booklet.home
    rm -f "${PIDFILE}"
    log "done"
}
trap restore EXIT
trap 'exit 1' INT TERM HUP

# Pause the Kindle UI, the same way KOReader does on this firmware.
# Each flag is set *before* acting, so an interruption can't skip its undo.
log "pausing the Kindle UI"
cat /dev/fb0 >"${FB_DUMP}" 2>/dev/null
PILLOW_DISABLED=yes
lipc-set-prop com.lab126.pillow disableEnablePillow disable  # status bar
AWESOME_STOPPED=yes
killall -STOP awesome 2>/dev/null  # window manager: stops the UI drawing over us
STATUS_STOPPED=yes
killall -STOP JunoStatusBarDriver 2>/dev/null  # the clock has its own process on PW6
VOLUMD_STOPPED=yes
killall -STOP volumd 2>/dev/null  # USB: keeps /mnt/us mounted if a cable is plugged in

log "starting Verso"
cd "${APP_DIR}" || exit 1
./verso >>"${LOG}" 2>&1 &
VERSO_PID=$!

# If we're asked to stop, pass it on to Verso and wait for it to finish, so
# the Kindle UI never comes back while Verso is still drawing.
trap 'kill -TERM "${VERSO_PID}" 2>/dev/null' INT TERM HUP
while :; do
    wait "${VERSO_PID}"
    RC=$?
    kill -0 "${VERSO_PID}" 2>/dev/null || break
done
log "Verso exited with code ${RC}"
exit "${RC}"
