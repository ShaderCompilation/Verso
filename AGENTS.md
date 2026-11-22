# Agent guide for Verso

Verso is a calm, lightweight e-reader app for jailbroken Kindles, written in Rust. Start with [docs/README.md](docs/README.md); the plan and progress live in [docs/plan.md](docs/plan.md).

## How to work with the user

- **Ask for help when needed.** Feel free to ask the user to do something you can't do yourself. Explain what you need them to do and why.
- **Questions get answers; instructions get action.** If the user is exploring an idea or asking for advice ("how would we…", "what do you think about…", "is it possible to…", "can we…", "what would we do next"), research and answer, and wait for a go-ahead before editing files or implementing. A request to do work ("implement Phase 5", "fix this", "change the wording", pasted text with "implement this") is already authorization to carry it out; don't ask for a second go-ahead just because it is a new phase or a sizable feature.
- **Use the existing plan.** `docs/plan.md` is the implementation plan. When asked to implement a phase or item, work from it and keep its progress current. Create a separate plan or wait for plan approval only when the user explicitly asks for that workflow. Make routine technical decisions as part of implementation and record them in the relevant docs.
- **Show the tradeoffs on significant technical decisions.** Make routine code-level decisions yourself. For significant ones (adding a library, choosing an engine or approach), explain the tradeoffs in plain language with a recommendation before adopting it. Record each decision as a short "Decision / Why" note in the relevant doc.
- **Keep docs current.** Update the status markers (✅ 🔄 ⬜) in `docs/plan.md` as work progresses. Mark device facts as **(measured)** or **(to verify)**.
- **No backwards compatibility needed yet.** Verso hasn't launched and only runs on the user's test Kindle. Don't keep legacy data migrations, deprecated config fields, or compatibility shims—cleanly replace formats and code when a design changes. (Still keep the user's books and reading progress intact on the device.)
- **Push back if appropriate.** If the user asks for features that are very hard to implement, unconventional or are likely to cause performance issues state that plainly.

## Commits

- **Commit once per implementation phase.** A commit is one finished phase from `docs/plan.md` (with its status markers updated), not each checklist item or small step along the way. Keep working through the steps of the phase and commit when the phase is done. If a phase is genuinely large, split it into a few substantial, self-contained commits rather than a stream of small ones.
- **Bug fixes get their own commit.** When asked to fix a bug or a set of bugs (e.g. findings from `docs/bugs-*.md`), commit right after fixing and validating, without waiting to be asked. Don't commit if any requested fix wasn't done or needs the user's attention or decision: report what's open and wait.
- **Don't wait to be asked to commit**, but don't commit mid-phase just because some steps are finished. A standalone docs update requested on its own is its own commit; docs written as part of a phase belong in that phase's commit.
- **Keep unrelated changes out of a commit.** If other work is already in the workspace, leave it for its own commit rather than bundling it in.
- **Documentation-only changes need no code checks.** If you did not change code, do not run builds, tests, linters or formatters. Unrelated code changes already in the workspace do not count as your changes.
- **Validate code changes once, before committing.** For the code you changed, make sure relevant tests pass, `cargo clippy` reports no errors and `cargo fmt --check` is clean (run `cargo fmt` only when it reports changes). Reuse successful results while the code and build configuration are unchanged. A test run counts as a build, so run `cargo build` (or `cargo kindle-build` for Kindle-only code) only if nothing has compiled the latest changes yet.
- Write short, clear commit messages: a one-line summary, then a few lines on what and why if needed.
- Never push, force-push, rewrite history, or change any `third_party/` submodule pointer without asking.

## Project basics

| Command                                          | What it does                                                                                                                                             |
| ------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `cargo sim`                                      | Build and run the desktop simulator                                                                                                                      |
| `cargo kindle-build`                             | Release build for the Kindle (armv7, via cargo-zigbuild)                                                                                                 |
| `cargo test`                                     | Unit tests                                                                                                                                               |
| `scripts/deploy.sh`                              | Build, copy to the Kindle, run through the launcher, stream the log (Kindle must be awake, KOReader closed)                                              |
| `scripts/test-launcher.sh`                       | Test the launcher's update switch-over and rollback with fake Kindle commands and app versions, including a power loss before every step (docs/setup.md) |
| `scripts/kindle-awake.sh on\|off\|status`        | Keep the Kindle from sleeping on its idle timeout during device work, and turn that off again                                                            |
| `scripts/kindle-screenshot.sh [out.png]`         | Save a PNG of the Kindle's current screen to `screenshots/kindle/` (read-only, over SSH)                                                                 |
| `scripts/build-ssh.sh`                           | Build the Kindle SSH server (Dropbear, sftp-server); `deploy.sh` runs it                                                                                 |
| `scripts/build-wordforms.sh`                     | Build the Russian word-form pack (`target/wordforms/ru.vwf`) from the kept OpenCorpora export and run its quality list                                   |
| `scripts/package.sh` / `scripts/sign-release.sh` | Build a release in `target/package/<version>/`, then sign it (the user signs: the key has a password). See docs/setup.md#releasing                       |
| `scripts/gen-fbink-bindings.sh`                  | Regenerate FBInk bindings after updating the submodule                                                                                                   |

- Layout: `crates/verso` (the app, `desktop` / `kindle` features), `crates/verso-mdns` (the small mDNS responder that runs next to the SSH server), `crates/verso-wordforms` (word-form packs: reader for the app, `vwf-build` builder), `crates/verso-update` (release manifests and signatures, `verso-release` packaging tool), `crates/fbink-sys` (FBInk bindings), `crates/crengine-sys` (builds and binds crengine), `crates/hunspell-sys` (builds Hunspell, safe stemming wrapper), `third_party/` (git submodules: FBInk, crengine and its libraries, Hunspell, Dropbear, OpenSSH), `assets/`, `dist/`, `scripts/`, `docs/`.
- Don't edit files inside `third_party/`: changes there aren't part of Verso's history and a submodule update wipes them. Fix library code at build time instead, the way `CRENGINE_FIXES` in `crates/crengine-sys/build.rs` patches a copy of crengine.
- The Kindle is reachable with `ssh kindle`. It's the user's own reading device: don't run anything destructive on it, and always make sure the Kindle UI comes back after testing.
  - Only write files inside `/mnt/us/verso`, `/mnt/us/extensions/verso` and `/tmp`. Writing to `/dev/fb0` (the screen) is fine, and so is sending test touches to the touchscreen (`/dev/input/event1`) while Verso is running and has grabbed it.
  - Never run `mntroot`, factory reset, firmware updates, or `dd`, and never touch `/var/local`, `/etc` or the jailbreak files.
  - Before running Verso, check the Kindle is awake (`lipc-get-prop com.lab126.powerd state` is `active`) and no other app like KOReader is running. If it isn't ready, ask the user.
  - Never kill or pause other apps (KOReader, Amazon's UI) by hand. Only Verso's launcher may pause and resume the Kindle UI.
  - If the device is left in an odd state, put it back exactly as it was and tell the user.
  - **KUAL always runs the latest build.** The app is in active development and the user tests every change on the device, so there is no separate "production" build to protect. After device work, the normal `/mnt/us/verso/verso` (and the launcher and KUAL files) must match the latest build: `scripts/deploy.sh` (or `--no-run`) installs it. Isolated copies for experiments are fine, but install the latest build when done instead of leaving an older one in place. Keep the user's settings, reading state and books.
- Full setup and build details: [docs/setup.md](docs/setup.md).
