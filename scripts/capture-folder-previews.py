#!/usr/bin/env python3
"""Folder preview captures (docs/folder-previews.md): every mode in
portrait and landscape, plus selection, a pressed tile, missing art and an
empty folder.

Builds original EPUBs with generated covers; no third-party content. Run
`cargo build` first. Output and disposable state stay under
screenshots/folder-previews.
"""
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess
import zlib
from zipfile import ZIP_DEFLATED, ZipFile, ZipInfo

REPO = Path(__file__).resolve().parents[1]
OUT = REPO / 'screenshots/folder-previews'
SIZE = (1272, 1696)
DPI = 300


def mm(v, dpi):
    return round(v * dpi / 25.4)


def library_card_center(size, dpi):
    w, h = size
    margin, target = mm(5.0, dpi), mm(9.0, dpi)
    card_h = max(target, min(mm(13.2, dpi), h // 8))
    return w // 2, h - margin - card_h // 2


def grid_cell_center(size, dpi, col, row):
    w, h = size
    margin, top_bar, target = mm(5.0, dpi), mm(13.0, dpi), mm(9.0, dpi)
    cols, rows = (6, 2) if w > h else ((3, 3) if w < mm(100.0, dpi) else (4, 3))
    gap_x = mm(2.6 if cols == 6 else (2.8 if cols == 3 else 3.4), dpi)
    gap_y = mm(2.0, dpi)
    grid_top = top_bar + mm(2.2, dpi) + mm(11.0, dpi) + mm(2.9, dpi)
    grid_w = w - 2 * margin
    grid_h = max(target, h - margin - grid_top)
    cell_w = (grid_w - gap_x * (cols - 1)) // cols
    cell_h = (grid_h - gap_y * (rows - 1)) // rows
    return margin + col * (cell_w + gap_x) + cell_w // 2, grid_top + row * (cell_h + gap_y) + cell_h // 2


def settings_button_center(size, dpi):
    w, _ = size
    margin, top_bar, icon = mm(5.0, dpi), mm(13.0, dpi), mm(5.0, dpi)
    narrow = w < mm(100.0, dpi)
    exit_step = mm(10.0 if narrow else 10.5, dpi)
    exit_cx = w - margin - icon // 2
    return exit_cx - exit_step, top_bar // 2


def menu_point(size, dpi, anchor, count, index):
    w, h = size
    margin, top = mm(5.0, dpi), mm(13.0, dpi)
    available = h - top - 2 * margin - 2 * mm(1.0, dpi)
    columns = 2 if available // count < mm(9.0, dpi) else 1
    rows = (count + columns - 1) // columns
    row = min(mm(12.0, dpi), available // rows)
    height = row * rows + 2 * mm(1.0, dpi)
    width = min(mm(70.0 * columns, dpi), w - 2 * margin)
    x, y = anchor
    centered = max(margin, min(x - width // 2, w - margin - width))
    below, above = y + mm(2.0, dpi), y - mm(2.0, dpi) - height
    if below + height <= h - margin:
        mx, my = centered, below
    elif above >= top + margin:
        mx, my = centered, above
    else:
        right = x + mm(2.0, dpi)
        mx = max(margin, min(right if right + width <= w - margin else x - mm(2.0, dpi) - width, w - margin - width))
        my = max(top + margin, min(y - height // 2, max(top + margin, h - margin - height)))
    col_w = (width - 2 * mm(0.35, dpi)) // columns
    return mx + mm(0.35, dpi) + (index // rows) * col_w + col_w // 2, my + mm(1.0, dpi) + (index % rows) * row + row // 2


def chunk(kind, data):
    return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))


def cover_png(n, w=200, h=300):
    """A plain grayscale cover: a tone, a band and a block that vary with n."""
    background = 0xee - (n * 37) % 0x88
    band = (n * 53) % h
    block = (20 + (n * 29) % (w - 80), 40 + (n * 71) % (h - 120))
    rows = []
    for y in range(h):
        row = bytearray([background]) * w
        if band <= y < band + 24:
            row = bytearray([0x33]) * w
        if block[1] <= y < block[1] + 60:
            row[block[0]:block[0] + 60] = bytes([0x00 if n % 2 else 0xff]) * 60
        rows.append(b'\0' + bytes(row))
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 0, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress(b''.join(rows))) + chunk(b'IEND', b''))


def epub(path, title, n, art=True):
    path.parent.mkdir(parents=True, exist_ok=True)
    meta = f'<dc:title>{title}</dc:title><dc:creator>Verso</dc:creator><dc:language>en</dc:language>'
    manifest = '<item id="text" href="text.xhtml" media-type="application/xhtml+xml"/>'
    entries = {}
    if art:
        meta += '<meta name="cover" content="cover"/>'
        manifest += '<item id="cover" href="cover.png" media-type="image/png" properties="cover-image"/>'
        entries['OPS/cover.png'] = cover_png(n)
    opf = (f'<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id">'
           f'<metadata xmlns:dc="http://purl.org/dc/elements/1.1/">{meta}'
           f'<dc:identifier id="id">verso-preview-{n}</dc:identifier></metadata>'
           f'<manifest>{manifest}</manifest><spine><itemref idref="text"/></spine></package>')
    entries = {
        'mimetype': b'application/epub+zip',
        'META-INF/container.xml': b'<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles>'
                                  b'<rootfile full-path="OPS/book.opf" media-type="application/oebps-package+xml"/>'
                                  b'</rootfiles></container>',
        'OPS/book.opf': opf.encode(),
        'OPS/text.xhtml': f'<html xmlns="http://www.w3.org/1999/xhtml"><head><title>{title}</title></head>'
                          f'<body><h1>{title}</h1><p>An original little book for testing folder previews.</p>'
                          f'</body></html>'.encode(),
        **entries,
    }
    with ZipFile(path, 'w') as archive:
        for name, data in entries.items():
            info = ZipInfo(name, (2026, 1, 1, 0, 0, 0))
            info.compress_type = 0 if name == 'mimetype' else ZIP_DEFLATED
            archive.writestr(info, data)


def library(root):
    n = iter(range(100))
    for i, title in enumerate(['Amber', 'Birch', 'Cedar', 'Dune', 'Elm', 'Fern']):
        epub(root / 'Novels' / f'{title}.epub', title, next(n))
    for title in ['Gale', 'Heron']:
        epub(root / 'Poetry' / f'{title}.epub', title, next(n))
    epub(root / 'Essays/Iris.epub', 'Iris', next(n))
    epub(root / 'Mixed/Juniper.epub', 'Juniper', next(n))
    epub(root / 'Mixed/Kestrel.epub', 'Kestrel', next(n))
    epub(root / 'Mixed/Aardvark.epub', 'Aardvark', next(n), art=False)
    epub(root / 'Plain/Lark.epub', 'Lark', next(n), art=False)
    epub(root / 'Plain/Moss.epub', 'Moss', next(n), art=False)
    (root / 'Empty').mkdir(parents=True)
    for title in ['Nettle', 'Oak', 'Pine']:
        epub(root / 'Series/Part 1' / f'{title}.epub', title, next(n))
    for title in ['Quince', 'Rowan']:
        epub(root / 'Series/Part 2' / f'{title}.epub', title, next(n))
    epub(root / 'Loose.epub', 'Loose', next(n))


def capture(name, mode, orientation, steps=(), size=SIZE, dpi=DPI, open_library=True):
    case = OUT / 'state' / name
    if case.exists():
        shutil.rmtree(case)
    case.mkdir(parents=True)
    root = OUT / 'Library'
    (case / 'settings.json').write_text(json.dumps({
        'library_folders': [str(root)], 'orientation': orientation, 'folder_preview': mode,
    }))
    (case / 'library-navigation.json').write_text(json.dumps({'folder': str(root)}))
    env = dict(os.environ, VERSO_DATA=str(case), RUST_LOG='warn',
               VERSO_SIM_SIZE=f'{size[0]}x{size[1]}', VERSO_SIM_DPI=str(dpi))
    env.pop('VERSO_BOOKS', None)
    eff = (size[1], size[0]) if orientation == 'landscape' else size
    prefix = []
    if open_library:
        cx, cy = library_card_center(eff, dpi)
        prefix = ['--tap', f'{cx},{cy}']
    subprocess.run([str(REPO / 'target/debug/verso'), '--screenshot', str(OUT / f'{name}.png'),
                    *prefix, *steps],
                   env=env, cwd=REPO, check=True, timeout=120)


if __name__ == '__main__':
    shutil.rmtree(OUT, ignore_errors=True)
    OUT.mkdir(parents=True)
    library(OUT / 'Library')
    for orientation, eff in [('portrait', SIZE), ('landscape', (SIZE[1], SIZE[0]))]:
        # Novels is the fourth folder (col 3, row 0).
        tile = grid_cell_center(eff, DPI, 3, 0)
        sel = menu_point(eff, DPI, tile, 8, 3)
        for mode in ['stack', 'latest', 'icon']:
            capture(f'{orientation}-{mode}', mode, orientation)
        capture(f'{orientation}-stack-pressed', 'stack', orientation, ['--press', f'{tile[0]},{tile[1]}'])
        capture(f'{orientation}-stack-selected', 'stack', orientation,
                ['--long-press', f'{tile[0]},{tile[1]}', '--tap', f'{sel[0]},{sel[1]}'])
    # Settings → Display, where the choice lives.
    gear_p = settings_button_center(SIZE, DPI)
    gear_l = settings_button_center((SIZE[1], SIZE[0]), DPI)
    gear_s = settings_button_center((600, 800), 167)
    capture('portrait-settings', 'stack', 'portrait',
            ['--tap', f'{gear_p[0]},{gear_p[1]}', '--tap', '405,272'], open_library=False)
    capture('landscape-settings', 'stack', 'landscape',
            ['--tap', f'{gear_l[0]},{gear_l[1]}', '--tap', '532,272'], open_library=False)
    capture('small-settings', 'latest', 'portrait',
            ['--tap', f'{gear_s[0]},{gear_s[1]}', '--tap', '193,151'],
            size=(600, 800), dpi=167, open_library=False)
    capture('small-stack', 'stack', 'portrait', size=(600, 800), dpi=167)
    print('captured', sorted(p.name for p in OUT.glob('*.png')))
