#!/usr/bin/env bash
# Runs the desktop simulator for any Kindle model, with the refresh
# visualisation (the coloured flash over what each refresh touched) on or off.
#
#   scripts/sim.sh                         the default model (pw6)
#   scripts/sim.sh -d colorsoft            a colour model, real colours on screen
#   scripts/sim.sh -d colorsoft -k         and the Kaleido 3 panel preview
#   scripts/sim.sh -d scribe -s 0.33       smaller window (VERSO_SIM_SCALE)
#   scripts/sim.sh -d pw5 -g settings      start on another screen
#   scripts/sim.sh -l                      list the models
#   scripts/sim.sh -- --screenshot out.png render headless instead of a window
#
# Keys in the window: Q/Esc quit · S sleep/wake · V refresh visualisation ·
# P screenshot · PgDn/PgUp page buttons · C Kaleido 3 preview (colour models).
# The visualisation starts on (red = Fast, blue = Ui, green = Page,
# orange = Full); V turns it off. It is a tool-only overlay: it never reaches
# the Kindle, a saved screenshot or the refresh-debt bookkeeping.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

DEVICES="basic-167 basic-300 pw5 pw6 oasis3 colorsoft scribe"

usage() {
    sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//'
}

die() {
    echo "$0: $*" >&2
    echo >&2
    usage >&2
    exit 2
}

device=
scale=
screen=
extra=()
while [ $# -gt 0 ]; do
    case "$1" in
        -d | --device)
            [ $# -ge 2 ] || die "$1 needs a model name (see --list)"
            device="$2"
            shift 2
            ;;
        -l | --list)
            echo "models: $DEVICES"
            exit 0
            ;;
        -k | --kaleido)
            export VERSO_SIM_KALEIDO=1
            shift
            ;;
        -s | --scale)
            [ $# -ge 2 ] || die "$1 needs a number, e.g. 0.5"
            scale="$2"
            shift 2
            ;;
        -g | --screen)
            [ $# -ge 2 ] || die "$1 needs a screen name"
            screen="$2"
            shift 2
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        --)
            shift
            extra=("$@")
            break
            ;;
        -*)
            die "unknown option $1"
            ;;
        *)
            die "unexpected argument $1 (put simulator flags after --)"
            ;;
    esac
done

if [ -n "$device" ]; then
    case " $DEVICES " in
        *" $device "*) export VERSO_SIM_DEVICE="$device" ;;
        *) die "unknown model $device" ;;
    esac
fi
if [ -n "$scale" ]; then
    export VERSO_SIM_SCALE="$scale"
fi

args=()
if [ -n "$screen" ]; then
    args+=(--screen "$screen")
fi
if [ ${#extra[@]} -gt 0 ]; then
    args+=("${extra[@]}")
fi

echo "==> verso simulator: ${device:-pw6} (Q/Esc quits, V toggles refresh visualisation)"
exec cargo run --quiet -p verso -- "${args[@]}"
