#!/usr/bin/env python3
"""Builds three small EPUBs from Project Gutenberg plain-text files, for the
Hunspell/language-detection checks (docs/universal-languages.md, Phase 4).

    gen-language-testbooks.py <de.txt> <fr.txt> <es.txt> <output dir>

The German and Spanish books are deliberately tagged `en` (the mistagged
case); the French one carries its right tag. Output goes to testbooks/
(git-ignored); the texts are public domain (Kafka, Verne, Cervantes).
"""
import re
import sys
import zipfile
from html import escape
from pathlib import Path

BOOKS = [
    # (file stem, text path index, title, author, language tag, encoding)
    ('kafka-verwandlung-de', 0, 'Die Verwandlung', 'Franz Kafka', 'en', 'latin-1'),
    ('verne-80-jours-fr', 1, 'Le Tour du Monde en 80 Jours', 'Jules Verne', 'fr', 'latin-1'),
    ('cervantes-quijote-es', 2, 'El ingenioso hidalgo don Quijote de la Mancha', 'Miguel de Cervantes', 'en', 'utf-8'),
]


def body_of(text):
    text = text.replace('\r\n', '\n')
    start = re.search(r'\*\*\* ?START OF (THE|THIS) PROJECT GUTENBERG[^\n]*\n', text)
    end = re.search(r'\*\*\* ?END OF (THE|THIS) PROJECT GUTENBERG', text)
    if start:
        text = text[start.end():]
    if end:
        text = text[:text.find(end.group(0))]
    return text.strip()


def chapters(text, size=40):
    paras = [re.sub(r'\s+', ' ', p).strip() for p in re.split(r'\n\s*\n', text)]
    paras = [p for p in paras if p]
    return [paras[i:i + size] for i in range(0, len(paras), size)]


def build(path, title, author, lang, parts):
    items = ''.join(f'<item id="c{i}" href="c{i}.xhtml" media-type="application/xhtml+xml"/>' for i in range(len(parts)))
    spine = ''.join(f'<itemref idref="c{i}"/>' for i in range(len(parts)))
    opf = (f'<?xml version="1.0" encoding="utf-8"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0" '
           f'unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/">'
           f'<dc:identifier id="id">verso-testbook-{path.stem}</dc:identifier><dc:title>{escape(title)}</dc:title>'
           f'<dc:creator>{escape(author)}</dc:creator><dc:language>{lang}</dc:language>'
           f'<meta property="dcterms:modified">2026-11-22T00:00:00Z</meta></metadata>'
           f'<manifest><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>{items}</manifest>'
           f'<spine>{spine}</spine></package>')
    nav = ('<?xml version="1.0" encoding="utf-8"?><html xmlns="http://www.w3.org/1999/xhtml" '
           'xmlns:epub="http://www.idpf.org/2007/ops"><head><title>Contents</title></head><body>'
           '<nav epub:type="toc"><ol><li><a href="c0.xhtml">Start</a></li></ol></nav></body></html>')
    with zipfile.ZipFile(path, 'w') as z:
        z.writestr('mimetype', 'application/epub+zip', zipfile.ZIP_STORED)
        z.writestr('META-INF/container.xml',
                   '<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">'
                   '<rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>')
        z.writestr('content.opf', opf)
        z.writestr('nav.xhtml', nav)
        for i, paras in enumerate(parts):
            heading = f'<h1>{escape(title)}</h1>' if i == 0 else ''
            html = ''.join(f'<p>{escape(p)}</p>' for p in paras)
            z.writestr(f'c{i}.xhtml', '<?xml version="1.0" encoding="utf-8"?><html xmlns="http://www.w3.org/1999/xhtml">'
                       f'<head><title>{escape(title)}</title></head><body>{heading}{html}</body></html>')


def main():
    if len(sys.argv) != 5:
        sys.exit(__doc__)
    texts, out = sys.argv[1:4], Path(sys.argv[4])
    out.mkdir(parents=True, exist_ok=True)
    for stem, idx, title, author, lang, enc in BOOKS:
        raw = Path(texts[idx]).read_bytes().decode(enc)
        parts = chapters(body_of(raw))
        build(out / f'{stem}.epub', title, author, lang, parts)
        print(stem, len(parts), 'sections')


main()
