#!/usr/bin/env python3
"""Folder preview captures: every mode in
portrait and landscape, plus selection, a pressed tile, missing art and an
empty folder.

Builds original EPUBs with generated covers; no third-party content. Run
`cargo build` first. Output and disposable state stay under
screenshots/folder-previews.
"""
import shutil
import struct
import zlib
from zipfile import ZIP_DEFLATED, ZipFile, ZipInfo

import sim
from sim import grid_cell_center, library_card_center, menu_point, settings_button_center

OUT = sim.out_dir('folder-previews')
SIZE = (1272, 1696)
DPI = 300


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
    root = OUT / 'Library'
    case = sim.state(OUT / 'state' / name, root, {'orientation': orientation, 'folder_preview': mode},
                     {'library-navigation.json': {'folder': str(root)}})
    eff = (size[1], size[0]) if orientation == 'landscape' else size
    prefix = sim.tap(library_card_center(eff, dpi)) if open_library else []
    sim.run(OUT / f'{name}.png', [*prefix, *steps], case, size=(size, dpi), quiet=False)


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
