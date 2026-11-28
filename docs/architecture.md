# Architecture

How Verso's crates, layers, threads, rendering pipeline and storage fit
together. Module-internal details live in each module's `//!` doc comments;
Kindle hardware and firmware specifics live in [kindle-notes.md](kindle-notes.md).

---

## 1. Workspace crates

| Crate | Role |
| --- | --- |
| `crates/verso` | Main e-reader binary (`desktop` or `kindle` feature; compile error unless exactly one is enabled). |
| `crates/verso-mdns` | Minimal RFC 6762 IPv4 mDNS responder (`verso-mdns` binary + helper lib, `libc`-only). Separate binary because BusyBox `pidof`/`killall` match by executable name and would confuse a second `verso` process with the main app. Keeps its own crate version so app releases don't churn `tools`. |
| `crates/verso-update` | Release manifests (`latest.json`), Ed25519 minisign verification (`keys/main.pub`, `keys/backup.pub`), component SHA-256 hashing, `installed.json`, staging plans, launcher exit codes/rollback types and the `verso-release` packaging CLI. |
| `crates/verso-wordforms` | Memory-mapped `.vwf` (`memmap2` + `fst`) form→lemma packs for dictionary base-form lookup, shared Unicode/Cyrillic key folding and the `vwf-build` OpenCorpora builder. |
| `crates/fbink-sys` | Builds `third_party/FBInk` from source via `cc` with pre-generated ARM bindings (`kindle` feature only). |
| `crates/crengine-sys` | Builds KOReader's crengine fork, its C/C++ dependencies (`third_party/`: FreeType, HarfBuzz, libunibreak, zlib, libpng, libjpeg-turbo, utf8proc, xxHash) and vendored extended LunaSVG purely via `cc` behind a small C shim (`shim/verso_cre.cpp`). Patches a copy of crengine at build time (`CRENGINE_FIXES` in `build.rs`) so `third_party/` stays unmodified. |
| `crates/hunspell-sys` | Builds `third_party/hunspell` via `cc` behind an exception-catching C++ shim for user-installed `.aff`/`.dic` stemming. |

The release profile optimizes cold dependencies for size (`opt-level = "z"`)
while keeping `verso`, `crengine-sys`, `tiny-skia`, `image`, codec crates and
`cosmic-text`/`swash` at `opt-level = 3`.

---

## 2. Layers and main event loop

```
┌────────────────────────────────────────────────────────────────────┐
│ Screens: Books (Home + Library + Reader) · Settings · Transfer     │
├────────────────────────────────────────────────────────────────────┤
│ App: screen stack, overlays, frame diffing, sleep/wake, rotation   │
├────────────────────────────────────────────────────────────────────┤
│ UI toolkit: Theme (mm/pt), layout, GestureDetector, FrameDiff,     │
│             widgets (no scrolling — everything is paged)           │
├──────────────────────────────┬─────────────────────────────────────┤
│ Graphics (gfx/): Canvas      │ Domain workers: Document (crengine) │
│ (Gray8/Color32), cosmic-text │ Library · Dictionary · Annotations  │
│ + swash, resvg SVG icons     │ Store · Transfer · Updates · Online │
├──────────────────────────────┴─────────────────────────────────────┤
│ Platform trait: display · input · power · frontlight · battery     │
├──────────────────────────────┬─────────────────────────────────────┤
│ Kindle backend               │ Desktop & Headless simulators       │
│ FBInk, evdev, lipc, sysfs    │ winit + softbuffer / scripted PNG   │
└──────────────────────────────┴─────────────────────────────────────┘
```

Only `platform/` knows which device it runs on. Everything above uses physical
units (`mm`/`pt` converted via `Theme::for_dpi`) and `Platform`.

### Event loop (`crates/verso/src/main.rs`)

1. If invoked with `--prelayout`, immediately enter `document::prelayout::run_child()`
   before touching UI or logging.
2. Otherwise open `Platform`, load `Store`, apply saved `Orientation`, build
   `Theme`, spawn `document::thread::Worker`, create the start `Screen` (`Books`)
   and `App`.
3. Loop:
   - Apply any pending frontlight change via `platform.set_frontlight()`.
   - Render via `app.frame()`. If dirty rectangles are returned, call
     `platform.present(canvas, &updates)` and then `app.after_present()`.
   - On the first presented frame, trigger deferred startup: `worker.start()`
     (book font registration), SSH mDNS advertisement, delayed started-marker
     creation (5 s, for update trials), stray crash cleanups (update keep-awake,
     transfer firewall), rollback recording and the update check if one is due.
   - Handle quit (`app.on_quit()`, which may refuse exit if a durable save fails),
     live orientation changes (`prepare_orientation` → drop old stack → rebuild
     via `RelayoutState`), and two-step pending work (runs after a "please wait"
     frame reaches the screen).
   - Drain up to `MAX_BATCH = 64` ready events (`begin_batch` → `next_event` →
     `finish_batch`) so drags and burst touches coalesce into one redraw, then
     run `gestures.on_tick` and `app.on_tick`.

Background threads wake the blocking event wait through a cloneable
`platform::Waker` (`eventfd` polled alongside evdev on Kindle; `EventLoopProxy`
on desktop; condvar in headless mode).

### Overlay stack (`crates/verso/src/app.rs`)

`App` owns `stack: Vec<Box<dyn Screen>>` and draws app-wide layers in fixed
z-order (bottom to top): active `Screen` → `TopDrawer` → queued startup
`Notice` (`RollbackSheet`, `WhatsNewSheet`, `UntestedSheet`) → `UsbPrompt` → `ExitPrompt` →
`Toast`. Screens return `screens::Action` (`Push`, `Pop`, `Exit`,
`ExitWith(i32)`, `Orientation`, `OpenTopDrawer`) instead of mutating `App`.

---

## 3. Threads and child processes

| Worker | Owner | Responsibility |
| --- | --- | --- |
| **Main UI thread** | Process | Owns `Platform`, `App`, `Canvas`, `FrameDiff`, `Screen` stack and `GestureDetector`. Never blocks on engine, disk scan or network I/O. |
| `verso-book` | `document::thread::Worker` (`Rc`) | **Sole owner of `CreEngine` / `CreDocument`.** Registers fonts on `Command::Start`, opens books, lays out typography, renders pages + neighbor prefetch into a bounded `PixelPool` (≤4 buffers / 32 MiB), resolves TOC, links, selection, incremental search (60 ms slices) and annotations (a relayout keeps checked ones painted and only re-locates them), and calls `malloc_trim(0)` on book close. |
| `verso --prelayout` | `document::prelayout::Runner` (child process) | Runs at idle (`nice 19`, `IOPRIO_CLASS_IDLE`, `PR_SET_PDEATHSIG(SIGKILL)`) to pre-populate crengine's `.cr3` cache for up to 8 likely-next books. Killed before any interactive book open, followed by `verso_cre_cache_reload()`. Never writes Verso JSON data. |
| `verso-library` | Shared via `Weak` in `Store` | Incremental EPUB and Markdown scanning, metadata/cover extraction, tile thumbnails, `library.json`/`home.json`/`opened.json` rebuilds, folder hierarchy and off-thread Home/Library warm-up. Pauses while reading or during Wi-Fi transfer. |
| `verso-thumb` | `screens::books::Thumbnails` | Decodes cached cover PNGs (bounded cache: 6 MiB `Gray8`, 18 MiB `Color32`) and prefetches visible/nearby library pages and Home covers. |
| `verso-dictionary` | `Store::dictionary_service` (`OnceLock`, lazy) | **Sole owner** of StarDict indexes/definitions, `dictionaries.json` writes, `.vwf` mmaps and Hunspell handles (with `malloc_trim(0)` on release). |
| `verso-annotations` | Per open `Reader` | Checks KOReader `.sdr` mtime before hashing, computes SHA-256 book fingerprint, loads/commits immutable annotation records + manifest, KOReader highlight import and Markdown export. |
| `verso-position` | Per open `Reader` | Single ordered FIFO writing `books/<id>.json` (`Presented`, `Status`, `Language`, `Seen`), `reading-speed.json` and syncing `home.json`/`opened.json`. |
| `verso-navigation` | `Books` | Coalescing async writer for `library-navigation.json` and the library sort choice (`library_sort` in `settings.json`, serialized with other settings writers through `Store::update_settings`), queued after the frame presents, so FUSE fsyncs never stall UI turns. |
| `verso-net` | `App` (`DrawerWorker`, lazy) | Runs `lipc` Wi-Fi toggles and SSH start/stop off the UI thread. |
| `transfer` / `web-*` | `transfer::Session` | Active only while Wi-Fi transfer is open: HTTP server (≤8 connection threads), firewall port 80, `verso-mdns`, 10-minute idle stop, 60 s sleep-timeout refresh. |
| `update-check` / `update-wake` / `update-download` | `update::Updates` | One-shot HTTPS manifest check (`ureq` + `rustls`; after a wake, a due one waits up to 2 min for Wi-Fi), archive download, hash/signature check and staging. |
| `online-lookup` | `online::Service` | Wikipedia and translation (Google `gtx` or optional DeepL Free/Pro API) requests: at most 3 short-lived threads, each checking Wi-Fi before opening a socket; blocking calls can't be aborted, so each tab's `online::Worker` drops obsolete replies by request ID. |
| `lipc-wait-event` ×2 | `Kindle::power` / `Kindle::_hal` (children of threads `verso-power` / `verso-hal`) | Stream `com.lab126.powerd` events and `com.lab126.hal` USB events (`PR_SET_PDEATHSIG(SIGTERM)`). The powerd one exiting emits `Event::Quit` so Verso returns cleanly via the launcher; the hal one exiting falls back to powerd for cables. |
| `verso-remotes` / `verso-remote-port` | `platform::kindle` | Page-turner remotes: opens the ones that appear in `/dev/input` (inotify) and hands them to the UI thread; answers page turns on 127.0.0.1:8323. |
| One-shot helpers | Various | `verso-input-init` (touch and buttons open during the first frame), `verso-postpone` (idle sleep, while a remote is used), `verso-frontlight` (startup read), `verso-language` (`whatlang`), `verso-koreader` (bulk import), `verso-report` (`report.sh`), `started` (5 s trial marker), `ssh-restart`. |

---

## 4. Drawing, damage tracking and e-ink refresh

### Dual-mode canvas and rotation

- `Canvas` (`gfx/canvas.rs`) selects `Gray8` (1 byte/px) or `Color32`
  (4 bytes/px, when `has_color_panel && bpp == 32`) once at startup, branching
  per draw call rather than per pixel. Monochrome Kindles keep 1 byte/px
  buffers, 8-bit crengine rendering and grayscale cover caches.
- Kindle panels cannot rotate in hardware (`canRotate = 0`). In `Landscape`
  (90° clockwise), Verso draws onto a `height × width` canvas and rotates into
  the framebuffer in 4×4 blocks with ARM `PLD` prefetch. Sleep covers stay
  portrait by rendering upright and rotating 90° onto the landscape canvas.
- The simulator's `colorsoft` preset can preview Kaleido 3 color (`C` key,
  `platform/kaleido.rs`): 300-dpi luma with 2×2 averaged 150-dpi muted chroma.
- **Dark mode:** screens draw light-polarity pixels; `Writer` flips them on the
  copy into `/dev/fb0` (and simulators), while covers, behind-cover strips,
  sleep covers and crengine book images/colored text pre-flip so pictures stay
  normal and colored text keeps its hue with inverted luma. Live switching defers
  `canvas.dark` and `App::frame()` up to 500 ms while `Reader` re-renders, then
  commits with one `Full` refresh.

### Frame comparison and refresh planning (`ui/damage.rs`, `app.rs`)

Screens redraw the full logical scene into `Canvas`; `FrameDiff` compares it
against the previous frame in **16×16 tiles**:

- **Full-screen bypass:** when `forced` already covers the whole screen at
  `>= Ui` (page turn, wake, first frame) with no stronger local rect, tile
  diffing is skipped and `mark_canvas_as_baseline()` defers copying `Canvas`
  into `prev` until a partial frame actually needs to diff (`sync_baseline`).
- **Skip rects (`changed_outside`):** pixels inside forced `Full` rects (such
  as grid-only library paging) copy straight into `prev` without comparison;
  remaining bands check 16-row strips in 64-byte chunks before per-tile tests.
- **Hints & ordering:** changed tiles take the strongest overlapping widget
  hint (default `Ui`), merge into rectangles, drop rects subsumed by stronger
  forced updates, and sort **non-`Full` before `Full`** so non-blocking flashes
  never stall same-frame chrome updates.
- **Frame check (`frame_watch.rs`):** after the launch and wake frames the log
  says whether the framebuffer still holds Verso's frame (2 s later on the
  Kindle, and at the first touch). Only after Amazon's own sleep screen does it
  also repaint: every 0.1 s for 5 s, the whole screen with `Ui` if overwritten.
- **`fast_debt`:** every `Fast` (1-bit B&W `DU`) rect is remembered and given a
  clean `Ui` pass automatically once the finger lifts.

### Waveforms and flash cadence

| `RefreshMode` | Purpose | Light mono | Light color | Dark mono (eclipse) | Dark color (eclipse) | Flashes? |
| --- | --- | --- | --- | --- | --- | --- |
| `Fast` | Press feedback, slider drags, selection | `WFM_DU` | `WFM_DU` | `WFM_DUNM` (MTK) / `WFM_DU` (i.MX) | `WFM_DUNM` / `WFM_DU` | No |
| `Ui` | Menus, chrome, `fast_debt` cleanup, wake strip | `WFM_GL16` (MTK) / `WFM_GC16` (i.MX) | `WFM_GL16` | `WFM_GLKW16` | `WFM_GLKW16` | No |
| `Page` | Reading page turns | `WFM_REAGL` | `WFM_GLRC16` | `WFM_GLKW16` | `WFM_GLRCK16` | No |
| `Full` | Images, chapter starts, periodic flash, wake, library grid | `WFM_GC16` | `WFM_GCC16` | `WFM_GCK16` | `WFM_GCCK16` | Yes |

Eclipse modes gate on `has_eclipse_wfm` (falling back to light waveforms when
absent), and `FlashFence` gates only on `RefreshMode::Full`.

- **Non-blocking flashes (`platform/flash.rs`):** `Display::present` records
  FBInk's refresh marker in `FlashFence` instead of blocking ~300 ms immediately,
  and awaits it lazily right before the next framebuffer write, sleep handoff or
  exit.
- **Periodic cadence (`reader/mod.rs`):** flashes every `page_flash` turns
  (default `0` = never). Pages with images and TOC chapter starts (`level <= 2`)
  always request `Full` and reset the counter. Rapid turns (`< 400 ms` apart)
  defer periodic flashes up to `2 × page_flash` turns.

---

## 5. Persistence and file formats

All user data lives under `/mnt/us/verso/data/` on Kindle (`$XDG_DATA_HOME/verso`
or `VERSO_DATA` on desktop). Every durable write goes through
`store::atomic_bytes` (write `<path>.tmp`, `sync_all`, `rename`, sync parent dir
tolerating `EINVAL`/`ENOSYS` on FAT/FUSE; each folder's existence and dir-sync
support are checked once per run).

```
<data>/
  settings.json                Global Settings (field-by-field fallback on parse error)
  library.json                 LibraryIndex (v1): book paths, ids, stamps, metadata, covers
  home.json                    Derived Home summary (v4, ≤64 KiB)
  opened.json                  Derived opened-order ranks for folder previews (≤256 KiB)
  library-navigation.json      Saved Library folder and page
  reading-speed.json           Global seed reading speed (chars/sec)
  books/<id>.json              Authoritative BookState (XPointer, page, progress, finished,
                               status_at, rereading, language, reading_speed, koreader_seen, wikipedia_language)
  covers/<key>[-s|-WxH].png    Cached cover thumbnails (sizes tracked in covers/sizes.json)
  annotations/book-<id>.json   Atomic per-book manifest (v1, record ids + import tombstones)
  annotations/<rec-id>.json    Immutable highlight/bookmark records (never edited in place)
  exports/                     Exported Markdown highlights
  dictionaries/                Native StarDict folders
  hunspell/                    User-installed Hunspell .aff/.dic pairs
  dictionary-cache/            Rebuildable StarDict search indexes
  dictionaries.json            Sets, ordering, enabled/disabled flags
  dictionary-folders.json      Extra StarDict folders (Kindle default: KOReader's dict dir)
```

App-level files sit beside `data/` in `/mnt/us/verso/`: `installed.json`,
`cache/` (crengine `.cr3` cache capped at 64 MiB, `fonts.list`, `prepared.json`),
`running` (the launcher's session marker), `update/` (`state.json`, `staged/`,
`previous/`, `trial`, `rollback.json`, `seen-version`, `journal-*`), `reports/`, `ssh/`, `bin/`,
`tools/`, `fonts/` and `wordforms/`.

### Release notes (`verso-update`, `update/whats_new.rs`)

The manifest's `details` holds the full notes (`MAX_DETAILS` 16 000 bytes, the
format of `dist/release-notes/<VERSION>.txt`: `## ` headings, `- ` bullets,
paragraphs); `Offer.notes` carries it, or `notes` from older manifests. `notes`
stays for builds up to beta.10, which reject over `MAX_NOTES` (2000 bytes):
`verso-release pack` derives it from the same text (`legacy_notes`: no
headings, about 8 lines / 600 bytes, then a pointer to What’s new). `build.rs`
embeds every `dist/release-notes/*.txt`, so What’s new works offline. At start
`Updates::whats_new()` compares this version with `update/seen-version` (else
the launcher’s previous version): a fresh, dev or older install remembers
quietly; a newer one shows up to five versions’ notes until OK.

### Position-writer ordering rule (`store/position.rs`)

1. **Presented-only:** a page enters `verso-position` only from
   `Reader::after_present()` after `platform.present` returns; speculative or
   prefetched pages never enter the FIFO.
2. **Single FIFO:** `Presented`, `Status` (finished/unfinished/rereading),
   `Language`, `Seen` (KOReader) and `Seed` all serialize through `verso-position`
   so background metadata never clobbers a newer reading position. Consecutive
   `Presented` saves coalesce to the newest without crossing barriers or status
   changes.
3. **Authoritative vs derived:** `books/<id>.json` is authoritative; `home.json`
   and `opened.json` are derived caches serialized under `SUMMARY_WRITE` and
   updated only on fresh open, status change and `Flush`.
4. **Barriers:** close, sleep, relayout and quit send `Flush`, which retries any
   failed save and acknowledges only after durable disk commit.
5. **External cache coherence:** `StoreBackend` tags its cached `BookState` with
   `Store::book_saves` so routine page turns skip FUSE re-reads yet pick up
   external writes (`set_finished`, bulk import, etc.).

### Freezing writes for live-data restart

When developer-mode Wi-Fi transfer edits live files under `data/`, `Store` sets
`live_data_edited`. Choosing **Restart** calls `Store::freeze()`, registering
the data root so `atomic_bytes` and `remove_book` become no-ops during teardown
and Verso exits with `EXIT_RESTART` (87) for the launcher to relaunch cleanly.

---

## 6. Cross-module rules

- **crengine is single-threaded and generational:** only `verso-book` calls
  `crengine-sys`. Every reply carries a monotonic `generation` (preserved across
  orientation rebuilds via `RelayoutState::book_generation`) and request ID so
  stale replies are always dropped.
- **Complete font registration:** every font in the configured font directories
  must register (via `fonts.list` or file open) so crengine's `font_list_hash`
  stays stable across runs; omitting a font invalidates every cached `.cr3`.
- **Prelayout coordination:** always call `prelayout.before_open(&engine)` (kill
  child + `verso_cre_cache_reload()`) before opening a book.
- **No `Store` self-cycles:** worker clones strip `dictionary_service` and
  `library_worker` handles (`Store::dictionary_context`, `Store::library_worker`).
- **Sort keys are built once** per index or sort change (`library::sort::Keys`;
  Recently read also when the opened order changes), so comparisons never fold
  text or parse numbers.
- **Immutable annotations:** editing a note writes a new `<rec-id>.json` and
  atomically swaps `book-<id>.json`; never mutate an existing record file in
  place.
- **Launcher owns Kindle UI processes:** Rust never signals `awesome`,
  `JunoStatusBarDriver`, `pillow` or `volumd` directly; native sleep handoff
  uses acknowledged `SIGUSR1`/`SIGUSR2` with `verso.sh`.
- **One HTTPS stack:** updates and online lookup both go through `net.rs`
  (`ureq`, bundled `webpki-roots`, bounded replies). Looked-up text comes from
  someone's book: it never goes into the log, and replies are never stored.
  Optional DeepL API keys are sent only in `Authorization: DeepL-Auth-Key`,
  never echoed by Wi-Fi transfer GETs, and redacted from problem reports.
- **Never edit `third_party/`:** patch library code at build time instead.
