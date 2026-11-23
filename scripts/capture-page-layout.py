#!/usr/bin/env python3
"""Simulator screenshots of the Page layout screen and the reader bars.

Run cargo build first. Outputs live in gitignored screenshots/page-layout/.
Coordinates assume the 1272 x 1696 simulator screen, turned for landscape.
Between them, the layouts show every bar item somewhere: A and B in the
sketch and on the page, the rest (and every item's value) in the picker.
The state seeds a reading speed (20 characters a second) and the simulator
reports an 82% battery, so time left and the battery show values.
"""
import sim

OUT = sim.out_dir('page-layout')

# Home → Library → book → menu → Text or Layout, per orientation.
STEPS = {
    'portrait': {
        'open': ['--tap', '636,1560', '--tap', '165,500'],
        'menu': ['--tap', '636,848'],
        'text': ['--tap', '782,1584'],
        'layout': ['--tap', '1077,1584'],
    },
    'landscape': {
        'open': ['--tap', '848,1135', '--tap', '165,500'],
        'menu': ['--tap', '848,636'],
        'text': ['--tap', '1047,1169'],
        'layout': ['--tap', '1448,1169'],
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
    books = sim.books(OUT / 'books', 'search.epub')
    for orientation, s in STEPS.items():
        opened = [*s['open'], '--swipe', '1000,700,300,700']  # page 2: a chapter title and progress
        layout = [*opened, *s['menu'], *s['layout']]
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
    settings = {'orientation': orientation, **({'page_layout': page_layout} if page_layout else {})}
    # A reading speed carried over from earlier books, so time left shows.
    state = sim.state(OUT / 'state' / name, books, settings,
                      {'reading-speed.json': {'chars_per_sec': 20.0, 'samples': 30}})
    sim.run(OUT / (name + '.png'), steps, state, books)


if __name__ == '__main__':
    main()
