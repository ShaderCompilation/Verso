#!/usr/bin/env python3
"""Reproducible headless Library & folder collection captures, using original EPUB fixtures.

Run cargo build first. Output and temporary state stay under screenshots/collections.
"""
import shutil
import zipfile

import sim
from sim import grid_cell_center, library_card_center, mm

out = sim.out_dir("collections", clean=True)
fixtures = sim.FIXTURES
root = out / "fixtures/Library"
other = out / "fixtures/Other/Library"
for folder in [root, root / "Fiction/Same", root / "History/Same", other / "Same", root / "Empty"]:
    folder.mkdir(parents=True, exist_ok=True)
shutil.copyfile(fixtures / "library2.epub", root / "Loose book.epub")
for i in range(19):
    shutil.copyfile(fixtures / "reader.epub", root / "Fiction/Same" / f"Book {i:02}.epub")
for folder in [root / "History/Same", other / "Same"]:
    shutil.copyfile(fixtures / "library3.epub", folder / "Quiet.epub")


def series_book(name, title, index=None):
    """library2.epub retitled, with calibre series metadata when `index` is given."""
    meta = f'<meta name="calibre:series" content="The Tides"/><meta name="calibre:series_index" content="{index}"/>'
    with zipfile.ZipFile(fixtures / "library2.epub") as src, zipfile.ZipFile(root / "Series" / name, "w") as dst:
        for item in src.infolist():
            data = src.read(item)
            if item.filename.endswith(".opf"):
                data = data.decode().replace("Quiet &amp; Calm", title)
                data = data.replace("</metadata>", (meta if index else "") + "</metadata>").encode()
            dst.writestr(item, data)


(root / "Series").mkdir(exist_ok=True)
for name, title, index in [("Tides 3.epub", "The Last Lantern", "3.0"), ("Tides 1.epub", "The First Tide", "1"),
                           ("Tides 2.epub", "Salt Roads", "2"), ("Tides 2.5.epub", "Harbour Tales", "2.5"),
                           ("Tides 4 - Afterword.epub", "Afterword", None)]:
    series_book(name, title, index)


def capture(name, folder=None, page=0, steps=(), size=(1272, 1696), dpi=300, settings=None):
    navigation = {"folder": str(folder) if folder else None,
                  "folder_pages": {str(folder): page} if folder else {}}
    state = sim.state(out / "state" / name, settings=settings, files={"library-navigation.json": navigation})
    sim.run(out / f"{name}.png", [*sim.tap(library_card_center(size, dpi)), *steps], state,
            [root, root / "Fiction", other], size=(size, dpi), quiet=False)


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
# Series sort: badges for metadata numbers only, and the sort menu (one page, so
# Sort sits a target left of New folder at the right margin).
series = {"library_sort": "series"}
capture("series-badges", root / "Series", settings=series)
sort_button = (size[0] - mm(5.0, dpi) - mm(5.0, dpi) // 2 - mm(9.0, dpi), mm(13.0, dpi) + mm(2.2, dpi) + mm(5.7, dpi))
capture("sort-menu", root / "Series", steps=sim.tap(sort_button), settings=series)
print("captured", sorted(p.name for p in out.glob("*.png")))
