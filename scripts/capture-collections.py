#!/usr/bin/env python3
"""Reproducible headless Phase 6 captures, using only original EPUB fixtures.

Run cargo build first. Output and temporary state stay under screenshots/phase6.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess

repo = Path(__file__).resolve().parents[1]
out = repo / "screenshots/phase6"
out.mkdir(parents=True, exist_ok=True)
fixtures = repo / "crates/verso/tests/fixtures"
root = out / "fixtures/Library"
other = out / "fixtures/Other/Library"
for folder in [root, root / "Fiction/Same", root / "History/Same", other / "Same", root / "Empty"]:
    folder.mkdir(parents=True, exist_ok=True)
shutil.copyfile(fixtures / "library2.epub", root / "Loose book.epub")
for i in range(19):
    shutil.copyfile(fixtures / "reader.epub", root / "Fiction/Same" / f"Book {i:02}.epub")
for folder in [root / "History/Same", other / "Same"]:
    shutil.copyfile(fixtures / "library3.epub", folder / "Quiet.epub")

def capture(name, folder=None, page=0, steps=(), small=False, tab="collections"):
    state = out / "state" / name
    state.mkdir(parents=True, exist_ok=True)
    navigation = {"tab": tab, "folder": str(folder) if folder else None,
                  "folder_pages": {str(folder): page} if folder else {}}
    (state / "library-navigation.json").write_text(json.dumps(navigation))
    env = dict(os.environ, VERSO_BOOKS=os.pathsep.join(map(str, [root, root / "Fiction", other])),
               VERSO_DATA=str(state), RUST_LOG="warn")
    if small:
        env.update(VERSO_SIM_SIZE="600x800", VERSO_SIM_DPI="167")
    subprocess.run([str(repo / "target/debug/verso"), "--screenshot", str(out / f"{name}.png"), *steps],
                   env=env, check=True, cwd=repo)

capture("roots")
capture("root-and-loose", root)
capture("nested", root / "Fiction/Same")
capture("page2", root / "Fiction/Same", 1)
capture("book-menu", root / "Fiction/Same", 1, ["--long-press", "190,650"])
capture("folder-details", None, steps=["--long-press", "190,650"])
capture("full-path", root / "Fiction/Same", steps=["--tap", "500,350"])
capture("small", root, small=True)
capture("all-books", tab="all_books")
capture("missing-folder", root / "Removed")
print(out)
