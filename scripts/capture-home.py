#!/usr/bin/env python3
"""Reproducible headless Home captures, using only original EPUB fixtures.

Run cargo build first. Output and temporary state stay under screenshots/home.
Each state is captured twice: once while the worker migrates/rebuilds the
summary, then again as a warm, summary-only launch.
"""
import json
from pathlib import Path
import shutil

import sim

out = sim.out_dir("home", clean=True)
fixtures = sim.FIXTURES
library = out / "fixtures/Library"
library.mkdir(parents=True)
names = ["Quiet Harbour", "The Long Way", "Notes", "Field Guide", "Letters", "Morning", "Unopened"]
sources = ["library3.epub", "library2.epub", "reader.epub"]
for i, name in enumerate(names):
    shutil.copyfile(fixtures / sources[i % len(sources)], library / f"{name}.epub")


def run(state, png, *steps, small=False, landscape=False, books=library):
    size = ((600, 800), 167) if small else ((1696, 1272), 300) if landscape else None
    sim.run(png, steps, state, books, size=size, quiet=False)


def capture(name, history, small=False, landscape=False):
    """history: list of (file name, last_opened, flags) applied to book records."""
    state = sim.state(out / "state" / name)
    run(state, out / f"{name}-scan.png", small=small, landscape=landscape)  # Index and covers only.
    index = json.loads((state / "library.json").read_text())
    ids = {Path(b["path"]).stem: b["id"] for b in index["books"]}
    (state / "books").mkdir(exist_ok=True)
    for stem, opened, flags in history:
        record = dict(position="fixture-anchor", page=3, progress=1000 + 900 * opened,
                      last_opened=opened, finished="finished" in flags,
                      hidden_from_continue="hidden" in flags)
        (state / "books" / f"{ids[stem]}.json").write_text(json.dumps(record))
    (state / "home.json").unlink(missing_ok=True)  # Like an older installation.
    run(state, out / f"{name}-migrated.png", small=small, landscape=landscape)
    run(state, out / f"{name}.png", small=small, landscape=landscape)


populated = [("Quiet Harbour", 7, ()), ("The Long Way", 6, ("finished",)), ("Notes", 5, ()),
             ("Field Guide", 4, ("hidden",)), ("Letters", 3, ()), ("Morning", 2, ())]
capture("populated", populated)
capture("populated-landscape", populated, landscape=True)
capture("populated-small", populated, small=True)
capture("resume-only", [("Quiet Harbour", 1, ())])
capture("recents-only", [("Quiet Harbour", 3, ("finished",)), ("The Long Way", 2, ("hidden",))])
capture("empty", [])
# No books, with an empty folder that the Library card still counts.
(out / "fixtures/None/Empty").mkdir(parents=True)
for name, kind in [("no-books", {}), ("no-books-landscape", {"landscape": True}), ("no-books-small", {"small": True})]:
    state = sim.state(out / "state" / name, out / "fixtures/None")
    run(state, out / f"{name}.png", books=None, **kind)
# Its buttons: Wi-Fi transfer, and Settings on the library folders.
state = out / "state/no-books"
run(state, out / "no-books-folders.png", *sim.tap((636, 948)), books=None)
# Tap the bottom Library card from Home to open Library.
state = out / "state/populated"
run(state, out / "populated-library.png", *sim.tap(sim.library_card_center((1272, 1696), 300)))
print("captured", sorted(p.name for p in out.glob("*.png")))
