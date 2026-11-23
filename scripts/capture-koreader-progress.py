#!/usr/bin/env python3
"""KOReader progress, Phases 2–3: the card
that offers KOReader's newer position when a book opens, in portrait,
landscape and on a 6" screen; Go there with the way back, Stay here, a page
turn, the setting off, a position that isn't in the book, and the Settings
row. Then the bulk import: Settings → Library's KOReader row, the preview
sheet, Import, and the saved states.

Run cargo build first. Outputs go to gitignored screenshots/koreader-progress/,
each scene with isolated state. Taps come from the theme's millimetre sizes
(scripts/sim.py).

    python3 scripts/capture-koreader-progress.py
"""
import json
import os
import shutil
import time

import sim

OUT = sim.out_dir('koreader-progress')

# Page 7 of the fixture's 8 in the simulator's portrait layout, mid-page.
KOREADER = '/body[1]/DocFragment[1]/body[1]/p[5]/text()[1].6200'
MISSING = '/body[1]/DocFragment[9]/body[1]/p[1]/text()[1].0'
THREE_DAYS = 3 * 24 * 3600


def book(s):
    """Home's Library card, then the fixture book."""
    return s.open_book()


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


def run(name, device, settings, script, state=None, screen=None, book_state=None, books=None, env_extra=None):
    books = books or OUT / 'book'
    state = state or sim.state(OUT / 'state' / name, books, settings)
    if book_state is not None:
        (state / 'books').mkdir(exist_ok=True)
        sim.write_json(state / 'books' / f'{book_id(state)}.json', book_state)
    return sim.run(OUT / f'{name}.png', script, state, books, device=device, screen=screen, env=env_extra)


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
    shutil.copyfile(sim.FIXTURES / 'selection.epub', OUT / 'book/selection.epub')
    portrait = sim.Screen(*sim.DEVICES['pw6'])
    stay, go = card_buttons(portrait)
    source = sidecar(KOREADER)
    before = source.read_bytes()
    modified = int(source.stat().st_mtime)
    seen = {'modified': modified, 'position': KOREADER}

    # The card on the first page of a book Verso has never opened.
    run('card', 'pw6', {}, book(portrait))
    for device in ('pw6', 'basic-167'):
        w, h, dpi = sim.DEVICES[device]
        landscape = sim.Screen(h, w, dpi)
        run(f'card-landscape-{device}', device, {'orientation': 'landscape'}, book(landscape))
    small = sim.Screen(*sim.DEVICES['basic-167'])
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
    bulk_import()
    print(OUT)


def library_tab(s):
    """Settings → Library: the third of five tabs."""
    margin = s.mm(5)
    return s.tap(margin + 2.5 * (s.w - 2 * margin) / 5, s.mm(13) + margin + s.mm(10) / 2)


def import_row(s):
    """The KOReader row: beside Dictionaries in portrait, below it on the right in landscape."""
    margin = s.mm(5)
    if s.w > s.h:
        column = (s.w - 2 * margin - s.mm(10)) / 2
        return s.tap(s.w - margin - column / 2, s.mm(34 + 23 + 6 + 8 + 7.5))
    return s.tap(s.w * 0.75, s.h - s.mm(5 + 11 + 2 + 23 - 8 - 7.5))


def sheet_buttons(s):
    """Cancel and Import at the bottom of the import sheet."""
    margin = s.mm(5)
    w = (s.w - 2 * margin - s.mm(3)) / 2
    y = s.h - s.mm(10)
    return s.tap(margin + w / 2, y), s.tap(margin + w + s.mm(3) + w / 2, y)


def bulk_import():
    """Phase 3: four books with KOReader data (one finished, one unreadable)
    and one without, imported from Settings → Library."""
    library = OUT / 'library'
    shutil.rmtree(library, ignore_errors=True)
    library.mkdir(parents=True)
    fixture = (sim.FIXTURES / 'selection.epub').read_bytes()
    # Distinct sizes give distinct ids (the id hashes the name and size).
    sidecars = {
        'Current.epub': ('reading', 0.78, 3600),
        'Finished.epub': ('complete', 1.0, THREE_DAYS),
        'Started.epub': ('reading', 0.12, 2 * THREE_DAYS),
        'Broken.epub': (None, 0, THREE_DAYS),
        'Plain.epub': None,
    }
    now = time.time()
    for n, (name, data) in enumerate(sidecars.items()):
        (library / name).write_bytes(fixture + b'\0' * n)
        if data is None:
            continue
        status, percent, age = data
        folder = library / (name[:-5] + '.sdr')
        folder.mkdir()
        path = folder / 'metadata.epub.lua'
        if status is None:
            path.write_text('return { last_xpointer = os.exit() }\n')
        else:
            path.write_text('return {\n'
                            f'    ["last_xpointer"] = "{KOREADER}",\n'
                            f'    ["percent_finished"] = {percent},\n'
                            f'    ["summary"] = {{ ["status"] = "{status}" }},\n}}\n')
        os.utime(path, (now - age, now - age))
    koreader = OUT / 'koreader'
    koreader.mkdir(exist_ok=True)
    opened = int(now - 1800)
    (koreader / 'history.lua').write_text(
        f'return {{ [1] = {{ ["file"] = "{library / "Current.epub"}", ["time"] = {opened} }} }}\n')
    env = {'VERSO_KOREADER': str(koreader)}
    checksums = {p: p.read_bytes() for p in library.rglob('*.lua')}

    def scanned(name, device, settings):
        """A state whose library.json has the five books, from a scan run."""
        state = run(name, device, settings, [], books=library, env_extra=env)
        assert len(json.loads((state / 'library.json').read_text())['books']) == 5
        return state

    for device, settings, suffix in (('pw6', {}, ''), ('pw6', {'orientation': 'landscape'}, '-landscape'),
                                     ('basic-167', {}, '-basic-167'),
                                     ('basic-167', {'orientation': 'landscape'}, '-landscape-basic-167')):
        w, h, dpi = sim.DEVICES[device]
        s = sim.Screen(h, w, dpi) if settings else sim.Screen(w, h, dpi)
        state = scanned(f'import-scan{suffix}', device, settings)
        run(f'settings-library{suffix}', device, settings, library_tab(s), state=state, screen='settings',
            books=library, env_extra=env)
        run(f'import-preview{suffix}', device, settings, [*library_tab(s), *import_row(s)], state=state,
            screen='settings', books=library, env_extra=env)
        assert not list((state / 'books').glob('*.json')), 'the preview writes nothing'

    # Import, then the toast over Settings.
    s = sim.Screen(*sim.DEVICES['pw6'])
    cancel, import_ = sheet_buttons(s)
    state = scanned('import-done', 'pw6', {})
    run('import-cancelled', 'pw6', {}, [*library_tab(s), *import_row(s), *cancel], state=state, screen='settings',
        books=library, env_extra=env)
    assert not list((state / 'books').glob('*.json')), 'Cancel writes nothing'
    run('import-done', 'pw6', {}, [*library_tab(s), *import_row(s), *import_], state=state, screen='settings',
        books=library, env_extra=env)
    ids = {b['path'].rsplit('/', 1)[1]: b['id'] for b in json.loads((state / 'library.json').read_text())['books']}
    saved_state = lambda name: json.loads((state / 'books' / f'{ids[name]}.json').read_text())
    current, finished, started = saved_state('Current.epub'), saved_state('Finished.epub'), saved_state('Started.epub')
    assert current['position'] == KOREADER and current['last_opened'] == opened, current
    assert finished['finished'] and finished['progress'] == 10000, finished
    assert started['progress'] == 1200 and 'koreader_seen' in started, started
    assert not (state / 'books' / f'{ids["Plain.epub"]}.json').exists()
    assert not (state / 'books' / f'{ids["Broken.epub"]}.json').exists()
    home = json.loads((state / 'home.json').read_text())
    assert home['books'][0]['book']['id'] == ids['Current.epub'], 'Continue reading is the current KOReader book'
    # Home after the import, and the same import again: up to date.
    run('import-home', 'pw6', {}, [], state=state, books=library, env_extra=env)
    run('import-again', 'pw6', {}, [*library_tab(s), *import_row(s)], state=state, screen='settings',
        books=library, env_extra=env)
    assert all(p.read_bytes() == b for p, b in checksums.items()), 'KOReader files stay untouched'


if __name__ == '__main__':
    main()
