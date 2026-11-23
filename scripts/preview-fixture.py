#!/usr/bin/env python3
"""Writes a cached-only 10,000-entry library for VERSO_PREVIEW_BENCH.

20 groups of 500 books: 100 loose in each group and 100 in each of four
subfolders. A tenth of the covers are pending and a tenth have no art;
1,000 random books have an opened order, with a reading state each so the
app's own rebuild gives the same order. Paths are offline, so no EPUB is read.
Cover keys follow covers::key, so the app treats them as current; it draws a
Ready cover only once covers/<key>.png and <key>-s.png exist (see
the folder-preview capture scripts). Usage: scripts/preview-fixture.py OUT_DIR,
then VERSO_DATA=OUT_DIR VERSO_PREVIEW_BENCH=1 verso
"""
import hashlib
import json
import random
import sys
from pathlib import Path

ROOT = "/mnt/us/verso/preview-fixture"
WORDS = ["river", "glass", "north", "silent", "amber", "winter", "paper", "stone", "garden", "hollow", "iron", "violet"]


def cover_key(book_id: str, stamp: dict) -> str:
    """covers::key: SHA-256 of the JSON (version, id, stamp)."""
    payload = json.dumps([1, book_id, stamp], separators=(",", ":"), ensure_ascii=False)
    return hashlib.sha256(payload.encode()).hexdigest()


def main(out: Path) -> None:
    random.seed(7)
    out.mkdir(parents=True, exist_ok=True)
    books = []
    for i in range(10000):
        group, sub = i % 20, (i // 20) % 5
        folder = f"{ROOT}/Group{group:02}" + ("" if sub == 0 else f"/Sub{sub}")
        r = random.random()
        stamp = {"size": 1000 + i, "modified_secs": 1700000000 + i, "modified_nanos": 0}
        key = cover_key(f"fx-{i:05}", stamp)
        cover = "Pending" if r < 0.1 else ({"Unavailable": key} if r < 0.2 else {"Ready": key})
        title = " ".join(random.choice(WORDS).capitalize() for _ in range(3)) + f" {i}"
        books.append({
            "path": f"{folder}/Book {i:05}.epub",
            "id": f"fx-{i:05}",
            "annotation_id": f"fx-{i:05}",
            "legacy_id": f"fx-{i:05}",
            "fingerprint": str(i),
            "fingerprint_attempted": True,
            "stamp": stamp,
            "metadata": {"identifier": "", "title": title, "authors": "Author " + random.choice(WORDS),
                         "language": "en", "cover": None},
            "error": None,
            "cover": cover,
            "available": True,
            "metadata_pending": False,
        })
    (out / "library.json").write_text(json.dumps({"version": 1, "books": books, "empty_folders": []}))
    (out / "settings.json").write_text(json.dumps({"library_folders": [ROOT]}))
    opened = [[f"fx-{i:05}", 1763000000 - n] for n, i in enumerate(random.sample(range(10000), 1000))]
    (out / "opened.json").write_text(json.dumps({"version": 1, "complete": True, "books": opened}))
    (out / "books").mkdir(exist_ok=True)
    for book_id, last_opened in opened:
        (out / "books" / f"{book_id}.json").write_text(json.dumps({"last_opened": last_opened, "progress": 1000}))


if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    main(Path(sys.argv[1]))
