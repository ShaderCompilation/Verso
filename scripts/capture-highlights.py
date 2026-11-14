#!/usr/bin/env python3
"""Capture Phase 4 with original fixtures and isolated state; build the simulator first."""
import json
import os
import shutil
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / 'screenshots/highlights'


def main():
    books = OUT / 'books'
    books.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(ROOT / 'crates/verso/tests/fixtures/selection.epub', books / 'selection.epub')
    opening = ['--tap', '1000,230', '--tap', '200,550']
    passage = [*opening, '--hold-drag', '200,110,500,190']
    extend = [*passage, '--tap', '950,1535']
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
    listing = [*resume, '--swipe', '636,10,636,400', '--tap', '790,1600']
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
    importing = [*listing, '--tap', '600,230']
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
    print(OUT)


if __name__ == '__main__':
    main()
