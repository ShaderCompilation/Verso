#!/usr/bin/env python3
"""Simulator screenshots of the Top Drawer, the Reader Menu and the Text sheet.

Run cargo build first. Outputs live in gitignored screenshots/top-drawer/.
Coordinates assume the 1272 x 1696 simulator screen (turned for landscape)
and the 600 x 800, 167 dpi small screen. The simulator reports an 82%
battery; its Wi-Fi starts on and SSH off, and its switches finish at once,
so pending tiles are covered by unit tests, not here.
"""
import sim

OUT = sim.out_dir('top-drawer')

SMALL = {'VERSO_SIM_SIZE': '600x800', 'VERSO_SIM_DPI': '167'}

# Per screen: the top-edge swipe, the battery slot, Home → Library → book,
# the menu's buttons and the drawer's seven tiles (4 + 3 portrait, 7 × 1 PW6 landscape).
P = {
    'swipe': ['--swipe', '636,20,636,900'],
    'battery': ['--tap', '532,77'],
    'library': ['--tap', '636,1560'],
    'open': ['--tap', '636,1560', '--tap', '165,500'],
    'next_page': ['--swipe', '1000,700,300,700'],
    'menu': ['--tap', '636,848'],
    'sun': ['--tap', '1001,77'],
    'quick': ['--tap', '1133,77'],
    'search': ['--tap', '895,77'],
    'drawer_back': ['--tap', '88,70'],
    'drawer_search': ['--tap', '874,70'],
    'text': ['--tap', '782,1584'],
    'layout': ['--tap', '1077,1584'],
    'text_next': ['--tap', '1166,1615'],
    'back': ['--tap', '80,77'],
    'tiles': {'wifi': '196,431', 'ssh': '489,431', 'dark': '782,431', 'orientation': '1075,431',
              'refresh': '245,613', 'transfer': '635,613', 'settings': '1026,613'},
    'book_tiles': {'wifi': '196,465', 'ssh': '489,465', 'dark': '782,465', 'orientation': '1075,465',
                   'refresh': '245,647', 'transfer': '635,647', 'settings': '1026,647'},
}
L = {
    'swipe': ['--swipe', '848,20,848,700'],
    'open': ['--tap', '848,1135', '--tap', '165,500'],
    'next_page': ['--swipe', '1300,600,400,600'],
    'menu': ['--tap', '848,636'],
    'text': ['--tap', '1047,1169'],
    'layout': ['--tap', '1448,1169'],
    'tiles': {'wifi': '164,317', 'ssh': '392,317', 'dark': '620,317', 'orientation': '848,317',
              'refresh': '1076,317', 'transfer': '1304,317', 'settings': '1532,317'},
    'book_tiles': {'wifi': '164,334', 'ssh': '392,334', 'dark': '620,334', 'orientation': '848,334',
                   'refresh': '1076,334', 'transfer': '1304,334', 'settings': '1532,334'},
}


def tap(screen, name, book=False):
    key = 'book_tiles' if book else 'tiles'
    return ['--tap', screen[key][name]]


def main():
    books = sim.books(OUT / 'books', 'search.epub')
    land = {'orientation': 'landscape'}
    book = [*P['open'], *P['next_page']]
    menu = [*book, *P['menu']]
    in_drawer = [*book, *P['swipe']]
    scenes = {
        # Portrait: Home, Library and a book.
        'portrait-home': ({}, P['swipe']),
        'portrait-home-battery': ({}, P['battery']),
        'portrait-home-wifi-off': ({}, [*P['swipe'], *tap(P, 'wifi')]),
        'portrait-home-ssh': ({}, [*P['swipe'], *tap(P, 'ssh')]),
        'portrait-library': ({}, [*P['library'], *P['swipe']]),
        'portrait-reader': ({}, in_drawer),
        'portrait-reader-ssh': ({}, [*in_drawer, *tap(P, 'ssh', True)]),
        'portrait-reader-back': ({}, [*in_drawer, *P['drawer_back']]),
        'portrait-reader-search': ({}, [*in_drawer, *P['drawer_search']]),
        'portrait-reader-closed': ({}, [*in_drawer, '--tap', '636,1300']),
        'portrait-menu': ({}, menu),
        'portrait-menu-sun': ({}, [*menu, *P['sun']]),
        'portrait-menu-battery': ({}, [*menu, *P['quick']]),
        'portrait-menu-search': ({}, [*menu, *P['search']]),
        'portrait-text': ({}, [*menu, *P['text']]),
        'portrait-text-page-2': ({}, [*menu, *P['text'], *P['text_next']]),
        'portrait-layout': ({}, [*menu, *P['layout']]),
        'portrait-layout-closed': ({}, [*menu, *P['layout'], *P['back']]),
        'portrait-reader-settings': ({}, [*in_drawer, *tap(P, 'settings', True)]),
        'portrait-reader-settings-back': ({}, [*in_drawer, *tap(P, 'settings', True), *P['back']]),
        'portrait-reader-transfer': ({}, [*in_drawer, *tap(P, 'transfer', True)]),
        # Turning in place: the book and the Library folder come back.
        'portrait-reader-turned': ({}, [*in_drawer, *tap(P, 'orientation', True)]),
        'portrait-reader-turned-back': ({}, [*in_drawer, *tap(P, 'orientation', True),
                                             *L['swipe'], *tap(L, 'orientation', True)]),
        'portrait-library-turned': ({}, [*P['library'], *P['swipe'], *tap(P, 'orientation')]),
        # Dark mode: switched via the tile and shown in both orientations on 7" and 6".
        'dark-portrait-home': ({'dark_mode': True}, P['swipe']),
        'dark-landscape-home': ({'dark_mode': True, **land}, L['swipe']),
        'dark-portrait-switched-reader': ({}, [*in_drawer, *tap(P, 'dark', True)]),
        'dark-small-home': ({'dark_mode': True}, ['--swipe', '300,10,300,500'], SMALL),
        'dark-small-landscape-home': ({'dark_mode': True, **land}, ['--swipe', '400,10,400,400'], SMALL),
        # Landscape.
        'landscape-home': (land, L['swipe']),
        'landscape-reader': (land, [*L['open'], *L['next_page'], *L['swipe']]),
        'landscape-menu': (land, [*L['open'], *L['next_page'], *L['menu']]),
        'landscape-text': (land, [*L['open'], *L['next_page'], *L['menu'], *L['text']]),
        'landscape-layout': (land, [*L['open'], *L['next_page'], *L['menu'], *L['layout']]),
        # 6" small screen: the seven tiles wrap to 4 + 3 in both orientations.
        'small-home': ({}, ['--swipe', '300,10,300,500'], SMALL),
        'small-reader': ({}, ['--tap', '300,740', '--tap', '90,260', '--swipe', '300,10,300,500'], SMALL),
        'small-landscape-home': (land, ['--swipe', '400,10,400,400'], SMALL),
    }
    states = {}
    for name, (settings, steps, *env) in scenes.items():
        states[name] = capture(name, settings, steps, books, env[0] if env else {})
    check(states)
    print(OUT)


def capture(name, settings, steps, books, extra_env):
    state = sim.state(OUT / 'state' / name, books, settings)
    return sim.run(OUT / (name + '.png'), steps, state, books, env=extra_env)


def check(states):
    """What the drawer saved, beyond what the pictures show."""
    def settings(name):
        return sim.read_json(states[name] / 'settings.json')
    assert settings('portrait-reader-turned')['orientation'] == 'landscape'
    assert settings('portrait-reader-turned-back').get('orientation', 'portrait') == 'portrait'
    assert settings('portrait-library-turned')['orientation'] == 'landscape'
    assert settings('dark-portrait-switched-reader')['dark_mode'] is True


if __name__ == '__main__':
    main()
