#!/usr/bin/env python3
"""Capture Phases 4 and 5 with original fixtures and isolated state; build the simulator first."""
import json
import os
import shutil
import subprocess
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / 'screenshots/highlights'


def main():
    books = OUT / 'books'
    books.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(ROOT / 'crates/verso/tests/fixtures/selection.epub', books / 'selection.epub')
    opening = ['--tap', '636,1560', '--tap', '165,500']  # Home → Library → book
    passage = [*opening, '--hold-drag', '200,110,500,190']
    extend = [*passage, '--tap', '338,1535']
    moved = [*extend, '--tap', '490,1600', '--tap', '500,200']
    save = [*moved, '--tap', '800,1600']
    scenes = {'passage': passage, 'extension': extend, 'cross-page': moved,
              'saved': save, 'cancel': [*moved, '--tap', '1100,1600'],
              'backward': [*moved, '--tap', '200,1600', '--tap', '180,110']}
    for name, steps in scenes.items():
        state = OUT / (name + '-state')
        shutil.rmtree(state, ignore_errors=True)
        state.mkdir(parents=True)
        (state / 'settings.json').write_text(json.dumps({'library_folders': [str(books)]}))
        env = dict(os.environ, VERSO_DATA=str(state), VERSO_BOOKS=str(books), RUST_LOG='error')
        subprocess.run([str(ROOT / 'target/debug/verso'), '--screenshot', str(OUT / (name + '.png')), *steps], env=env, check=True, timeout=30)
    state = OUT / 'saved-state'
    env = dict(os.environ, VERSO_DATA=str(state), VERSO_BOOKS=str(books), RUST_LOG='error')
    resume = ['--tap', '200,550']
    # Centre tap → Reader Menu → Highlights.
    listing = [*resume, '--tap', '636,848', '--tap', '487,1584']
    for name, steps in {'restarted': resume, 'list': listing, 'details': [*listing, '--tap', '400,400'],
                        'jump': [*listing, '--tap', '400,400', '--tap', '300,1500']}.items():
        subprocess.run([str(ROOT / 'target/debug/verso'), '--screenshot', str(OUT / (name + '.png')), *steps], env=env, check=True, timeout=30)
    # Import only original fixture text. An edited source quotation must stay
    # readable/removable as unresolved; removing a valid import retains its ID.
    manifest = next((state / 'annotations').glob('book-*.json'))
    ids = json.loads(manifest.read_text())['records']
    annotation = json.loads((state / 'annotations' / (ids[0] + '.json')).read_text())
    sidecar = books / 'selection.sdr'
    sidecar.mkdir(exist_ok=True)
    quote = lambda text: json.dumps(text, ensure_ascii=False)
    entries = []
    for text in [annotation['quotation'], 'Edited quotation']:
        entries.append('{pos0=' + quote(annotation['range']['start']) + ',pos1=' + quote(annotation['range']['end']) + ',text=' + quote(text) + ',note="Imported note",datetime="2026-11-15 00:00:00",chapter="Fixture"}')
    source = sidecar / 'metadata.epub.lua'
    source.write_text('return {annotations={' + ','.join(entries) + '}}')
    before = source.read_bytes()
    importing = [*listing, '--tap', '338,230']
    for name, steps in {'preview': importing, 'imported': [*importing, '--tap', '300,1500'],
                        'imported-note': [*listing, '--tap', '400,650'],
                        'unresolved': [*listing, '--tap', '400,900'],
                        'removed-import': [*listing, '--tap', '400,650', '--tap', '900,1500'],
                        'duplicates-after-removal': importing,
                        # Editing the path hides the old counts; a missing file
                        # reports an import error, never "Retry saving".
                        'stale-path': [*importing, '--tap', '600,370', '--text', 'x', '--edit', 'submit'],
                        'import-error': [*importing, '--tap', '600,370', '--text', 'x', '--edit', 'submit',
                                         '--tap', '300,1500'],
                        'after-import-error': [*importing, '--tap', '600,370', '--text', 'x', '--edit', 'submit',
                                               '--tap', '300,1500', '--tap', '80,75']}.items():
        subprocess.run([str(ROOT / 'target/debug/verso'), '--screenshot', str(OUT / (name + '.png')), *steps], env=env, check=True, timeout=30)
    result = json.loads(manifest.read_text())
    assert len(result['records']) == 2 and len(result['sources']) == 2
    assert source.read_bytes() == before
    settings = json.loads((state / 'settings.json').read_text())
    settings['orientation'] = 'landscape'
    settings['typography'] = {'size_pt': 16.0}
    (state / 'settings.json').write_text(json.dumps(settings))
    subprocess.run([str(ROOT / 'target/debug/verso'), '--screenshot', str(OUT / 'landscape.png'), *resume], env=env, check=True, timeout=30)
    notes(passage, resume)
    print(OUT)


def notes(passage, resume):
    """Phase 5: notes from a selection and from details, the list, export."""
    # A copy of the fixture with an author, so the export shows one.
    books = OUT / 'notes-books'
    shutil.rmtree(books, ignore_errors=True)
    books.mkdir(parents=True)
    with zipfile.ZipFile(ROOT / 'crates/verso/tests/fixtures/selection.epub') as src, \
            zipfile.ZipFile(books / 'selection.epub', 'w') as dst:
        for item in src.infolist():
            data = src.read(item)
            if item.filename.endswith('.opf'):
                data = data.replace(b'</dc:title>', b'</dc:title><dc:creator>Fixture Author</dc:creator>')
            dst.writestr(item, data)

    def run(name, steps, state, fresh=False):
        if fresh:
            shutil.rmtree(state, ignore_errors=True)
            state.mkdir(parents=True)
            (state / 'settings.json').write_text(json.dumps({'library_folders': [str(books)]}))
        env = dict(os.environ, VERSO_DATA=str(state), VERSO_BOOKS=str(books), RUST_LOG='error')
        subprocess.run([str(ROOT / 'target/debug/verso'), '--screenshot', str(OUT / (name + '.png')), *steps],
                       env=env, check=True, timeout=30)

    add_note = [*passage, '--tap', '933,1394']
    first = 'A first thought on this passage.'
    run('note-sheet', passage, OUT / 'scratch-state', fresh=True)
    run('note-editor', [*add_note, '--text', first], OUT / 'scratch-state', fresh=True)
    state = OUT / 'notes-state'
    run('note-saved', [*add_note, '--text', first, '--edit', 'submit'], state, fresh=True)
    listing = [*resume, '--tap', '636,848', '--tap', '487,1584']
    edit = [*listing, '--tap', '400,400', '--tap', '300,1337', '--edit', 'clear', '--text',
            'An edited note that is long enough to wrap across more than one line of the note box, with ünïcödé.']
    run('notes-list', listing, state)
    run('note-details', [*listing, '--tap', '400,400'], state)
    run('note-edit', edit, state)
    run('note-edit-hidden-keyboard', [*edit, '--edit', 'dismiss'], state)
    run('note-edited', [*edit, '--edit', 'submit'], state)
    run('notes-exported', [*listing, '--tap', '930,230'], state)
    manifest = next((state / 'annotations').glob('book-*.json'))
    ids = json.loads(manifest.read_text())['records']
    assert len(ids) == 1
    record = json.loads((state / 'annotations' / (ids[0] + '.json')).read_text())
    assert record['note'].startswith('An edited note'), record
    exports = list((state / 'exports').glob('*.md'))
    assert len(exports) == 1, exports
    md = exports[0].read_text(encoding='utf-8')
    quote = ' '.join(record['quotation'].split())
    for expected in ['# Selection fixture', 'Fixture Author', '## ', '> ' + quote.split()[0],
                     '**Note:** An edited note', 'ünïcödé']:
        assert expected in md, (expected, md)
    print(exports[0])
    # Deleting the note keeps the highlight.
    run('note-deleted', [*listing, '--tap', '400,400', '--tap', '900,1337'], state)
    record = json.loads((state / 'annotations' / (json.loads(manifest.read_text())['records'][0] + '.json')).read_text())
    assert record['note'] is None and len(json.loads(manifest.read_text())['records']) == 1
    # Landscape: tapping the highlight opens details; the editor and the
    # three-column selection sheet fit the short screen.
    settings = json.loads((state / 'settings.json').read_text())
    settings['orientation'] = 'landscape'
    (state / 'settings.json').write_text(json.dumps(settings))
    run('landscape-note-editor', [*resume, '--tap', '500,110', '--tap', '445,913', '--text', 'Landscape note'], state)
    run('landscape-selection-sheet', [*resume, '--hold-drag', '300,260,700,330'], state)


if __name__ == '__main__':
    main()
