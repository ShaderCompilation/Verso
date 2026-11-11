#!/bin/sh
# Portable, read-only hardware inventory. No Verso installation is required.
# Keep this compatible with older BusyBox shells: no Bash or GNU-only syntax.

LC_ALL=C
export LC_ALL
PATH="/usr/sbin:/usr/bin:/sbin:/bin:${PATH}"
export PATH

usage() {
    printf '%s\n' \
        'Usage: sh collect-diagnostics.sh [--output FILE]' \
        'Print an e-reader hardware report to stdout, or create FILE.' \
        'Existing files are never overwritten. No network access is used.'
}

output=
case $# in
    0) ;;
    1)
        case $1 in
            -h|--help) usage; exit 0 ;;
            *) usage >&2; exit 2 ;;
        esac
        ;;
    2)
        case $1 in
            --output) output=$2; [ -n "$output" ] || { usage >&2; exit 2; } ;;
            *) usage >&2; exit 2 ;;
        esac
        ;;
    *) usage >&2; exit 2 ;;
esac

# Filtering is mandatory: never fall back to dumping whole system files.
if ! command -v awk >/dev/null 2>&1; then
    printf '%s\n' 'Cannot collect diagnostics: awk is required (BusyBox awk is sufficient).' >&2
    exit 1
fi

if [ -n "$output" ]; then
    if [ -e "$output" ] || [ -L "$output" ]; then
        printf '%s\n' 'Cannot create report: output path already exists.' >&2
        exit 1
    fi
    umask 077
    set -C
    exec >"$output"
fi

# timeout syntax differs between old and new BusyBox versions. IPC queries
# are skipped without a working timeout, so an unresponsive daemon cannot hang us.
timeout_style=none
if command -v timeout >/dev/null 2>&1; then
    if timeout 1 sh -c 'exit 0' >/dev/null 2>&1; then
        timeout_style=seconds
    elif timeout -t 1 sh -c 'exit 0' >/dev/null 2>&1; then
        timeout_style=flag
    fi
fi

section() {
    printf '\n== %s ==\n' "$1"
}

# Remove control characters (including device-tree NULs), cap line length and
# line count. This does not replace the field allowlists below.
clean() {
    awk 'NR > 80 { print "[truncated]"; exit }
        { gsub(/[^[:print:]\t]/, "");
          print substr($0, 1, 256) (length($0) > 256 ? " [truncated]" : "") }'
}

field() {
    printf '%s: ' "$1"
    if [ -r "$2" ] && [ -f "$2" ]; then
        if value=$(awk 'NR > 8 { exit }
            { gsub(/[^[:print:]\t]/, "");
              print substr($0, 1, 256) (length($0) > 256 ? " [truncated]" : "") }' "$2" 2>/dev/null); then
            if [ -n "$value" ]; then
                printf '%s\n' "$value"
            else
                printf '%s\n' '[empty]'
            fi
        else
            printf '%s\n' '[read failed]'
        fi
    else
        printf '%s\n' '[unavailable or not readable]'
    fi
}

query() {
    label=$1
    shift
    printf '%s:\n' "$label"
    if ! command -v "$1" >/dev/null 2>&1; then
        printf '%s\n' '[command unavailable]'
        return
    fi
    case $timeout_style in
        seconds) value=$(timeout 3 "$@" 2>/dev/null); status=$? ;;
        flag) value=$(timeout -t 3 "$@" 2>/dev/null); status=$? ;;
        none) value=$("$@" 2>/dev/null); status=$? ;;
    esac
    if [ "$status" -eq 0 ]; then
        if [ -n "$value" ]; then
            printf '%s\n' "$value" | clean
        else
            printf '%s\n' '[empty]'
        fi
    else
        printf '[query failed or timed out; exit %s]\n' "$status"
    fi
}

bounded_query() {
    if [ "$timeout_style" = none ]; then
        printf '%s: [skipped: no working timeout]\n' "$1"
    else
        query "$@"
    fi
}

report() {
    printf '%s\n' \
        'Verso device diagnostics - format 1' \
        'Observed hardware/interfaces only; this does not certify Verso compatibility.' \
        'No books, account data, network settings, full serials, or system logs requested.' \
        'Review vendor-provided hardware names before sharing.'

    section 'System and CPU'
    query 'Kernel and architecture' uname -srm
    query 'Collector effective UID' id -u
    if [ -r /proc/cpuinfo ]; then
        # In particular, exclude Serial and Revision (which can encode identity).
        awk -F: '
            { key=$1; gsub(/^[ \t]+|[ \t]+$/, "", key) }
            key ~ /^(model name|Processor|Hardware|CPU architecture|CPU implementer|CPU variant|CPU part|CPU revision|Features|flags)$/ && !seen[key]++ {
                print
            }' /proc/cpuinfo 2>/dev/null | clean
    else
        printf '%s\n' '/proc/cpuinfo: [unavailable or not readable]'
    fi
    field 'Device-tree model' /sys/firmware/devicetree/base/model
    field 'Device-tree compatible' /sys/firmware/devicetree/base/compatible
    field 'SoC family' /sys/devices/soc0/family
    field 'SoC machine' /sys/devices/soc0/machine

    section 'Memory (kB)'
    if [ -r /proc/meminfo ]; then
        awk '/^(MemTotal|MemFree|MemAvailable|Buffers|Cached|SwapTotal|SwapFree):/ { print }' \
            /proc/meminfo 2>/dev/null | clean
    else
        printf '%s\n' '[unavailable or not readable]'
    fi

    section 'Userspace and ABI'
    query 'libc reported by getconf' getconf GNU_LIBC_VERSION
    # Read ELF bytes, never execute a loader or a library. The first 52 bytes
    # include class, byte order, machine and ARM ABI flags even on a 64-bit host.
    query '/bin/sh ELF header (hex; ABI evidence)' od -An -tx1 -N52 /bin/sh
    found=no
    for library in /lib/ld-*.so* /lib/ld-musl-*.so* /lib/libc.so* /lib/libc-*.so \
        /lib/*-linux-*/ld-*.so* /lib/*-linux-*/libc.so* /lib64/ld-*.so*; do
        [ -f "$library" ] || continue
        printf 'Loader/libc path: %s\n' "$library"
        found=yes
    done
    [ "$found" = yes ] || printf '%s\n' '[no loader/libc paths found in common locations]'

    section 'Framebuffer metadata (no screen pixels collected)'
    found=no
    for framebuffer in /sys/class/graphics/fb[0-9]*; do
        [ -d "$framebuffer" ] || continue
        found=yes
        printf '\n%s\n' "$framebuffer"
        for attribute in name virtual_size bits_per_pixel stride rotate modes; do
            field "$attribute" "$framebuffer/$attribute"
        done
    done
    [ "$found" = yes ] || printf '%s\n' '[no framebuffer sysfs entries found]'
    # virtual_size may include multiple buffers; fbset gives visible geometry.
    bounded_query 'Visible framebuffer geometry (fbset -s)' fbset -s

    section 'Input capabilities (no events read or devices grabbed)'
    if [ -r /proc/bus/input/devices ]; then
        # Omit Phys, Sysfs and Uniq: these can expose topology or unique IDs.
        awk '/^[INHB]:/ || /^$/ { print }' /proc/bus/input/devices 2>/dev/null | clean
    else
        printf '%s\n' '[input inventory unavailable or not readable]'
    fi

    section 'Backlight and LED interfaces (LEDs may be status lights)'
    found=no
    for light in /sys/class/backlight/* /sys/class/leds/*; do
        [ -d "$light" ] || continue
        found=yes
        printf '\n%s\n' "$light"
        for attribute in type brightness actual_brightness max_brightness bl_power; do
            field "$attribute" "$light/$attribute"
        done
    done
    [ "$found" = yes ] || printf '%s\n' '[no backlight/LED sysfs entries found]'

    section 'Battery and power interfaces'
    found=no
    for supply in /sys/class/power_supply/*; do
        [ -d "$supply" ] || continue
        found=yes
        printf '\n%s\n' "$supply"
        for attribute in type status capacity online present temp voltage_now; do
            field "$attribute" "$supply/$attribute"
        done
    done
    [ "$found" = yes ] || printf '%s\n' '[no power-supply sysfs entries found]'

    section 'Optional platform queries'
    if command -v lipc-get-prop >/dev/null 2>&1; then
        printf '%s\n' 'Kindle LIPC tool detected (inference; not a model identification).'
        for property in state flIntensity flMaxIntensity currentAmberLevel; do
            bounded_query "powerd $property" lipc-get-prop com.lab126.powerd "$property"
        done
    else
        printf '%s\n' 'Kindle LIPC: [unavailable]'
    fi
    if command -v getprop >/dev/null 2>&1; then
        for property in ro.product.manufacturer ro.product.model ro.product.device \
            ro.build.version.release ro.build.version.sdk; do
            bounded_query "Android $property" getprop "$property"
        done
    else
        printf '%s\n' 'Android property tool: [unavailable]'
    fi
    printf '%s\n' 'Marketing model / firmware: please supply separately if not identified above.'

    section 'Available tools'
    for tool in sh awk sed head od getconf timeout busybox fbset \
        fbink evtest lipc-get-prop lipc-wait-event getprop; do
        if command -v "$tool" >/dev/null 2>&1; then
            printf '%s: available\n' "$tool"
        else
            printf '%s: unavailable\n' "$tool"
        fi
    done
    printf 'Timeout mode: %s\n' "$timeout_style"
    section 'End of report'
}

report
