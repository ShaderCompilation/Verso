# Agent guide for Verso

Verso is a calm, lightweight e-reader for jailbroken Kindles, written in Rust:
crengine for books, FBInk for the e-ink screen, and a desktop simulator for
development. It is in closed beta.

## What to read

- **Always:** this file, the code you're changing, and its `//!` module comment.
- **For design work:** [docs/architecture.md](docs/architecture.md). For device
  facts: [docs/kindle-notes.md](docs/kindle-notes.md). For what the user sees:
  [docs/user-guide.md](docs/user-guide.md).
- **The plan the task names**, if any. Read other plans, `notes/archive/` or
  `notes/research/` only when asked.
- **Searches across many files** (where is X used, how do the screens do Y) go to
  a read-only research subagent where you have one, so its file dumps stay out
  of your context.

`notes/` is the maintainer's private, gitignored notes folder (plans, research,
archive, roadmap, skills). It may not exist in your checkout. It is its own git
repository: commit changes to it with `git -C notes …`, never in this one, and
don't link paths inside it from public files.

## Where things live

| Path | What |
|---|---|
| `crates/verso` | The app (`desktop` or `kindle` feature): `app.rs`, `screens/`, `ui/`, `gfx/`, `document/`, `store/`, `platform/`, `transfer/`, `update/` |
| `crates/crengine-sys` | Builds and binds crengine; `CRENGINE_FIXES` in `build.rs` patches a copy |
| `crates/fbink-sys`, `crates/hunspell-sys` | FBInk bindings; Hunspell with a safe stemming wrapper |
| `crates/verso-update` | Release manifests, signatures, the `verso-release` packaging tool |
| `crates/verso-wordforms`, `crates/verso-mdns` | Word-form packs (`vwf-build`); the mDNS responder |
| `third_party/` | Git submodules. Never edit; fix at build time instead |
| `dist/` | Launcher (`verso.sh`), KUAL entry, release components |
| `scripts/` | Deploy, package, Kindle tools, `perf/`, simulator captures (`capture-*.py` on the shared `sim.py`) |
| `crates/verso/tests/fixtures` | Original EPUB fixtures for tests and captures |

## Commands

| Command | What it does |
|---|---|
| `cargo sim` | Desktop simulator (`-- --screenshot out.png --tap x,y …` renders headless; see `main.rs`) |
| `cargo test` / `cargo test -p verso <filter>` | Full suite (seconds) / targeted tests |
| `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check` | Lint and format checks |
| `cargo kindle-build` | Release build for the Kindle (armv7, cargo-zigbuild) |
| `scripts/deploy.sh [--no-run]` | Build, install on the Kindle, run through the launcher, stream the log |
| `scripts/kindle-awake.sh on\|off\|status` | Keep the Kindle from sleeping during device work |
| `scripts/kindle-screenshot.sh [out]` | PNG of the Kindle's screen (read-only) |
| `scripts/test-launcher.sh` | Launcher update and rollback tests with fake Kindle commands |
| `scripts/package.sh` / `scripts/sign-release.sh` | Build a release / sign it (the user signs: the key has a password) |

## Rules that break things

- **Threads.** The main UI thread never blocks on the engine, disk scans or the
  network. Only `verso-book` calls crengine, and stale replies are dropped by
  generation. Only `verso-dictionary` owns dictionary files and Hunspell. Call
  `prelayout.before_open` before opening a book. Worker clones of `Store` strip
  the service handles (no self-cycles). Table: architecture.md §3.
- **Position writes.** A page enters `verso-position` only from
  `Reader::after_present()`, after it reached the screen. Every write about a
  book (position, status, language, seen) goes through that one FIFO.
  `books/<id>.json` is authoritative; `home.json` and `opened.json` are derived.
  Close, sleep, relayout and quit send `Flush`. Details: architecture.md §5.
- **Durable writes** go through `store::atomic_bytes`. Annotation records are
  immutable: write a new record and swap the manifest.
- **Damage and refresh.** Screens redraw the whole scene; `FrameDiff` finds what
  changed. Widgets *hint* a mode for their area and screens *force* refreshes;
  don't present from a screen. `Fast` is black and white only and leaves debt
  that gets a `Ui` pass on lift; `Full` flashes, so use it only where
  architecture.md §4 says. Keep refreshed areas tight: a change that grows them
  costs time on e-ink.
- **Fonts.** Every font in the configured folders must register, or every cached
  `.cr3` is invalidated.
- **The launcher owns the Kindle UI.** Rust never signals Amazon's processes.

## On the Kindle

It's someone's own reading device (`ssh kindle`). Follow the `verify` skill
(`notes/skills/verify/`) for readiness, wake/app-closing and verification steps;
these safety invariants always hold:

- Write only inside `/mnt/us/verso`, `/mnt/us/extensions/verso` and `/tmp`, plus
  `/mnt/us/documents/Verso.sh` (the library launcher). Writing `/dev/fb0` and
  sending touches to `/dev/input/event1` while Verso has grabbed it are fine.
- Never run `mntroot`, `dd`, a factory reset or a firmware update, and never
  touch `/var/local`, `/etc` or the jailbreak files.
- Never kill or pause Amazon's UI by hand; only Verso's launcher pauses and
  resumes it. Stopping a running Verso or KOReader cleanly (`SIGTERM`) is fine.
- Keep the user's books, settings and reading state; experiment in isolated
  state under `/mnt/us/verso/<name>-check`. When done, leave the Kindle UI
  running, awake mode off, and the latest build installed
  (`scripts/deploy.sh --no-run`).

## What goes where

| Knowledge | Goes in |
|---|---|
| How the parts fit: crates, layers, threads, data flow, file formats, cross-module rules | `docs/architecture.md` (about 400 lines) |
| How one module works inside | Its `//!` doc comment |
| Why code looks odd and must not be "fixed" | A comment at that code |
| What the user sees and does | `docs/user-guide.md` (about 400 lines) |
| Device facts | `docs/kindle-notes.md` (about 200 lines) |
| Measurements, timings, verification results | The commit message; `notes/` if worth keeping |
| History, rejected options, planning debate | `notes/` only |

Test each line: would an agent make a mistake without it, and can't it find
this out from the code in a minute? Otherwise leave it out. A change that would
push a doc past its budget first cuts or merges something. When a plan is done,
fold at most about 15 lines into the public docs, fix what the feature made
untrue, and move the plan to `notes/archive/` (listed in its `README.md`).

## Tests

- **One test per rule**, at the lowest level that shows it. Before writing one,
  ask what regression it catches that no other test does. Extend an existing
  table or scenario rather than adding a near-copy.
- Targeted tests while working; the full suite, `cargo clippy --all-targets -- -D warnings`
  and `cargo fmt --check` once before committing, plus `cargo kindle-build` when
  changing Kindle-only code.
- Simulator captures only for new or changed UI, in one orientation unless the
  layouts differ. Device checks only for what needs the device: touch, refresh,
  device integration, performance on paths with a target.
- Extend the shared helpers (`scripts/sim.py`, `scripts/perf/redraw.py`) instead
  of writing new scripts from scratch.

## Commits

- **One commit per finished plan phase**, with its status markers updated in the
  plan (and committed in `notes/`). Split a genuinely large phase into a few
  self-contained commits, not a stream of small ones. Don't wait to be asked.
- **Bug fixes** get their own commit right after fixing and validating, unless
  something requested is undone or needs the user's decision: then report.
- Keep unrelated workspace changes out of a commit.
- **Docs-only changes need no builds, tests or linters.** For code, validate once
  before committing and reuse passing results while the code is unchanged; a
  test run counts as a build.
- Messages: a one-line summary, then a few lines on what and why, and any
  measurements.
- Never push, force-push, rewrite history or change a `third_party/` submodule
  pointer without asking.

## Working with the user

- **Questions get answers; instructions get action.** "How would we…", "what do
  you think…", "can we…": research and answer, then wait. "Implement Phase 5",
  "fix this", pasted text with "implement this": do it, no second go-ahead.
- **Work from the plan the task names** and keep its status markers (✅ 🔄 ⬜)
  current. Make routine decisions yourself. For significant ones (a new library,
  an engine, an approach) explain the tradeoffs with a recommendation first, and
  record a short "Decision / Why" note in the plan.
- **Ask for help** when you need the user to do something, and say why.
- **Push back** plainly on features that are very hard, unconventional or likely
  to hurt performance.
- **Closed beta compatibility:** refactor internal code cleanly without shims,
  but keep existing users' books, settings, reading state and annotations
  working across updates (migrate or fall back cleanly when changing persisted
  formats).
