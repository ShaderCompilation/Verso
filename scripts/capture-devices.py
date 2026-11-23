#!/usr/bin/env python3
"""Simulator screenshots of the main screens on every target screen size.

Run cargo build first. Outputs live in gitignored screenshots/devices/,
one folder per device and orientation, plus a contact sheet per screen
(sheet-<screen>.png, needs ImageMagick's montage). Taps are worked out from
the theme's millimetre sizes, so the same scenes play on every preset.

    python3 scripts/capture-devices.py                   # everything
    python3 scripts/capture-devices.py home menu         # some screens
    DEVICES=basic-167 python3 scripts/capture-devices.py # some devices
"""
import os
import shutil
import subprocess
import sys

import sim

OUT = sim.out_dir('devices')


def scenes(s):
    """Per screen: (books folder, settings, steps, start screen)."""
    cx, cy = s.w / 2, s.h / 2
    library = sim.tap(sim.library_card_center(s.size, s.dpi))
    search = sim.tap(sim.search_button_center(s.size, s.dpi))
    book = s.open_book()
    page2 = [*book, *s.swipe(s.w * 0.8, cy, s.w * 0.2, cy)]
    menu = [*page2, *s.tap(cx, cy)]
    # The top-right corner bookmarks the page; the menu's ribbon is then filled.
    bookmark = [*page2, *s.tap(s.w - s.mm(4), s.mm(4))]
    # The menu's bottom row: Contents, Notes, Text, Layout.
    margin = s.mm(5)
    column = (s.w - 2 * margin) / 4
    hello_x = (max(s.mm(7), (s.w - s.mm(107)) / 2) if s.w > s.h else s.mm(7)) + s.mm(8)
    menu_button = lambda i: s.tap(margin + (i + 0.5) * column, s.h - margin - s.mm(10) / 2)
    return {
        'home': ('all', {}, []),
        'library': ('all', {}, library),
        'keyboard': ('all', {}, [*library, *search]),
        'reader': ('one', {}, page2),
        'menu': ('one', {}, menu),
        'bookmark': ('one', {}, bookmark),
        'bookmark-menu': ('one', {}, [*bookmark, *s.tap(cx, cy)]),
        # The Notes list with the bookmark: it has to fit 6" screens too.
        'notes': ('one', {}, [*bookmark, *s.tap(cx, cy), *menu_button(1)]),
        'text': ('one', {}, [*menu, *menu_button(2)]),
        'layout': ('one', {}, [*menu, *menu_button(3)]),
        'drawer': ('one', {}, [*page2, *s.swipe(cx, 2, cx, s.h * 0.55)]),
        # "Hello" starts the first line; landscape pages centre a line of
        # about 107 mm.
        'dictionary': ('one', {'footer': False}, [*book, *s.long_press(hello_x, s.mm(9.3))]),
        'settings': ('all', {}, [], 'settings'),
    }


def main():
    only = set(sys.argv[1:])
    devices = os.environ.get('DEVICES', ','.join(sim.DEVICES)).split(',')
    books = {'all': sim.books(OUT / 'books', *(p.name for p in sim.FIXTURES.glob('*.epub'))),
             'one': sim.books(OUT / 'book', 'selection.epub')}
    dictionaries = OUT / 'dictionaries'
    shutil.rmtree(dictionaries, ignore_errors=True)
    # A small original dictionary with the selection fixture's words.
    sim.dictionary(dictionaries, 'Device fixture', [
        (w, f'<ar><k>{w}</k><def>An original test definition of <kref>{w}</kref>, long enough to wrap onto '
            'a second line on every screen size.</def><def>A second meaning, for the card height.</def></ar>')
        for w in ['Hello', 'selection', 'foundation', 'typography', 'book', 'text']])
    made = {}
    for device, orientation, s in sim.device_screens(devices):
        for name, (folder, settings, steps, *screen) in scenes(s).items():
            if only and name not in only:
                continue
            out = OUT / f'{device}-{orientation}' / f'{name}.png'
            out.parent.mkdir(parents=True, exist_ok=True)
            data = sim.state(OUT / 'state' / out.parent.name / out.stem, books[folder],
                             {'orientation': orientation, **settings},
                             {'dictionary-folders.json': {'version': 1, 'folders': []}})
            shutil.copytree(dictionaries / 'dictionaries', data / 'dictionaries')
            sim.run(out, steps, data, books[folder], device=device, screen=screen[0] if screen else None)
            made.setdefault(name, []).append(out)
    if shutil.which('montage'):
        for name, files in made.items():
            subprocess.run(['montage', *map(str, files), '-geometry', '+8+8', '-tile', 'x2', '-resize', 'x700',
                            '-background', '#777', str(OUT / f'sheet-{name}.png')], check=True)
    print(OUT)


if __name__ == '__main__':
    main()
