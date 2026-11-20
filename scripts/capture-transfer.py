#!/usr/bin/env python3
"""Simulator screenshots of the Wi-Fi transfer screen in every state, and
the two ways in: Settings → Device and the library's top bar.

Run cargo build first. Outputs live in gitignored screenshots/transfer/.
VERSO_SIM_TRANSFER sets each state up (see transfer/device.rs): `kindle`
shows a Kindle's name and address, `code=` fixes the code, `activity`
fills the list and `phase=` shows a state without starting anything. The
ready states really serve on 127.0.0.1 (VERSO_TRANSFER_PORT, default 8080).
"""
import json
import os
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / 'screenshots/transfer'

TRANSFER = {
    'ready': 'kindle,code=4827,activity',
    'ready-empty': 'kindle,code=4827',
    'ready-simulator': 'code=4827',
    'starting': 'phase=starting',
    'waiting': 'phase=waiting',
    'no-network': 'phase=no-network',
    'stopped-idle': 'phase=idle',
    'stopped-slept': 'phase=slept',
    'failed-wifi': 'phase=no-wifi',
    'failed-port': 'phase=port',
}

# The ways in, portrait: Settings → Device, and Home's top bar.
ENTRIES = {
    'entry-settings': ['--screen', 'settings', '--tap', '867,272'],
    'entry-home': ['--screen', 'books'],
}


def main():
    books = OUT / 'books'
    books.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(ROOT / 'crates/verso/tests/fixtures/search.epub', books / 'search.epub')
    for name, options in TRANSFER.items():
        capture(f'portrait-{name}', 'portrait', options, ['--screen', 'transfer'], books)
    for name in ['ready', 'no-network', 'failed-port']:
        capture(f'landscape-{name}', 'landscape', TRANSFER[name], ['--screen', 'transfer'], books)
    for name, steps in ENTRIES.items():
        capture(name, 'portrait', '', steps, books)
    print(OUT)


def capture(name, orientation, options, steps, books):
    state = OUT / 'state' / name
    shutil.rmtree(state, ignore_errors=True)
    state.mkdir(parents=True)
    settings = {'library_folders': [str(books)], 'orientation': orientation}
    (state / 'settings.json').write_text(json.dumps(settings))
    env = dict(os.environ, VERSO_DATA=str(state), VERSO_BOOKS=str(books), VERSO_SIM_BATTERY='82',
               VERSO_SIM_TRANSFER=options, RUST_LOG='error')
    subprocess.run([str(ROOT / 'target/debug/verso'), '--screenshot', str(OUT / (name + '.png')), *steps],
                   env=env, check=True, stderr=subprocess.DEVNULL)


if __name__ == '__main__':
    main()
