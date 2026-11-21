#!/usr/bin/env python3
"""Reproducible headless Library & folder collection captures, using original EPUB fixtures.

Run cargo build first. Output and temporary state stay under screenshots/phase6.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess

repo = Path(__file__).resolve().parents[1]
out = repo / "screenshots/phase6"
shutil.rmtree(out, ignore_errors=True)
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


def mm(v, dpi):
    return round(v * dpi / 25.4)


def library_card_center(size, dpi):
    w, h = size
    margin, target = mm(5.0, dpi), mm(9.0, dpi)
    card_h = max(target, min(mm(13.2, dpi), h // 8))
    return w // 2, h - margin - card_h // 2


def grid_cell_center(size, dpi, col, row):
    w, h = size
    margin, top_bar, target = mm(5.0, dpi), mm(13.0, dpi), mm(9.0, dpi)
    cols, rows = (6, 2) if w > h else ((3, 3) if w < mm(100.0, dpi) else (4, 3))
    gap_x = mm(2.6 if cols == 6 else (2.8 if cols == 3 else 3.4), dpi)
    gap_y = mm(2.0, dpi)
    grid_top = top_bar + mm(2.2, dpi) + mm(11.0, dpi) + mm(2.9, dpi)
    grid_w = w - 2 * margin
    grid_h = max(target, h - margin - grid_top)
    cell_w = (grid_w - gap_x * (cols - 1)) // cols
    cell_h = (grid_h - gap_y * (rows - 1)) // rows
    return margin + col * (cell_w + gap_x) + cell_w // 2, grid_top + row * (cell_h + gap_y) + cell_h // 2


def capture(name, folder=None, page=0, steps=(), size=(1272, 1696), dpi=300):
    state = out / "state" / name
    state.mkdir(parents=True, exist_ok=True)
    navigation = {"folder": str(folder) if folder else None,
                  "folder_pages": {str(folder): page} if folder else {}}
    (state / "library-navigation.json").write_text(json.dumps(navigation))
    env = dict(os.environ, VERSO_BOOKS=os.pathsep.join(map(str, [root, root / "Fiction", other])),
               VERSO_DATA=str(state), RUST_LOG="warn",
               VERSO_SIM_SIZE=f"{size[0]}x{size[1]}", VERSO_SIM_DPI=str(dpi))
    cx, cy = library_card_center(size, dpi)
    subprocess.run([str(repo / "target/debug/verso"), "--screenshot", str(out / f"{name}.png"),
                    "--tap", f"{cx},{cy}", *steps],
                   env=env, check=True, cwd=repo)


size, dpi = (1272, 1696), 300
first_tile = grid_cell_center(size, dpi, 0, 0)
path_tap = (mm(5.0, dpi) + mm(5.0, dpi) + mm(15.0, dpi), mm(13.0, dpi) + mm(2.2, dpi) + mm(5.5, dpi))

capture("roots")
capture("root-and-loose", root)
capture("nested", root / "Fiction/Same")
capture("page2", root / "Fiction/Same", 1)
capture("book-menu", root / "Fiction/Same", 1, ["--long-press", f"{first_tile[0]},{first_tile[1]}"])
capture("folder-details", None, steps=["--long-press", f"{first_tile[0]},{first_tile[1]}"])
capture("full-path", root / "Fiction/Same", steps=["--tap", f"{path_tap[0]},{path_tap[1]}"])
capture("landscape", root / "Fiction/Same", size=(1696, 1272), dpi=300)
capture("small", root, size=(600, 800), dpi=167)
capture("missing-folder", root / "Removed")
print("captured", sorted(p.name for p in out.glob("*.png")))
