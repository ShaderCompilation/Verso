# Contributing to Verso

Thanks for your interest in Verso. This guide covers building the simulator and
Kindle binaries, running checks, our testing and verification approach and
commit conventions.

Before making architectural changes, read [docs/architecture.md](docs/architecture.md)
and [docs/kindle-notes.md](docs/kindle-notes.md). For user-facing behaviour, see
[docs/user-guide.md](docs/user-guide.md).

---

## 1. Setup

The desktop simulator needs only a recent Rust toolchain (`edition = "2024"`)
and git submodules. Cross-compiling for the Kindle additionally uses **Zig**
(as the ARM C/C++ compiler and linker) and `cargo-zigbuild`:

```sh
# Fetch submodules (FBInk, crengine + deps, Hunspell, Dropbear, OpenSSH)
git submodule update --init --recursive --depth 1

# Kindle cross-compile prerequisites (only needed for ARM builds)
rustup target add armv7-unknown-linux-gnueabihf
cargo install cargo-zigbuild
cargo install bindgen-cli   # only if regenerating FBInk bindings
```

Install Zig (e.g. from [ziglang.org](https://ziglang.org/download/) or your
package manager) and make sure `zig` is on `PATH`.

---

## 2. Common commands

| Command | What it does |
| --- | --- |
| `cargo sim` | Build and run the desktop simulator (defaults to Paperwhite 6). |
| `cargo test` | Run workspace unit and integration tests. |
| `cargo test -p verso <filter>` | Run targeted tests while iterating. |
| `cargo kindle-build` | Release build for hard-float Kindles (`verso` + `verso-mdns`). |
| `cargo clippy --all-targets` | Lint the desktop build (also check `--no-default-features --features kindle` for Kindle code). |
| `cargo fmt --check` | Check formatting against `rustfmt.toml`. |
| `scripts/test-launcher.sh` | Power-loss and rollback test suite for `verso.sh` using fake Kindle commands. |
| `scripts/build-wordforms.sh` | Build `target/wordforms/ru.vwf` from OpenCorpora and run its quality check. |
| `scripts/build-ssh.sh` | Build static ARM `dropbear` and `sftp-server` for the Kindle. |
| `scripts/deploy.sh [--no-run]` | Build, stage, copy to a Kindle reachable as `ssh kindle` and run via `verso.sh`. |
| `scripts/gen-fbink-bindings.sh` | Regenerate `crates/fbink-sys/src/bindings.rs` after updating FBInk. |

Aliases (`sim`, `kindle-build`) live in `.cargo/config.toml`.

### Using the simulator

Copy any test EPUBs into the gitignored `testbooks/` folder (or pass
`VERSO_BOOKS=/path/to/books`).

- **Window controls:** left mouse acts as touch; `Q`/`Esc` quits; `S` simulates
  sleep/wake; `V` toggles refresh-mode tinting (red `Fast`, blue `Ui`, green
  `Page`, orange `Full`); `P` saves a screenshot to `screenshots/`; `PgUp`/
  `PgDn` trigger hardware page-turn buttons; `C` toggles the Kaleido 3 color
  preview on `colorsoft`.
- **Device presets:** `--device basic-167|basic-300|pw5|pw6|oasis3|colorsoft|scribe`
  (or `VERSO_SIM_DEVICE`).
- **Headless screenshots:** `--screenshot out.png` renders at device size
  without opening a window and accepts scripted `--screen`, `--tap`,
  `--long-press`, `--hold-drag`, `--press`, `--swipe`, `--key`, `--text` and
  `--edit` steps (see the top of `crates/verso/src/main.rs`).
- **Useful env vars:** `VERSO_DATA` (state folder), `VERSO_FONT_DIRS`
  (`:`-separated), `VERSO_WORDFORMS`, `VERSO_SIM_BATTERY` (`82` or `82+`),
  `VERSO_SIM_SCALE`, `VERSO_SIM_KALEIDO=1`, `VERSO_UPDATE_URL`,
  `VERSO_UPDATE_KEY` (simulator-only minisign `.pub`), `VERSO_LAUNCHER_PROTOCOL=2`.

### Testing on a Kindle

If you have a jailbroken Kindle with SSH access (either Verso's developer SSH
toggle or KOReader's SSH server on port 2222), configure a `Host kindle` entry
in `~/.ssh/config` (using `verso.local` or the Kindle's IP) and run
`scripts/deploy.sh`. Make sure the Kindle is awake and KOReader is closed
before launching Verso.

To collect a read-only hardware inventory from an untested Kindle without
installing Verso:

```sh
ssh kindle sh -s < scripts/collect-diagnostics.sh > kindle-diagnostics.txt
```

---

## 3. Code and documentation guidelines

- **Never edit files inside `third_party/`.** Submodules stay unmodified; fix
  library bugs at build time on a copy, as `CRENGINE_FIXES` in
  `crates/crengine-sys/build.rs` does.
- **Respect cross-module invariants.** In particular: only `verso-book` touches
  crengine, and only presented pages enter `verso-position`. See [docs/architecture.md](docs/architecture.md#6-cross-module-rules).
- **Keep public docs small and accurate:**
  - Cross-module design and invariants → `docs/architecture.md` (~400 lines).
  - Module internals → that module's `//!` doc comment.
  - Non-obvious "don't change this" reasons → a comment right at the code.
  - User-visible behaviour → `docs/user-guide.md` (~400 lines).
  - Kindle hardware/firmware facts → `docs/kindle-notes.md` (~200 lines).
  - Don't grow a doc past its budget without trimming or merging older detail.
- **Licenses:** Verso is `AGPL-3.0-or-later` ([LICENSE](LICENSE)). Any new
  vendored code, font, icon or dataset must be license-compatible and recorded
  in [NOTICE.md](NOTICE.md).

---

## 4. Verification policy

Keep verification focused and fast:

- **One test per rule**, at the lowest level that demonstrates it. Before
  adding a test, check what regression it catches that no existing test covers;
  prefer extending an existing table or scenario over duplicating setup.
- **Targeted tests while working** (`cargo test -p verso <module>`), then the
  full suite (`cargo test`), `cargo clippy` and `cargo fmt --check` once before
  committing. Documentation-only changes need no builds or code checks.
- **Simulator captures** only for new or changed UI, in one orientation unless
  portrait and landscape layouts differ. The `scripts/capture-*.py` scripts
  share `scripts/sim.py` (isolated state, one simulator run, tap positions);
  extend the closest one.
- **On-device checks** only for what actually needs hardware: touch feel, e-ink
  refresh/ghosting, Kindle integration (`powerd`, USB, launcher) and
  performance on paths with a latency or memory target. Measure only when
  touching a performance-sensitive path.
- **Keep tooling compact:** extend existing shared scripts rather than adding
  large one-off scripts.

---

## 5. Commit style

- Keep each commit a self-contained logical unit (one completed feature phase,
  one bug fix or one standalone docs update). Keep unrelated changes out.
- Write a short, clear imperative one-line summary, followed by a brief body
  explaining what changed and why (plus key measurements when relevant).
- Never rewrite shared history or bump `third_party/` submodule pointers
  unintentionally.
