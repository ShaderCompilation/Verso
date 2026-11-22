#!/usr/bin/env python3
"""Page bookmarks, Phase 2 (docs/bookmarks.md): the mark, the menu's ribbon,
the corner tap and its setting, and the remove-with-note question.

Run cargo build first. Outputs go to gitignored screenshots/bookmarks/, each
scene with isolated state. Taps come from the theme's millimetre sizes, as in
capture-devices.py.

    python3 scripts/capture-bookmarks.py
"""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / 'screenshots/bookmarks'
spec = importlib.util.spec_from_file_location('capture_devices', ROOT / 'scripts/capture-devices.py')
devices = importlib.util.module_from_spec(spec)
spec.loader.exec_module(devices)


def steps(s):
    """Opening the book at page 2, the corner, the menu and its ribbon."""
    page2 = devices.scenes(s)['reader'][2]
    corner = s.tap(s.w - s.mm(4), s.mm(4))
    menu = s.tap(s.w / 2, s.h / 2)
    # The ribbon sits left of Search, the Sun and the battery (13.5 mm).
    ribbon = s.tap(s.w - s.mm(5 + 13.5 + 22.5), s.mm(13) / 2)
    return page2, corner, menu, ribbon


def run(name, device, settings, script, state=None, screen=None):
    w, h, dpi = devices.DEVICES[device]
    fresh = state is None
    state = state or OUT / 'state' / name
    if fresh:
        shutil.rmtree(state, ignore_errors=True)
        state.mkdir(parents=True)
        (state / 'settings.json').write_text(json.dumps({'library_folders': [str(OUT / 'book')], **settings}))
    env = {k: v for k, v in os.environ.items() if not k.startswith('VERSO_SIM_')}
    env.update(VERSO_DATA=str(state), VERSO_BOOKS=str(OUT / 'book'), VERSO_SIM_BATTERY='82', RUST_LOG='error')
    args = [str(ROOT / 'target/debug/verso'), '--device', device, '--screenshot', str(OUT / f'{name}.png'), *script]
    if screen:
        args += ['--screen', screen]
    subprocess.run(args, env=env, check=True, timeout=90, stderr=subprocess.DEVNULL)
    return state


def records(state):
    manifests = list((state / 'annotations').glob('book-*.json'))
    if not manifests:
        return []
    ids = json.loads(manifests[0].read_text())['records']
    return [json.loads((state / 'annotations' / f'{i}.json').read_text()) for i in ids]


def main():
    (OUT / 'book').mkdir(parents=True, exist_ok=True)
    shutil.copyfile(ROOT / 'crates/verso/tests/fixtures/selection.epub', OUT / 'book/selection.epub')
    portrait = devices.Screen(*devices.DEVICES['pw6'])
    page2, corner, menu, ribbon = steps(portrait)

    # The corner tap, then the menu shows the ribbon filled; tapping it
    # removes the bookmark and the menu stays open.
    state = run('mark', 'pw6', {}, [*page2, *corner])
    saved = records(state)
    assert len(saved) == 1 and saved[0]['kind'] == 'bookmark' and saved[0]['quotation'], saved
    run('menu-on', 'pw6', {}, [*page2, *corner, *menu])
    state = run('menu-off', 'pw6', {}, [*page2, *corner, *menu, *ribbon])
    assert records(state) == []
    run('menu-add', 'pw6', {}, [*page2, *menu, *ribbon])

    # Margins: centred in a side margin of 5 mm or more, 1.5 mm from the
    # edge below that; with the top bar on; landscape; a 6" screen.
    for sides in (2, 4, 5, 7, 15):
        run(f'margin-{sides}', 'pw6', {'page_layout': {'margins': {'sides': sides}}}, [*page2, *corner])
    run('top-bar', 'pw6', {'page_layout': {'top': {'on': True}, 'margins': {'sides': 3}}}, [*page2, *corner])
    for device in ('pw6', 'basic-167'):
        w, h, dpi = devices.DEVICES[device]
        s = devices.Screen(h, w, dpi)
        p2, c, m, _ = steps(s)
        run(f'landscape-{device}', device, {'orientation': 'landscape'}, [*p2, *c])
        run(f'landscape-menu-{device}', device, {'orientation': 'landscape'}, [*p2, *c, *m])
    small = devices.Screen(*devices.DEVICES['basic-167'])
    p2, c, m, _ = steps(small)
    run('basic-167', 'basic-167', {}, [*p2, *c])
    run('basic-167-menu', 'basic-167', {}, [*p2, *c, *m])

    # Turned off, the corner turns the page like the rest of the forward zone.
    state = run('corner-off', 'pw6', {'bookmark_corner': False}, [*page2, *corner])
    assert records(state) == []
    run('settings', 'pw6', {}, [], screen='settings')
    run('settings-off', 'pw6', {'bookmark_corner': False}, [], screen='settings')
    run('settings-left-handed', 'pw6', {'tap_zones': 'mirrored'}, [], screen='settings')

    # A bookmark with a note: the corner asks first. Notes are added from the
    # Notes list (Phase 3), so this one is written into the record.
    state = run('note', 'pw6', {}, [*page2, *corner])
    record = records(state)[0]
    path = state / 'annotations' / f"{record['id']}.json"
    text = path.read_text()
    assert '"note":null' in text
    path.write_text(text.replace('"note":null', '"note":"Opening image"'))
    resume = portrait.tap(200, 550)
    run('confirm', 'pw6', {}, [*resume, *corner], state=state)
    assert len(records(state)) == 1, 'nothing removed before confirming'
    margin, button_h = portrait.mm(5), portrait.mm(10)
    width = (portrait.w - 2 * margin - portrait.mm(3)) / 2
    remove = portrait.tap(portrait.w - margin - width / 2, portrait.h - margin - button_h / 2)
    run('confirm-menu', 'pw6', {}, [*resume, *menu, *ribbon], state=state)
    run('confirmed', 'pw6', {}, [*resume, *corner, *remove], state=state)
    assert records(state) == []
    print(OUT)


if __name__ == '__main__':
    main()
