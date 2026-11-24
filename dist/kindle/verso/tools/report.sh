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

# Redacts book paths under any library root on stdin -> stdout.
# Each unique path gets a deterministic <book N> placeholder for the whole stream.
redact_stream() {
    roots_data="$(library_roots | awk '
        {
            sub(/[\/]+$/, "")
            if ($0 != "" && !seen[$0]++) print $0
        }
    ')"
    awk -v roots_raw="${roots_data}" '
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
        book_count = 0
    }
    function book_token(p) {
        if (!(p in book_id)) {
            book_count++
            book_id[p] = book_count
        }
        return "<book " book_id[p] ">"
    }
    function find_end(s,    n, i, c, nextc, lower_s, epub_pos, q_limit) {
        # First check for .epub before any quote/tab/CR: real book filenames
        # often contain commas, parentheses, and brackets (e.g. "Author, Name (2024).epub").
        n = length(s)
        q_limit = n
        for (i = 1; i <= n; i++) {
            c = substr(s, i, 1)
            if (c == "\"" || c == "\047" || c == "\t" || c == "\r") {
                q_limit = i - 1
                break
            }
        }
        lower_s = tolower(substr(s, 1, q_limit))
        epub_pos = index(lower_s, ".epub")
        if (epub_pos > 0) {
            return epub_pos + 4
        }
        # Non-.epub path (e.g. a folder): find earliest delimiter.
        best = q_limit
        for (i = 1; i <= q_limit; i++) {
            c = substr(s, i, 1)
            if (c == "]" || c == ")") {
                best = i - 1
                break
            }
            if ((c == ":" || c == "," || c == ";") && i < q_limit) {
                nextc = substr(s, i + 1, 1)
                if (nextc == " " || nextc == "\t") {
                    best = i - 1
                    break
                }
            }
        }
        return best
    }
    function redact_line(line,    out, pos, best_pos, best_r, i, r, pat, after, end_len, full_path) {
        # Never leak custom library folder lists in "library folders changed: [...]".
        if (index(line, "library folders changed:") > 0) {
            sub(/library folders changed: \[.*\]/, "library folders changed: [<redacted>]", line)
        }
        # Redact legacy metadata log lines ("book \"...\", authors \"...\", language ...").
        if (index(line, "book \"") > 0 && index(line, ", authors \"") > 0 && index(line, ", language ") > 0) {
            sub(/book ".*", authors ".*", language /, "book <redacted>, language ", line)
        }
        # Redact crengine cache filenames that embed the book filename (e.g. "File Book_Name.epub.571d7876.0.cr3").
        if (index(line, ".epub.") > 0 && index(line, ".cr3") > 0) {
            gsub(/[^ \t\/]+\.[eE][pP][uU][bB]\.[0-9a-fA-F]+\.[0-9]+\.cr3/, "<cache>", line)
        }
        out = ""
        while (length(line) > 0) {
            best_pos = 0
            best_r = 0
            for (i = 1; i <= n_roots; i++) {
                pat = roots[i] "/"
                pos = index(line, pat)
                if (pos > 0 && (best_pos == 0 || pos < best_pos)) {
                    best_pos = pos
                    best_r = i
                }
            }
            if (best_pos == 0) {
                out = out line
                break
            }
            out = out substr(line, 1, best_pos - 1)
            r = roots[best_r]
            after = substr(line, best_pos + length(r) + 1)
            end_len = find_end(after)
            if (end_len <= 0) {
                out = out r "/"
                line = after
            } else {
                full_path = r "/" substr(after, 1, end_len)
                out = out book_token(full_path)
                line = substr(after, end_len + 1)
            }
        }
        return out
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

do_crash() {
    ended="$1"
    mkdir -p "${REPORTS_DIR}" || exit 1
    out="$(unique_report_path crash)"
    tmp="${out}.tmp"
    {
        printf '%s\n' 'Verso crash report - format 1'
        printf 'Created: %s\n' "$(date '+%Y-%m-%d %H:%M:%S')"
        printf 'Ended: %s\n' "${ended}"
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
            "Created: $(date '+%Y-%m-%d %H:%M:%S')" \
            'Review this file before sharing. Book paths under your library folders have been replaced with <book N>.'

        printf '\n== 1. Verso version and settings ==\n'
        installed_summary
        sess_ver="$(last_session_version)"
        printf 'Last session version: %s\n' "${sess_ver:-unknown}"
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
            redact_stream <"${UPDATE_DIR}/rollback.json"
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
                cat "${REPORTS_DIR}/${newest}" 2>/dev/null
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
        printf '%s report: created %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "${out}" >>"${LOG}" 2>/dev/null || true
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
