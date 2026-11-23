#!/usr/bin/env python3
"""Search screenshots and a 10,000-entry cached-index performance fixture.

Run cargo build first. Outputs and isolated state remain in screenshots/search.
All EPUB contents come from the project's original reader fixture.
"""
import copy
import os
from xml.sax.saxutils import escape
import zipfile

import sim
from sim import library_card_center, mm, search_button_center, tap

out = sim.out_dir("search", clean=True)
root = out / "fixtures/Library"
base = sim.FIXTURES / "reader.epub"
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


def capture(name, steps=(), folder=None, page=0, size=(1272, 1696), dpi=300):
    nav = {"folder": str(folder) if folder else None,
           "folder_pages": {str(folder): page} if folder else {}}
    state = sim.state(out / "state" / name, files={"library-navigation.json": nav})
    prefix = tap(library_card_center(size, dpi)) if folder is not None else []
    return sim.run(out / f"{name}.png", [*prefix, *steps], state, root, size=(size, dpi), quiet=False)


size, dpi = (1272, 1696), 300
search = tap(search_button_center(size, dpi))
hide = ["--edit", "submit"]
scope_this_folder = tap(this_folder_scope_center(size, dpi))
first_result = sim.grid_cell_center(size, dpi, 0, 0, search=True)

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
index = sim.read_json(state / "library.json")
fixture = {"version": index["version"], "books": []}
for i in range(10000):
    book = copy.deepcopy(index["books"][i % len(index["books"])])
    book.update(id=f"fixture-{i}", legacy_id=f"fixture-{i}", path=f"/mnt/us/verso/phase7-check/offline-fixture/Group{i%20}/Book {i:05}.epub",
                cover={"Unavailable": "fixture"})
    book["metadata"]["title"] += f" {i:05}"
    fixture["books"].append(book)
bench = out / "large-state"
bench.mkdir(exist_ok=True)
sim.write_json(bench / "library.json", fixture)
sim.write_json(bench / "settings.json", {"library_folders": ["/mnt/us/verso/phase7-check/offline-fixture"]})
print("captured", sorted(p.name for p in out.glob("*.png")))
