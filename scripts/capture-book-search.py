#!/usr/bin/env python3
"""Simulator screenshots of search in book, using the original search fixture.

Run cargo build first. Outputs live in gitignored screenshots/book-search/.
Coordinates assume the 1272 x 1696 simulator screen.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / 'screenshots/book-search'


def main():
    books = OUT / 'books'
    books.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(ROOT / 'crates/verso/tests/fixtures/search.epub', books / 'search.epub')
    opening = ['--tap', '1000,230', '--tap', '200,550']
    menu = [*opening, '--tap', '636,848']
    search = [*menu, '--tap', '636,1601']
    results = [*search, '--text', 'ada', '--edit', 'submit']
    opened = [*results, '--swipe', '900,1000,300,1000', '--tap', '600,1000']
    passage = [*opening, '--hold-drag', '190,297,500,297']
    scenes = {
        'reader': opening,
        'menu': menu,
        'keyboard': [*search, '--text', 'new york'],
        'results': results,
        'variants': [*search, '--text', 'привет', '--edit', 'submit'],
        'word-start': [*search, '--text', 'vin', '--edit', 'submit'],
        'no-result': [*search, '--text', 'absent phrase', '--edit', 'submit'],
        'opened': opened,
        'next': [*opened, '--tap', '500,1625'],
        'results-again': [*opened, '--tap', '697,1625'],
        'returned': [*opened, '--tap', '960,1625'],
        'passage': passage,
        'passage-search': [*passage, '--tap', '330,1572'],
        'dictionary-search': [*opening, '--long-press', '190,297', '--tap', '1036,685'],
    }
    for name, steps in scenes.items():
        state = OUT / (name + '-state')
        shutil.rmtree(state, ignore_errors=True)
        state.mkdir(parents=True)
        (state / 'settings.json').write_text(json.dumps({'library_folders': [str(books)]}))
        env = dict(os.environ, VERSO_DATA=str(state), VERSO_BOOKS=str(books), RUST_LOG='error')
        subprocess.run([str(ROOT / 'target/debug/verso'), '--screenshot', str(OUT / (name + '.png')), *steps],
                       env=env, check=True)
    print(OUT)


if __name__ == '__main__':
    main()
