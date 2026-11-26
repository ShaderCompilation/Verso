#!/bin/sh
# Verso problem and crash report builder.
# Plain POSIX sh + BusyBox awk: runs on the Kindle even when Verso cannot start.
#
#   sh report.sh crash <ended-description>
#       Writes <root>/verso/reports/crash-<date>-<time>.txt (session header,
#       last 300 redacted log lines, how Verso ended) and keeps the newest 5.
#
#   sh report.sh report [--output FILE]
#       Writes <root>/verso/reports/report-<date>-<time>.txt (or FILE):
#         0. Sessions: how each started and ended, its timings and frame checks
#         1. Verso version, commit and settings summary (no library folders/book names)
#         2. Update state (installed, previous, trial, last rollback)
#         3. Crash report list + newest crash report
#         4. Tail of verso.log and verso.log.old (up to 1,000 lines, redacted)
#         5. Hardware diagnostics (collect-diagnostics.sh)
#
# Environment overrides (simulator / tests):
#   VERSO_STORAGE_ROOT  replaces /mnt/us
#   VERSO_DATA          replaces <root>/verso/data
#   VERSO_COLLECTOR     replaces <tools>/collect-diagnostics.sh
#   VERSO_BOOKS         colon-separated library folders to redact in addition to settings

LC_ALL=C
export LC_ALL
PATH="/usr/sbin:/usr/bin:/sbin:/bin:${PATH}"
export PATH

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="${VERSO_STORAGE_ROOT:-/mnt/us}"
APP_DIR="${ROOT}/verso"
DATA_DIR="${VERSO_DATA:-${APP_DIR}/data}"
REPORTS_DIR="${APP_DIR}/reports"
UPDATE_DIR="${APP_DIR}/update"
LOG="${APP_DIR}/verso.log"
LOG_OLD="${APP_DIR}/verso.log.old"
SETTINGS="${DATA_DIR}/settings.json"
INSTALLED="${APP_DIR}/installed.json"
COLLECTOR="${VERSO_COLLECTOR:-${SCRIPT_DIR}/collect-diagnostics.sh}"

usage() {
    printf '%s\n' \
        'Usage:' \
        '  sh report.sh crash <ended-description>' \
        '  sh report.sh report [--output FILE]'
}

if ! command -v awk >/dev/null 2>&1; then
    printf '%s\n' 'Cannot build report: awk is required.' >&2
    exit 1
fi

# Prints a unique filename under REPORTS_DIR with prefix $1 (crash or report).
# Suffixes use _02, _03, ... ('_' > '.' in ASCII) so lexicographic and mtime
# ordering both put newer files last.
unique_report_path() {
    prefix=$1
    stamp="$(date '+%Y%m%d-%H%M%S')"
    base="${REPORTS_DIR}/${prefix}-${stamp}"
    # Find the highest existing suffix for this timestamp so rotating out the
    # base file never lets a newer crash reuse an older lexicographical slot.
    max_n=0
    if [ -e "${base}.txt" ]; then
        max_n=1
    fi
    for existing in "${base}"_*.txt; do
        [ -e "${existing}" ] || continue
        suf="${existing#${base}_}"
        suf="${suf%.txt}"
        # Strip leading zeros safely in POSIX sh.
        suf="$(printf '%s' "${suf}" | sed 's/^0*//')"
        case "${suf}" in
            '' | *[!0-9]*) continue ;;
        esac
        if [ "${suf}" -gt "${max_n}" ]; then
            max_n="${suf}"
        fi
    done
    if [ "${max_n}" -eq 0 ]; then
        printf '%s.txt\n' "${base}"
    else
        printf '%s_%02d.txt\n' "${base}" "$((max_n + 1))"
    fi
}

# Extracts configured library folders (one per line) from settings.json,
# VERSO_BOOKS, and default Kindle/root paths.
library_roots() {
    if [ -r "${SETTINGS}" ]; then
        awk '
        function unescape(s,    out, i, c, n) {
            out = ""
            n = length(s)
            for (i = 1; i <= n; i++) {
                c = substr(s, i, 1)
                if (c == "\\" && i < n) {
                    i++
                    c = substr(s, i, 1)
                }
                out = out c
            }
            return out
        }
        { content = content "\n" $0 }
        END {
            # library_folders: [ "...", "..." ]
            rest = content
            if (match(rest, /"library_folders"[ \t\r\n]*:[ \t\r\n]*\[/)) {
                rest = substr(rest, RSTART + RLENGTH)
                # Walk until matching ] outside quotes
                in_str = 0; esc = 0; cur = ""
                n = length(rest)
                for (i = 1; i <= n; i++) {
                    c = substr(rest, i, 1)
                    if (in_str) {
                        if (esc) { cur = cur "\\" c; esc = 0 }
                        else if (c == "\\") { esc = 1 }
                        else if (c == "\"") {
                            p = unescape(cur)
                            if (p != "") print p
                            cur = ""
                            in_str = 0
                        } else { cur = cur c }
                    } else {
                        if (c == "\"") { in_str = 1; cur = "" }
                        else if (c == "]") { break }
                    }
                }
            }
            # Legacy library_folder: "..."
            rest = content
            if (match(rest, /"library_folder"[ \t\r\n]*:[ \t\r\n]*"/)) {
                rest = substr(rest, RSTART + RLENGTH)
                esc = 0; cur = ""
                n = length(rest)
                for (i = 1; i <= n; i++) {
                    c = substr(rest, i, 1)
                    if (esc) { cur = cur "\\" c; esc = 0 }
                    else if (c == "\\") { esc = 1 }
                    else if (c == "\"") {
                        p = unescape(cur)
                        if (p != "") print p
                        break
                    } else { cur = cur c }
                }
            }
        }
        ' "${SETTINGS}" 2>/dev/null
    fi
    if [ -n "${VERSO_BOOKS:-}" ]; then
        printf '%s\n' "${VERSO_BOOKS}" | tr ':' '\n'
    fi
    printf '%s\n' \
        "/mnt/us/documents" \
        "${ROOT}/documents"
}

# Extracts stored secrets (API keys) from settings.json so redact_stream scrubs
# them if they ever appear in logs or crash reports.
stored_secrets() {
    if [ -r "${SETTINGS}" ]; then
        awk '
        function unescape(s,    out, i, c, n) {
            out = ""
            n = length(s)
            for (i = 1; i <= n; i++) {
                c = substr(s, i, 1)
                if (c == "\\" && i < n) {
                    i++
                    c = substr(s, i, 1)
                }
                out = out c
            }
            return out
        }
        { content = content "\n" $0 }
        END {
            rest = content
            if (match(rest, /"deepl_api_key"[ \t\r\n]*:[ \t\r\n]*"/)) {
                rest = substr(rest, RSTART + RLENGTH)
                esc = 0; cur = ""
                n = length(rest)
                for (i = 1; i <= n; i++) {
                    c = substr(rest, i, 1)
                    if (esc) { cur = cur "\\" c; esc = 0 }
                    else if (c == "\\") { esc = 1 }
                    else if (c == "\"") {
                        v = unescape(cur)
                        if (v != "") print v
                        break
                    } else { cur = cur c }
                }
            }
        }
        ' "${SETTINGS}" 2>/dev/null
    fi
}

# Redacts book paths, stored secrets and the device name on stdin -> stdout. A
# path is replaced by <book N>, numbered per stream by its spelling. Where a
# path ends is a guess, as names hold quotes, colons and arrows: when unsure,
# redact too much.
redact_stream() {
    roots_data="$(library_roots | awk '
        {
            sub(/[\/]+$/, "")
            if ($0 != "" && !seen[$0]++) print $0
        }
    ')"
    secrets_data="$(stored_secrets)"
    awk -v roots_raw="${roots_data}" -v secrets_raw="${secrets_data}" -v storage_root="${ROOT}" '
    BEGIN {
        n_roots = 0
        if (roots_raw != "") {
            m = split(roots_raw, tmp, "\n")
            for (i = 1; i <= m; i++) {
                if (tmp[i] != "") {
                    n_roots++
                    roots[n_roots] = tmp[i]
                }
            }
        }
        n_secrets = 0
        if (secrets_raw != "") {
            m = split(secrets_raw, stmp, "\n")
            for (i = 1; i <= m; i++) {
                if (stmp[i] != "") {
                    n_secrets++
                    secrets[n_secrets] = stmp[i]
                }
            }
        }
        # Sort roots by length descending (insertion sort) so nested roots match first.
        for (i = 2; i <= n_roots; i++) {
            key = roots[i]
            j = i - 1
            while (j >= 1 && length(roots[j]) < length(key)) {
                roots[j + 1] = roots[j]
                j--
            }
            roots[j + 1] = key
        }
        storage[1] = "/mnt/us"
        n_storage = 1
        if (storage_root != storage[1]) storage[++n_storage] = storage_root
        # Every Kindle has this folder, so its own name tells nothing.
        for (i = 1; i <= n_storage; i++) plain[storage[i] "/documents"] = 1
        book_count = 0
    }
    function book_token(p) {
        if (!(p in book_id)) {
            book_count++
            book_id[p] = book_count
        }
        return "<book " book_id[p] ">"
    }
    # Finds the first path to redact in s. Sets at (0: none), at_len (its root)
    # and at_root ("" for the storage root, which counts on wide lines only).
    function find_path(s, wide,    i, pos, base, rest) {
        at = 0
        for (i = 1; i <= n_roots; i++) {
            pos = index(s, roots[i])
            if (pos > 0 && (at == 0 || pos < at)) {
                at = pos
                at_len = length(roots[i])
                at_root = roots[i]
            }
        }
        for (i = 1; wide && i <= n_storage; i++) {
            base = 0
            rest = s
            while ((pos = index(rest, storage[i] "/")) > 0) {
                pos += base
                base = pos + length(storage[i])
                rest = substr(s, base + 1)
                if (rest ~ /^(extensions\/)?verso([\/":]|$)/) continue
                if (at == 0 || pos < at) {
                    at = pos
                    at_len = length(storage[i])
                    at_root = ""
                }
                break
            }
        }
        return at
    }
    # Whether a library root stands alone, given the text after it.
    function is_bare(s, quoted) {
        if (quoted) return s ~ /^\\?"/
        return s == "" || s == "\r" || s ~ /^\\?"/ || s ~ /^: [^\/]*$/
    }
    # The length of the path that s starts with; quoted: it follows a ".
    function path_end(s, quoted, wide,    i, n, c, low, pos) {
        n = length(s)
        if (quoted) {
            for (i = 1; i <= n; i++) {
                c = substr(s, i, 1)
                if (c == "\\") i++
                else if (c == "\"") return i - 1
            }
            return n
        }
        if (find_path(s, wide)) s = substr(s, 1, at - 1)
        # The last .epub, with what extends it (.part) unless it is a folder.
        low = tolower(s)
        n = 0
        while ((pos = index(substr(low, n + 1), ".epub")) > 0) n += pos + 4
        if (n > 0) {
            while (substr(s, n + 1, 1) ~ /[A-Za-z0-9._-]/) n++
            if (substr(s, n + 1, 1) != "/") return n
        }
        # All of it, but for an OS error or the words between two paths.
        if (match(s, /: [^:\/]*\(os error [0-9]+\)[]",}]*$/)) return RSTART - 1
        if (match(s, / (→|to) $/)) return RSTART - 1
        return length(s)
    }
    function redact_line(line,    out, wide, root, after, len, quoted, i, pos) {
        # Never leak stored API keys or DeepL authorization headers.
        gsub(/DeepL-Auth-Key [^ \t"'\''\r\n]+/, "DeepL-Auth-Key <redacted>", line)
        for (i = 1; i <= n_secrets; i++) {
            while ((pos = index(line, secrets[i])) > 0) {
                line = substr(line, 1, pos - 1) "<redacted>" substr(line, pos + length(secrets[i]))
            }
        }
        # Never leak custom library folder lists in "library folders changed: [...]".
        if (index(line, "library folders changed:") > 0) {
            sub(/library folders changed: \[.*\]/, "library folders changed: [<redacted>]", line)
        }
        # Lines of earlier versions, kept in the log across updates: titles in
        # "book \"...\", authors \"...\", language ...", and names that are not
        # paths. The first arrow may be in the title; only the last one is
        # followed by a slash.
        if (index(line, "book \"") > 0 && index(line, ", authors \"") > 0 && index(line, ", language ") > 0) {
            sub(/book ".*", authors ".*", language /, "book <redacted>, language ", line)
        }
        sub(/web app: received .* → \//, "web app: received /", line)
        sub(/highlights zip entry .*\.md/, "highlights zip entry <redacted>", line)
        sub(/web app: refusing .* in \//, "web app: refusing a name in /", line)
        sub(/mDNS: advertising .*\.local/, "mDNS: advertising <redacted>", line)
        if (index(line, "Wi-Fi transfer: ") > 0) {
            sub(/ at Some\([0-9.]+\) as /, " at <redacted> as ", line)
            sub(/ as Some\(".*"\)/, " as <redacted>", line)
        }
        sub(/web app: preparing a connection from [^ ]+: /, "web app: preparing a connection: ", line)
        # crengine cache files are named after the book ("Book_Name.epub.571d7876.0.cr3").
        if (index(line, ".cr3") > 0) {
            gsub(/[^ \t\/"]+\.cr3/, "<cache>", line)
        }
        # These name the files and folders of the user anywhere in the storage.
        # (Debug output may escape a folder so that it no longer matches its root.)
        wide = line ~ /web app: |\] listing \/|collection switch |open book removed: /
        out = ""
        while (find_path(line, wide)) {
            len = at_len
            root = at_root
            out = out substr(line, 1, at - 1)
            line = substr(line, at)
            after = substr(line, len + 1)
            quoted = substr(out, length(out)) == "\""
            if (root != "" && is_bare(after, quoted)) {
                out = out ((root in plain) ? root : "<library folder>")
            } else {
                len += path_end(after, quoted, wide)
                out = out book_token(substr(line, 1, len))
            }
            line = substr(line, len + 1)
        }
        return out line
    }
    { print redact_line($0) }
    '
}

# Extracts the last === Verso session === ... === end session === block from
# verso.log (plus any "session frontlight:" line logged after it).
extract_session_header() {
    if [ ! -r "${LOG}" ]; then
        printf '%s\n' '[no verso.log found]'
        return
    fi
    awk '
        /=== Verso session ===/ {
            in_block = 1
            block = $0
            frontlight = ""
            next
        }
        in_block {
            block = block "\n" $0
            if (/=== end session ===/) {
                in_block = 0
            }
            next
        }
        /session frontlight:/ {
            frontlight = $0
        }
        END {
            if (block != "") {
                print block
                if (frontlight != "") print frontlight
            } else {
                print "[no session header found in verso.log]"
            }
        }
    ' "${LOG}" 2>/dev/null
}

# Extracts the last "version: ..." line from verso.log (including indented continuation lines).
last_session_version() {
    if [ -r "${LOG}" ]; then
        awk '/^[ \t]*version: / {
            v = $0
            sub(/^[ \t]*version: /, "", v)
        } END { if (v != "") print v }' "${LOG}" 2>/dev/null
    fi
}

# Prints a safe summary of settings.json with library_folders, library_folder,
# and hostname omitted.
settings_summary() {
    if [ ! -r "${SETTINGS}" ]; then
        printf '%s\n' '[no settings.json found]'
        return
    fi
    awk '
    { content = content "\n" $0 }
    END {
        # Strip library_folders ([...]) and library_folder ("...") and hostname ("...").
        # Walk character by character at depth 1 of the root JSON object so we
        # never depend on formatting (pretty vs compact) and never leak paths.
        n = length(content)
        i = 1
        while (i <= n && substr(content, i, 1) != "{") i++
        if (i > n) { print "[unreadable settings.json]"; exit }
        i++
        printed = 0
        while (i <= n) {
            # Skip whitespace and commas
            while (i <= n && substr(content, i, 1) ~ /[ \t\r\n,]/) i++
            if (i > n || substr(content, i, 1) == "}") break
            # Read key string
            if (substr(content, i, 1) != "\"") break
            i++
            key = ""; esc = 0
            while (i <= n) {
                c = substr(content, i, 1); i++
                if (esc) { key = key c; esc = 0 }
                else if (c == "\\") { esc = 1 }
                else if (c == "\"") { break }
                else { key = key c }
            }
            while (i <= n && substr(content, i, 1) ~ /[ \t\r\n:]/) i++
            # Read value (balanced {}, [], "", or primitive)
            val_start = i
            c = substr(content, i, 1)
            if (c == "{" || c == "[") {
                open_c = c
                close_c = (c == "{") ? "}" : "]"
                depth = 0; in_s = 0; esc = 0
                while (i <= n) {
                    ch = substr(content, i, 1); i++
                    if (in_s) {
                        if (esc) esc = 0
                        else if (ch == "\\") esc = 1
                        else if (ch == "\"") in_s = 0
                    } else {
                        if (ch == "\"") in_s = 1
                        else if (ch == open_c) depth++
                        else if (ch == close_c) { depth--; if (depth == 0) break }
                    }
                }
            } else if (c == "\"") {
                i++; esc = 0
                while (i <= n) {
                    ch = substr(content, i, 1); i++
                    if (esc) esc = 0
                    else if (ch == "\\") esc = 1
                    else if (ch == "\"") break
                }
            } else {
                while (i <= n && substr(content, i, 1) !~ /[,}\r\n]/) i++
            }
            val = substr(content, val_start, i - val_start)
            gsub(/[ \t\r\n]+/, " ", val)
            sub(/^ /, "", val); sub(/ $/, "", val)
            if (key == "deepl_api_key") {
                if (val != "null" && val != "\"\"") {
                    printf "  %s: <redacted>\n", key
                    printed++
                }
                continue
            }
            if (key == "library_folders" || key == "library_folder" || key == "hostname") {
                continue
            }
            printf "  %s: %s\n", key, val
            printed++
        }
        if (printed == 0) print "[empty settings.json]"
    }
    ' "${SETTINGS}" 2>/dev/null
}

# Prints a readable summary of installed.json.
installed_summary() {
    if [ ! -r "${INSTALLED}" ]; then
        printf '%s\n' '[no installed.json found]'
        return
    fi
    awk '
        { content = content " " $0 }
        END {
            if (match(content, /"version"[ \t]*:[ \t]*"[^"]*"/)) {
                v = substr(content, RSTART, RLENGTH)
                sub(/^"version"[ \t]*:[ \t]*"/, "", v); sub(/"$/, "", v)
                printf "Installed version: %s\n", v
            }
            if (match(content, /"dev"[ \t]*:[ \t]*(true|false)/)) {
                d = substr(content, RSTART, RLENGTH)
                sub(/^"dev"[ \t]*:[ \t]*/, "", d)
                printf "Dev install: %s\n", d
            }
        }
    ' "${INSTALLED}" 2>/dev/null
}

# Tails up to $1 lines across verso.log.old and verso.log combined.
combined_log_tail() {
    limit=$1
    if [ ! -r "${LOG}" ] && [ ! -r "${LOG_OLD}" ]; then
        printf '%s\n' '[no log files found]'
        return
    fi
    cur_lines=0
    if [ -r "${LOG}" ]; then
        cur_lines="$(wc -l <"${LOG}" 2>/dev/null | tr -d ' ')"
        case "${cur_lines}" in '' | *[!0-9]*) cur_lines=0 ;; esac
    fi
    if [ "${cur_lines}" -ge "${limit}" ]; then
        tail -n "${limit}" "${LOG}" 2>/dev/null
    else
        rem=$((limit - cur_lines))
        if [ -r "${LOG_OLD}" ] && [ "${rem}" -gt 0 ]; then
            printf '%s\n' '--- verso.log.old ---'
            tail -n "${rem}" "${LOG_OLD}" 2>/dev/null
        fi
        if [ -r "${LOG}" ]; then
            printf '%s\n' '--- verso.log ---'
            cat "${LOG}" 2>/dev/null
        fi
    fi
}

# The lines that tell sessions apart and how each went, from both logs.
session_lines() {
    for f in "${LOG_OLD}" "${LOG}"; do
        [ -r "${f}" ] && grep -E ' launcher: (launched from|the last session|active app|starting Verso|Verso ended)|\] (perf:|first frame on screen|frame check|first touch|(remote|page buttons) [^:]*: ([0-9]+ forward|first |left to ))' "${f}" 2>/dev/null
    done | tail -n 60
}

# Lists crash-*.txt files, newest first.
list_crash_reports() {
    [ -d "${REPORTS_DIR}" ] || return 0
    # Use ls -1t for mtime order; sort ties by name descending via awk if needed.
    # Since filenames have YYYYMMDD-HHMMSS[_NN], reverse lexicographical order
    # matches chronological order reliably even when mtimes are identical.
    (cd "${REPORTS_DIR}" 2>/dev/null && for f in crash-*.txt; do
        [ -f "${f}" ] || continue
        printf '%s\n' "${f}"
    done) | sort -r
}

# Keeps the newest 5 crash-*.txt files.
rotate_crash_reports() {
    count=0
    list_crash_reports | while IFS= read -r f; do
        [ -n "${f}" ] || continue
        count=$((count + 1))
        if [ "${count}" -gt 5 ]; then
            rm -f "${REPORTS_DIR}/${f}" 2>/dev/null
        fi
    done
}

# Prints a brief non-PII device/ABI summary (Kindle model code, firmware,
# kernel, libc). Delegates to collect-diagnostics.sh --identity when available.
device_identity() {
    if [ -r "${COLLECTOR}" ] && grep -q -- '--identity' "${COLLECTOR}" 2>/dev/null; then
        sh "${COLLECTOR}" --identity 2>/dev/null && return
    fi
    printf 'Kernel and architecture: %s\n' "$(uname -srm 2>/dev/null || printf '[unavailable]')"
    libc_ver="$(getconf GNU_LIBC_VERSION 2>/dev/null || true)"
    printf 'libc: %s\n' "${libc_ver:-[unavailable]}"
}

do_crash() {
    ended="$1"
    mkdir -p "${REPORTS_DIR}" || exit 1
    out="$(unique_report_path crash)"
    tmp="${out}.tmp"
    {
        printf '%s\n' 'Verso crash report - format 2'
        printf 'Created: %s (log times are UTC)\n' "$(date '+%Y-%m-%d %H:%M:%S %z')"
        printf 'Ended: %s\n' "${ended}"
        installed_summary
        printf '\n== Device identity ==\n'
        device_identity
        printf '\n== Session header ==\n'
        extract_session_header
        printf '\n== Last 300 log lines (book names redacted) ==\n'
        if [ -r "${LOG}" ]; then
            tail -n 300 "${LOG}" 2>/dev/null | redact_stream
        else
            printf '%s\n' '[no verso.log found]'
        fi
    } >"${tmp}" && mv -f "${tmp}" "${out}" || {
        rm -f "${tmp}"
        exit 1
    }
    rotate_crash_reports
    printf '%s\n' "${out}"
}

do_report() {
    out="$1"
    if [ -z "${out}" ]; then
        mkdir -p "${REPORTS_DIR}" || exit 1
        out="$(unique_report_path report)"
    else
        mkdir -p "$(dirname "${out}")" || exit 1
    fi
    tmp="${out}.tmp"
    TAB="$(printf '\t')"
    {
        printf '%s\n' \
            'Verso problem report - format 1' \
            "Created: $(date '+%Y-%m-%d %H:%M:%S %z') (log times are UTC)" \
            'Review this file before sharing. Book paths under your library folders have been replaced with <book N>.'

        printf '\n== Sessions (newest last) ==\n'
        session_lines | redact_stream

        printf '\n== 1. Verso version and settings ==\n'
        installed_summary
        sess_ver="$(last_session_version)"
        printf 'Last session version: %s\n' "${sess_ver:-unknown}"
        device_identity
        printf 'Settings (library folders and book names omitted):\n'
        settings_summary

        printf '\n== 2. Update state ==\n'
        installed_summary
        if [ -r "${UPDATE_DIR}/previous/plan.txt" ]; then
            prev_ver="$(sed -n "s/^from${TAB}//p" "${UPDATE_DIR}/previous/plan.txt" 2>/dev/null | head -n 1)"
            printf 'Previous version (available to go back): %s\n' "${prev_ver:-unknown}"
        else
            printf '%s\n' 'Previous version: none'
        fi
        if [ -r "${UPDATE_DIR}/trial" ]; then
            printf 'Trial attempts: %s\n' "$(cat "${UPDATE_DIR}/trial" 2>/dev/null)"
        else
            printf '%s\n' 'Trial: none'
        fi
        if [ -r "${UPDATE_DIR}/rollback.json" ]; then
            printf '%s\n' 'Last rollback:'
            # One log line a line, so that a path ends with its own.
            awk '{ gsub(/","/, "\",\n\""); print }' "${UPDATE_DIR}/rollback.json" | redact_stream
        else
            printf '%s\n' 'Last rollback: none'
        fi

        printf '\n== 3. Crash reports ==\n'
        crashes="$(list_crash_reports)"
        if [ -z "${crashes}" ]; then
            printf '%s\n' '[none]'
        else
            printf '%s\n' "${crashes}" | while IFS= read -r cf; do
                [ -n "${cf}" ] || continue
                sz="$(wc -c <"${REPORTS_DIR}/${cf}" 2>/dev/null | tr -d ' ')"
                printf '  %s (%s bytes)\n' "${cf}" "${sz:-?}"
            done
            newest="$(printf '%s\n' "${crashes}" | head -n 1)"
            if [ -n "${newest}" ] && [ -r "${REPORTS_DIR}/${newest}" ]; then
                printf '\n--- Newest crash report: %s ---\n' "${newest}"
                # Format 1 cut titles in half, which can't be redacted any more.
                awk '
                    NR == 1 { old = / format 1$/ }
                    { print }
                    old && /^== Last / { print "[left out: written by an earlier version]"; exit }
                ' "${REPORTS_DIR}/${newest}" 2>/dev/null | redact_stream
            fi
        fi

        printf '\n== 4. Logs (up to 1000 lines, book names redacted) ==\n'
        combined_log_tail 1000 | redact_stream

        printf '\n== 5. Hardware diagnostics ==\n'
        if [ -r "${COLLECTOR}" ]; then
            sh "${COLLECTOR}" 2>/dev/null || printf '%s\n' '[collect-diagnostics.sh failed]'
        else
            printf '[collect-diagnostics.sh not found at %s]\n' "${COLLECTOR}"
        fi

        printf '\n== End of problem report ==\n'
    } >"${tmp}" && mv -f "${tmp}" "${out}" || {
        rm -f "${tmp}"
        exit 1
    }
    if [ -d "${APP_DIR}" ]; then
        printf '%s report: created %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "${out}" >>"${LOG}" 2>/dev/null || true
    fi
    printf '%s\n' "${out}"
}

case "${1:-}" in
    crash)
        shift
        do_crash "${1:-crash}"
        ;;
    report)
        shift
        out_file=""
        if [ "${1:-}" = "--output" ]; then
            out_file="${2:-}"
            [ -n "${out_file}" ] || { usage >&2; exit 2; }
        elif [ -n "${1:-}" ]; then
            usage >&2
            exit 2
        fi
        do_report "${out_file}"
        ;;
    -h|--help)
        usage
        exit 0
        ;;
    *)
        usage >&2
        exit 2
        ;;
esac
