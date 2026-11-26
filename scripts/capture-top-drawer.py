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
# the menu's buttons and the drawer's six tiles.
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
    'tiles': {'wifi': '243,430', 'ssh': '636,430', 'orientation': '1028,430',
              'refresh': '243,612', 'transfer': '636,612', 'settings': '1028,612'},
}
L = {
    'swipe': ['--swipe', '848,20,848,700'],
    'open': ['--tap', '848,1135', '--tap', '165,500'],
    'next_page': ['--swipe', '1300,600,400,600'],
    'menu': ['--tap', '848,636'],
    'text': ['--tap', '1047,1169'],
    'layout': ['--tap', '1448,1169'],
    'tiles': {'wifi': '182,333', 'orientation': '714,333'},
}


def tap(screen, name):
    return ['--tap', screen['tiles'][name]]


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
        'portrait-reader-ssh': ({}, [*in_drawer, *tap(P, 'ssh')]),
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
        'portrait-reader-settings': ({}, [*in_drawer, *tap(P, 'settings')]),
        'portrait-reader-settings-back': ({}, [*in_drawer, *tap(P, 'settings'), *P['back']]),
        'portrait-reader-transfer': ({}, [*in_drawer, *tap(P, 'transfer')]),
        # Turning in place: the book and the Library folder come back.
        'portrait-reader-turned': ({}, [*in_drawer, *tap(P, 'orientation')]),
        'portrait-reader-turned-back': ({}, [*in_drawer, *tap(P, 'orientation'),
                                             *L['swipe'], *tap(L, 'orientation')]),
        'portrait-library-turned': ({}, [*P['library'], *P['swipe'], *tap(P, 'orientation')]),
        # Landscape.
        'landscape-home': (land, L['swipe']),
        'landscape-reader': (land, [*L['open'], *L['next_page'], *L['swipe']]),
        'landscape-menu': (land, [*L['open'], *L['next_page'], *L['menu']]),
        'landscape-text': (land, [*L['open'], *L['next_page'], *L['menu'], *L['text']]),
        'landscape-layout': (land, [*L['open'], *L['next_page'], *L['menu'], *L['layout']]),
        # 6" small screen: the six tiles wrap to 3 x 2 in both orientations.
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


if __name__ == '__main__':
    main()
