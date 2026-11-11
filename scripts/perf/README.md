# Kindle performance probes

Sources used for the [2026-11-11 comparison](../../docs/research/performance.md).
These are measurement components, not a deploy command. Running a reader still
requires the launcher and device-readiness checks in `AGENTS.md`.

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
not optical response. See [results](../../docs/research/redraw-optimization.md).

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
only aggregate numbers. `search-results.csv` contains the 2026-11-11 device
results. Match queries include empty, broad Latin, Cyrillic and absent strings.
The diagnostic RSS is not normal app RSS. For app startup compare the same copied
index, covers and per-book state between builds, through the normal launcher.
For keyboard timing use the probe's release markers and successful SEND_UPDATE
calls before the next touch; distinguish first update request, last submission
and optical panel response. The report does not measure optical response.

If injected coordinates repeat across fresh evdev sessions, prime both axes
with a harmless corner contact before the first scripted action: the input
subsystem can omit unchanged ABS values. Send touches only while Verso holds the
touchscreen. Always check launcher cleanup and restored Kindle UI after a run.
