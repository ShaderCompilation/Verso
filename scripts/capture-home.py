#!/usr/bin/env python3
"""Reproducible headless Home captures, using only original EPUB fixtures.

Run cargo build first. Output and temporary state stay under screenshots/home.
Each state is captured twice: once while the worker migrates/rebuilds the
summary, then again as a warm, summary-only launch.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess

repo = Path(__file__).resolve().parents[1]
out = repo / "screenshots/home"
shutil.rmtree(out, ignore_errors=True)
fixtures = repo / "crates/verso/tests/fixtures"
library = out / "fixtures/Library"
library.mkdir(parents=True)
names = ["Quiet Harbour", "The Long Way", "Notes", "Field Guide", "Letters", "Morning", "Unopened"]
sources = ["library3.epub", "library2.epub", "reader.epub"]
for i, name in enumerate(names):
    shutil.copyfile(fixtures / sources[i % len(sources)], library / f"{name}.epub")


def run(state, png, *steps, small=False):
    env = dict(os.environ, VERSO_BOOKS=str(library), VERSO_DATA=str(state), RUST_LOG="warn")
    if small:
        env.update(VERSO_SIM_SIZE="600x800", VERSO_SIM_DPI="167")
    subprocess.run([str(repo / "target/debug/verso"), "--screenshot", str(png), *steps],
                   env=env, check=True, cwd=repo)


def capture(name, history, small=False):
    """history: list of (file name, last_opened, flags) applied to book records."""
    state = out / "state" / name
    state.mkdir(parents=True)
    run(state, out / f"{name}-scan.png", small=small)  # Index and covers only.
    index = json.loads((state / "library.json").read_text())
    ids = {Path(b["path"]).stem: b["id"] for b in index["books"]}
    (state / "books").mkdir(exist_ok=True)
    for stem, opened, flags in history:
        record = dict(position="fixture-anchor", page=3, progress=1000 + 900 * opened,
                      last_opened=opened, finished="finished" in flags,
                      hidden_from_continue="hidden" in flags)
        (state / "books" / f"{ids[stem]}.json").write_text(json.dumps(record))
    (state / "home.json").unlink(missing_ok=True)  # Like an older installation.
    run(state, out / f"{name}-migrated.png", small=small)
    run(state, out / f"{name}.png", small=small)


populated = [("Quiet Harbour", 7, ()), ("The Long Way", 6, ("finished",)), ("Notes", 5, ()),
             ("Field Guide", 4, ("hidden",)), ("Letters", 3, ()), ("Morning", 2, ())]
capture("populated", populated)
capture("populated-small", populated, small=True)
capture("resume-only", [("Quiet Harbour", 1, ())])
capture("recents-only", [("Quiet Harbour", 3, ("finished",)), ("The Long Way", 2, ("hidden",))])
capture("empty", [])
# Search from Home and back, then Collections: navigation stays available.
state = out / "state/populated"
run(state, out / "populated-collections.png", "--tap", "636,186")
print("captured", sorted(p.name for p in out.glob("*.png")))
