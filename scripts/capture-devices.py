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
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / 'screenshots/devices'
sys.path.insert(0, str(ROOT / 'scripts'))

DEVICES = {
    'basic-167': (600, 800, 167),
    'basic-300': (1072, 1448, 300),
    'pw6': (1272, 1696, 300),
    'scribe': (1860, 2480, 300),
}


class Screen:
    """Sizes for one device and orientation, in pixels."""

    def __init__(self, w, h, dpi):
        self.w, self.h, self.dpi = w, h, dpi

    def mm(self, v):
        return round(v * self.dpi / 25.4)

    def tap(self, x, y):
        return ['--tap', f'{round(x)},{round(y)}']

    def long_press(self, x, y):
        return ['--long-press', f'{round(x)},{round(y)}']

    def swipe(self, x1, y1, x2, y2):
        return ['--swipe', f'{round(x1)},{round(y1)},{round(x2)},{round(y2)}']


def scenes(s):
    """Per screen: (books folder, settings, steps, start screen)."""
    cx, cy = s.w / 2, s.h / 2
    margin, target, icon = s.mm(5), s.mm(9), s.mm(5)
    # Home's Library card, at the bottom.
    card_h = max(target, min(s.mm(13.2), s.h // 8))
    library = s.tap(cx, s.h - margin - card_h / 2)
    # The top bar's search button, left of rescan, Wi-Fi, settings and exit.
    narrow = s.w < s.mm(100)
    exit_cx = s.w - margin - icon // 2
    search_cx = exit_cx - s.mm(10 if narrow else 10.5) - target - s.mm(10 if narrow else 12.2) - target
    search = s.tap(search_cx, s.mm(13) // 2)
    book = [*library, *s.tap(margin + s.mm(9), s.mm(42))]
    page2 = [*book, *s.swipe(s.w * 0.8, cy, s.w * 0.2, cy)]
    menu = [*page2, *s.tap(cx, cy)]
    # The top-right corner bookmarks the page; the menu's ribbon is then filled.
    bookmark = [*page2, *s.tap(s.w - s.mm(4), s.mm(4))]
    # The menu's bottom row: Contents, Highlights, Text, Layout.
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
    devices = os.environ.get('DEVICES', ','.join(DEVICES)).split(',')
    fixtures = ROOT / 'crates/verso/tests/fixtures'
    books = {'all': OUT / 'books', 'one': OUT / 'book'}
    for folder in books.values():
        folder.mkdir(parents=True, exist_ok=True)
    for epub in fixtures.glob('*.epub'):
        shutil.copyfile(epub, books['all'] / epub.name)
    shutil.copyfile(fixtures / 'selection.epub', books['one'] / 'selection.epub')
    dictionaries = OUT / 'dictionaries'
    shutil.rmtree(dictionaries, ignore_errors=True)
    dictionary_fixture(dictionaries)
    made = {}
    for device in devices:
        w, h, dpi = DEVICES[device]
        for orientation in ('portrait', 'landscape'):
            s = Screen(w, h, dpi) if orientation == 'portrait' else Screen(h, w, dpi)
            for name, (folder, settings, steps, *screen) in scenes(s).items():
                if only and name not in only:
                    continue
                out = OUT / f'{device}-{orientation}' / f'{name}.png'
                capture(out, device, {'orientation': orientation, **settings}, steps, books[folder],
                        dictionaries, screen[0] if screen else None)
                made.setdefault(name, []).append(out)
    if shutil.which('montage'):
        for name, files in made.items():
            subprocess.run(['montage', *map(str, files), '-geometry', '+8+8', '-tile', 'x2', '-resize', 'x700',
                            '-background', '#777', str(OUT / f'sheet-{name}.png')], check=True)
    print(OUT)


def dictionary_fixture(root):
    """A small original dictionary with the selection fixture's words."""
    spec = importlib.util.spec_from_file_location('capture_dictionary', ROOT / 'scripts/capture-dictionary.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    words = ['Hello', 'selection', 'foundation', 'typography', 'book', 'text']
    module.install(root, 'Device fixture', 'x', words=[
        (w, f'<ar><k>{w}</k><def>An original test definition of <kref>{w}</kref>, long enough to wrap onto '
            'a second line on every screen size.</def><def>A second meaning, for the card height.</def></ar>')
        for w in words])


def capture(out, device, settings, steps, books, dictionaries, screen):
    state = OUT / 'state' / out.parent.name / out.stem
    shutil.rmtree(state, ignore_errors=True)
    state.mkdir(parents=True)
    shutil.copytree(dictionaries / 'dictionaries', state / 'dictionaries')
    (state / 'dictionary-folders.json').write_text(json.dumps({'version': 1, 'folders': []}))
    (state / 'settings.json').write_text(json.dumps({'library_folders': [str(books)], **settings}))
    out.parent.mkdir(parents=True, exist_ok=True)
    env = {k: v for k, v in os.environ.items() if not k.startswith('VERSO_SIM_')}
    env.update(VERSO_DATA=str(state), VERSO_BOOKS=str(books), VERSO_SIM_BATTERY='82', RUST_LOG='error')
    args = [str(ROOT / 'target/debug/verso'), '--device', device, '--screenshot', str(out), *steps]
    if screen:
        args += ['--screen', screen]
    subprocess.run(args, env=env, check=True, timeout=90, stderr=subprocess.DEVNULL)


if __name__ == '__main__':
    main()
