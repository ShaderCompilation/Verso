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
    # The lookup card: a side panel on the right in landscape, else the lower 80%.
    panel = max(s.w * 52 // 100, s.mm(72)) if s.w > s.h else s.w
    card_y = 0 if s.w > s.h else s.h - s.h * 4 // 5
    panel_swipe = (s.w - panel * 0.2, s.h * 0.6, s.w - panel * 0.8, s.h * 0.6)
    tab = lambda i: s.tap(s.w - panel + margin + (i + 0.5) * (panel - 2 * margin) / 3, card_y + s.mm(15.2))
    row_y = card_y + s.mm(24.2)  # Wikipedia's matches and language
    inner = panel - 2 * margin
    other_matches = s.tap(s.w - panel + margin + inner * 0.29, row_y)
    language = s.tap(s.w - margin - inner * 0.21, row_y)
    close = s.tap(s.w - margin + s.mm(2) - s.mm(9) / 2, card_y + s.mm(6))
    edit = [*s.tap(s.w - panel + margin + s.mm(4), card_y + s.mm(6)), '--edit', 'clear']
    wifi = s.tap(margin + (s.w - 2 * margin) / (10 if s.w > s.h else 6), s.mm(27 if s.w > s.h else 36.5))
    offline = [*s.swipe(cx, 2, cx, s.h * 0.55), *wifi, *s.tap(cx, s.h * 0.95)]
    drag = lambda y: ['--hold-drag', f'{round(hello_x)},{s.mm(6)},{round(hello_x) + s.mm(30)},{y}']
    lookup = [*book, *s.long_press(hello_x, s.mm(6))]
    wikipedia = [*lookup, *tab(1)]
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
        'dictionary': ('one', {'footer': False}, lookup),
        'wikipedia': ('one', {'footer': False}, wikipedia),
        'wikipedia-end': ('one', {'footer': False}, [*wikipedia, *s.swipe(*panel_swipe) * 9]),
        'wikipedia-matches': ('one', {'footer': False}, [*wikipedia, *other_matches]),
        'wikipedia-languages': ('one', {'footer': False}, [*wikipedia, *language]),
        'wikipedia-list': ('one', {'footer': False}, [*wikipedia, *edit, '--text', 'mercury', '--edit', 'submit']),
        'wikipedia-miss': ('one', {'footer': False}, [*wikipedia, *edit, '--text', 'nothing', '--edit', 'submit']),
        'wikipedia-offline': ('one', {'footer': False}, [*book, *offline, *wikipedia[len(book):]]),
        'wikipedia-closed': ('one', {'footer': False}, [*wikipedia, *close]),
        'wikipedia-page': ('one', {'footer': False}, book),
        'translate': ('one', {'footer': False}, [*lookup, *tab(2)]),
        'passage': ('one', {'footer': False}, [*book, *drag(s.mm(6))]),
        'passage-long': ('one', {'footer': False}, [*book, *drag(s.mm(45))]),
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
    wikipedia = sim.wikipedia()
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
            sim.run(out, steps, data, books[folder], device=device, screen=screen[0] if screen else None,
                    env={'VERSO_WIKIPEDIA_URL': wikipedia})
            made.setdefault(name, []).append(out)
    # Closing the card puts back the page exactly.
    for closed, page in zip(made.get('wikipedia-closed', []), made.get('wikipedia-page', [])):
        assert closed.read_bytes() == page.read_bytes(), f'{closed} differs from {page}'
    if shutil.which('montage'):
        for name, files in made.items():
            subprocess.run(['montage', *map(str, files), '-geometry', '+8+8', '-tile', 'x2', '-resize', 'x700',
                            '-background', '#777', str(OUT / f'sheet-{name}.png')], check=True)
    print(OUT)


if __name__ == '__main__':
    main()
