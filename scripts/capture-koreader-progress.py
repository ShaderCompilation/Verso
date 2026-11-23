#!/usr/bin/env python3
"""KOReader progress, Phase 2 (docs/koreader-progress.md §4): the card that
offers KOReader's newer position when a book opens, in portrait, landscape
and on a 6" screen; Go there with the way back, Stay here, a page turn, the
setting off, a position that isn't in the book, and the Settings row.

Run cargo build first. Outputs go to gitignored screenshots/koreader-progress/,
each scene with isolated state. Taps come from the theme's millimetre sizes,
as in capture-devices.py.

    python3 scripts/capture-koreader-progress.py
"""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import time

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / 'screenshots/koreader-progress'
spec = importlib.util.spec_from_file_location('capture_devices', ROOT / 'scripts/capture-devices.py')
devices = importlib.util.module_from_spec(spec)
spec.loader.exec_module(devices)

# Page 7 of the fixture's 8 in the simulator's portrait layout, mid-page.
KOREADER = '/body[1]/DocFragment[1]/body[1]/p[5]/text()[1].6200'
MISSING = '/body[1]/DocFragment[9]/body[1]/p[1]/text()[1].0'
THREE_DAYS = 3 * 24 * 3600


def book(s):
    """Home's Library card, then the fixture book."""
    margin, target = s.mm(5), s.mm(9)
    card_h = max(target, min(s.mm(13.2), s.h // 8))
    return [*s.tap(s.w / 2, s.h - margin - card_h / 2), *s.tap(margin + s.mm(9), s.mm(42))]


def card_buttons(s):
    """Stay here and Go there, on the card above the default bottom bar."""
    w = min(s.w - 2 * s.mm(5), s.mm(110))
    x = s.w / 2 - w / 2
    bottom = s.h - s.mm(9) - s.mm(1.5)
    y = bottom - s.mm(4) - s.mm(10) / 2
    button = min((w - s.mm(8) - s.mm(3)) / 2, s.mm(36))
    go = x + w - s.mm(4) - button / 2
    return s.tap(go - button - s.mm(3), y), s.tap(go, y)


def sidecar(position, age=THREE_DAYS):
    """KOReader's metadata file beside the book, saved `age` seconds ago."""
    folder = OUT / 'book/selection.sdr'
    folder.mkdir(parents=True, exist_ok=True)
    path = folder / 'metadata.epub.lua'
    path.write_text('-- we can read Lua syntax here!\nreturn {\n'
                    f'    ["last_xpointer"] = "{position}",\n'
                    '    ["percent_finished"] = 0.78,\n'
                    '    ["summary"] = {\n        ["status"] = "reading",\n    },\n}\n')
    at = time.time() - age
    os.utime(path, (at, at))
    return path


def run(name, device, settings, script, state=None, screen=None, book_state=None):
    fresh = state is None
    state = state or OUT / 'state' / name
    if fresh:
        shutil.rmtree(state, ignore_errors=True)
        state.mkdir(parents=True)
        (state / 'settings.json').write_text(json.dumps({'library_folders': [str(OUT / 'book')], **settings}))
    if book_state is not None:
        (state / 'books').mkdir(exist_ok=True)
        (state / 'books' / f'{book_id(state)}.json').write_text(json.dumps(book_state))
    env = {k: v for k, v in os.environ.items() if not k.startswith('VERSO_SIM_')}
    env.update(VERSO_DATA=str(state), VERSO_BOOKS=str(OUT / 'book'), VERSO_SIM_BATTERY='82', RUST_LOG='error')
    args = [str(ROOT / 'target/debug/verso'), '--device', device, '--screenshot', str(OUT / f'{name}.png'), *script]
    if screen:
        args += ['--screen', screen]
    subprocess.run(args, env=env, check=True, timeout=90, stderr=subprocess.DEVNULL)
    return state


def book_id(state):
    """The fixture's id, from a scan of the library (run once per state)."""
    library = state / 'library.json'
    if not library.exists():
        run('scan', 'pw6', {}, [], state=state)
    return json.loads(library.read_text())['books'][0]['id']


def saved(state):
    files = list((state / 'books').glob('*.json'))
    return json.loads(files[0].read_text()) if files else {}


def main():
    shutil.rmtree(OUT / 'book', ignore_errors=True)
    (OUT / 'book').mkdir(parents=True)
    shutil.copyfile(ROOT / 'crates/verso/tests/fixtures/selection.epub', OUT / 'book/selection.epub')
    portrait = devices.Screen(*devices.DEVICES['pw6'])
    stay, go = card_buttons(portrait)
    source = sidecar(KOREADER)
    before = source.read_bytes()
    modified = int(source.stat().st_mtime)
    seen = {'modified': modified, 'position': KOREADER}

    # The card on the first page of a book Verso has never opened.
    run('card', 'pw6', {}, book(portrait))
    for device in ('pw6', 'basic-167'):
        w, h, dpi = devices.DEVICES[device]
        landscape = devices.Screen(h, w, dpi)
        run(f'card-landscape-{device}', device, {'orientation': 'landscape'}, book(landscape))
    small = devices.Screen(*devices.DEVICES['basic-167'])
    run('card-basic-167', 'basic-167', {}, book(small))
    run('card-top-bar', 'pw6', {'page_layout': {'top': {'on': True}, 'bottom': {'on': False}}}, book(portrait))

    # Go there: KOReader's page, with the way back to page 1.
    state = run('go-there', 'pw6', {}, [*book(portrait), *go])
    s = saved(state)
    assert s.get('koreader_seen') == seen and s['page'] > 0, s
    # Stay here: page 1, and opening the book again doesn't ask twice.
    state = run('stay-here', 'pw6', {}, [*book(portrait), *stay])
    s = saved(state)
    assert s.get('koreader_seen') == seen and s['page'] == 0, s
    run('stay-here-reopened', 'pw6', {}, book(portrait), state=state)
    # A page turn counts as Stay here.
    cy = portrait.h / 2
    state = run('page-turn', 'pw6', {}, [*book(portrait), *portrait.swipe(portrait.w * 0.8, cy, portrait.w * 0.2, cy)])
    s = saved(state)
    assert s.get('koreader_seen') == seen and s['page'] == 1, s
    # Off: no card, nothing recorded.
    state = run('setting-off', 'pw6', {'koreader_progress': False}, book(portrait))
    assert 'koreader_seen' not in saved(state)

    # An imported position that isn't in the book opens at the start, saying so.
    sidecar(MISSING)
    state = OUT / 'state/lost'
    shutil.rmtree(state, ignore_errors=True)
    state.mkdir(parents=True)
    (state / 'settings.json').write_text(json.dumps({'library_folders': [str(OUT / 'book')]}))
    imported = {'position': MISSING, 'progress': 7800, 'last_opened': int(time.time()),
                'koreader_seen': {'modified': int(source.stat().st_mtime), 'position': MISSING}}
    run('lost', 'pw6', {}, book(portrait), state=state, book_state=imported)

    # The Settings row, in both orientations and on 6".
    run('settings', 'pw6', {}, [], screen='settings')
    run('settings-off', 'pw6', {'koreader_progress': False}, [], screen='settings')
    run('settings-landscape', 'pw6', {'orientation': 'landscape'}, [], screen='settings')
    run('settings-basic-167', 'basic-167', {}, [], screen='settings')
    run('settings-landscape-basic-167', 'basic-167', {'orientation': 'landscape'}, [], screen='settings')

    sidecar(KOREADER)
    assert source.read_bytes() == before, 'KOReader files stay untouched'
    print(OUT)


if __name__ == '__main__':
    main()
