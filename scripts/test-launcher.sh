#!/bin/sh
# Tests the launcher's update switch-over, journal and rollback
# (dist/kindle/verso/verso.sh) on a pretend Kindle: a
# storage root in a work folder, fake Kindle commands (nothing on this
# machine is paused or restarted) and fake Verso versions that do what each
# case needs: start, ask for the update, crash, panic, hang, lose power.
#
#   scripts/test-launcher.sh                 all cases, with /bin/sh
#   VERSO_TEST_SHELL="busybox sh" scripts/test-launcher.sh
#   STEP_EVERY=3 scripts/test-launcher.sh    every third power-loss point only
#   SWEEP=install STEP_FROM=17 STEP_EVERY=1000 KEEP=1 scripts/test-launcher.sh
#                                            one point of one sweep, files kept
#   scripts/test-launcher.sh LAUNCHER WORKDIR  (the Kindle run)
#
# Power loss: the launcher's mv, rm, cp, mkdir and sync go through counting
# wrappers; with CRASH_AT=N the Nth one kills the launcher's whole process
# group instead, the pretend /tmp is wiped, and the launcher runs again.
# Plain POSIX sh, so it also runs under the Kindle's busybox.
set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
SELF="${HERE}/$(basename "$0")"
SH="${VERSO_TEST_SHELL:-/bin/sh}"
if [ -n "${VERSO_TEST_STAGE:-}" ]; then
    # Called by a fake Verso to stage version 2 (see stage_v2).
    W="${VERSO_TEST_DIR}"
else
    LAUNCHER="${1:-${HERE}/../dist/kindle/verso/verso.sh}"
    W="${2:-$(mktemp -d "${TMPDIR:-/tmp}/verso-launcher-test.XXXXXX")}"
    SCRIPTLET="${VERSO_TEST_SCRIPTLET:-${HERE}/../dist/kindle/documents/Verso.sh}"
fi
STEP_EVERY="${STEP_EVERY:-1}"
if [ -n "${KEEP:-}" ] || [ -n "${STEP_FROM:-}" ]; then
    JOBS=1
elif [ -z "${JOBS:-}" ]; then
    JOBS="$(nproc 2>/dev/null || echo 1)"
    case "${JOBS}" in '' | *[!0-9]*) JOBS=1 ;; esac
    [ "${JOBS}" -gt 8 ] && JOBS=8
fi
FAILED=0
CASES=0

mkdir -p "${W}"
W="$(cd "${W}" && pwd)"
R="${W}/root"
U="${R}/verso/update"
TAB="$(printf '\t')"

# --- Fixtures ------------------------------------------------------------------

# A fake Verso: records the run, prints a line to the log, then does the next
# thing queued for its version (queue-<v>), else its default (default-<v>):
# stage (stage version 2 and ask for the update, as Verso does), update (ask
# without staging), ok, goback, restart (Restart to apply, noting the
# protocol the launcher told it), fail, unsupported, crash, panic, hang or
# powerloss.
make_app() {
    printf '#!/bin/sh\n# Fake Verso %s (scripts/test-launcher.sh)\nV=%s\n' "$1" "$1"
    cat <<'EOF'
T="${VERSO_TEST_DIR}"
echo "${V}" >>"${T}/runs"
echo "fake Verso ${V} running"
b=""
if [ -s "${T}/queue-${V}" ]; then
    b="$(head -n 1 "${T}/queue-${V}")"
    sed -i 1d "${T}/queue-${V}"
fi
[ -n "${b}" ] || b="$(cat "${T}/default-${V}" 2>/dev/null)"
case "${b:-ok}" in
    ok) : >"${VERSO_STARTED_MARKER}"; exit 0 ;;
    stage) VERSO_TEST_STAGE=1 ${VERSO_TEST_SHELL} "${VERSO_TEST_SCRIPT}"; : >"${VERSO_STARTED_MARKER}"; exit 85 ;;
    update) : >"${VERSO_STARTED_MARKER}"; exit 85 ;;
    goback) : >"${VERSO_STARTED_MARKER}"; exit 86 ;;
    restart) echo "${VERSO_LAUNCHER_PROTOCOL:-}" >>"${T}/protocol"; : >"${VERSO_STARTED_MARKER}"; exit 87 ;;
    fail) echo "Error: opening the framebuffer" >&2; exit 3 ;;
    unsupported) echo "this Kindle isn't supported yet: 32 bpp" >&2; exit 88 ;;
    crash) kill -SEGV $$ ;;
    panic)
        echo "thread 'main' panicked at crates/verso/src/main.rs:12:5:" >&2
        echo 'called `Option::unwrap()` on a "None" value \ here' >&2
        kill -ABRT $$ ;;
    hang) exec sleep 600 ;;
    powerloss) kill -KILL 0 ;;
esac
EOF
}

# The fake Kindle commands, and the counting wrappers.
make_bin() {
    B="${W}/bin"
    rm -rf "${B}"
    mkdir -p "${B}"
    cat >"${B}/pidof" <<'EOF'
#!/bin/sh
if [ "$1" = "sh_integration_launcher" ] && [ -f "${VERSO_TEST_DIR}/sh-active" ]; then
    echo 4242
    exit 0
fi
exit 1
EOF
    cat >"${B}/lipc-get-prop" <<'EOF'
#!/bin/sh
if [ "$1" = "com.lab126.appmgrd" ] && [ "$2" = "activeApp" ]; then
    if [ -f "${VERSO_TEST_DIR}/sh-active" ]; then
        echo com.notmarek.shell_integration.launcher
    else
        echo com.lab126.KPPMainApp
    fi
    exit 0
fi
echo active
EOF
    for c in killall lipc-set-prop eips; do
        printf '#!/bin/sh\necho "%s $*" >>"${VERSO_TEST_DIR}/calls"\n' "${c}" >"${B}/${c}"
    done
    # A firewall that keeps its rules in a file, one per line.
    cat >"${B}/iptables" <<'EOF'
#!/bin/sh
echo "iptables $*" >>"${VERSO_TEST_DIR}/calls"
f="${VERSO_TEST_DIR}/rules"
op="$1"
shift
touch "${f}"
case "${op}" in
    -A) echo "$*" >>"${f}" ;;
    -C) grep -qxF -- "$*" "${f}" ;;
    -D)
        grep -qxF -- "$*" "${f}" || exit 1
        awk -v r="$*" 'd || $0 != r { print; next } { d = 1 }' "${f}" >"${f}.new"
        cat "${f}.new" >"${f}"
        ;;
    *) exit 2 ;;
esac
EOF
    for c in mv rm cp mkdir sync; do
        real="$(command -v "${c}")"
        if [ "${c}" = sync ] && [ -z "${REAL_SYNC:-}" ]; then
            finish="exit 0" # Don't sync this machine's disks hundreds of times.
        else
            finish="exec ${real} \"\$@\""
        fi
        cat >"${B}/${c}" <<EOF
#!/bin/sh
if [ "\${COUNT_STEPS:-}" = 1 ] && [ -z "\${VERSO_TEST_STAGE:-}" ]; then
    count=0
    if [ "\${PPID}" = "\${VERSO_LAUNCHER_PID:-}" ]; then
        count=1
    else
        case "\$(tr '\\0' ' ' </proc/\${PPID}/cmdline 2>/dev/null)" in
            */verso-launcher-[0-9]*.sh*) count=1 ;;
        esac
    fi
    if [ "\${count}" = 1 ]; then
        T="\${VERSO_TEST_DIR}"
        read -r n <"\${T}/steps" 2>/dev/null || n=0
        n=\$((n + 1))
        echo "\${n}" >"\${T}/steps"
        if [ "\${n}" = "\${CRASH_AT:-}" ]; then
            echo "power loss before step \${n}: ${c} \$*" >>"\${T}/calls"
            kill -KILL 0
        fi
    fi
fi
${finish}
EOF
    done
    chmod 755 "${B}"/*
}

# The files of version 1 (0.1.0) or 2 (0.2.0) as installed under $1. Version
# 2 changes the app, the launcher, a font, the KUAL menu and the library
# scriptlet, removes a font, adds one, and turns the hyph folder into a file.
install_files() {
    d="$1"
    mkdir -p "${d}/verso/fonts" "${d}/verso/cre" "${d}/verso/tools" "${d}/extensions/verso" "${d}/documents"
    echo "user settings" >"${d}/verso/settings.json"
    echo "a book" >"${d}/documents/book.epub"
    echo "user font" >"${d}/verso/fonts/Mine.ttf"
    if [ -f "${HERE}/../dist/kindle/verso/tools/report.sh" ]; then
        cp "${HERE}/../dist/kindle/verso/tools/report.sh" "${d}/verso/tools/report.sh"
        chmod 755 "${d}/verso/tools/report.sh"
    fi
    if [ "$2" = 1 ]; then
        make_app 0.1.0 >"${d}/verso/verso"
        cp "${W}/launcher-1.sh" "${d}/verso/verso.sh"
        echo A1 >"${d}/verso/fonts/A.ttf"
        echo B1 >"${d}/verso/fonts/B.ttf"
        mkdir -p "${d}/verso/cre/hyph"
        echo pattern >"${d}/verso/cre/hyph/a.pat"
        echo menu1 >"${d}/extensions/verso/menu.json"
        echo scriptlet1 >"${d}/documents/Verso.sh"
        echo "installed 0.1.0" >"${d}/verso/installed.json"
    else
        make_app 0.2.0 >"${d}/verso/verso"
        cp "${W}/launcher-2.sh" "${d}/verso/verso.sh"
        echo A2 >"${d}/verso/fonts/A.ttf"
        echo C2 >"${d}/verso/fonts/C.ttf"
        echo hyph2 >"${d}/verso/cre/hyph"
        echo menu2 >"${d}/extensions/verso/menu.json"
        echo scriptlet2 >"${d}/documents/Verso.sh"
        echo "installed 0.2.0" >"${d}/verso/installed.json"
    fi
    chmod 755 "${d}/verso/verso" "${d}/verso/verso.sh" "${d}/documents/Verso.sh"
}

# Version 2, staged the way Verso stages it (crates/verso-update/src/stage.rs).
stage_v2() {
    s="${U}/staged"
    mkdir -p "${s}/app" "${s}/launcher" "${s}/fonts" "${s}/crengine-data" "${s}/kual" "${s}/scriptlet"
    make_app 0.2.0 >"${s}/app/verso"
    cp "${W}/launcher-2.sh" "${s}/launcher/verso.sh"
    echo scriptlet2 >"${s}/scriptlet/Verso.sh"
    chmod 755 "${s}/app/verso" "${s}/launcher/verso.sh" "${s}/scriptlet/Verso.sh"
    echo A2 >"${s}/fonts/A.ttf"
    echo C2 >"${s}/fonts/C.ttf"
    echo hyph2 >"${s}/crengine-data/hyph"
    echo menu2 >"${s}/kual/menu.json"
    echo "installed 0.2.0" >"${s}/installed.json"
    {
        printf 'verso-plan\t1\nversion\t0.2.0\nfrom\t0.1.0\n'
        printf 'remove\tverso/cre/hyph/a.pat\nremove\tverso/fonts/B.ttf\n'
        p="verso/update/staged"
        printf 'install\t%s/app/verso\tverso/verso\n' "${p}"
        printf 'replace\t%s/launcher/verso.sh\tverso/verso.sh\n' "${p}"
        printf 'install\t%s/fonts/A.ttf\tverso/fonts/A.ttf\n' "${p}"
        printf 'install\t%s/fonts/C.ttf\tverso/fonts/C.ttf\n' "${p}"
        printf 'install\t%s/crengine-data/hyph\tverso/cre/hyph\n' "${p}"
        printf 'replace\t%s/kual/menu.json\textensions/verso/menu.json\n' "${p}"
        printf 'replace\t%s/scriptlet/Verso.sh\tdocuments/Verso.sh\n' "${p}"
        printf 'install\t%s/installed.json\tverso/installed.json\n' "${p}"
    } >"${s}/plan.txt"
    echo '{"format":1}' >"${s}/staged.json"
}

if [ -n "${VERSO_TEST_STAGE:-}" ]; then
    stage_v2
    exit
fi

# Every file under $1 with its checksum, leaving out the log, update/ and reports/.
snapshot() {
    (
        cd "$1" || exit 1
        files="$(find . -type f ! -path './verso/update/*' ! -path './verso/reports/*' ! -name 'verso.log*' | sort)"
        [ -z "${files}" ] || printf '%s\n' "${files}" | xargs cksum
    )
}

# A fresh pretend Kindle with version 1 installed; $1 = 2 for version 2
# installed by an update (previous/ holding version 1's files).
fresh() {
    rm -rf "${R}" "${W}/tmp" "${W}/runs" "${W}/calls" "${W}/protocol" "${W}/steps" "${W}/rules" "${W}/rules.new" "${W}/sh-active" "${W}"/queue-* \
        "${W}"/default-*
    mkdir -p "${R}" "${W}/tmp"
    : >"${W}/runs"
    : >"${W}/calls"
    install_files "${R}" 1
    printf 'framebuffer' >"${W}/fb"
    : >"${W}/ld-linux-armhf.so.3"
}

# Runs the installed launcher like KUAL does, in its own process group;
# CRASH_AT=$1 loses power at that step. ARMHF names the hard-float loader
# (a stand-in file that fresh makes).
launch() {
    : >"${W}/steps"
    (
        COUNT_STEPS="${COUNT_STEPS:-}" CRASH_AT="${1:-}" VERSO_TEST_DIR="${W}" VERSO_TEST_SCRIPT="${SELF}" VERSO_TEST_SHELL="${SH}" \
            VERSO_STORAGE_ROOT="${R}" VERSO_TMP="${W}/tmp" VERSO_FB="${W}/fb" VERSO_LAUNCHER_BIN="${W}/bin" \
            VERSO_LAUNCHER_SHELL="${SH}" VERSO_START_TIMEOUT=2 VERSO_REFUSAL_WAIT=0 \
            VERSO_ARMHF_LOADER="${ARMHF:-${W}/ld-linux-armhf.so.3}" \
            setsid ${SH} "${R}/verso/verso.sh" </dev/null >/dev/null 2>&1 &
        wait $!
    ) 2>/dev/null
}

# Runs the library scriptlet (documents/Verso.sh) like SH_Integration does.
# Verso.sh detaches and returns immediately; call wait_scriptlet to wait for
# the launched verso.sh to complete.
launch_scriptlet() {
    cp "${SCRIPTLET}" "${R}/documents/Verso.sh"
    chmod 755 "${R}/documents/Verso.sh"
    (
        VERSO_TEST_DIR="${W}" VERSO_TEST_SCRIPT="${SELF}" VERSO_TEST_SHELL="${SH}" \
            VERSO_STORAGE_ROOT="${R}" VERSO_TMP="${W}/tmp" VERSO_FB="${W}/fb" VERSO_LAUNCHER_BIN="${W}/bin" \
            VERSO_LAUNCHER_SHELL="${SH}" VERSO_START_TIMEOUT=2 VERSO_REFUSAL_WAIT=0 \
            VERSO_ARMHF_LOADER="${ARMHF:-${W}/ld-linux-armhf.so.3}" \
            ${SH} "${R}/documents/Verso.sh" </dev/null >/dev/null 2>&1
    )
}

# Waits until verso.sh started by launch_scriptlet has run and finished restoring.
wait_scriptlet() {
    want_runs="${1:-1}"
    i=0
    while [ "${i}" -lt 80 ]; do
        got_runs="$(wc -l <"${W}/runs" | tr -d ' ')"
        if [ "${got_runs}" -ge "${want_runs}" ] &&
            [ ! -e "${W}/tmp/verso-launcher.lock" ] &&
            [ ! -e "${W}/tmp/verso-launcher.pid" ] &&
            [ -z "$(ls "${W}/tmp"/verso-launcher-*.sh 2>/dev/null)" ]; then
            return 0
        fi
        usleep 100000 2>/dev/null || sleep 0.1 2>/dev/null || sleep 1
        i=$((i + 1))
    done
    return 1
}

# A reboot after a power loss: /tmp is empty again.
reboot() {
    rm -rf "${W}/tmp"
    mkdir -p "${W}/tmp"
}

# --- Checks --------------------------------------------------------------------

fail() {
    echo "FAIL [${CASE}]: $*"
    FAILED=$((FAILED + 1))
}

begin() {
    CASE="$1"
    CASES=$((CASES + 1))
}

runs_are() {
    got="$(tr '\n' ' ' <"${W}/runs" | sed 's/ $//')"
    [ "${got}" = "$1" ] || fail "runs: expected '$1', got '${got}'"
}

installed_is() {
    snapshot "${R}" >"${W}/got.snap"
    if ! cmp -s "${W}/got.snap" "${W}/expect-$1.snap"; then
        fail "installed files aren't version $1:"
        diff "${W}/expect-$1.snap" "${W}/got.snap" | sed 's/^/    /' | head -n 20
    fi
}

absent() {
    for p in "$@"; do
        [ ! -e "${p}" ] || fail "${p#"${W}"/} should be gone"
    done
}

present() {
    for p in "$@"; do
        [ -e "${p}" ] || fail "${p#"${W}"/} is missing"
    done
}

no_update_state() {
    absent "${U}/journal-install" "${U}/journal-rollback" "${U}/journal-cleanup" "${U}/trial" \
        "${U}/staged" "${U}/rollback.json.new"
}

# The previous/ folder holds exactly version 1's replaced and removed files.
previous_is_v1() {
    present "${U}/previous/plan.txt"
    (
        cd "${U}/previous/files" 2>/dev/null || exit 0
        files="$(find . -type f | sort)"
        [ -z "${files}" ] || printf '%s\n' "${files}" | xargs cksum
    ) >"${W}/got-prev.snap"
    cmp -s "${W}/got-prev.snap" "${W}/expect-prev.snap" || {
        fail "previous/ isn't version 1's replaced files:"
        diff "${W}/expect-prev.snap" "${W}/got-prev.snap" | sed 's/^/    /' | head -n 20
    }
}

rollback_says() {
    f="${U}/rollback.json"
    [ -f "${f}" ] || {
        fail "no rollback.json"
        return
    }
    for want in "$@"; do
        grep -qF -- "${want}" "${f}" || fail "rollback.json lacks ${want}: $(cat "${f}")"
    done
}

ui_restored_once() {
    n="$(grep -c 'appmgrd start' "${W}/calls")"
    [ "${n}" = 1 ] || fail "the Kindle UI came back ${n} times"
    n="$(grep -c 'killall -STOP awesome' "${W}/calls")"
    [ "${n}" = 1 ] || fail "the Kindle UI was paused ${n} times"
    grep -q 'killall -CONT volumd' "${W}/calls" || fail "volumd wasn't resumed"
}

# --- Setup ---------------------------------------------------------------------

cp "${LAUNCHER}" "${W}/launcher-1.sh"
# Version 2's launcher says so in the log.
sed '/^trap restore EXIT$/a\
log "launcher version 2"' "${LAUNCHER}" >"${W}/launcher-2.sh"
grep -q 'launcher version 2' "${W}/launcher-2.sh" || {
    echo "couldn't make the second launcher" >&2
    exit 1
}
make_bin
fresh
snapshot "${R}" >"${W}/expect-1.snap"
rm -rf "${W}/v2" && install_files "${W}/v2" 2
snapshot "${W}/v2" >"${W}/expect-2.snap"
(
    mkdir -p "${W}/prev/verso/fonts" "${W}/prev/verso/cre/hyph" "${W}/prev/extensions/verso" "${W}/prev/documents"
    cd "${W}/prev" || exit 1
    make_app 0.1.0 >verso/verso
    cp "${W}/launcher-1.sh" verso/verso.sh
    echo scriptlet1 >documents/Verso.sh
    chmod 755 verso/verso verso/verso.sh documents/Verso.sh
    echo A1 >verso/fonts/A.ttf
    echo B1 >verso/fonts/B.ttf
    echo pattern >verso/cre/hyph/a.pat
    echo menu1 >extensions/verso/menu.json
    echo "installed 0.1.0" >verso/installed.json
    find . -type f | sort | xargs cksum
) >"${W}/expect-prev.snap"
rm -rf "${W}/prev" "${W}/v2"

echo "==> launcher tests in ${W} with ${SH}"

# --- Cases ---------------------------------------------------------------------

begin "plain start"
fresh
launch
runs_are "0.1.0"
installed_is 1
ui_restored_once
no_update_state
! grep -q 'iptables\|preventScreenSaver' "${W}/calls" || fail "touched the firewall or keep-awake"
grep -q "Verso ended: bye (code 0)" "${R}/verso/verso.log" || fail "clean exit not logged as bye"
[ -z "$(ls "${R}/verso/reports"/crash-*.txt 2>/dev/null)" ] || fail "clean exit created a crash report"

if [ -f "${SCRIPTLET}" ]; then
    begin "scriptlet headers and SH_Integration handoff"
    fresh
    # readScriptHeader in SH_Integration stops after 6 lines.
    hdr="$(head -n 6 "${SCRIPTLET}")"
    printf '%s\n' "${hdr}" | grep -qx "# Name: Verso" || fail "missing '# Name: Verso' in first 6 lines"
    printf '%s\n' "${hdr}" | grep -qx "# Author: Verso" || fail "missing '# Author: Verso' in first 6 lines"
    printf '%s\n' "${hdr}" | grep -qx "# DontUseFBInk" || fail "missing '# DontUseFBInk' in first 6 lines"
    printf '%s\n' "${hdr}" | grep -q "^# Icon: data:image/png;base64," || fail "missing '# Icon:' data URI in first 6 lines"
    : >"${W}/sh-active"
    launch_scriptlet || fail "Verso.sh exited non-zero"
    usleep 250000 2>/dev/null || sleep 0.25 2>/dev/null || sleep 1
    [ ! -s "${W}/calls" ] || fail "verso.sh paused Kindle UI before SH_Integration unloaded: $(cat "${W}/calls")"
    rm -f "${W}/sh-active"
    wait_scriptlet 1 || fail "scriptlet launch did not finish"
    runs_are "0.1.0"
    ui_restored_once
    absent "${W}/tmp/verso-launcher.lock" "${W}/tmp/verso-launcher.pid"
    [ -z "$(ls "${W}/tmp"/verso-launcher* 2>/dev/null)" ] || fail "leftover launcher tmp files: $(ls "${W}/tmp"/verso-launcher*)"

    begin "scriptlet repeated launches leave clean state"
    fresh
    for i in 1 2 3; do
        launch_scriptlet || fail "Verso.sh failed on run ${i}"
        wait_scriptlet "${i}" || fail "scriptlet run ${i} did not finish"
    done
    runs_are "0.1.0 0.1.0 0.1.0"
    [ "$(grep -c 'killall -STOP awesome' "${W}/calls")" = 3 ] || fail "expected 3 UI pauses"
    [ "$(grep -c 'appmgrd start' "${W}/calls")" = 3 ] || fail "expected 3 UI restores"
    [ "$(grep -c 'killall -CONT volumd' "${W}/calls")" = 3 ] || fail "expected 3 volumd resumes"
    absent "${W}/tmp/verso-launcher.lock" "${W}/tmp/verso-launcher.pid"
    [ -z "$(ls "${W}/tmp"/verso-launcher* 2>/dev/null)" ] || fail "leftover launcher tmp files: $(ls "${W}/tmp"/verso-launcher*)"
fi

begin "crash report on signal exit"
fresh
echo crash >"${W}/queue-0.1.0"
launch
runs_are "0.1.0"
ui_restored_once
grep -q "Verso ended: crash (SIGSEGV, code 139)" "${R}/verso/verso.log" || fail "SIGSEGV not logged by name"
crash_file="$(ls "${R}/verso/reports"/crash-*.txt 2>/dev/null | head -n 1)"
[ -n "${crash_file}" ] || fail "no crash report created on SIGSEGV"
grep -q "Ended: crash (SIGSEGV, code 139)" "${crash_file}" || fail "crash report missing Ended line"

begin "crash report on panic exit"
fresh
echo panic >"${W}/queue-0.1.0"
launch
runs_are "0.1.0"
ui_restored_once
grep -q "Verso ended: panic (SIGABRT, code 134)" "${R}/verso/verso.log" || fail "panic not classified"
crash_file="$(ls "${R}/verso/reports"/crash-*.txt 2>/dev/null | head -n 1)"
[ -n "${crash_file}" ] || fail "no crash report created on panic"
grep -q "Ended: panic (SIGABRT, code 134)" "${crash_file}" || fail "panic report missing Ended line"
grep -q "panicked at" "${crash_file}" || fail "panic report missing panic message"

begin "crash report rotation keeps newest 5"
fresh
for i in 1 2 3 4 5 6 7; do
    echo crash >"${W}/queue-0.1.0"
    launch
done
n_reports="$(ls "${R}/verso/reports"/crash-*.txt 2>/dev/null | wc -l | tr -d ' ')"
[ "${n_reports}" = 5 ] || fail "expected 5 crash reports after 7 crashes, got ${n_reports}"

begin "soft-float firmware is refused before pausing the Kindle UI"
fresh
ARMHF="${W}/no-such-loader" launch
runs_are ""
! grep -q 'killall -STOP\|appmgrd' "${W}/calls" || fail "touched the Kindle UI: $(cat "${W}/calls")"
grep -q "eips .*Verso does not support this Kindle firmware yet" "${W}/calls" || fail "no message on screen"
grep -q "no hard-float loader" "${R}/verso/verso.log" || fail "refusal not logged"
report_file="$(ls "${R}/verso/reports"/report-*.txt 2>/dev/null | head -n 1)"
[ -n "${report_file}" ] || fail "no problem report written"
absent "${W}/tmp/verso-launcher.lock" "${W}/tmp/verso-launcher.pid"

begin "unsupported screen: problem report, then the Kindle UI"
fresh
echo unsupported >"${W}/queue-0.1.0"
launch
runs_are "0.1.0"
ui_restored_once
grep -q "Verso ended: unsupported (code 88)" "${R}/verso/verso.log" || fail "unsupported exit not logged"
report_file="$(ls "${R}/verso/reports"/report-*.txt 2>/dev/null | head -n 1)"
[ -n "${report_file}" ] || fail "no problem report written"
grep -q "isn't supported yet: 32 bpp" "${report_file}" || fail "report lacks Verso's reason"
[ -z "$(ls "${R}/verso/reports"/crash-*.txt 2>/dev/null)" ] || fail "unsupported exit counted as a crash"

begin "Wi-Fi transfer left on"
fresh
# Port 80 opened twice (a rule piled up), and SSH's port 22, which stays.
open80="INPUT -p tcp --dport 80 -m conntrack --ctstate NEW,ESTABLISHED -j ACCEPT"
printf '%s\n' "${open80}" "OUTPUT -p tcp --sport 80 -m conntrack --ctstate ESTABLISHED -j ACCEPT" "${open80}" \
    "INPUT -p tcp --dport 22 -m conntrack --ctstate NEW,ESTABLISHED -j ACCEPT" >"${W}/rules"
: >"${W}/tmp/verso-transfer-firewall"
echo crash >"${W}/queue-0.1.0"
launch
runs_are "0.1.0"
ui_restored_once
absent "${W}/tmp/verso-transfer-firewall"
! grep -q 'preventScreenSaver' "${W}/calls" || fail "touched keep-awake"
[ "$(cat "${W}/rules")" = "INPUT -p tcp --dport 22 -m conntrack --ctstate NEW,ESTABLISHED -j ACCEPT" ] ||
    fail "firewall rules left: $(cat "${W}/rules")"

begin "update"
fresh
echo stage >"${W}/queue-0.1.0"
launch
runs_are "0.1.0 0.2.0"
installed_is 2
previous_is_v1
no_update_state
absent "${U}/rollback.json"
ui_restored_once
grep -q "installed Verso 0.2.0" "${R}/verso/verso.log" || fail "no 'installed' line in the log"
grep -q "launcher version 2" "${R}/verso/verso.log" && fail "the new launcher ran in the same session"
# The next start runs the new launcher, and nothing more is installed.
: >"${W}/runs"
launch
runs_are "0.2.0"
grep -q "launcher version 2" "${R}/verso/verso.log" || fail "the new launcher didn't take over"
installed_is 2
previous_is_v1

begin "staged update left over"
fresh
stage_v2
launch
runs_are "0.2.0"
installed_is 2
no_update_state

begin "crash before the marker"
fresh
echo stage >"${W}/queue-0.1.0"
echo crash >"${W}/default-0.2.0"
launch
runs_are "0.1.0 0.2.0 0.1.0"
installed_is 1
no_update_state
absent "${U}/previous"
rollback_says '"from":"0.2.0"' '"to":"0.1.0"' '"reason":"exit"' '"status":139' 'fake Verso 0.2.0 running'
ui_restored_once

begin "rollback removes first-time scriptlet addition"
fresh
rm -f "${R}/documents/Verso.sh"
echo stage >"${W}/queue-0.1.0"
echo crash >"${W}/default-0.2.0"
launch
runs_are "0.1.0 0.2.0 0.1.0"
absent "${R}/documents/Verso.sh" "${U}/previous"
no_update_state
rollback_says '"from":"0.2.0"' '"to":"0.1.0"' '"reason":"exit"'
[ "$(cat "${R}/verso/installed.json")" = "installed 0.1.0" ] || fail "installed.json wasn't restored"
present "${R}/documents/book.epub"

begin "panic"
fresh
echo stage >"${W}/queue-0.1.0"
echo panic >"${W}/default-0.2.0"
launch
runs_are "0.1.0 0.2.0 0.1.0"
installed_is 1
rollback_says '"status":134' "panicked at crates/verso/src/main.rs:12:5:" '\"None\" value \\ here'

begin "error exit"
fresh
echo stage >"${W}/queue-0.1.0"
echo fail >"${W}/default-0.2.0"
launch
runs_are "0.1.0 0.2.0 0.1.0"
installed_is 1
rollback_says '"status":3' 'opening the framebuffer'

begin "hang"
fresh
echo stage >"${W}/queue-0.1.0"
echo hang >"${W}/default-0.2.0"
launch
runs_are "0.1.0 0.2.0 0.1.0"
installed_is 1
no_update_state
rollback_says '"reason":"hang"'
ui_restored_once

begin "power loss twice on trial"
fresh
echo stage >"${W}/queue-0.1.0"
echo powerloss >"${W}/default-0.2.0"
launch
reboot
[ "$(cat "${U}/trial" 2>/dev/null)" = 1 ] || fail "first attempt not counted"
launch
reboot
[ "$(cat "${U}/trial" 2>/dev/null)" = 2 ] || fail "second attempt not counted"
launch
runs_are "0.1.0 0.2.0 0.2.0 0.1.0"
installed_is 1
no_update_state
rollback_says '"reason":"interrupted"' '"from":"0.2.0"'

begin "power loss once on trial"
fresh
echo stage >"${W}/queue-0.1.0"
echo powerloss >"${W}/queue-0.2.0"
launch
reboot
launch
runs_are "0.1.0 0.2.0 0.2.0"
installed_is 2
no_update_state
previous_is_v1

begin "go back"
fresh
echo stage >"${W}/queue-0.1.0"
echo goback >"${W}/queue-0.2.0"
launch
runs_are "0.1.0 0.2.0 0.1.0"
installed_is 1
no_update_state
absent "${U}/previous"
rollback_says '"reason":"requested"' '"from":"0.2.0"' '"to":"0.1.0"'
ui_restored_once
# Nothing left to go back to: Verso exits and the Kindle UI comes back.
: >"${W}/runs"
: >"${W}/calls"
echo goback >"${W}/queue-0.1.0"
launch
runs_are "0.1.0"
installed_is 1
ui_restored_once

begin "restart to apply"
fresh
printf 'restart\nrestart\n' >"${W}/queue-0.1.0"
launch
runs_are "0.1.0 0.1.0 0.1.0"
installed_is 1
no_update_state
ui_restored_once
[ "$(tr '\n' ' ' <"${W}/protocol")" = "2 2 " ] || fail "protocol told: $(cat "${W}/protocol")"
grep -q "restarting Verso" "${R}/verso/verso.log" || fail "no 'restarting' line in the log"

begin "restart doesn't loop"
fresh
echo restart >"${W}/default-0.1.0"
launch
runs_are "0.1.0 0.1.0 0.1.0 0.1.0 0.1.0 0.1.0 0.1.0 0.1.0"
installed_is 1
ui_restored_once

begin "restart after an update"
fresh
echo stage >"${W}/queue-0.1.0"
echo restart >"${W}/queue-0.2.0"
launch
runs_are "0.1.0 0.2.0 0.2.0"
installed_is 2
no_update_state
previous_is_v1
absent "${U}/rollback.json"
ui_restored_once

begin "update code with nothing staged"
fresh
echo update >"${W}/queue-0.1.0"
launch
runs_are "0.1.0"
installed_is 1
ui_restored_once

begin "unreadable plan"
fresh
stage_v2
printf 'verso-plan\t9\n' >"${U}/staged/plan.txt"
launch
runs_are "0.1.0"
installed_is 1
absent "${U}/staged"

begin "hand install clears old update state"
fresh
stage_v2
mkdir -p "${U}/previous" "${U}/download"
printf 'verso-plan\t1\nversion\t0.1.0\nfrom\t0.0.9\n' >"${U}/previous/plan.txt"
printf '2\n' >"${U}/trial"
printf '{"format":1,"from":"0.2.0","to":"0.1.0","reason":"hang","status":null,"time":0,"log":[]}\n' >"${U}/rollback.json"
: >"${U}/journal-install"
: >"${U}/hand-install"
launch
runs_are "0.1.0"
installed_is 1
no_update_state
absent "${U}/previous" "${U}/download" "${U}/rollback.json" "${U}/hand-install"
ui_restored_once

if [ "$(id -u)" != 0 ]; then
    begin "install fails halfway"
    fresh
    echo stage >"${W}/queue-0.1.0"
    chmod 555 "${R}/extensions/verso"
    launch
    chmod 755 "${R}/extensions/verso"
    runs_are "0.1.0 0.1.0"
    installed_is 1
    no_update_state
    absent "${U}/previous"
    rollback_says '"reason":"install"' '"from":"0.2.0"'
else
    echo "    (skipping 'install fails halfway': root ignores folder permissions)"
fi

# Power loss before every step of a job, then a normal start: the job is
# finished (or undone) and nothing is lost.
sweep_step() {
    name="$1"
    setup="$2"
    verify="$3"
    n="$4"
    total="$5"
    begin "power loss: ${name}, before step ${n} of ${total}"
    fresh
    ${setup}
    COUNT_STEPS=1 launch "${n}"
    # KUAL and the library scriptlet can always start the launcher, which finishes the job.
    present "${R}/verso/verso.sh" "${R}/extensions/verso/menu.json" "${R}/documents/Verso.sh"
    reboot
    launch
    ${verify}
}

sweep() {
    name="$1"
    setup="$2"
    verify="$3"
    case "${name}" in *"${SWEEP:-}"*) ;; *) return ;; esac
    fresh
    ${setup}
    COUNT_STEPS=1 launch
    total="$(cat "${W}/steps")"
    if [ "${total:-0}" -lt 5 ]; then
        begin "power loss: ${name}"
        fail "only ${total:-0} launcher steps were counted; can this shell's mv be wrapped?"
        return
    fi
    if [ "${JOBS}" -le 1 ]; then
        n="${STEP_FROM:-1}"
        while [ "${n}" -le "$((total + 1))" ]; do
            sweep_step "${name}" "${setup}" "${verify}" "${n}" "${total}"
            n=$((n + STEP_EVERY))
        done
    else
        w_parent="${W}"
        pids=""
        j=0
        while [ "${j}" -lt "${JOBS}" ]; do
            (
                W="${w_parent}/w-${j}"
                R="${W}/root"
                U="${R}/verso/update"
                mkdir -p "${W}"
                cp "${w_parent}/launcher-1.sh" "${w_parent}/launcher-2.sh" \
                    "${w_parent}/expect-1.snap" "${w_parent}/expect-2.snap" \
                    "${w_parent}/expect-prev.snap" "${W}/"
                make_bin
                CASES=0
                FAILED=0
                n=$((1 + j * STEP_EVERY))
                stride=$((JOBS * STEP_EVERY))
                while [ "${n}" -le "$((total + 1))" ]; do
                    sweep_step "${name}" "${setup}" "${verify}" "${n}" "${total}"
                    n=$((n + stride))
                done
                printf '%s %s\n' "${CASES}" "${FAILED}" >"${w_parent}/res-${j}"
            ) >"${w_parent}/out-${j}" 2>&1 &
            pids="${pids} $!"
            j=$((j + 1))
        done
        for p in ${pids}; do
            wait "${p}"
        done
        j=0
        while [ "${j}" -lt "${JOBS}" ]; do
            [ -s "${w_parent}/out-${j}" ] && cat "${w_parent}/out-${j}"
            if read -r c f <"${w_parent}/res-${j}" 2>/dev/null; then
                CASES=$((CASES + c))
                FAILED=$((FAILED + f))
            else
                fail "worker ${j} exited without reporting results"
            fi
            j=$((j + 1))
        done
        rm -rf "${w_parent}"/w-* "${w_parent}"/res-* "${w_parent}"/out-*
    fi
    echo "    ${name}: ${total} steps"
}

setup_update() {
    echo stage >"${W}/queue-0.1.0"
}
verify_update() {
    installed_is 2
    no_update_state
    previous_is_v1
    tail -n 1 "${W}/runs" | grep -qx "0.2.0" || fail "0.2.0 didn't run last: $(tr '\n' ' ' <"${W}/runs")"
}
sweep "install" setup_update verify_update

setup_crash() {
    setup_update
    echo crash >"${W}/default-0.2.0"
}
verify_crash() {
    installed_is 1
    no_update_state
    absent "${U}/previous"
    rollback_says '"from":"0.2.0"' '"to":"0.1.0"'
    tail -n 1 "${W}/runs" | grep -qx "0.1.0" || fail "0.1.0 didn't run last: $(tr '\n' ' ' <"${W}/runs")"
}
sweep "rollback after a crash" setup_crash verify_crash

setup_go_back() {
    setup_update
    echo goback >"${W}/default-0.2.0"
}
verify_go_back() {
    installed_is 1
    no_update_state
    absent "${U}/previous"
    rollback_says '"from":"0.2.0"'
}
sweep "go back" setup_go_back verify_go_back

echo "==> ${CASES} cases, ${FAILED} failures"
if [ "${FAILED}" = 0 ] && [ -z "${KEEP:-}" ]; then
    rm -rf "${W}"
fi
[ "${FAILED}" = 0 ]
