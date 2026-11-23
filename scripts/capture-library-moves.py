#!/usr/bin/env python3
"""Cut-and-paste move simulator captures with disposable fixture books.

Run cargo build first. Outputs go to gitignored screenshots/library-moves/.
"""
import shutil

import sim
from sim import grid_cell_center, hold, library_card_center, menu_point, mm, search_button_center, tap

OUT = sim.out_dir('library-moves')


def selection_buttons(size, dpi):
    w, _ = size
    margin, top_bar = mm(5.0, dpi), mm(13.0, dpi)
    narrow = w < mm(100.0, dpi)
    gap = mm(1.4 if narrow else 1.7, dpi)
    width = w - 2 * margin
    all_w, move_w, del_w, cancel_w = mm(18.0, dpi), mm(13.0, dpi), mm(17.5, dpi), mm(16.0, dpi)
    total = all_w + move_w + del_w + cancel_w + 3 * gap
    max_b = max(1, width - mm(10.5, dpi))
    scale = lambda x: x * max_b // total if total > max_b else x
    all_w, move_w, del_w, cancel_w = scale(all_w), scale(move_w), scale(del_w), scale(cancel_w)
    cy = top_bar // 2
    cancel_x = w - margin - cancel_w
    del_x = cancel_x - gap - del_w
    move_x = del_x - gap - move_w
    paste_x = cancel_x - gap - cancel_w
    all_x = move_x - gap - all_w
    return {
        'all': (all_x + all_w // 2, cy),
        'move': (move_x + move_w // 2, cy),
        'delete': (del_x + del_w // 2, cy),
        'paste': (paste_x + cancel_w // 2, cy),
        'cancel': (cancel_x + cancel_w // 2, cy),
    }


def header_buttons(size, dpi, multipage=False):
    w, _ = size
    margin, top_bar, icon = mm(5.0, dpi), mm(13.0, dpi), mm(5.0, dpi)
    header_y = top_bar + mm(2.2, dpi)
    back = (margin + icon // 2, header_y + mm(3.6, dpi))
    if multipage:
        pager_x = w - margin - mm(25.4, dpi)
        new_folder_cx = pager_x - mm(6.0, dpi) - icon // 2
    else:
        new_folder_cx = w - margin - icon // 2
    new_folder = (new_folder_cx, header_y + mm(5.7, dpi))
    return {'back': back, 'new_folder': new_folder}


def capture(name, steps=(), size=(1272, 1696), dpi=300, clashes=False, multi_root=False):
    case = OUT / name
    if case.exists():
        shutil.rmtree(case)
    library, state = case / 'Library', case / 'state'
    dest = library / 'Destination'
    dest.mkdir(parents=True)
    (library / 'Tree/Child').mkdir(parents=True)
    for title in ['A', 'B', 'C']:
        shutil.copyfile(sim.FIXTURES / 'library3.epub', library / f'{title}.epub')
        if clashes:
            shutil.copyfile(library / f'{title}.epub', dest / f'{title}.epub')
    (library / 'A.sdr').mkdir()
    (library / 'A.sdr/metadata.epub.lua').write_text('return { ["percent_finished"] = 0.37 }')
    roots = [str(library)]
    if multi_root:
        other = case / 'Other'
        other.mkdir()
        shutil.copyfile(sim.FIXTURES / 'library2.epub', other / 'Extra.epub')
        roots.append(str(other))
    sim.state(state, roots, files={'library-navigation.json': {'folder': str(library)}})
    sim.run(OUT / f'{case.name}.png', [*tap(library_card_center(size, dpi)), *steps], state,
            size=(size, dpi), quiet=False)
    return library


def main():
    size, dpi = (1272, 1696), 300
    btns = selection_buttons(size, dpi)
    hdr = header_buttons(size, dpi)
    # In Library: col 0 = Destination, col 1 = Tree, col 2 = A, col 3 = B, row 1 col 0 = C.
    dest_tile = grid_cell_center(size, dpi, 0, 0)
    tree_tile = grid_cell_center(size, dpi, 1, 0)
    book_a = grid_cell_center(size, dpi, 2, 0)
    book_b = grid_cell_center(size, dpi, 3, 0)
    book_c = grid_cell_center(size, dpi, 0, 1)
    menu = lambda anchor, n, i: tap(menu_point(size, dpi, anchor, n, i))

    book_move = [*hold(book_a), *menu(book_a, 6, 4)]
    selected = [*hold(tree_tile), *menu(tree_tile, 8, 3)]
    all_moving = [*hold(book_a), *menu(book_a, 6, 3), *tap(book_b), *tap(book_c), *tap(btns['move'])]

    capture('folder-menu', hold(tree_tile))
    capture('book-menu', hold(book_a))
    capture('selection-bar', selected)
    capture('carrying-source', book_move)
    capture('carrying-all', all_moving)
    capture('top-level-disabled', [*book_move, *tap(hdr['back'])], multi_root=True)
    capture('new-folder-while-carrying', [*book_move, *tap(hdr['new_folder'])])
    folder_move = [*hold(tree_tile), *menu(tree_tile, 8, 4), *tap(tree_tile)]
    capture('inside-carried-folder-disabled', folder_move)
    capture('inside-carried-subfolder-disabled', [*folder_move, *tap(dest_tile)])

    paste = [*all_moving, *tap(dest_tile), *tap(btns['paste'])]
    capture('clash', paste, clashes=True)
    capture('skip-all-result', [*paste, *tap((1160, 1360)), *tap((340, 1520))], clashes=True)
    capture('skip-then-keep-clash', [*paste, *tap((340, 1520)), *tap((950, 1520))], clashes=True)
    keep_all = [*paste, *tap((1160, 1360)), *tap((950, 1520))]
    root = capture('keep-both-all-result', keep_all, clashes=True)
    assert (root / 'Destination/A (2).epub').exists()
    assert (root / 'Destination/A (2).sdr/metadata.epub.lua').exists()
    root = capture('paste-result', [*book_move, *tap(dest_tile), *tap(btns['paste'])])
    assert (root / 'Destination/A.epub').exists() and not (root / 'A.epub').exists()

    search_tile = grid_cell_center(size, dpi, 0, 0, search=True)
    capture('move-from-search', [
        *tap(search_button_center(size, dpi)), '--edit', 'dismiss',
        *hold(search_tile), *menu(search_tile, 6, 4),
    ])

    for name, sz, d in [('small', (600, 800), 167),
                        ('small-landscape', (800, 600), 167),
                        ('landscape', (1696, 1272), 300)]:
        anchor = grid_cell_center(sz, d, 1, 0)
        capture(name + '-folder-menu', hold(anchor), sz, d)
        capture(name + '-selection', [*hold(anchor), *tap(menu_point(sz, d, anchor, 8, 3))], sz, d)
        capture(name + '-carrying', [*hold(anchor), *tap(menu_point(sz, d, anchor, 8, 4))], sz, d)

    print(OUT)


if __name__ == '__main__':
    main()
