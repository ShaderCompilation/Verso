#!/usr/bin/env python3
"""Original, reproducible EPUB library fixtures; no third-party book content."""
from pathlib import Path
from zipfile import ZipFile, ZipInfo, ZIP_DEFLATED
import struct
import zlib

root = Path(__file__).resolve().parents[1] / 'crates/verso/tests/fixtures'
def chunk(kind, data):
    return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))
png = b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', 80, 120, 8, 0, 0, 0, 0))
png += chunk(b'IDAT', zlib.compress(b''.join(b'\0' + bytes([40 + y]) * 80 for y in range(120)))) + chunk(b'IEND', b'')
for version in (2, 3):
    metadata = '<dc:title>Quiet &amp; Calm</dc:title><dc:creator>Verso</dc:creator><dc:creator>Test Author</dc:creator><dc:language>en</dc:language>'
    if version == 2:
        metadata += '<meta name="cover" content="cover"/>'
    cover = ('image/png', '../art/quiet%20cover.png', png) if version == 2 else ('image/svg+xml', '../art/cover.svg', b'<svg xmlns="http://www.w3.org/2000/svg" width="80" height="120"><rect width="80" height="120" fill="#ddd"/><path d="M15 30h50v60H15z" fill="#555"/></svg>')
    opf = f'<package xmlns="http://www.idpf.org/2007/opf" version="{version}.0" unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/">{metadata}<dc:identifier id="id">verso-library-{version}</dc:identifier></metadata><manifest><item id="cover" href="{cover[1]}" media-type="{cover[0]}" properties="cover-image"/><item id="text" href="text.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="text"/></spine></package>'
    entries = {'mimetype': b'application/epub+zip', 'META-INF/container.xml': b'<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OPS/book.opf" media-type="application/oebps-package+xml"/></rootfiles></container>', 'OPS/book.opf': opf.encode(), 'OPS/text.xhtml': b'<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Quiet</title></head><body><h1>Quiet</h1><p>An original little book for testing the Verso library.</p></body></html>', cover[1].replace('../', '').replace('%20', ' '): cover[2]}
    with ZipFile(root / f'library{version}.epub', 'w') as archive:
        for name, data in entries.items():
            info = ZipInfo(name, (2026, 1, 1, 0, 0, 0))
            info.compress_type = 0 if name == 'mimetype' else ZIP_DEFLATED
            archive.writestr(info, data)
