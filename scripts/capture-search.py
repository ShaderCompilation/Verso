#!/usr/bin/env python3
"""Search screenshots and a 10,000-entry cached-index performance fixture.

Run cargo build first. Outputs and isolated state remain in screenshots/phase7.
All EPUB contents come from the project's original reader fixture.
"""
import copy
import json
import os
from pathlib import Path
import shutil
import subprocess
from xml.sax.saxutils import escape
import zipfile

repo = Path(__file__).resolve().parents[1]
out = repo / "screenshots/phase7"
shutil.rmtree(out, ignore_errors=True)
root = out / "fixtures/Library"
base = repo / "crates/verso/tests/fixtures/reader.epub"
for i in range(30):
    folder = root / ("Fiction/Nested" if i < 20 else "Other")
    folder.mkdir(parents=True, exist_ok=True)
    title = f"The Quiet Journey {i:02}" if i < 26 else f"Легенды и мифы {i:02}"
    if i == 25:
        title = "Stille rejse æøå"
    author = "Verso contributors" if i < 26 else "Николай Кун"
    destination = folder / f"Fixture {i:02}.epub"
    with zipfile.ZipFile(base) as source, zipfile.ZipFile(destination, "w") as target:
        for item in source.infolist():
            data = source.read(item.filename)
            if item.filename.endswith(".opf"):
                text = data.decode().replace("Verso reader test", escape(title)).replace("Verso contributors", escape(author))
                text = text.replace("verso-reader-test", f"verso-search-{i}")
                if i == 25:
                    text = text.replace("<dc:language>en", "<dc:language>da-DK")
                if i >= 26:
                    text = text.replace("<dc:language>en", "<dc:language>ru")
                data = text.encode()
            target.writestr(item, data)
    os.utime(destination, (1_700_000_000 + i, 1_700_000_000 + i))


def mm(v, dpi):
    return round(v * dpi / 25.4)


def library_card_center(size, dpi):
    w, h = size
    margin, target = mm(5.0, dpi), mm(9.0, dpi)
    card_h = max(target, min(mm(13.2, dpi), h // 8))
    return w // 2, h - margin - card_h // 2


def search_button_center(size, dpi):
    w, _ = size
    margin, top_bar, target, icon = mm(5.0, dpi), mm(13.0, dpi), mm(9.0, dpi), mm(5.0, dpi)
    narrow = w < mm(100.0, dpi)
    group_step = mm(10.0 if narrow else 12.2, dpi)
    exit_step = mm(10.0 if narrow else 10.5, dpi)
    exit_cx = w - margin - icon // 2
    search_cx = exit_cx - exit_step - target - group_step - target
    return search_cx, top_bar // 2


def this_folder_scope_center(size, dpi):
    w, h = size
    margin, top_bar, target = mm(5.0, dpi), mm(13.0, dpi), mm(9.0, dpi)
    width = w - 2 * margin
    y = top_bar + mm(2.0, dpi)
    gap = mm(2.0, dpi)
    if w > h:
        btn_w = (width * 2 // 5 - gap) // 2
        return w - margin - btn_w // 2, y + target // 2
    scope_y = y + target + gap
    btn_w = (width - gap) // 2
    return margin + btn_w + gap + btn_w // 2, scope_y + target // 2


def grid_cell_center(size, dpi, col, row):
    w, h = size
    margin, top_bar, target = mm(5.0, dpi), mm(13.0, dpi), mm(9.0, dpi)
    cols, rows = (6, 2) if w > h else ((3, 3) if w < mm(100.0, dpi) else (4, 3))
    gap_x = mm(2.6 if cols == 6 else (2.8 if cols == 3 else 3.4), dpi)
    gap_y = mm(2.0, dpi)
    y = top_bar + mm(2.0, dpi)
    gap = mm(2.0, dpi)
    header_y = y + target + mm(1.8, dpi) if w > h else y + 2 * target + gap + mm(1.7, dpi)
    grid_top = header_y + mm(7.0, dpi) + mm(1.8, dpi)
    grid_w = w - 2 * margin
    grid_h = max(target, h - margin - grid_top)
    cell_w = (grid_w - gap_x * (cols - 1)) // cols
    cell_h = (grid_h - gap_y * (rows - 1)) // rows
    return margin + col * (cell_w + gap_x) + cell_w // 2, grid_top + row * (cell_h + gap_y) + cell_h // 2


def tap(pt):
    return ["--tap", f"{pt[0]},{pt[1]}"]


def capture(name, steps=(), folder=None, page=0, size=(1272, 1696), dpi=300):
    state = out / "state" / name
    state.mkdir(parents=True, exist_ok=True)
    nav = {"folder": str(folder) if folder else None,
           "folder_pages": {str(folder): page} if folder else {}}
    (state / "library-navigation.json").write_text(json.dumps(nav))
    env = dict(os.environ, VERSO_DATA=str(state), VERSO_BOOKS=str(root), RUST_LOG="warn",
               VERSO_SIM_SIZE=f"{size[0]}x{size[1]}", VERSO_SIM_DPI=str(dpi))
    prefix = tap(library_card_center(size, dpi)) if folder is not None else []
    subprocess.run([str(repo / "target/debug/verso"), "--screenshot", str(out / f"{name}.png"),
                    *prefix, *steps],
                   env=env, cwd=repo, check=True)
    return state


size, dpi = (1272, 1696), 300
search = tap(search_button_center(size, dpi))
hide = ["--edit", "submit"]
scope_this_folder = tap(this_folder_scope_center(size, dpi))
first_result = grid_cell_center(size, dpi, 0, 0)

state = capture("entry", search)
capture("english", [*search, "--text", "JOURNEY"])
capture("russian-keyboard", [*search, "--tap", "263,1635", "--text", "ЛЕГЕНДЫ"])
capture("danish-keyboard", [*search, "--tap", "263,1635", "--tap", "263,1635", "--text", "ÆØÅ"])
capture("no-matches", [*search, "--text", "no-such-book", *hide])
capture("filename", [*search, "--text", "Fixture 29", *hide])
capture("shortcut-letters", [*search, "--text", "qspv"])
capture("scoped", [*search, "--text", "journey", *hide, *scope_this_folder], root / "Fiction")
capture("result-page2", [*search, "--text", "journey", *hide, "--swipe", "1100,800,200,800"])
capture("book-menu", [*search, "--text", "ЛЕГЕНДЫ", *hide, "--long-press", f"{first_result[0]},{first_result[1]}"])
capture("restored-folder", [*search, "--text", "journey", *hide, *search], root / "Fiction/Nested", 1)

land_sz = (1696, 1272)
capture("landscape", [*tap(search_button_center(land_sz, 300)), "--text", "journey", *hide],
        size=land_sz, dpi=300)
small_sz = (600, 800)
capture("small-keyboard", [*tap(search_button_center(small_sz, 167)), "--text", "Легенды", "--tap", "125,766"],
        size=small_sz, dpi=167)

# Cached-only fixture: intentionally offline paths, no source EPUB access needed.
index = json.loads((state / "library.json").read_text())
fixture = {"version": index["version"], "books": []}
for i in range(10000):
    book = copy.deepcopy(index["books"][i % len(index["books"])])
    book.update(id=f"fixture-{i}", legacy_id=f"fixture-{i}", path=f"/mnt/us/verso/phase7-check/offline-fixture/Group{i%20}/Book {i:05}.epub",
                cover={"Unavailable": "fixture"})
    book["metadata"]["title"] += f" {i:05}"
    fixture["books"].append(book)
bench = out / "large-state"
bench.mkdir(exist_ok=True)
(bench / "library.json").write_text(json.dumps(fixture, ensure_ascii=False))
(bench / "settings.json").write_text(json.dumps({"library_folders": ["/mnt/us/verso/phase7-check/offline-fixture"]}))
print("captured", sorted(p.name for p in out.glob("*.png")))
