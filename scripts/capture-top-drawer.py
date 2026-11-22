#!/usr/bin/env python3
"""Simulator screenshots of the Top Drawer, the Reader Menu and the Text sheet.

Run cargo build first. Outputs live in gitignored screenshots/top-drawer/.
Coordinates assume the 1272 x 1696 simulator screen (turned for landscape)
and the 600 x 800, 167 dpi small screen. The simulator reports an 82%
battery; its Wi-Fi starts on and SSH off, and its switches finish at once,
so pending tiles are covered by unit tests, not here.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / 'screenshots/top-drawer'

SMALL = {'VERSO_SIM_SIZE': '600x800', 'VERSO_SIM_DPI': '167'}

# Per screen: the top-edge swipe, the battery slot, Home → Library → book,
# the menu's buttons and the drawer's tiles (5 standard, 6 with SSH).
P = {
    'swipe': ['--swipe', '636,20,636,900'],
    'battery': ['--tap', '532,77'],
    'library': ['--tap', '636,1560'],
    'open': ['--tap', '636,1560', '--tap', '165,500'],
    'next_page': ['--swipe', '1000,700,300,700'],
    'menu': ['--tap', '636,848'],
    'quick': ['--tap', '1150,77'],
    'search': ['--tap', '948,77'],
    'text': ['--tap', '782,1584'],
    'layout': ['--tap', '1077,1584'],
    'back': ['--tap', '80,77'],
    5: {'wifi': '243,430', 'orientation': '636,430', 'refresh': '1028,430',
        'transfer': '341,612', 'settings': '930,612'},
    6: {'wifi': '243,430', 'ssh': '636,430', 'orientation': '1028,430',
        'refresh': '243,612', 'transfer': '636,612', 'settings': '1028,612'},
}
L = {
    'swipe': ['--swipe', '848,20,848,700'],
    'open': ['--tap', '848,1135', '--tap', '165,500'],
    'next_page': ['--swipe', '1300,600,400,600'],
    'menu': ['--tap', '848,636'],
    'text': ['--tap', '1047,1169'],
    'layout': ['--tap', '1448,1169'],
    5: {'wifi': '197,316', 'orientation': '522,316'},
}


def tap(screen, tiles, name):
    return ['--tap', screen[tiles][name]]


def main():
    books = OUT / 'books'
    books.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(ROOT / 'crates/verso/tests/fixtures/search.epub', books / 'search.epub')
    dev = {'developer': True}
    land = {'orientation': 'landscape'}
    book = [*P['open'], *P['next_page']]
    menu = [*book, *P['menu']]
    in_drawer = [*book, *P['swipe']]
    scenes = {
        # Portrait: Home, Library and a book.
        'portrait-home': ({}, P['swipe']),
        'portrait-home-battery': ({}, P['battery']),
        'portrait-home-wifi-off': ({}, [*P['swipe'], *tap(P, 5, 'wifi')]),
        'portrait-home-developer': (dev, P['swipe']),
        'portrait-home-ssh': (dev, [*P['swipe'], *tap(P, 6, 'ssh')]),
        'portrait-library': ({}, [*P['library'], *P['swipe']]),
        'portrait-reader': ({}, in_drawer),
        'portrait-reader-closed': ({}, [*in_drawer, '--tap', '636,1300']),
        'portrait-menu': ({}, menu),
        'portrait-menu-battery': ({}, [*menu, *P['quick']]),
        'portrait-menu-search': ({}, [*menu, *P['search']]),
        'portrait-text': ({}, [*menu, *P['text']]),
        'portrait-layout': ({}, [*menu, *P['layout']]),
        'portrait-layout-closed': ({}, [*menu, *P['layout'], *P['back']]),
        'portrait-reader-settings': ({}, [*in_drawer, *tap(P, 5, 'settings')]),
        'portrait-reader-settings-back': ({}, [*in_drawer, *tap(P, 5, 'settings'), *P['back']]),
        'portrait-reader-transfer': ({}, [*in_drawer, *tap(P, 5, 'transfer')]),
        # Turning in place: the book and the Library folder come back.
        'portrait-reader-turned': ({}, [*in_drawer, *tap(P, 5, 'orientation')]),
        'portrait-reader-turned-back': ({}, [*in_drawer, *tap(P, 5, 'orientation'),
                                             *L['swipe'], *tap(L, 5, 'orientation')]),
        'portrait-library-turned': ({}, [*P['library'], *P['swipe'], *tap(P, 5, 'orientation')]),
        # Landscape.
        'landscape-home': (land, L['swipe']),
        'landscape-home-developer': ({**land, **dev}, L['swipe']),
        'landscape-reader': (land, [*L['open'], *L['next_page'], *L['swipe']]),
        'landscape-menu': (land, [*L['open'], *L['next_page'], *L['menu']]),
        'landscape-text': (land, [*L['open'], *L['next_page'], *L['menu'], *L['text']]),
        'landscape-layout': (land, [*L['open'], *L['next_page'], *L['menu'], *L['layout']]),
        # 6" small screen: five tiles fit one landscape row, six wrap to 3 x 2.
        'small-home': ({}, ['--swipe', '300,10,300,500'], SMALL),
        'small-home-developer': (dev, ['--swipe', '300,10,300,500'], SMALL),
        'small-landscape-home': (land, ['--swipe', '400,10,400,400'], SMALL),
        'small-landscape-developer': ({**land, **dev}, ['--swipe', '400,10,400,400'], SMALL),
    }
    states = {}
    for name, (settings, steps, *env) in scenes.items():
        states[name] = capture(name, settings, steps, books, env[0] if env else {})
    check(states)
    print(OUT)


def capture(name, settings, steps, books, extra_env):
    state = OUT / 'state' / name
    shutil.rmtree(state, ignore_errors=True)
    state.mkdir(parents=True)
    (state / 'settings.json').write_text(json.dumps({'library_folders': [str(books)], **settings}))
    env = dict(os.environ, VERSO_DATA=str(state), VERSO_BOOKS=str(books), VERSO_SIM_BATTERY='82',
               RUST_LOG='error', **extra_env)
    subprocess.run([str(ROOT / 'target/debug/verso'), '--screenshot', str(OUT / (name + '.png')), *steps],
                   env=env, check=True, timeout=60, stderr=subprocess.DEVNULL)
    return state


def check(states):
    """What the drawer saved, beyond what the pictures show."""
    def settings(name):
        return json.loads((states[name] / 'settings.json').read_text())
    assert settings('portrait-reader-turned')['orientation'] == 'landscape'
    assert settings('portrait-reader-turned-back').get('orientation', 'portrait') == 'portrait'
    assert settings('portrait-library-turned')['orientation'] == 'landscape'


if __name__ == '__main__':
    main()
