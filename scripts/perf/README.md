# Kindle performance probes

Sources used for the [2026-11-11 comparison](../../docs/archive/performance-2026-11-11.md).
The [2026-11-12 repeat](../../docs/archive/performance-2026-11-12.md) reuses the
probe and KOReader wrapper, with updated sampling for slower startup and the
new Home → library path. Its report records run order, boundaries and corrections.
These are measurement components, not a deploy command. Running a reader still
requires the launcher and device-readiness checks in `AGENTS.md`.

## Current performance audit

The [2026-11-16 pass](../../docs/archive/performance-2026-11-16.md) reuses these
components for current bottleneck attribution. Build both probes below, install
the latest release with `scripts/deploy.sh --no-run` **before** measuring, verify
its hash, and run one device workload at a time. The captures record the executed
binary's SHA-256; startup `--installed` also verifies it against the supplied
local binary. A clean Git tree does not prove an existing build is current.
Each turn run uses
fresh isolated settings and 24 turns. Repeat independent launches; do not count
consecutive memory snapshots as independent runs.

Readiness also resets powerd's current idle countdown using
`touchScreenSaverTimeout`, without changing the normal sleep policy. Contacts
injected into Verso's grabbed input do not reliably reset the paused native UI's
idle timer. Sleep during a run invalidates its touch/update/page counts. Rejected
captures are saved separately as `*.failed.json` for diagnosis and are rejected
by the analyzer; keep them out of accepted-input globs.

```sh
python3 scripts/perf/redraw.py portrait-1 \
  target/armv7-unknown-linux-gnueabihf/release/verso \
  --book /mnt/us/verso/testbooks/BOOK.epub \
  --probe /tmp/probe --preload /tmp/probe.so --home \
  --memory-check --idle-seconds 12 --output /tmp/perf/portrait-1.json
# Repeat with --orientation landscape for landscape; lifecycle flags remain portrait-only.
# For rapid input, use --turn-delay-us 150000 --queued-input.
# Compare the same rapid workload with --page-flash 0 (chapter/image flashes remain).
python3 scripts/perf/analyze-redraw.py /tmp/perf/*.json \
  --workloads --output /tmp/perf/numeric
```

Use a dedicated directory containing **only redraw captures** with that glob;
startup and dictionary JSON have different schemas. The ordinary default rejects
updates that cannot be associated with a single touch before the next contact.
`--queued-input` permits a later FIFO update only if the run has exactly 24
recognized taps, frames, successful full-screen updates, saves and saved-page
advances. It adds one final settling second before CPU accounting ends. This
captures a queue behind a full flash rather than discarding slow touches.

The analyzer reports overlap between touch-to-request latency and earlier
full-refresh completion waits, plus render/presentation distributions and
main/book/position/library thread CPU. All CPU calculations use the measured
device's 100 Hz accounting. `--workloads` validates final pixels and waveform
sequences within each book hash/orientation/font/refresh/cadence/annotation workload;
default comparison mode keeps the stricter global pixel/waveform checks.
`--font-size-pt` makes layout comparisons explicit. The current pass uses
10 pt for the illustrated book and 10.5 pt (44 engine pixels) for the large book,
so its first open misses the previously retained 10 pt layout without deleting
the user's engine caches. Repeat the same size to measure a matching-cache reopen.

Startup can now run without a synthetic fixture or probes left by an old run:

```sh
python3 scripts/perf/startup.py current \
  target/armv7-unknown-linux-gnueabihf/release/verso \
  --probe /tmp/probe --preload /tmp/probe.so \
  --fixture real --mode startup --runs 3 --collections-tap 636,230 \
  --output /tmp/perf/startup.json
python3 scripts/perf/analyze-startup.py /tmp/perf/startup.json \
  --output /tmp/perf/startup.csv
```

For cached-only scaling, use `--fixture large --large screenshots/phase7/large-state`
after generating the original fixture. `startup.py` replaces its `Unavailable:
fixture` cover placeholders with the real source-stamped cover keys **in the
uploaded copy only**. Otherwise the run measures 10,000 cover retries and index
writes rather than unchanged startup. Check for zero cover changes and index
writes before interpreting that workload. It retains the copied Home summary,
as in the original Home-startup report; it is not a real 10,000-file scan.

`analyze-startup.py` uses the reader's preload-constructor timestamp for initial
request and matching driver-completion boundaries. Internal stage timers overlap
and must not be summed into total startup. The app's `first frame on screen`
message is emitted after presentation returns. Likewise, the legacy
`highlights_load.py` field `first_page_ms` comes from `Reader::after_present`,
including a full-refresh wait when applicable: it is not first SEND_UPDATE.
The current report labels that value `first_present_return_ms`.

Since 2026-11-20, a fresh isolated state builds every dictionary index in the
background after the first page (the reader's one-time preparation), which costs
seconds of CPU on `verso-dictionary` during the timed turns. For steady-state CPU
pass `--dictionary-cache STATE` (to `redraw.py` or `highlights_load.py`) with a
`dictionary.py cold` state; the run copies only its `dictionary-cache`. Scripts
find the app with `redraw.app_pid` rather than `pidof verso`: the background
pre-layout child runs the same executable. Keep benchmark binaries and fixtures
under `/mnt/us/verso/*-check`, not `/tmp`: the Kindle's `/tmp` is a 64 MiB tmpfs
shared with `/var`, and filling it slows the device and skews timings.

Keep executable identity reads **before launch**. The current pass measured
31,968 → 21,976 KiB RSS after running `sha256sum` on the live executable,
with unchanged private dirty memory and page pixels. That observer effect can
make reading memory look ~9.8 MiB lower. `--memory-check --identity-memory-check`
reproduces the control outside the timed turn interval. Exported memory rows
label whether an executable identity read occurred during the app's lifetime
before that snapshot. Treat those post-read samples separately from normal RSS;
the request/CPU interval precedes the read and remains usable.

## Redraw comparison

`redraw.py` runs 24 text turns through the existing launcher with isolated state
under `/mnt/us/verso/redraw-check`. It never replaces the installed binary or
normal settings. Supply the before/after ARM binaries, a book already on-device,
and both probes built below. Run from the repository root with the Kindle awake
and other readers closed. This script injects touches while Verso owns input,
and checks launcher/UI restoration even if measurement validation fails.

```sh
python3 scripts/perf/redraw.py before /tmp/before/verso \
  --book /mnt/us/verso/testbooks/BOOK.epub --probe /tmp/probe \
  --preload /tmp/probe.so --output /tmp/redraw/before-1.json
python3 scripts/perf/redraw.py after /tmp/after/verso \
  --book /mnt/us/verso/testbooks/BOOK.epub --probe /tmp/probe \
  --preload /tmp/probe.so --output /tmp/redraw/after-1.json
python3 scripts/perf/analyze-redraw.py /tmp/redraw/before-1.json \
  /tmp/redraw/after-1.json --output /tmp/redraw/results
```

Coordinates assume our 1272 × 1696 Paperwhite and current layout. Every tap must
produce one successful full-screen update. Comparison rejects different final
page hashes or waveform sequences. Menu close must restore the same framebuffer.
Use a sufficiently long book; the script seeks to the same slider position before
each run. It retains per-run state/logs on-device for diagnosis. Local JSON holds
numeric traces, frame timings, touch positions, CPU records and hashes only.

For the cached-page presentation comparison, use baseline `e6aa996` and the
optimized binary with no logging changes. Repeat before-1, after-1, before-2,
after-2 with the same book/probes and analyze all four JSON files together.
Add `--wake-check` to an optimized run to verify real sleep/wake, delayed banner
cleanup, the restored framebuffer hash and a successful first turn after wake.
These checks happen after the measured interval and menu restoration. The flag
uses powerd's power-button property and returns the device to its awake state.

The 2026-11-11 baseline is `56d9aff` plus only the `App::frame` trace enhancement
that appends the number of updates; apply that logging change to both builds.
Both use `RUST_LOG=info,verso=debug,verso::app=trace` and the same ioctl observer.
CPU snapshots surround only the controlled turn interval, including prefetch,
presentation and saving. Thread/process CPU uses the device's previously measured
100 Hz accounting. The analyzer uses nearest-rank p95. Draw/diff totals measure
elapsed time inside `App::frame`, including scheduling; they are not CPU time.
Request latency starts at injected touch release and ends at SEND_UPDATE; it is
not optical response. See [results](../../docs/archive/redraw-optimization.md).

## Build

```sh
zig cc -target arm-linux-gnueabihf.2.35 -O2 -DPRELOAD -shared -fPIC \
  scripts/perf/probe.c -o /tmp/probe.so -ldl
zig cc -target arm-linux-gnueabihf.2.35 -O2 \
  scripts/perf/probe.c -o /tmp/probe
```

`probe.so` observes the Kindle MXCFB send-update, wait-for-submission and
wait-for-completion ioctls. It passes arguments and results through unchanged.
The original errno is restored after logging. Only ARMv7 Kindle data layouts are
supported. `process_start` is a preload-constructor timestamp, not an exec/KUAL
timestamp. Other subprocesses inheriting the preload can also log starts; use
the sampled reader PID to select the app's records.

Load it **only in the reader executable**, with `LD_PRELOAD=/path/to/probe.so`
and `VERSO_BENCH_TRACE=/allowed/path/RUN.trace`. Do not preload the launcher or
every utility in the measurement shell. The trace is append-only; use a new run
name every time. It records the request's start, return time/result, marker,
waveform, update mode, left/top and width/height. Request time precedes the ioctl;
the timestamp is written to the file after the call returns.

The helper uses the same clock and trace environment:

```sh
probe mark LABEL
probe tap VERSO_PID X Y LABEL
```

`tap` verifies that the PID's command name is `verso`, sends a 50 ms contact,
marks immediately before release, and releases the finger. Use it only while
Verso has grabbed `/dev/input/event1`. It must never target another app or the
Amazon UI. A marker in a helper has a different PID from the reader by design.

## KOReader wrapper

Run `koreader.lua` with the installed KOReader's `luajit`, from its installation
directory. Set `KO_HOME` to an isolated writable directory, copy settings there,
and direct document metadata there. Use copied books inside the allowed test
directory. Preserve plugin databases needed by enabled plugins; exclude external
automatic plugins as described in the report. Do not place live settings or
credentials in the repository.

Environment: `BENCH_DIRECTORY` is the initial folder, `BENCH_BOOK` the copied
EPUB, `BENCH_FONT_PX=42` the effective engine font size, and
`EXT_FONT_DIR=/mnt/us/verso/fonts` supplies Literata. `BENCH_STARTUP_ONLY=1`
exits six seconds after entering the UI loop. Otherwise the script opens the
book, seeks, dispatches 24 ordinary tap gestures, opens/closes the menu and
exits after an idle interval. The wrapper logs reader paints and font size.

The normal KOReader launcher must not run under this wrapper. The experiment
used a copy of Verso's launcher with a distinct application directory, PID file
and `/tmp` script name; its executable wrapper selected either reader. Its
pause/restore logic was unchanged. The KOReader startup-script version check
alone is bypassed because that launcher is intentionally absent. Existing
readers must be closed and the device awake before starting.

For Verso, use `VERSO_DATA` for isolated state. The library startup runs used a
copy of the real 299-book index and covers with the original library roots.
Reading runs used a one-book library, opened it, sought into the text using the
progress slider, then sent 24 turns and menu interactions. The compiled crengine
cache remains under `/mnt/us/verso/cache`.

## Samples and analysis

Save each trace as `RUN.trace`. For snapshots create
`RUN.samples/{library,reading-start,after-turns,idle-start,idle-end}/` and save:

- `/proc/uptime` as `uptime`;
- `/proc/PID/stat`, `status`, `smaps_rollup`, `io` under their original names.

Startup-only runs need just `library`. Take the initial snapshot about three
seconds after launch. Separate idle snapshots by at least ten seconds. The
analyzer uses the measured device's 100 ticks/second; verify this with
`sysconf(_SC_CLK_TCK)` on another device before reusing the calculations.
Avoid frequent sampling while measuring latency. Capture framebuffer evidence
after the turn sequence; on this device the first 1272 × 1696 bytes of `/dev/fb0`
are the visible 8-bit plane, while the complete allocation is larger.

```sh
python3 scripts/perf/analyze.py /path/to/captured-data \
  verso-reader-2 ko-reader-2 verso-reader-3 ko-reader-3 --csv /tmp/events.csv
```

The analyzer rejects failed updates, missing process starts and turn sequences
with other than 24 recorded taps. P95 uses the nearest-rank definition. It does
not establish that a tap changed the intended page; confirm logs and captures.
An `open_book` event in its event output measures **first feedback**, which may
be a loading message. The report's separate open-page columns were selected
from the first full-screen book update and its matching completion marker.
Do not treat subsequent delayed waits as optical page-completion timestamps.

The committed CSVs contain numeric results only. The original settings and
copyrighted page captures are not committed. Remove temporary device settings
and caches after collecting evidence, and verify the launcher has restored the
Kindle UI and removed its PID file.


## Library search

`python3 scripts/capture-search.py` generates the original multilingual screenshot
fixtures and a 10,000-entry cached-only index under `screenshots/phase7/`.
`VERSO_SEARCH_BENCH=1 VERSO_DATA=/path/to/state verso` reads that index and exits
before any platform/engine initialization. It measures lazy cache construction,
192 literal queries, matching plus cloning/Recent sorting with default reading
state, folded-text bytes and process RSS. Timings exclude UI drawing and panel I/O.

Keep the real library index and private framebuffer evidence on the Kindle; copy
only aggregate numbers. The 2026-11-11 device results live in
`docs/research/performance-2026-11-11/search-results.csv`. Match queries include
empty, broad Latin, Cyrillic and absent strings.
The diagnostic RSS is not normal app RSS. For app startup compare the same copied
index, covers and per-book state between builds, through the normal launcher.
For keyboard timing use the probe's release markers and successful SEND_UPDATE
calls before the next touch; distinguish first update request, last submission
and optical panel response. The report does not measure optical response.

If injected coordinates repeat across fresh evdev sessions, prime both axes
with a harmless corner contact before the first scripted action: the input
subsystem can omit unchanged ABS values. Send touches only while Verso holds the
touchscreen. Always check launcher cleanup and restored Kindle UI after a run.

## Buffer-memory comparison

Use the same runner with `--memory-check` to sample process `smaps` and `status`
in five settled states outside the timed turns: library, reader after seek,
reader after 24 turns, menu, and restored page. It takes three consecutive
snapshots per state. Repeat independent launches; consecutive snapshots are not
independent repetitions. The analyzer exports `memory.csv` with RSS, PSS,
private resident memory, swap, approximate status RSS/high-water, virtual size,
and screen hashes, alongside `runs.csv`, `events.csv` and numeric setup-frame
`setup.csv` timings. Values labelled `kib` are Linux's `kB` units (1024 bytes).

```sh
python3 scripts/perf/redraw.py before /tmp/before/verso \
  --book /mnt/us/verso/testbooks/BOOK.epub --probe /tmp/probe \
  --preload /tmp/probe.so --memory-check --output /tmp/buffers/before-1.json
```

The 2026-11-11 baseline is `bf1d3b1`, comparison-only stage `c25ce36`, and final
canvas `fcf0c51`. Run order: before-1, comparison-1, before-2, after-1,
comparison-2, after-2; add `--wake-check` to after-1. Analyze all six JSON files
together with `analyze-redraw.py`. No extra logging changes are needed. Memory
figures describe the controlled one-book library and reader workload; they are
not full-library or long-session measurements. The analyzer requires reader
hashes to match globally. If library/menu status text varies, each observed
hash must match at least two distinct builds; inspect those matches rather than
assuming that arbitrary UI differences are harmless.

Simulator verification (Pillow required for decoded-pixel comparison):

```sh
python3 scripts/perf/capture-buffers.py /tmp/before-desktop \
  --output /tmp/buffers/screens-before
python3 scripts/perf/capture-buffers.py target/debug/verso \
  --output /tmp/buffers/screens-after --compare /tmp/buffers/screens-before
```

Use fresh output directories. This uses only original project EPUB fixtures,
never production state. It records decoded grayscale SHA-256, changed pixel
count and maximum difference for 13 scenes in `screens.csv`; PNG container bytes
can differ with the grayscale encoder. See the
[buffer-memory report](../../docs/archive/buffer-memory.md) for results and the
distinction between payload allocation and measured process memory.

## Position persistence comparison

`redraw.py --experiment persistence` uses `/mnt/us/verso/persistence-check`
with fresh state per run. The baseline is `9f838e4` plus the same two timing
logs retained in the implementation: `Store::save_book` records elapsed atomic
write time and success; the main loop records the `after_present` UI hook.
The write timer includes serialization, directory creation, temporary-file
write, file synchronization, rename and supported directory synchronization.
It excludes the preceding book-record read; the baseline UI-hook timer includes
that read. Both variants use identical logging, probes, book and typography.
The optimized write timer runs on the position thread. Debug logging happens
after each measured scope. No artificial storage delay is added.

Run two independent launches per variant at each pace. Keep `--turn-delay-us
850000` for ordinary turns; use `150000` for rapid input. Each helper contact
lasts 50 ms; helper startup and device scheduling add time, so derive actual
release intervals from the trace instead of treating the delay as the cadence.
The faster 50000-delay trial did not retain one identifiable update per tap and
is excluded. All reported runs must pass the existing 24-update, frame-hash and
flash-order checks. Do not compare coalesced burst output as 24 independent turns.

```sh
python3 scripts/perf/redraw.py after /tmp/after-verso \
  --experiment persistence --turn-delay-us 150000 \
  --book /mnt/us/verso/testbooks/BOOK.epub \
  --probe /tmp/probe --preload /tmp/probe.so \
  --output /tmp/persistence/after-rapid-1.json
```

Add `--memory-check` to one ordinary run per build. Add `--position-check` to
verify the saved page advances by 24, book close/reopen restores exactly the
same page, real sleep/wake preserves it, and normal termination completes the
last displayed turn. `--failure-check` deliberately puts an empty directory at
the isolated position temporary-file path, verifies that a turn cannot replace
the previous record, that close and SIGTERM are refused, and that input still
works before repairing the path and retrying close. It removes that directory
before launcher cleanup even if validation fails. These checks run after the
timed interval; no production records are modified. Use both check flags on an
optimized run. Position strings stay out of exported JSON; only numeric page
and progress values and hashes are saved.

`analyze-redraw.py` also exports per-turn save and UI-hook times, confirms 24
successful writes per timed run, and reports the persistence thread's CPU.
Compare normal and rapid groups separately; process CPU includes the worker.
Memory snapshots from one launch per build are diagnostic, not independent
memory repetitions. See the [persistence report](../../docs/archive/position-persistence.md).

## Home startup comparison

`startup.py` measures Home/library startup, the first Collections visit and an
immediate resume-card open through an isolated launcher copy under
`/mnt/us/verso/startup-check`, using copies of the real data. `--summary
missing` measures the one-time Home migration and saves the rebuilt summary as
the seed; `--summary seeded` uses that seed for warm launches. Use
`--collections-tap 636,230` for the three-tab Home layout (950,200 for the
pre-Home baseline). `--mode lifecycle` runs real sleep/wake on Home and in the
reader, exact reopen, close to Home and exit; it returns the device to awake.
`--large` needs a fixture whose cover keys match the app's key formula; see the
[report](../../docs/archive/home-startup-2026-11-11.md#method-and-boundaries).

```sh
python3 scripts/perf/startup.py after /tmp/after/verso --large /tmp/large \
  --output docs/research/home-startup-2026-11-11/after.json \
  --collections-tap 636,230 --fixture real --mode startup --summary missing --runs 1
python3 scripts/perf/startup.py after /tmp/after/verso --large /tmp/large --installed \
  --output docs/research/home-startup-2026-11-11/after.json \
  --collections-tap 636,230 --summary seeded --mode lifecycle --runs 1
```

## Highlights (text-selection Phase 4)

Both scripts use only the original fixtures, synthetic records and isolated
state under `/mnt/us/verso/highlights-*-check`; run them from the repository root
with the Kindle awake and no reader open. They restore the Kindle UI at the end.

- `highlights.py --binary … --probe … --output DIR`: injected-touch lifecycle
  check. Cross-page save, restart, jump, KOReader import/removal/duplicates, a
  blocked manifest write flushed on sleep, idle CPU, sleep/wake cleanup, then
  landscape with larger text: find, jump and live reflow. Landscape touches are
  injected at physical `(y, 1695 - x)` for logical `(x, y)`.
- `highlights_load.py --binary … --probe … --output DIR`: opening, live relayout,
  swipe page turns and memory with 0, 100 and 1,000 short highlights and 1,000
  dense long ranges (21 paragraphs each, ~100 overlapping per page). Timings
  come from the app's log timestamps and `elapsed_us` fields. Relayout opens
  the reader menu with a swipe up from the bottom edge (the top edge belongs to
  the Top Drawer), taps **Text** and steps the size in the Text sheet.
- `redraw.py --experiment annotations --annotation-count N --annotation-fixture
  reader.epub`: page-turn request latency with N short highlights.

- `bookmarks.py --binary … --probe … --output DIR`: page-bookmark lifecycle with
  injected touches (corner tap, menu ribbon, restart, live size change, jump and
  back, removals, blocked save flushed on sleep, landscape).
- `bookmarks_koreader.py --book KINDLE_EPUB …`: imports a real KOReader sidecar's
  bookmarks from a copy and checks each lands on the same passage in the EPUB.
- `redraw.py … --annotation-kind bookmark`: the same page-turn run with N
  synthetic page bookmarks.

`annotation_fixture.py` builds the synthetic records from `reader.epub`; numeric
results are in `docs/research/highlights-2026-11-15/`.

## Dictionary indexing, sets and counters

`dictionary.py` verifies dictionary-improvements Phase 6 on our portrait
1272×1696 Paperwhite. Install the latest app with `scripts/deploy.sh --no-run`
first, and build `probe` / `probe.so` as above. It uses the installed binary,
real KOReader dictionaries read-only, original one-word EPUBs and isolated
state under `/mnt/us/verso/dictionary-performance-check`. A copy of the normal
launcher owns UI pause/restore. It accepts only test-state paths under that
folder. Ctrl-C/SIGTERM exits through launcher cleanup.

For example, from the repository root:

```sh
bench() {
  python3 scripts/perf/dictionary.py "$@" \
    --probe /tmp/probe --preload /tmp/probe.so --output /tmp/dictionary-results
}
bench cold --language und
# Read "state" from /tmp/dictionary-results/cold.json and use that path below.
COLD_STATE=/mnt/us/verso/dictionary-performance-check/cold-TIMESTAMP
bench cancel
bench warm --name warm-english --language en-US --cache-state "$COLD_STATE"
bench warm --name warm-russian --word свет --language rus --cache-state "$COLD_STATE"
bench warm --name warm-all --language und --cache-state "$COLD_STATE"
bench added --name before-add --language und
bench language --name wrong-tag --word light --language ru \
  --expect-set english --cache-state "$COLD_STATE"
bench language --name missing-tag --language '' \
  --expect-set default --cache-state "$COLD_STATE"
bench language --name cyrillic-in-english --word свет --language en \
  --expect-set russian --cache-state "$COLD_STATE"
bench language --name book-choice --language en --choose-default \
  --expect-set english --cache-state "$COLD_STATE"
```

Cold times every index build, observes preparation and verifies byte-identical
book dismissal. It does not flush Linux's page cache. Warm copies only dictionary
caches, then runs five queries in one session: the first by long press, then four
repeats (`Run.look_up_again`: tap the compact card's heading to open the field,
then **Look up**). It is not five independent launches. English enables eight
English/bilingual dictionaries, Russian the five monolingual Russian ones, and
Default all 13. Concise Oxford leads the timed English/Default preferences.
These fixture choices never replace the user's own preferences.

Cancel turns definition pages while preparation is active, closes the sheet,
turns the book, and checks partial `.tmp`/`.refs` cleanup. Added first indexes
12 dictionaries, exits, copies the installed `eng_rus_full` dictionary into the
isolated native dictionary folder, restarts, exercises cancellation/turns again,
and finishes the retry. It checks that every old cache hash is unchanged and
only the addition builds successfully. Language creates EPUBs with the specified
metadata (an empty argument omits `dc:language`), checks the actual set and hits,
and optionally selects Default through the picker and verifies its persisted
book field after a full restart.

JSON, debug logs, display traces and private PNG captures go into `--output`.
Index logs name metadata paths and report elapsed time/success; search logs
report first-definition readiness, completion, match count and set. Queries and
definition content are not logged. Status RSS is sampled every 200 ms; detailed
settled memory uses `smaps`. A cancelled build is recorded with `success=false`.
Check successful display ioctls and captures together: release-to-SEND_UPDATE
is a request boundary, and multiple changed regions/pressed feedback can produce
several requests per action. It is not optical panel response. Long-press release
is after the lookup has already started, so use worker timers for lookup latency,
not the long-press release marker.

After collecting evidence, stop the test app, verify its launcher PID file is
gone and `awesome` is running, then remove the test folder's temporary state,
cache and dictionary copy. Preserve raw evidence locally; commit aggregate CSVs
only. Check the installed binary, launcher and KUAL hashes against the latest
artifacts, and keep the user's settings/books/positions. Counter feel requires the user to try the actual panel. On 2026-11-16 the user
reported slow-feeling definitions and blending during updates, and requested
documentation without fixes. The 150 ms deadline is unchanged. Results and
measurement limits:
[dictionary.md](../../docs/dictionary.md#dictionary-improvements-kindle-verification-and-performance--2026-11-16).

## Compact card, chained lookups and passage bar

`dictionary-selection.py` verifies [dictionary-selection.md](../../docs/archive/dictionary-selection.md)
Phase 4 on our portrait 1272×1696 Paperwhite, then in landscape. Install the
latest app first (`scripts/deploy.sh --no-run`) and build both probes as above.
It reuses `dictionary.py`'s launcher copy and isolated state, with the
`selection.epub` fixture and the real KOReader dictionaries read-only: a cold
launch builds the indexes, a warm launch runs the scenario. Touches use the
simulator's logical coordinates; landscape ones are injected at
`(y, 1695 - x)`.

```sh
python3 scripts/perf/dictionary-selection.py \
  --probe /tmp/probe --preload /tmp/probe.so --output /tmp/selection
```

It asserts that closing (at depth 0 and 1, top card, Russian card), the dismissed
passage bar, collapse and a cached Back give back the earlier framebuffer
byte for byte, and that a swipe in a definition starts no search. The output
holds captures, the debug log and `selection-results.json`: per labelled touch
the SEND_UPDATE count, first and last request time after release and the union
of the updated rectangles (later requests in a window include settling
redraws, so use the first), worker timers and RSS. The probe cannot keep a finger
down, so the pressed-word invert and long-press chaining are simulator-only.
It removes nothing itself: delete `/mnt/us/verso/dictionary-performance-check`
afterwards.

## Word-form lookups

`wordforms.py` measures base-form lookups with the installed Russian pack
(`/mnt/us/verso/wordforms/ru.vwf`). It reuses `dictionary.py`'s isolated
state, one-word books, launcher copy and dictionary sets, so the same rules
apply: install the latest build first (`scripts/deploy.sh --no-run`), build
both probes, and choose a word that no installed dictionary has as an exact
entry. Each call is one session: a long press (the first miss, which opens the
pack), then repeats through the card's heading and **Look up** (warm misses). It records the pack-open,
candidate and per-dictionary base-form debug lines and the pack's `Size`,
`Rss` and `Pss` from `/proc/<pid>/smaps` after every search.

```sh
wf() {
  python3 scripts/perf/wordforms.py "$@" \
    --probe /tmp/probe --preload /tmp/probe.so --output /tmp/wordforms-results
}
wf --name wf-cold --word людей --language und --repeats 1
# Read "state" from /tmp/wordforms-results/wf-cold.json.
COLD_STATE=/mnt/us/verso/dictionary-performance-check/wf-cold-TIMESTAMP
wf --name wf-russian --word людей --language rus --cache-state "$COLD_STATE"
wf --name wf-all --word людей --language und --cache-state "$COLD_STATE"
```

`rus` selects the Russian set (five dictionaries), `und` Default (all 13).
Afterwards remove the `wf-*` folders.

For Hunspell ([Phase 4](../../docs/archive/universal-languages.md#phase-4-results--2026-11-22)),
install the dictionaries in `/mnt/us/verso/data/hunspell/` and add
`--hunspell de_DE.aff,de_DE.dic` (those files are copied into the run's
isolated state, so only they are loaded), a one-word book of an inflected form
(`--word Häuser --language de`), and `--close-reader` for the RSS after
leaving the book. `--book-text FILE` puts real text (UTF-8) after the word, so
language detection can run: with `--language en` and German text it is the
mistagged book. `--open-pause 0.3 --early-press` looks up before the warm-up,
so the first miss pays the Hunspell open. The JSON gains `language_detection`,
`hunspell_open`, `hunspell_released`, `hunspell_order` (log order of those
lines and the searches) and `memory_after_close`. Results:
[dictionary.md](../../docs/dictionary.md#verification--2026-11-17-russian-base-forms-word-forms-phase-4).

## Background pre-layout

`prelayout.py` checks [background pre-layout](../../docs/reader.md) through the
launcher. Each launch uses isolated state under `/mnt/us/verso/prelayout-check`:
a fresh copy of the real library data and an empty book cache
(`VERSO_CACHE`). Scenarios (`--scenario`, repeatable; all by default):
`control` (`VERSO_PRELAYOUT=0`), `run`, `kill` (open a book during a run),
`orphan` (SIGKILL the app during a run) and `sleep` (powerd button press during
a run; `preventScreenSaver` is set to 0 for the press and restored afterwards).

```sh
python3 scripts/perf/prelayout.py target/armv7-unknown-linux-gnueabihf/release/verso \
  --probe /tmp/probe --preload /tmp/probe.so --output /tmp/perf/prelayout.json
```

Install the build first (`scripts/deploy.sh --no-run`); the script checks its
hash. Pre-layout lines in `verso.log` carry a `prelayout` tag after the
module name. `redraw.py` and `analyze-startup.py` ignore them.
