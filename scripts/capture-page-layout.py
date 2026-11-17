#!/usr/bin/env python3
"""Simulator screenshots of the Page layout screen and the reader bars.

Run cargo build first. Outputs live in gitignored screenshots/page-layout/.
Coordinates assume the 1272 x 1696 simulator screen, turned for landscape.
Between them, the layouts show every bar item somewhere: A and B in the
sketch and on the page, the rest (and every item's value) in the picker.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / 'screenshots/page-layout'

# Library → book → menu → Text → Page layout, per orientation.
STEPS = {
    'portrait': {
        'open': ['--tap', '1000,230', '--tap', '200,550'],
        'menu': ['--tap', '636,848'],
        'text': ['--tap', '1073,1601'],
        'layout': ['--tap', '636,1570'],
    },
    'landscape': {
        'open': ['--tap', '1373,230', '--tap', '240,700'],
        'menu': ['--tap', '848,636'],
        'text': ['--tap', '1452,1177'],
        'layout': ['--tap', '848,1146'],
    },
}


def bar(on, left=None, centre=None, right=None):
    return {'on': on, 'slots': [left, centre, right]}


LAYOUTS = {
    'default': None,
    'a': {
        'top': bar(True, 'chapter_title', 'page_in_chapter', 'percent'),
        'bottom': bar(True, 'chapter_pages_left', 'time_left_chapter', 'page_total'),
        'progress': 'bottom',
    },
    'b': {
        'top': bar(True, 'book_title', 'clock_24h', 'battery'),
        'bottom': bar(True, 'author', 'clock_12h', 'time_left_book'),
        'progress': 'top',
    },
    # Margin extremes, a lone centre item and the bottom bar off.
    'c': {
        'top': bar(True, None, 'pages_left', None),
        'bottom': bar(False, 'chapter_pages_left', None, 'percent'),
        'margins': {'sides': 2, 'top': 20, 'bottom': 2},
    },
    'off': {'top': bar(False), 'bottom': bar(False)},
}

# The bottom bar's centre slot in the sketch, for the picker.
CENTRE_SLOT = {'portrait': '636,758', 'landscape': '848,720'}


def main():
    books = OUT / 'books'
    books.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(ROOT / 'crates/verso/tests/fixtures/search.epub', books / 'search.epub')
    for orientation, s in STEPS.items():
        opened = [*s['open'], '--swipe', '1000,700,300,700']  # page 2: a chapter title and progress
        layout = [*opened, *s['menu'], *s['text'], *s['layout']]
        scenes = {
            'sheet': (None, [*opened, *s['menu'], *s['text']]),
            'picker': (None, [*layout, '--tap', CENTRE_SLOT[orientation]]),
        }
        for name, page_layout in LAYOUTS.items():
            scenes['layout-' + name] = (page_layout, layout)
            if name != 'default':
                scenes['reader-' + name] = (page_layout, opened)
        for name, (page_layout, steps) in scenes.items():
            capture(f'{orientation}-{name}', orientation, page_layout, steps, books)
    print(OUT)


def capture(name, orientation, page_layout, steps, books):
    state = OUT / 'state' / name
    shutil.rmtree(state, ignore_errors=True)
    state.mkdir(parents=True)
    settings = {'library_folders': [str(books)], 'orientation': orientation}
    if page_layout:
        settings['page_layout'] = page_layout
    (state / 'settings.json').write_text(json.dumps(settings))
    env = dict(os.environ, VERSO_DATA=str(state), VERSO_BOOKS=str(books), RUST_LOG='error')
    subprocess.run([str(ROOT / 'target/debug/verso'), '--screenshot', str(OUT / (name + '.png')), *steps],
                   env=env, check=True, stderr=subprocess.DEVNULL)


if __name__ == '__main__':
    main()
