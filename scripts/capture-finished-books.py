#!/usr/bin/env python3
"""Finished books, Phase 2: the end-of-book
sheet after a tap past the last page, in portrait, landscape and on a 6"
screen, with and without a next book in the folder; Undo, Next and Home,
checking the saved states.

Run cargo build first. Outputs go to gitignored screenshots/finished-books/,
each scene with isolated state. The page buttons reach the end: over the
sheet they do nothing, so extra presses are harmless.

    python3 scripts/capture-finished-books.py
"""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / 'screenshots/finished-books'
spec = importlib.util.spec_from_file_location('capture_devices', ROOT / 'scripts/capture-devices.py')
devices = importlib.util.module_from_spec(spec)
spec.loader.exec_module(devices)

FIXTURE = ROOT / 'crates/verso/tests/fixtures/selection.epub'
# More than the fixture's pages on any screen, then one past the end.
TO_THE_END = ['--key', 'next'] * 40


def first_book(s):
    """Home's Library card, then the first book."""
    margin, target = s.mm(5), s.mm(9)
    card_h = max(target, min(s.mm(13.2), s.h // 8))
    return [*s.tap(s.w / 2, s.h - margin - card_h / 2), *s.tap(margin + s.mm(9), s.mm(42))]


def sheet_buttons(s, next_book):
    """Undo, Home and Next (or None) on the end-of-book sheet."""
    margin, gap = s.mm(5), s.mm(3)
    width = s.w - 2 * margin
    y = s.h - s.mm(4) - s.mm(10) / 2
    if next_book and s.w > s.h:
        pair = width // 2 - gap // 2
        right = width - width // 2 - gap // 2
        nxt = s.tap(s.w - margin - right / 2, y)
    else:
        pair = width
        nxt = s.tap(s.w / 2, y - s.mm(10) - gap) if next_book else None
    half = (pair - gap) // 2
    return s.tap(margin + half / 2, y), s.tap(margin + pair - half / 2, y), nxt


def run(name, device, settings, script, books):
    state = OUT / 'state' / name
    shutil.rmtree(state, ignore_errors=True)
    state.mkdir(parents=True)
    (state / 'settings.json').write_text(json.dumps({'library_folders': [str(books)], **settings}))
    env = {k: v for k, v in os.environ.items() if not k.startswith('VERSO_SIM_')}
    env.update(VERSO_DATA=str(state), VERSO_BOOKS=str(books), VERSO_SIM_BATTERY='82', RUST_LOG='error')
    args = [str(ROOT / 'target/debug/verso'), '--device', device, '--screenshot', str(OUT / f'{name}.png'), *script]
    subprocess.run(args, env=env, check=True, timeout=120, stderr=subprocess.DEVNULL)
    return state


def states(state):
    """Saved reading states by file name."""
    ids = {b['path'].rsplit('/', 1)[1]: b['id'] for b in json.loads((state / 'library.json').read_text())['books']}
    out = {}
    for name, id_ in ids.items():
        path = state / 'books' / f'{id_}.json'
        out[name] = json.loads(path.read_text()) if path.exists() else None
    return out


def folder(name, files):
    path = OUT / name
    shutil.rmtree(path, ignore_errors=True)
    path.mkdir(parents=True)
    fixture = FIXTURE.read_bytes()
    for n, file in enumerate(files):
        # Distinct sizes give distinct ids.
        (path / file).write_bytes(fixture + b'\0' * n)
    return path


def screens():
    for device in ('pw6', 'basic-167'):
        w, h, dpi = devices.DEVICES[device]
        yield device, {}, '', devices.Screen(w, h, dpi)
        yield device, {'orientation': 'landscape'}, '-landscape', devices.Screen(h, w, dpi)


def main():
    series = folder('series', ['Book 1.epub', 'Book 2.epub', 'Book 10.epub'])
    single = folder('single', ['Only.epub'])

    for device, settings, suffix, s in screens():
        tag = f'{suffix}-{device}'
        state = run(f'sheet{tag}', device, settings, [*first_book(s), *TO_THE_END], series)
        saved = states(state)['Book 1.epub']
        assert saved['finished'] and saved['status_at'] > 0 and not saved.get('rereading'), saved
        run(f'sheet-last{tag}', device, settings, [*first_book(s), *TO_THE_END], single)

    s = devices.Screen(*devices.DEVICES['pw6'])
    undo, home, nxt = sheet_buttons(s, True)
    # Undo: unmarked, the sheet gone, the last page left.
    state = run('undo', 'pw6', {}, [*first_book(s), *TO_THE_END, *undo], series)
    saved = states(state)['Book 1.epub']
    assert not saved['finished'] and saved['status_at'] > 0, saved
    # Next: Book 2 in natural order (not Book 10) opens, Book 1 stays finished.
    state = run('next', 'pw6', {}, [*first_book(s), *TO_THE_END, *nxt], series)
    saved = states(state)
    assert saved['Book 1.epub']['finished'], saved
    assert saved['Book 2.epub'] and saved['Book 2.epub']['page'] == 0, saved
    assert saved['Book 10.epub'] is None, saved
    # Home, from the last book in its folder.
    undo, home, _ = sheet_buttons(s, False)
    state = run('home', 'pw6', {}, [*first_book(s), *TO_THE_END, *home], single)
    saved = states(state)['Only.epub']
    assert saved['finished'], saved
    print(OUT)


if __name__ == '__main__':
    main()
