#!/usr/bin/env python3
"""Phase 7 screenshots and a 10,000-entry cached-index performance fixture.

Run cargo build first. Outputs and isolated state remain in screenshots/phase7.
All EPUB contents come from the project's original reader fixture.
"""
import copy
import json
import os
from pathlib import Path
import subprocess
from xml.sax.saxutils import escape
import zipfile

repo = Path(__file__).resolve().parents[1]
out = repo / "screenshots/phase7"
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

# Coordinates correspond to the standard 1272 x 1696 / 300 dpi layout.
search = ["--tap", "758,77"]
hide = ["--edit", "submit"]

def capture(name, steps=(), folder=None, page=0, small=False):
    state = out / "state" / name
    state.mkdir(parents=True, exist_ok=True)
    nav = {"tab": "collections" if folder else "all_books", "folder": str(folder) if folder else None,
           "all_page": page, "folder_pages": {str(folder): page} if folder else {}}
    (state / "library-navigation.json").write_text(json.dumps(nav))
    env = dict(os.environ, VERSO_DATA=str(state), VERSO_BOOKS=str(root), RUST_LOG="warn")
    if small:
        env.update(VERSO_SIM_SIZE="600x800", VERSO_SIM_DPI="167")
    subprocess.run([str(repo / "target/debug/verso"), "--screenshot", str(out / f"{name}.png"), *steps],
                   env=env, cwd=repo, check=True)
    return state

state = capture("entry", search)
capture("english", [*search, "--text", "JOURNEY"])
capture("russian-keyboard", [*search, "--tap", "221,1630", "--text", "ЛЕГЕНДЫ"])
capture("danish-keyboard", [*search, "--tap", "172,1630", "--tap", "172,1630", "--text", "ÆØÅ"])
capture("no-matches", [*search, "--text", "no-such-book", *hide])
capture("filename", [*search, "--text", "Fixture 29", *hide])
capture("shortcut-letters", [*search, "--text", "qspv"])
capture("scoped", [*search, "--text", "journey", *hide, "--tap", "920,350"], root / "Fiction")
capture("result-page2", [*search, "--text", "journey", *hide, "--swipe", "1100,800,200,800"])
capture("book-menu", [*search, "--text", "ЛЕГЕНДЫ", *hide, "--long-press", "190,700"])
capture("restored-folder", [*search, "--text", "journey", *hide, "--tap", "758,77"], root / "Fiction/Nested", 1)
capture("small-keyboard", ["--tap", "316,43", "--text", "Легенды", "--tap", "97,762"], small=True)

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
print(out)
