"""Distributed highlight records for our original reader.epub, never private books."""
import hashlib
import io
import json
import tarfile
from xml.etree import ElementTree as ET
from zipfile import ZipFile


LONG_PARAGRAPHS = 21


def fixture_tar(path, count, long=False, kind='highlight'):
    """`kind='bookmark'` writes page bookmarks: the same 12-character passages
    (a bookmark's range runs from its page start to the end of its opening
    words, and the check is only that this text reads as its quotation), with
    `"kind":"bookmark"` in each record.

    `long`: each record covers 21 whole paragraphs (several pages), starting
    at a different paragraph, so every page carries ~100 overlapping ranges."""
    raw = path.read_bytes()
    with ZipFile(path) as epub:
        assert b'verso-reader-test' in epub.read('book.opf'), 'Use the original reader fixture'
        passages = []
        long_passages = []
        for fragment, name in enumerate(['one.xhtml', 'two.xhtml'], 1):
            texts = [''.join(node.itertext()) for node in ET.fromstring(epub.read(name)).findall('.//{*}p')]
            base = lambda paragraph: f'/body[1]/DocFragment[{fragment}]/body[1]/p[{paragraph}]/text()[1].'
            for paragraph, text in enumerate(texts, 1):
                start = text.index('quiet reader')
                passages.append(({'start': base(paragraph) + str(start), 'end': base(paragraph) + str(start + 12)},
                                 'quiet reader'))
            for first in range(1, len(texts) - LONG_PARAGRAPHS + 2):
                last = first + LONG_PARAGRAPHS - 1
                long_passages.append(({'start': base(first) + '0', 'end': base(last) + str(len(texts[last - 1]))},
                                      '\n'.join(texts[first - 1:last])))
        if long:
            passages = long_passages
    identity = 0xcbf29ce484222325
    for byte in b'book.epub\0' + len(raw).to_bytes(8, 'little'):
        identity = ((identity ^ byte) * 0x100000001b3) & ((1 << 64) - 1)
    book = f'{identity:016x}'
    records = {}
    for index in range(count):
        # 100 highlights: every other paragraph. 1,000: five overlapping
        # copies in each paragraph, distributed over both chapters.
        position = index * len(passages) // count if count <= len(passages) else index % len(passages)
        key = f'fixture-{index:04}'
        records[key + '.json'] = dict(version=1, id=key, book=book,
                                     fingerprint=hashlib.sha256(raw).hexdigest(),
                                     range=passages[position][0], quotation=passages[position][1],
                                     created='2026-11-15 00:00:00 UTC', chapter='Fixture',
                                     note=None, provenance=None,
                                     **({'kind': 'bookmark'} if kind == 'bookmark' else {}))
    records['book-' + book + '.json'] = dict(version=1, records=[f'fixture-{i:04}' for i in range(count)], sources=[])
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode='w') as archive:
        for name, data in records.items():
            value = json.dumps(data, separators=(',', ':')).encode()
            entry = tarfile.TarInfo(name)
            entry.size = len(value)
            archive.addfile(entry, io.BytesIO(value))
    return output.getvalue()
