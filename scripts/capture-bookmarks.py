#!/usr/bin/env python3
"""Page bookmarks, Phases 2 to 4: the mark, the menu's
ribbon, the corner tap and its setting, the remove-with-note question, the
Notes list (filter, details, a note, jumping back and removing), and KOReader
import and Markdown export.

Run cargo build first. Outputs go to gitignored screenshots/bookmarks/, each
scene with isolated state. Taps come from the theme's millimetre sizes, through
scripts/sim.py.

    python3 scripts/capture-bookmarks.py
"""
import json
import shutil

import sim

OUT = sim.out_dir('bookmarks')


def steps(s):
    """Opening the book at page 2, the corner, the menu and its ribbon."""
    page2 = [*s.open_book(), *s.swipe(s.w * 0.8, s.h / 2, s.w * 0.2, s.h / 2)]
    corner = s.tap(s.w - s.mm(4), s.mm(4))
    menu = s.tap(s.w / 2, s.h / 2)
    # The ribbon sits left of Search, the Sun and the battery (13.5 mm).
    ribbon = s.tap(s.w - s.mm(5 + 13.5 + 22.5), s.mm(13) / 2)
    return page2, corner, menu, ribbon


def notes_taps(s):
    """The menu's Notes button and, in the Notes list: the filter's
    segments, the first row, and the details' buttons."""
    margin, target, gap = s.mm(5), s.mm(9), s.mm(3)
    notes = s.tap(margin + 1.5 * (s.w - 2 * margin) / 4, s.h - s.mm(9.5))
    filter_y = s.mm(13) + s.mm(2) + target + gap + target / 2
    segment = lambda i: s.tap(margin + (i + 0.5) * (s.w - 2 * margin) / 3, filter_y)
    row = s.tap(s.w / 2, filter_y + target / 2 + gap + s.mm(15) / 2)
    width = (s.w - 2 * margin - gap) / 2
    left, right = margin + width / 2, s.w - margin - width / 2
    buttons_y = s.h - s.mm(11) - gap - target / 2
    note_y = buttons_y - target - gap
    return {'notes': notes, 'segment': segment, 'row': row, 'jump': s.tap(left, buttons_y),
            'remove': s.tap(right, buttons_y), 'note': s.tap(left, note_y)}


def run(name, device, settings, script, state=None, screen=None):
    state = state or sim.state(OUT / 'state' / name, OUT / 'book', settings)
    return sim.run(OUT / f'{name}.png', script, state, OUT / 'book', device=device, screen=screen)


def records(state):
    manifests = list((state / 'annotations').glob('book-*.json'))
    if not manifests:
        return []
    ids = json.loads(manifests[0].read_text())['records']
    return [json.loads((state / 'annotations' / f'{i}.json').read_text()) for i in ids]


def main():
    sim.books(OUT / 'book', 'selection.epub')
    portrait = sim.Screen(*sim.DEVICES['pw6'])
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
        w, h, dpi = sim.DEVICES[device]
        s = sim.Screen(h, w, dpi)
        p2, c, m, _ = steps(s)
        run(f'landscape-{device}', device, {'orientation': 'landscape'}, [*p2, *c])
        run(f'landscape-menu-{device}', device, {'orientation': 'landscape'}, [*p2, *c, *m])
    small = sim.Screen(*sim.DEVICES['basic-167'])
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
    notes(portrait, page2, corner, menu)
    koreader(portrait, page2, corner, menu)
    print(OUT)


def notes(s, page2, corner, menu):
    """Phase 3: the Notes list, its filter, a bookmark's details and note,
    jumping to the bookmark and back, and removing it from the list."""
    n = notes_taps(s)
    cy = s.h / 2
    page3 = [*page2, *corner, *s.swipe(s.w * 0.8, cy, s.w * 0.2, cy)]
    listing = [*page3, *menu, *n['notes']]
    state = run('notes-list', 'pw6', {}, listing)
    assert len(records(state)) == 1
    run('notes-bookmarks', 'pw6', {}, [*listing, *n['segment'](2)])
    run('notes-highlights', 'pw6', {}, [*listing, *n['segment'](1)])
    details = [*listing, *n['row']]
    run('notes-details', 'pw6', {}, details)
    state = run('notes-note', 'pw6', {}, [*details, *n['note'], '--text', 'Opening image', '--edit', 'submit'])
    saved = records(state)
    assert len(saved) == 1 and saved[0]['kind'] == 'bookmark' and saved[0]['note'] == 'Opening image', saved
    # Jumping shows page 2 with the way back to page 3.
    run('notes-jump', 'pw6', {}, [*details, *n['jump']])
    state = run('notes-removed', 'pw6', {}, [*details, *n['remove']])
    assert records(state) == []
    # The list on a 6" screen in both orientations.
    for name, screen, settings in [('notes-basic-167', sim.Screen(600, 800, 167), {}),
                                   ('notes-landscape-basic-167', sim.Screen(800, 600, 167),
                                    {'orientation': 'landscape'})]:
        p2, c, m, _ = steps(screen)
        run(name, 'basic-167', settings, [*p2, *c, *m, *notes_taps(screen)['notes']])



def koreader(s, page2, corner, menu):
    """Phase 4: importing KOReader page bookmarks (a resolved one, one whose
    place isn't in the book and a PDF one, next to a highlight that doesn't
    resolve), exporting them, and importing the same file again."""
    n = notes_taps(s)
    # A real page start: where Verso's own bookmark on page 2 starts.
    own = records(run('koreader-own', 'pw6', {}, [*page2, *corner]))[0]
    sidecar = OUT / 'book/selection.sdr'
    sidecar.mkdir(exist_ok=True)
    source = sidecar / 'metadata.epub.lua'
    quote = lambda text: json.dumps(text, ensure_ascii=False)
    source.write_text('return {annotations={'
                      '{page=' + quote(own['range']['start']) + ',text="in Fixture",chapter="Fixture",'
                      'datetime="2026-11-20 08:00:00",note="From KOReader",pageno=2},'
                      '{page="/body/DocFragment[99]/body/p[1]/text().0",text="in Nowhere",chapter="Nowhere"},'
                      '{page=3,text="in PDF"},'
                      '{drawer="lighten",pos0="/body/DocFragment[99]/body/p[1]/text().0",'
                      'pos1="/body/DocFragment[99]/body/p[1]/text().5",text="Lost",chapter="Nowhere"}}}')
    before = source.read_bytes()
    try:
        listing = [*page2, *menu, *n['notes']]
        top = s.mm(13) + s.mm(2) + s.mm(9) / 2
        importing = [*listing, *s.tap(s.w * 0.27, top)]
        confirm = s.tap(300, 1500)
        state = run('koreader-preview', 'pw6', {}, importing)
        assert records(state) == []
        run('koreader-imported', 'pw6', {}, [*importing, *confirm], state=state)
        saved = records(state)
        kinds = sorted(r.get('kind', 'highlight') for r in saved)
        assert kinds == ['bookmark', 'highlight'], 'the lost highlight comes in, unresolved'
        b = next(r for r in saved if r.get('kind') == 'bookmark')
        assert b['kind'] == 'bookmark' and b['id'].startswith('import-'), b
        assert b['quotation'] == own['quotation'] and b['range'] == own['range'], (b, own)
        assert (b['note'], b['chapter'], b['created']) == ('From KOReader', 'Fixture', '2026-11-20 08:00:00'), b
        resume = s.tap(200, 550)
        relisting = [*resume, *menu, *n['notes']]
        run('koreader-list', 'pw6', {}, relisting, state=state)
        run('koreader-exported', 'pw6', {}, [*relisting, *s.tap(s.w * 0.73, top)], state=state)
        exports = list((state / 'exports').glob('*.md'))
        assert len(exports) == 1, exports
        md = exports[0].read_text(encoding='utf-8')
        words = ' '.join(own['quotation'].split())
        for expected in ['1 bookmark', f'**Bookmark** · p. 2 — "{words}…"', '**Note:** From KOReader']:
            assert expected in md, (expected, md)
        # Again: the resolved bookmark is already imported; the lost one is
        # still left out.
        run('koreader-again', 'pw6', {}, [*relisting, *s.tap(s.w * 0.27, top)], state=state)
        run('koreader-again-confirmed', 'pw6', {}, [*relisting, *s.tap(s.w * 0.27, top), *confirm], state=state)
        assert len(records(state)) == 2
        assert source.read_bytes() == before, 'KOReader files stay untouched'
        # The preview's text on a 6" screen in landscape.
        small = sim.Screen(800, 600, 167)
        p2, _, m, _ = steps(small)
        small_top = small.mm(13) + small.mm(2) + small.mm(9) / 2
        run('koreader-preview-landscape-basic-167', 'basic-167', {'orientation': 'landscape'},
            [*p2, *m, *notes_taps(small)['notes'], *small.tap(small.w * 0.27, small_top)])
    finally:
        shutil.rmtree(sidecar)


if __name__ == '__main__':
    main()
