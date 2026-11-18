#!/usr/bin/env bash
# Keeps the Kindle from going to sleep on its own, so SSH stays reachable
# while an agent works on the device unattended.
#
#   scripts/kindle-awake.sh on       keep the Kindle awake (it must be awake now)
#   scripts/kindle-awake.sh off      let it sleep on its idle timeout again
#   scripts/kindle-awake.sh status   show the power state and the setting
#
# It sets powerd's preventScreenSaver over lipc. That stops only the idle
# timeout: the power button and a sleep cover still put the Kindle to sleep.
# The setting goes back to 0 on reboot. A sleeping Kindle has Wi-Fi off and
# can't be woken over SSH, so turn this on before leaving it.
#
# The SSH host defaults to `kindle` (see docs/setup.md); override with VERSO_KINDLE_HOST.
set -euo pipefail

HOST="${VERSO_KINDLE_HOST:-kindle}"

usage() {
    sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'
}

kindle() {
    ssh -o ConnectTimeout=10 "$HOST" "$@"
}

status() {
    kindle 'echo "power state:        $(lipc-get-prop com.lab126.powerd state)"
        echo "preventScreenSaver: $(lipc-get-prop com.lab126.powerd preventScreenSaver)"'
}

case "${1:-}" in
    on)
        state="$(kindle 'lipc-get-prop com.lab126.powerd state')"
        if [ "$state" != active ]; then
            echo "The Kindle is not awake (power state: $state). Wake it up, then run this again." >&2
            exit 1
        fi
        kindle 'lipc-set-prop com.lab126.powerd preventScreenSaver 1'
        status
        ;;
    off)
        kindle 'lipc-set-prop com.lab126.powerd preventScreenSaver 0'
        status
        ;;
    status)
        status
        ;;
    "" | -h | --help)
        usage
        ;;
    *)
        echo "unknown command: $1" >&2
        echo >&2
        usage >&2
        exit 2
        ;;
esac
