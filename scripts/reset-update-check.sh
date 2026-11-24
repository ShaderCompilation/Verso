#!/usr/bin/env bash
# Makes Verso on the Kindle forget its last update check, so the next start
# checks again at once (with Wi-Fi connected and "Check automatically" on).
#
#   scripts/reset-update-check.sh
#
# Empties update/state.json: the last check, the update it found, and any
# "Later" or "Skip this version". Nothing else is touched. Quit Verso first: a
# running one keeps its own copy and would write it back.
#
# The SSH host defaults to `kindle` (an SSH config alias); override with VERSO_KINDLE_HOST.
set -euo pipefail

case "${1:-}" in
    "") ;;
    *)
        sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'
        [ "$1" = -h ] || [ "$1" = --help ] || exit 2
        exit 0
        ;;
esac

ssh -o ConnectTimeout=10 "${VERSO_KINDLE_HOST:-kindle}" sh -s <<'EOF'
set -eu
V=/mnt/us/verso
STATE=$V/update/state.json
if pgrep -f "$V/verso( |\$)" >/dev/null; then
    echo "Verso is running; quit it first" >&2
    exit 1
fi
if [ ! -f "$V/installed.json" ] || grep -q '"dev": *true' "$V/installed.json"; then
    echo "note: this is a development install, which never checks automatically" >&2
fi
if [ -f "$STATE" ]; then
    echo "was: $(cat "$STATE")"
    echo '{}' >"$STATE.tmp"
    mv "$STATE.tmp" "$STATE"
    sync
fi
echo "The next start of Verso checks for updates."
EOF
