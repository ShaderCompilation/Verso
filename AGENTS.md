# Agent guide for Verso

Verso is a calm, lightweight e-reader app for jailbroken Kindles, written in Rust. Start with [docs/README.md](docs/README.md); the plan and progress live in [docs/plan.md](docs/plan.md).

## How to work with the user

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
| `scripts/gen-fbink-bindings.sh` | Regenerate FBInk bindings after updating the submodule |

- Layout: `crates/verso` (the app, `desktop` / `kindle` features), `crates/fbink-sys` (FBInk bindings), `third_party/FBInk` (submodule), `assets/`, `dist/`, `scripts/`, `docs/`.
- The Kindle is reachable with `ssh kindle`. It's the user's own reading device: don't run anything destructive on it, and always make sure the Kindle UI comes back after testing.
- Full setup and build details: [docs/setup.md](docs/setup.md).
