#!/bin/sh
# Verso launcher. Pauses the Kindle UI, runs Verso, and always brings the
# Kindle UI back afterwards, even if Verso crashes or is killed. It also
# switches in updates that Verso staged, and goes back to the previous
# version if a new one fails to start, with renames from the plan Verso staged, and starts Verso
# again when it asks (Restart to apply).
#
# Started from the Kindle library (documents/Verso.sh), KUAL
# (extensions/verso/menu.json), KPM (its launch.sh) or scripts/deploy.sh; each
# passes its name as $1, for the log. Installed at /mnt/us/verso/verso.sh.
#
# For scripts/test-launcher.sh only: VERSO_STORAGE_ROOT replaces /mnt/us,
# VERSO_TMP replaces /tmp, VERSO_FB replaces /dev/fb0, VERSO_LAUNCHER_BIN
# holds fake Kindle commands and counting file tools, VERSO_LAUNCHER_SHELL
# replaces /bin/sh, VERSO_BOOT_ID the kernel's boot id file,
# VERSO_START_TIMEOUT the 120 s a new version gets to start,
# VERSO_ARMHF_LOADER the hard-float loader it checks for and
# VERSO_REFUSAL_WAIT the 8 s an "unsupported" message stays up.

export PATH="/usr/sbin:/usr/bin:/sbin:/bin:${PATH}"
if [ -n "${VERSO_LAUNCHER_BIN}" ]; then
    PATH="${VERSO_LAUNCHER_BIN}:${PATH}"
    # The Kindle's busybox runs its own applets before anything in PATH;
    # functions come first.
    for c in pidof killall mv rm cp mkdir sync; do
        if [ -x "${VERSO_LAUNCHER_BIN}/${c}" ]; then
            eval "${c}() { \"\${VERSO_LAUNCHER_BIN}/${c}\" \"\$@\"; }"
        fi
    done
fi

ROOT="${VERSO_STORAGE_ROOT:-/mnt/us}"
TMP="${VERSO_TMP:-/tmp}"
FB="${VERSO_FB:-/dev/fb0}"
APP_DIR="${ROOT}/verso"
LOG="${APP_DIR}/verso.log"
PIDFILE="${TMP}/verso-launcher.pid"
LOCKFILE="${TMP}/verso-launcher.lock"
OWNERFILE="${TMP}/verso-launcher-$$.owner"
FB_DUMP="${TMP}/verso-fb.dump"
SELF="${TMP}/verso-launcher-$$.sh"
# Which boot the running session started in, and when; see note_last_session.
RUNNING="${APP_DIR}/running"
BOOT_ID="${VERSO_BOOT_ID:-/proc/sys/kernel/random/boot_id}"

# Updates (crates/verso-update/src/launcher.rs has the same names).
UPDATE_DIR="${APP_DIR}/update"
AWAKE_MARKER="${UPDATE_DIR}/awake"
STAGED="${UPDATE_DIR}/staged"
PREVIOUS="${UPDATE_DIR}/previous"
TRIAL="${UPDATE_DIR}/trial"
ROLLBACK="${UPDATE_DIR}/rollback.json"
HAND_INSTALL="${UPDATE_DIR}/hand-install"
STARTED="${TMP}/verso-started"
HUNG="${TMP}/verso-hung-$$"
START_TIMEOUT="${VERSO_START_TIMEOUT:-120}"
# Wi-Fi transfer (crates/verso/src/transfer/mod.rs has the same names).
TRANSFER_FIREWALL="${TMP}/verso-transfer-firewall"
TRANSFER_PORT=80
EXIT_INSTALL=85
EXIT_GO_BACK=86
EXIT_RESTART=87
EXIT_UNSUPPORTED=88
ARMHF_LOADER="${VERSO_ARMHF_LOADER:-/lib/ld-linux-armhf.so.3}"
REFUSAL_WAIT="${VERSO_REFUSAL_WAIT:-8}"
TAB="$(printf '\t')"

# Run from a copy in /tmp. /mnt/us is a FUSE filesystem; if this file were
# replaced there (by a deploy or an update) while running, the rest of this
# run could break.
if [ "$0" != "${SELF}" ]; then
    cp -f "$0" "${SELF}" && exec "${VERSO_LAUNCHER_SHELL:-/bin/sh}" "${SELF}" "$@"
    # If the copy failed, carry on from the original.
fi

# Log times are UTC, like Verso's own lines.
now() {
    date -u '+%Y-%m-%dT%H:%M:%SZ'
}

log() {
    echo "$(now) launcher: $*" >>"${LOG}"
}

# Writes a full problem report (tools/report.sh).
save_report() {
    [ -f "${APP_DIR}/tools/report.sh" ] || return
    REPORT_FILE="$(VERSO_STORAGE_ROOT="${ROOT}" sh "${APP_DIR}/tools/report.sh" report 2>>"${LOG}")"
    [ -n "${REPORT_FILE}" ] && log "saved problem report: ${REPORT_FILE}"
}

# Installed version string from installed.json, if present.
installed_version() {
    [ -r "${APP_DIR}/installed.json" ] || return 0
    awk '
        { content = content " " $0 }
        END {
            if (match(content, /"version"[ \t]*:[ \t]*"[^"]*"/)) {
                v = substr(content, RSTART, RLENGTH)
                sub(/^"version"[ \t]*:[ \t]*"/, "", v); sub(/"$/, "", v)
                print v
            }
        }
    ' "${APP_DIR}/installed.json" 2>/dev/null
}

# Logs a single-line runtime environment stamp (model, firmware, kernel, host libc).
log_env() {
    collector="${APP_DIR}/tools/collect-diagnostics.sh"
    if [ -r "${collector}" ] && grep -q -- '--identity' "${collector}" 2>/dev/null; then
        info="$(sh "${collector}" --identity 2>/dev/null | tr '\n' '|' | sed 's/|$//; s/|/ | /g')"
    else
        libc_ver="$(getconf GNU_LIBC_VERSION 2>/dev/null || true)"
        info="Kernel and architecture: $(uname -srm 2>/dev/null || echo unknown) | host libc: ${libc_ver:-unknown}"
    fi
    [ -n "${info}" ] && log "env: ${info}"
}

# On signal crashes, capture filtered kernel fault/OOM lines.
probe_crash_context() {
    [ "${VERSO_NO_CRASH_PROBES:-0}" = 1 ] && return 0
    if command -v dmesg >/dev/null 2>&1; then
        dm="$(dmesg 2>/dev/null | grep -Ei 'verso|segfault|unhandled|alignment|out of memory|killed process' | tail -n 15 | cut -c 1-256)"
        if [ -n "${dm}" ]; then
            log "dmesg (filtered):"
            printf '%s\n' "${dm}" >>"${LOG}"
        fi
    fi
}

# Prepare the owner before claiming the lock, so even a kill at the instant of
# acquisition leaves a recoverable owner. A hard link is atomic on /tmp.
echo $$ >"${OWNERFILE}"
if ! ln "${OWNERFILE}" "${LOCKFILE}" 2>/dev/null; then
    # Serialise stale-lock removal: two contenders must not both decide that
    # an old owner died and remove each other's newly acquired lock.
    if mkdir "${LOCKFILE}.recovery" 2>/dev/null; then
        OWNER="$(cat "${LOCKFILE}" 2>/dev/null)"
        if [ -n "${OWNER}" ] && ! kill -0 "${OWNER}" 2>/dev/null; then
            rm -f "${LOCKFILE}"
        fi
        rmdir "${LOCKFILE}.recovery" 2>/dev/null || true
    fi
    if ! ln "${OWNERFILE}" "${LOCKFILE}" 2>/dev/null; then
        log "Verso is already running or its launcher lock needs cleanup"
        rm -f "${SELF}" "${OWNERFILE}"
        exit 0
    fi
fi

release_lock() {
    if [ "$(cat "${LOCKFILE}" 2>/dev/null)" = "$$" ]; then
        rm -f "${LOCKFILE}"
    fi
    rm -f "${SELF}" "${OWNERFILE}"
}
trap release_lock EXIT

# Only one Verso at a time, including a process from an older launcher.
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
if [ "${POWER_STATE}" != "active" ]; then
    log "the Kindle isn't awake (power state: ${POWER_STATE}); not starting"
    exit 1
fi
# Verso is built for hard-float firmware (5.16.3 and later); older firmware
# can't even load it. Say so over the Kindle UI, which was never paused, so
# there is nothing to restore.
if [ ! -e "${ARMHF_LOADER}" ]; then
    log "this firmware has no hard-float loader (${ARMHF_LOADER}); Verso needs firmware 5.16.3 or later"
    # eips can't draw apostrophes.
    eips 1 3 "Verso does not support this Kindle firmware yet" >/dev/null 2>&1
    eips 1 4 "A problem report is saved in verso/reports" >/dev/null 2>&1
    save_report
    exit 1
fi
echo $$ >"${PIDFILE}"

# Rotate only after we own the launch. A refused second launch must leave the
# running process's log inode and the stream followed by deploy intact.
if [ -f "${LOG}" ] && [ "$(stat -c %s "${LOG}" 2>/dev/null || echo 0)" -gt 262144 ]; then
    mv -f "${LOG}" "${LOG}.old"
fi

AWESOME_STOPPED=no
STATUS_STOPPED=no
VOLUMD_STOPPED=no
PILLOW_DISABLED=no
VERSO_PID=""
WATCHER=""
export VERSO_LAUNCHER_PID=$$
export VERSO_SYSTEM_ACK="${TMP}/verso-system-$$"
# What this launcher understands (crates/verso-update/src/launcher.rs).
export VERSO_LAUNCHER_PROTOCOL=2
export VERSO_STARTED_MARKER="${STARTED}"

# Only the launcher changes the UI's running state. Verso asks for a native
# sleep screen with USR1 and takes the display back with USR2. Acknowledge only
# after the handoff; a failed command makes Verso exit through normal recovery.
native_sleep() {
    if killall -CONT awesome 2>/dev/null; then
        echo native >"${VERSO_SYSTEM_ACK}"
        log "native sleep screen"
    fi
}
verso_wake() {
    if killall -STOP awesome 2>/dev/null; then
        echo verso >"${VERSO_SYSTEM_ACK}"
        log "display returned to Verso"
    fi
}

# Removes every copy of the two firewall rules that open port $1
# (crates/verso/src/platform/firewall.rs adds them).
close_port() {
    for rule in "INPUT -p tcp --dport $1 -m conntrack --ctstate NEW,ESTABLISHED -j ACCEPT" \
        "OUTPUT -p tcp --sport $1 -m conntrack --ctstate ESTABLISHED -j ACCEPT"; do
        n=0
        # shellcheck disable=SC2086 # the rule is split into words on purpose
        while [ "${n}" -lt 4 ] && iptables -D ${rule} 2>/dev/null; do
            n=$((n + 1))
        done
    done
}

restore() {
    # Nothing may interrupt this.
    trap '' INT TERM HUP USR1 USR2
    trap - EXIT
    [ -n "${WATCHER}" ] && kill "${WATCHER}" 2>/dev/null
    log "restoring the Kindle UI"
    [ "${STATUS_STOPPED}" = yes ] && killall -CONT JunoStatusBarDriver 2>/dev/null
    [ "${AWESOME_STOPPED}" = yes ] && killall -CONT awesome 2>/dev/null
    # Put back what was on screen before, so there's no flash of our UI
    # while the home screen loads.
    if [ -f "${FB_DUMP}" ]; then
        cat "${FB_DUMP}" >"${FB}" 2>/dev/null
        rm -f "${FB_DUMP}"
    fi
    [ "${PILLOW_DISABLED}" = yes ] && lipc-set-prop com.lab126.pillow disableEnablePillow enable
    lipc-set-prop com.lab126.appmgrd start app://com.lab126.booklet.home
    if [ "$(cat "${PIDFILE}" 2>/dev/null)" = "$$" ]; then
        rm -f "${PIDFILE}"
    fi
    rm -f "${VERSO_SYSTEM_ACK}" "${HUNG}" "${RUNNING}"
    # An update download keeps the Kindle awake and leaves this marker until
    # it turns that off again. If Verso died first, turn it off here.
    if [ -f "${AWAKE_MARKER}" ]; then
        lipc-set-prop com.lab126.powerd preventScreenSaver 0 && rm -f "${AWAKE_MARKER}"
        log "turned off keep-awake left by an update download"
    fi
    # Wi-Fi transfer opens port 80 while its screen is open, with a marker.
    # If Verso died first, close it. (It keeps the Kindle awake by restarting
    # the idle countdown, which needs no undoing.)
    if [ -f "${TRANSFER_FIREWALL}" ]; then
        close_port "${TRANSFER_PORT}"
        rm -f "${TRANSFER_FIREWALL}"
        log "closed port ${TRANSFER_PORT}, left open by Wi-Fi transfer"
    fi
    log "done"
    # Last: a pending USB insertion may unmount /mnt/us immediately. Finish
    # all app storage access before handing it back to volumd.
    [ "${VOLUMD_STOPPED}" = yes ] && killall -CONT volumd 2>/dev/null
    release_lock
}
trap restore EXIT
trap 'exit 1' INT TERM HUP
trap native_sleep USR1
trap verso_wake USR2

# --- Updates -----------------------------------------------------------------
#
# Verso stages an update in update/staged/ with plan.txt, the list of moves
# (crates/verso-update/src/stage.rs, Staged::plan_text), and staged.json
# written last. Installing moves each replaced file to previous/files/ and
# each staged file into place; going back moves them the other way. Each
# step checks what is on disk first, so a job cut short by a power loss is
# finished by running it again (scripts/test-launcher.sh cuts each step). The journal says which job was running: an
# empty file, journal-install, journal-rollback or journal-cleanup, renamed
# from one to the next, so it can never be half written.

# Writes a file durably: synced, then renamed into place.
write_file() {
    printf '%s\n' "$2" >"$1.tmp" && sync && mv -f "$1.tmp" "$1" && sync
}

# put_copy SRC DST: moves a file across an fscrypt encryption boundary
# (e.g. unencrypted verso/ <-> encrypted documents/) by copying into DST's
# folder first and renaming atomically within it. Dotfile + .tmp avoids
# Kindle indexing and is also cleaned on boot by userstore_common_utils.
put_copy() {
    tmp="${2%/*}/.${2##*/}.tmp"
    if { rm -f "${tmp}" && cp -p "$1" "${tmp}" && sync && mv -f "${tmp}" "$2"; } 2>>"${LOG}"; then
        rm -f "$1" 2>>"${LOG}"
    else
        rm -f "${tmp}" 2>/dev/null
        return 1
    fi
}

# put SRC DST: moves a file to DST, creating its folder. An empty folder
# where the file goes (one version's folder, another's file) is removed.
# Crossing the documents/ fscrypt boundary copies into DST's folder first;
# other moves try zero-copy rename and fall back to put_copy on error.
put() {
    mkdir -p "${2%/*}" 2>>"${LOG}" && { [ ! -d "$2" ] || rmdir "$2" 2>>"${LOG}"; } || return 1
    src_in_docs=0
    dst_in_docs=0
    case "$1" in "${ROOT}/documents/"*) src_in_docs=1 ;; esac
    case "$2" in "${ROOT}/documents/"*) dst_in_docs=1 ;; esac
    if [ "${src_in_docs}" != "${dst_in_docs}" ]; then
        put_copy "$1" "$2"
    else
        mv -f "$1" "$2" 2>>"${LOG}" || put_copy "$1" "$2"
    fi
}

# plan_value KEY FILE: a header value from a plan.
plan_value() {
    sed -n "s/^$1${TAB}//p" "$2" 2>/dev/null | head -n 1
}

# The job a journal names, if any; the furthest along if a rename left two.
journal_job() {
    for job in cleanup rollback install; do
        if [ -e "${UPDATE_DIR}/journal-${job}" ]; then
            echo "${job}"
            return
        fi
    done
}

set_journal() {
    job="$(journal_job)"
    if [ -z "${job}" ]; then
        : >"${UPDATE_DIR}/journal-$1" || return 1
    elif [ "${job}" != "$1" ]; then
        mv -f "${UPDATE_DIR}/journal-${job}" "${UPDATE_DIR}/journal-$1" || return 1
    fi
    sync
}

end_journal() {
    rm -f "${UPDATE_DIR}/journal-install" "${UPDATE_DIR}/journal-rollback" "${UPDATE_DIR}/journal-cleanup"
    sync
}

# Where the log ends now; a rollback note quotes what comes after.
mark_log() {
    LOG_SIZE="$(stat -c %s "${LOG}" 2>/dev/null || echo 0)"
}
LOG_SIZE=0

# Forward. Removed files go aside; new files are noted, so going back can
# remove them; then each staged file goes in and its old file aside: moved,
# or for a file Verso is started from ("replace"), copied first and the new
# one renamed over it, so its path is never empty.
install_moves() {
    plan="${PREVIOUS}/plan.txt"
    [ -f "${plan}" ] || return 1
    while IFS="${TAB}" read -r op a b; do
        if [ "${op}" = remove ] && [ -f "${ROOT}/${a}" ]; then
            put "${ROOT}/${a}" "${PREVIOUS}/files/${a}" || return 1
        fi
    done <"${plan}"
    while IFS="${TAB}" read -r op a b; do
        case "${op}" in install | replace) ;; *) continue ;; esac
        added="${PREVIOUS}/added/${b}"
        if [ -f "${ROOT}/${a}" ] && [ ! -f "${ROOT}/${b}" ] && [ ! -f "${PREVIOUS}/files/${b}" ]; then
            { mkdir -p "${added%/*}" && : >"${added}"; } 2>>"${LOG}" || return 1
        fi
    done <"${plan}"
    sync
    while IFS="${TAB}" read -r op a b; do
        case "${op}" in install | replace) ;; *) continue ;; esac
        [ -f "${ROOT}/${a}" ] || continue
        old="${PREVIOUS}/files/${b}"
        if [ -f "${ROOT}/${b}" ] && [ ! -f "${old}" ] && [ ! -f "${PREVIOUS}/added/${b}" ]; then
            if [ "${op}" = replace ]; then
                { mkdir -p "${old%/*}" && cp -p "${ROOT}/${b}" "${old}.tmp" && sync &&
                    mv -f "${old}.tmp" "${old}"; } 2>>"${LOG}" || return 1
            else
                put "${ROOT}/${b}" "${old}" || return 1
            fi
        fi
        put "${ROOT}/${a}" "${ROOT}/${b}" || return 1
    done <"${plan}"
}

# Back. Each old file is put back over the new one (skipping a still-staged
# file whose target already matches it, e.g. a read-only folder), new files
# are removed (leaving a directory that was never turned into a file), then
# the removed files come back.
rollback_moves() {
    plan="${PREVIOUS}/plan.txt"
    [ -f "${plan}" ] || return 1
    while IFS="${TAB}" read -r op a b; do
        case "${op}" in install | replace) ;; *) continue ;; esac
        old="${PREVIOUS}/files/${b}"
        added="${PREVIOUS}/added/${b}"
        if [ -f "${old}" ]; then
            if [ -f "${ROOT}/${a}" ] && [ -f "${ROOT}/${b}" ] && cmp -s "${old}" "${ROOT}/${b}" 2>/dev/null; then
                rm -f "${old}" 2>>"${LOG}" || return 1
            else
                put "${old}" "${ROOT}/${b}" || return 1
            fi
        elif [ -f "${added}" ]; then
            {
                { [ ! -f "${ROOT}/${b}" ] && [ ! -L "${ROOT}/${b}" ]; } || rm -f "${ROOT}/${b}"
            } 2>>"${LOG}" && rm -f "${added}" 2>>"${LOG}" || return 1
        fi
    done <"${plan}"
    while IFS="${TAB}" read -r op a b; do
        if [ "${op}" = remove ] && [ -f "${PREVIOUS}/files/${a}" ]; then
            put "${PREVIOUS}/files/${a}" "${ROOT}/${a}" || return 1
        fi
    done <"${plan}"
}

# Installs the staged update. Returns 0 when it did something (installed
# it, or failed and went back), 1 when there was nothing to install or it
# couldn't begin.
install_update() {
    [ -f "${STAGED}/staged.json" ] || return 1
    if [ "$(head -n 1 "${STAGED}/plan.txt" 2>/dev/null)" != "verso-plan${TAB}1" ]; then
        log "the staged update has no plan this launcher can read; removing it"
        rm -rf "${STAGED}"
        return 1
    fi
    log "installing Verso $(plan_value version "${STAGED}/plan.txt")"
    mark_log
    # The files kept from the update before go now. Until the journal
    # exists nothing has moved, so a power loss here just starts over.
    rm -rf "${PREVIOUS}" "${ROLLBACK}" "${ROLLBACK}.new" "${TRIAL}"
    if ! { mkdir -p "${PREVIOUS}" && cp "${STAGED}/plan.txt" "${PREVIOUS}/plan.txt" && sync &&
        set_journal install; }; then
        log "couldn't begin installing the update"
        rm -rf "${PREVIOUS}"
        return 1
    fi
    finish_install
    return 0
}

finish_install() {
    if ! install_moves; then
        log "switching the update in failed; going back"
        go_back install ""
        return
    fi
    sync
    # On trial until it has started (see run_verso).
    write_file "${TRIAL}" 0 &&
        rm -rf "${STAGED}" &&
        end_journal &&
        log "installed Verso $(plan_value version "${PREVIOUS}/plan.txt")"
}

# go_back REASON STATUS: moves the previous files back, notes why in
# rollback.json and forgets the previous files. REASON: exit (STATUS is the
# exit status), hang, interrupted, install or requested. Returns 1 if there
# was nothing to go back to, or if going back failed (the journal stays,
# and the next launch tries again).
go_back() {
    plan="${PREVIOUS}/plan.txt"
    if [ ! -f "${plan}" ]; then
        log "no previous version to go back to"
        rm -f "${TRIAL}"
        return 1
    fi
    from="$(plan_value version "${plan}")"
    to="$(plan_value from "${plan}")"
    rollback_note "$1" "$2" "${from}" "${to}" >"${ROLLBACK}.new"
    log "going back from Verso ${from} to ${to:-the previous version} ($1${2:+ $2})"
    # A later update staged on top of this version doesn't fit the old one.
    # (For a failed install, finish_cleanup removes staged/ once rollback finishes.)
    [ "$1" = install ] || rm -rf "${STAGED}"
    sync
    set_journal rollback || return 1
    finish_rollback
}

finish_rollback() {
    if ! rollback_moves; then
        log "going back failed; the next launch tries again"
        return 1
    fi
    sync
    set_journal cleanup && finish_cleanup
}

finish_cleanup() {
    if [ -f "${ROLLBACK}.new" ]; then
        mv -f "${ROLLBACK}.new" "${ROLLBACK}"
    fi
    rm -rf "${PREVIOUS}" "${STAGED}" "${TRIAL}" && end_journal && log "went back"
}

# rollback_note REASON STATUS FROM TO: rollback.json, with the end of the
# log since mark_log, as JSON strings.
rollback_note() {
    printf '{"format":1,"from":"%s","to":"%s","reason":"%s","status":%s,"time":%s,"log":[' \
        "$3" "$4" "$1" "${2:-null}" "$(date +%s)"
    tail -c +"$((LOG_SIZE + 1))" "${LOG}" 2>/dev/null | tail -n 20 | cut -c 1-300 |
        tr -d '\000-\011\013-\037' | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' | {
        sep=""
        while IFS= read -r line; do
            printf '%s"%s"' "${sep}" "${line}"
            sep=","
        done
    }
    printf ']}\n'
}

# Finishes a job a power loss cut short. Returns 1 if the files are in a
# state Verso can't start from.
recover() {
    job="$(journal_job)"
    [ -n "${job}" ] || return 0
    log "finishing an interrupted ${job}"
    mark_log
    case "${job}" in
        install) finish_install ;;
        rollback) finish_rollback ;;
        cleanup) finish_cleanup ;;
    esac
    [ -z "$(journal_job)" ]
}

# Before the first start: clear stale update state after a zip install, finish
# an interrupted job, go back from a version that didn't start twice in a row,
# install a staged update.
prepare() {
    if [ -f "${HAND_INSTALL}" ]; then
        log "installed from zip; clearing old update state"
        rm -rf "${STAGED}" "${UPDATE_DIR}/download" "${PREVIOUS}" "${TRIAL}" "${ROLLBACK}" "${ROLLBACK}.new" "${UPDATE_DIR}"/journal-*
        rm -f "${HAND_INSTALL}"
        sync
    fi
    recover || return 1
    if [ -f "${TRIAL}" ] && [ "$(cat "${TRIAL}" 2>/dev/null)" -ge 2 ] 2>/dev/null; then
        mark_log
        LOG_SIZE=0 # Quote the end of the log from the launches cut short.
        go_back interrupted "" || [ -z "$(journal_job)" ] || return 1
    fi
    if [ ! -f "${TRIAL}" ]; then
        install_update
        [ -z "$(journal_job)" ] || return 1
    fi
    return 0
}

# On trial: removes the trial once Verso creates the started marker, or
# stops Verso if it hasn't after START_TIMEOUT seconds.
watch_start() {
    trap - EXIT INT TERM HUP USR1 USR2
    i=0
    while kill -0 "${VERSO_PID}" 2>/dev/null; do
        if [ -f "${STARTED}" ]; then
            rm -f "${TRIAL}"
            sync
            log "the new version has started"
            exit 0
        fi
        if [ "${i}" -ge "${START_TIMEOUT}" ]; then
            log "the new version hasn't started after ${START_TIMEOUT} s; stopping it"
            : >"${HUNG}"
            kill -TERM "${VERSO_PID}" 2>/dev/null
            sleep 5
            kill -KILL "${VERSO_PID}" 2>/dev/null
            exit 0
        fi
        sleep 1
        i=$((i + 1))
    done
}

# Signal number to name for standard Linux signals.
signal_name() {
    case "$1" in
        1) echo SIGHUP ;;
        2) echo SIGINT ;;
        3) echo SIGQUIT ;;
        4) echo SIGILL ;;
        5) echo SIGTRAP ;;
        6) echo SIGABRT ;;
        7) echo SIGBUS ;;
        8) echo SIGFPE ;;
        9) echo SIGKILL ;;
        10) echo SIGUSR1 ;;
        11) echo SIGSEGV ;;
        12) echo SIGUSR2 ;;
        13) echo SIGPIPE ;;
        14) echo SIGALRM ;;
        15) echo SIGTERM ;;
        31) echo SIGSYS ;;
        *) echo "SIG$1" ;;
    esac
}

# Runs Verso until it exits; RC is its exit status. A version on trial
# counts an attempt first (a power loss during it counts too).
run_verso() {
    ON_TRIAL=no
    rm -f "${STARTED}" "${HUNG}"
    if [ -f "${TRIAL}" ]; then
        ON_TRIAL=yes
        ATTEMPTS="$(cat "${TRIAL}" 2>/dev/null)"
        case "${ATTEMPTS}" in '' | *[!0-9]*) ATTEMPTS=1 ;; esac
        ATTEMPTS=$((ATTEMPTS + 1))
        write_file "${TRIAL}" "${ATTEMPTS}"
        log "trying the new version (attempt ${ATTEMPTS})"
    fi
    mark_log
    VER="$(installed_version)"
    START_EPOCH="$(date +%s 2>/dev/null || echo 0)"
    log "starting Verso${VER:+ (${VER})}"
    VERSO_LOG_START="$(stat -c %s "${LOG}" 2>/dev/null || echo 0)"
    ./verso >>"${LOG}" 2>&1 &
    VERSO_PID=$!
    if [ "${ON_TRIAL}" = yes ]; then
        watch_start &
        WATCHER=$!
    fi
    # If we're asked to stop, pass it on to Verso and wait for it to finish, so
    # the Kindle UI never comes back while Verso is still drawing.
    trap 'kill -TERM "${VERSO_PID}" 2>/dev/null' INT TERM HUP
    while :; do
        wait "${VERSO_PID}"
        RC=$?
        kill -0 "${VERSO_PID}" 2>/dev/null || break
    done
    trap 'exit 1' INT TERM HUP
    if [ -n "${WATCHER}" ]; then
        kill "${WATCHER}" 2>/dev/null
        wait "${WATCHER}" 2>/dev/null
        WATCHER=""
    fi
    VERSO_PID=""
    END_EPOCH="$(date +%s 2>/dev/null || echo 0)"
    ELAPSED=$((END_EPOCH - START_EPOCH))
    [ "${ELAPSED}" -lt 0 ] && ELAPSED=0
    VERSO_LOG_END="$(stat -c %s "${LOG}" 2>/dev/null || echo 0)"
    VERSO_OUT_BYTES=$((VERSO_LOG_END - VERSO_LOG_START))
    [ "${VERSO_OUT_BYTES}" -lt 0 ] && VERSO_OUT_BYTES=0
    EXIT_KIND=crash
    EXIT_SIG=""
    if [ "${RC}" -gt 128 ] && [ "${RC}" -le 192 ]; then
        EXIT_SIG="$(signal_name $((RC - 128)))"
    fi
    case "${RC}" in
        0 | "${EXIT_INSTALL}" | "${EXIT_GO_BACK}" | "${EXIT_RESTART}")
            EXIT_KIND=bye
            ;;
        "${EXIT_UNSUPPORTED}")
            EXIT_KIND=unsupported
            ;;
        *)
            if tail -c +"$((LOG_SIZE + 1))" "${LOG}" 2>/dev/null | grep -E -q 'panicked at|panic:'; then
                EXIT_KIND=panic
            fi
            ;;
    esac
    if [ -n "${EXIT_SIG}" ]; then
        EXIT_DESC="${EXIT_KIND} (${EXIT_SIG}, code ${RC})"
    else
        EXIT_DESC="${EXIT_KIND} (code ${RC})"
    fi
    log "Verso ended: ${EXIT_DESC} (after ${ELAPSED}s, ${VERSO_OUT_BYTES}B logged)"
    if [ "${EXIT_KIND}" = crash ] || [ "${EXIT_KIND}" = panic ]; then
        [ -n "${EXIT_SIG}" ] && probe_crash_context
        if [ -f "${APP_DIR}/tools/report.sh" ]; then
            CRASH_FILE="$(VERSO_STORAGE_ROOT="${ROOT}" sh "${APP_DIR}/tools/report.sh" crash "${EXIT_DESC}" 2>>"${LOG}")"
            [ -n "${CRASH_FILE}" ] && log "saved crash report: ${CRASH_FILE}"
        fi
    fi
    # Verso said on screen why it can't run here. Save the hardware facts and
    # leave the message up long enough to read before the Kindle UI returns.
    if [ "${EXIT_KIND}" = unsupported ]; then
        save_report
        sleep "${REFUSAL_WAIT}"
    fi
}

# Says if the session before this one had no end, and whether the Kindle has
# restarted since (a forced restart or an empty battery, not just a launcher
# that was killed). Leaves the same marker for the next launch.
note_last_session() {
    boot=""
    read -r boot 2>/dev/null <"${BOOT_ID}"
    if [ -f "${RUNNING}" ]; then
        last_boot="" last_start=""
        read -r last_boot last_start 2>/dev/null <"${RUNNING}"
        since="unknown whether the Kindle restarted since"
        if [ -n "${boot}" ] && [ -n "${last_boot}" ]; then
            since="the Kindle has restarted since"
            [ "${boot}" = "${last_boot}" ] && since="the Kindle hasn't restarted since"
        fi
        log "the last session, started ${last_start:-at an unknown time}, has no end; ${since}"
    fi
    echo "${boot:-unknown} $(now)" 2>/dev/null >"${RUNNING}"
}

# The app Amazon's UI had in front. Its id only: nothing after it.
log_active_app() {
    app="$(lipc-get-prop com.lab126.appmgrd activeApp 2>/dev/null)"
    app="${app#app://}"
    log "active app at pause: ${app%%[!A-Za-z0-9._-]*}"
}

# What started us, for problem reports: only known names, so nothing else a
# caller passes ends up in the log. Older KPM packages pass nothing. $2 is how
# often documents/Verso.sh or KPM's launch.sh waited 0.1 s for the Home screen.
case "${2:-}" in
    '' | *[!0-9]*) WAITED="" ;;
    *) WAITED=" (waited $2 of 50 times for Home)" ;;
esac
case "${1:-}" in
    library | kual | kpm | deploy) log "launched from $1${WAITED}" ;;
    *) log "launched from an unknown source" ;;
esac
note_last_session
# In the background: asking takes a moment, and the pause must not wait for it.
log_active_app &

# Pause the Kindle UI, the same way KOReader does on this firmware.
# Each flag is set *before* acting, so an interruption can't skip its undo.
log "pausing the Kindle UI"
cat "${FB}" >"${FB_DUMP}" 2>/dev/null
PILLOW_DISABLED=yes
lipc-set-prop com.lab126.pillow disableEnablePillow disable  # status bar
AWESOME_STOPPED=yes
killall -STOP awesome 2>/dev/null  # window manager: stops the UI drawing over us
STATUS_STOPPED=yes
killall -STOP JunoStatusBarDriver 2>/dev/null  # the clock has its own process on PW6
VOLUMD_STOPPED=yes
killall -STOP volumd 2>/dev/null  # USB: keeps /mnt/us mounted if a cable is plugged in

# Update files move only while USB can't take the storage away.
if ! prepare; then
    log "the files of an update are in an unfinished state; not starting Verso"
    exit 1
fi

cd "${APP_DIR}" || exit 1
log_env
RUNS=0
while :; do
    run_verso
    RUNS=$((RUNS + 1))
    if [ "${ON_TRIAL}" = yes ] && [ -f "${TRIAL}" ]; then
        if [ -f "${STARTED}" ]; then
            rm -f "${TRIAL}"
        else
            REASON=exit
            [ -f "${HUNG}" ] && REASON=hang
            log "the new version didn't start"
            go_back "${REASON}" "${RC}" || break
            continue
        fi
    fi
    # Restarts are for updates, going back and Restart to apply; never loop
    # on their own.
    [ "${RUNS}" -lt 8 ] || break
    case "${RC}" in
        "${EXIT_INSTALL}")
            if install_update; then
                [ -z "$(journal_job)" ] && continue
            else
                log "no update to install"
            fi
            ;;
        "${EXIT_GO_BACK}")
            mark_log
            go_back requested "" && continue
            ;;
        "${EXIT_RESTART}")
            # Verso saved nothing on the way out, so it reads data edited
            # by hand (over Wi-Fi transfer) when it starts again.
            log "restarting Verso"
            continue
            ;;
    esac
    break
done
exit "${RC}"
