#!/bin/sh
# Portable, read-only hardware inventory. No Verso installation is required.
# Keep this compatible with older BusyBox shells: no Bash or GNU-only syntax.

LC_ALL=C
export LC_ALL
PATH="/usr/sbin:/usr/bin:/sbin:/bin:${PATH}"
export PATH

usage() {
    printf '%s\n' \
        'Usage: sh collect-diagnostics.sh [--output FILE | --identity]' \
        'Print an e-reader hardware report to stdout, or create FILE.' \
        'With --identity, print a brief 4-line model/firmware/kernel/libc summary.' \
        'Existing files are never overwritten. No network access is used.'
}

output=
mode=report
case $# in
    0) ;;
    1)
        case $1 in
            -h|--help) usage; exit 0 ;;
            --identity) mode=identity ;;
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

# Extract only the 2- or 3-character Kindle device type code from /proc/usid
# (never the trailing unique serial digits), decode its FBInk device ID and map
# known models (matching crates/verso/src/platform/kindle_profiles.rs).
kindle_model() {
    usid_file="${VERSO_USID_FILE:-/proc/usid}"
    if [ ! -r "$usid_file" ]; then
        printf '%s\n' '[unavailable]'
        return
    fi
    awk '
    BEGIN {
        b32 = "0123456789ABCDEFGHJKLMNPQRSTUVWX"
        hex = "0123456789ABCDEF"
        # Known Kindle model IDs (hex -> marketing name), matching kindle_profiles.rs.
        m["C89"]=m["C86"]=m["C7F"]=m["C7E"]=m["E2A"]=m["E25"]=m["E23"]=m["E28"]=m["E45"]=m["E5A"]="Kindle Paperwhite 6 (2024)"
        m["690"]=m["700"]=m["6FF"]=m["7AD"]=m["829"]=m["82A"]=m["971"]=m["972"]=m["9B3"]="Kindle Paperwhite 5 (2021)"
        m["E29"]=m["E24"]=m["E2B"]=m["E26"]=m["E22"]=m["C9F"]=m["E27"]=m["E5B"]=m["E46"]=m["10A6"]=m["10A5"]=m["11D7"]="Kindle Colorsoft (2024)"
        m["84D"]=m["8BB"]=m["86A"]=m["958"]=m["957"]=m["7F1"]=m["84C"]="Kindle Basic 4 (2022)"
        m["E85"]=m["E86"]=m["E84"]=m["E83"]=m["2909"]=m["E82"]=m["E75"]="Kindle Basic 5 (2024)"
        m["8F2"]=m["974"]=m["8C3"]=m["847"]=m["975"]=m["874"]=m["875"]=m["8E0"]="Kindle Scribe (2022)"
        m["FA0"]=m["FA1"]=m["FE5"]=m["F9D"]=m["FE4"]=m["FE3"]=m["102E"]=m["102D"]="Kindle Scribe 2 (2024)"
        m["12F0"]=m["12EE"]=m["12F4"]=m["11E8"]=m["11EA"]=m["10A4"]="Kindle Scribe 3 (2025)"
        m["13BF"]=m["12EF"]=m["12F1"]=m["11E9"]=m["11EB"]=m["10D7"]="Kindle Scribe Colorsoft (2025)"
        m["2F7"]=m["361"]=m["362"]=m["363"]=m["364"]=m["365"]=m["366"]=m["367"]=m["372"]=m["373"]=m["374"]=m["375"]=m["376"]="Kindle Paperwhite 4 (2018)"
        m["402"]=m["403"]=m["4D8"]=m["4D9"]=m["4DA"]=m["4DB"]=m["4DC"]=m["4DD"]="Kindle Paperwhite 4 (2018)"
        m["414"]=m["3CF"]=m["3D0"]=m["3D1"]=m["3D2"]=m["3AB"]="Kindle Basic 3 (2019)"
        m["434"]=m["3D8"]=m["3D7"]=m["3D6"]=m["3D5"]=m["3D4"]="Kindle Oasis 3 (2019)"
        m["295"]=m["296"]=m["297"]=m["298"]=m["2E1"]=m["2E2"]=m["2E6"]=m["2E7"]=m["2E8"]=m["341"]=m["342"]=m["343"]=m["344"]=m["347"]=m["34A"]="Kindle Oasis 2 (2017)"
        m["20C"]=m["20D"]=m["219"]=m["21A"]=m["21B"]=m["21C"]="Kindle Oasis (2016)"
        m["201"]=m["202"]=m["204"]=m["205"]=m["206"]=m["207"]=m["26B"]=m["26C"]=m["26D"]=m["26E"]=m["26F"]=m["270"]=m["293"]=m["294"]="Kindle Paperwhite 3 (2015)"
        m["1BC"]=m["269"]=m["26A"]="Kindle Basic 2 (2016)"
        m["13"]=m["54"]=m["2A"]=m["4F"]=m["52"]=m["53"]="Kindle Voyage (2014)"
        m["D4"]=m["5A"]=m["D5"]=m["D6"]=m["D7"]=m["D8"]=m["F2"]=m["17"]=m["60"]=m["F4"]=m["F9"]=m["62"]=m["61"]=m["5F"]="Kindle Paperwhite 2 (2013)"
        m["C6"]=m["DD"]="Kindle Basic (2014)"
    }
    NR == 1 {
        s = toupper($0)
        gsub(/[^A-Z0-9]/, "", s)
        first = substr(s, 1, 1)
        if ((first == "B" || first == "9") && length(s) >= 4) {
            code = substr(s, 3, 2)
            val = 0
            for (i = 1; i <= 2; i++) {
                p = index(hex, substr(code, i, 1))
                if (p == 0) { val = -1; break }
                val = val * 16 + (p - 1)
            }
        } else if (length(s) >= 6) {
            code = substr(s, 4, 3)
            val = 0
            for (i = 1; i <= 3; i++) {
                p = index(b32, substr(code, i, 1))
                if (p == 0) { val = -1; break }
                val = val * 32 + (p - 1)
            }
        } else {
            print "[unrecognized]"
            exit
        }
        if (val < 0) {
            printf "unknown (code %s)\n", code
            exit
        }
        key = sprintf("%X", val)
        name = (key in m) ? m[key] : "unknown"
        printf "%s (code %s, id=0x%x)\n", name, code, val
        exit
    }
    END { if (NR == 0) print "[empty]" }
    ' "$usid_file" 2>/dev/null
}

kindle_firmware() {
    pv_file="${VERSO_PRETTYVERSION_FILE:-/etc/prettyversion.txt}"
    if [ ! -r "$pv_file" ]; then
        printf '%s\n' '[unavailable]'
        return
    fi
    val=$(awk '
        # Some firmware leaves a template placeholder: "Kindle 5.18.5 (~~otaVersion~~)".
        { gsub(/[^[:print:]\t]/, ""); gsub(/[ \t]*\(~~[^~()]*~~\)/, ""); gsub(/^[ \t]+|[ \t]+$/, "") }
        $0 != "" { print substr($0, 1, 128); exit }
    ' "$pv_file" 2>/dev/null)
    if [ -n "$val" ]; then
        printf '%s\n' "$val"
    else
        printf '%s\n' '[empty]'
    fi
}

libc_summary() {
    if command -v getconf >/dev/null 2>&1; then
        val=$(getconf GNU_LIBC_VERSION 2>/dev/null)
        if [ -n "$val" ]; then
            printf '%s\n' "$val"
            return
        fi
    fi
    for p in /lib/libc-*.so /lib/*-linux-*/libc-*.so /lib/ld-musl-*.so*; do
        [ -f "$p" ] || continue
        printf '%s\n' "${p##*/}"
        return
    done
    printf '%s\n' '[unavailable]'
}

identity() {
    printf 'Kindle model: %s\n' "$(kindle_model)"
    printf 'Kindle firmware: %s\n' "$(kindle_firmware)"
    printf 'Kernel and architecture: %s\n' "$(uname -srm 2>/dev/null || printf '[unavailable]')"
    printf 'libc: %s\n' "$(libc_summary)"
}

report() {
    printf '%s\n' \
        'Verso device diagnostics - format 1' \
        'Observed hardware/interfaces only; this does not certify Verso compatibility.' \
        'No books, account data, network settings, full serials, or system logs requested.' \
        'Review vendor-provided hardware names before sharing.'

    section 'System and CPU'
    printf 'Kindle model: %s\n' "$(kindle_model)"
    printf 'Kindle firmware: %s\n' "$(kindle_firmware)"
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
    printf '%s\n' 'Marketing model / firmware: see System and CPU above; supply separately if unavailable.'

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

if [ "$mode" = identity ]; then
    identity
else
    report
fi
