# Agent guide for Verso

Verso is a calm, lightweight e-reader app for jailbroken Kindles, written in Rust. Start with [docs/README.md](docs/README.md); the plan and progress live in [docs/plan.md](docs/plan.md).

## How to work with the user

- **Ask for help when needed.** Feel free to ask the user to do something you can't do yourself. Explain what you need them to do and why.
- **Questions get answers, not code.** If the user asks a question ("how would we…", "what do you think about…", "is it possible to…"), answer it. Don't edit files or start implementing until the user says "go" (or otherwise clearly asks you to build it).
- **Plan before building.** For a new phase or a sizable feature, write or update the plan in `docs/` first and wait for the go-ahead.
- **The user delegates technical decisions.** Pick libraries, architecture and code style yourself, but explain choices in plain language. Record each decision as a short "Decision / Why" note in the relevant doc.
- **Keep docs current.** Update the status markers (✅ 🔄 ⬜) in `docs/plan.md` as work progresses. Mark device facts as **(measured)** or **(to verify)**.

## Commits

- **Commit when you finish an independent piece of work**: an implementation step, a plan phase item, a full feature, a docs update. Don't wait to be asked, and don't pile unrelated changes into one commit.
- Only commit work that builds (`cargo build`, and `cargo kindle-build` if you touched Kindle-only code). Run tests if there are any for the code you changed.
- Write short, clear commit messages: a one-line summary, then a few lines on what and why if needed.
- Never push, force-push, rewrite history, or change the `third_party/FBInk` submodule pointer without asking.

## Project basics

| Command | What it does |
|---|---|
| `cargo sim` | Build and run the desktop simulator |
| `cargo kindle-build` | Release build for the Kindle (armv7, via cargo-zigbuild) |
| `cargo test` | Unit tests |
| `scripts/deploy.sh` | Build, copy to the Kindle, run through the launcher, stream the log (Kindle must be awake, KOReader closed) |
| `scripts/gen-fbink-bindings.sh` | Regenerate FBInk bindings after updating the submodule |

- Layout: `crates/verso` (the app, `desktop` / `kindle` features), `crates/fbink-sys` (FBInk bindings), `third_party/FBInk` (submodule), `assets/`, `dist/`, `scripts/`, `docs/`.
- The Kindle is reachable with `ssh kindle`. It's the user's own reading device: don't run anything destructive on it, and always make sure the Kindle UI comes back after testing.
  - Only write files inside `/mnt/us/verso`, `/mnt/us/extensions/verso` and `/tmp`. Writing to `/dev/fb0` (the screen) is fine, and so is sending test touches to the touchscreen (`/dev/input/event1`) while Verso is running and has grabbed it.
  - Never run `mntroot`, factory reset, firmware updates, or `dd`, and never touch `/var/local`, `/etc` or the jailbreak files.
  - Before running Verso, check the Kindle is awake (`lipc-get-prop com.lab126.powerd state` is `active`) and no other app like KOReader is running. If it isn't ready, ask the user.
  - Never kill or pause other apps (KOReader, Amazon's UI) by hand. Only Verso's launcher may pause and resume the Kindle UI.
  - If the device is left in an odd state, put it back exactly as it was and tell the user.
- Full setup and build details: [docs/setup.md](docs/setup.md).
