#!/usr/bin/env python3
"""Phase 3 captures with disposable, original books. No user state is used."""
import json
import os
from pathlib import Path
import shutil
import subprocess

REPO = Path(__file__).resolve().parents[1]
OUT = REPO / 'screenshots/library-moves'


def menu_point(size, dpi, anchor, count, index):
    """PopupMenu's column-major layout, including its minimum touch target."""
    w, h = size
    mm = lambda n: round(n * dpi / 25.4)
    margin, top = mm(5), mm(13)
    available = h - top - 2 * margin - 2 * mm(1)
    columns = 2 if available // count < mm(9) else 1
    rows = (count + columns - 1) // columns
    row = min(mm(12), available // rows)
    height = row * rows + 2 * mm(1)
    width = min(mm(70 * columns), w - 2 * margin)
    x, y = anchor
    centered = max(margin, min(x - width // 2, w - margin - width))
    below, above = y + mm(2), y - mm(2) - height
    if below + height <= h - margin:
        mx, my = centered, below
    elif above >= top + margin:
        mx, my = centered, above
    else:
        right = x + mm(2)
        mx = max(margin, min(right if right + width <= w - margin else x - mm(2) - width, w - margin - width))
        my = max(top + margin, min(y - height // 2, h - margin - height))
    col_w = (width - 2 * mm(.35)) // columns
    return mx + mm(.35) + (index // rows) * col_w + col_w // 2, my + mm(1) + (index % rows) * row + row // 2


def tap(point):
    return ['--tap', f'{point[0]},{point[1]}']


def hold(point):
    return ['--long-press', f'{point[0]},{point[1]}']


def capture(name, steps=(), size=(1272, 1696), dpi=300, clashes=False, tab='collections'):
    case = OUT / name
    if case.exists():
        shutil.rmtree(case)
    library, state = case / 'Library', case / 'state'
    dest = library / 'Destination'
    dest.mkdir(parents=True)
    (library / 'Tree/Child').mkdir(parents=True)
    state.mkdir()
    for name in ['A', 'B', 'C']:
        shutil.copyfile(REPO / 'crates/verso/tests/fixtures/library3.epub', library / f'{name}.epub')
        if clashes:
            shutil.copyfile(library / f'{name}.epub', dest / f'{name}.epub')
    (library / 'A.sdr').mkdir()
    (library / 'A.sdr/metadata.epub.lua').write_text('return { ["percent_finished"] = 0.37 }')
    (state / 'library-navigation.json').write_text(json.dumps({'tab': tab, 'folder': str(library)}))
    (state / 'settings.json').write_text(json.dumps({'library_sort': 'title'}))
    env = dict(os.environ, VERSO_BOOKS=str(library), VERSO_DATA=str(state), RUST_LOG='warn',
               VERSO_SIM_SIZE=f'{size[0]}x{size[1]}', VERSO_SIM_DPI=str(dpi))
    mm = lambda n: round(n * dpi / 25.4)
    initial = (round(size[0] * (.5 if tab == 'collections' else .8)), mm(13) + mm(2) + mm(9) // 2)
    subprocess.run([str(REPO / 'target/debug/verso'), '--screenshot', str(OUT / f'{case.name}.png'),
                    *tap(initial), *steps], env=env, cwd=REPO, check=True, timeout=120)
    return library


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    subprocess.run(['cargo', 'build', '-p', 'verso'], cwd=REPO, check=True)
    size, dpi = (1272, 1696), 300
    folder, book = (480, 650), (770, 650)
    menu = lambda anchor, n, i: tap(menu_point(size, dpi, anchor, n, i))
    book_move = [*hold(book), *menu(book, 6, 4)]
    selected = [*hold(folder), *menu(folder, 8, 3)]
    all_moving = [*hold(book), *menu(book, 6, 3), *tap((1060, 650)), *tap((190, 1160)), *tap((635, 77))]
    capture('folder-menu', hold(folder))
    capture('book-menu', hold(book))
    capture('selection-bar', selected)
    capture('carrying-source', book_move)
    capture('carrying-all', all_moving)
    capture('top-level-disabled', [*book_move, *tap((110, 370))])
    capture('new-folder-while-carrying', [*book_move, *tap((1160, 370))])
    folder_move = [*hold(folder), *menu(folder, 8, 4), *tap(folder)]
    capture('inside-carried-folder-disabled', folder_move)
    capture('inside-carried-subfolder-disabled', [*folder_move, *tap((190, 650))])
    paste = [*all_moving, *tap((190, 650)), *tap((870, 77))]
    capture('clash', paste, clashes=True)
    capture('skip-all-result', [*paste, *tap((1160, 1360)), *tap((340, 1520))], clashes=True)
    capture('skip-then-keep-clash', [*paste, *tap((340, 1520)), *tap((950, 1520))], clashes=True)
    # Sheet: message, Apply to all pill, Keep both. Top of 65 mm sheet = 928.
    keep_all = [*paste, *tap((1160, 1360)), *tap((950, 1520))]
    root = capture('keep-both-all-result', keep_all, clashes=True)
    assert (root / 'Destination/A (2).epub').exists()
    assert (root / 'Destination/A (2).sdr/metadata.epub.lua').exists()
    root = capture('paste-result', [*book_move, *tap((190, 650)), *tap((870, 77))])
    assert (root / 'Destination/A.epub').exists() and not (root / 'A.epub').exists()
    anchor = (190, 550)
    capture('move-from-all-books', [*hold(anchor), *tap(menu_point(size, dpi, anchor, 6, 4))], tab='all_books')
    capture('move-from-search', [*tap((758, 77)), '--edit', 'dismiss', *hold((190, 750)),
                                 *tap(menu_point(size, dpi, (190, 750), 6, 4))])
    for name, size, dpi, anchor in [('small', (600, 800), 167, (270, 340)),
                                   ('small-landscape', (800, 600), 167, (250, 340)),
                                   ('landscape', (1696, 1272), 300, (480, 700))]:
        capture(name + '-folder-menu', hold(anchor), size, dpi)
        select = [*hold(anchor), *tap(menu_point(size, dpi, anchor, 8, 3))]
        capture(name + '-selection', select, size, dpi)
        move = [*hold(anchor), *tap(menu_point(size, dpi, anchor, 8, 4))]
        capture(name + '-carrying', move, size, dpi)
    print(OUT)


if __name__ == '__main__':
    main()
