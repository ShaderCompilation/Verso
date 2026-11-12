#!/usr/bin/env python3
"""Original, redistributable EPUB exercising selection across DOM/line boundaries."""
from pathlib import Path
from zipfile import ZipFile, ZIP_STORED

out = Path(__file__).resolve().parents[1] / "crates/verso/tests/fixtures/selection.epub"
with ZipFile(out, "w", ZIP_STORED) as epub:
    epub.writestr("mimetype", "application/epub+zip")
    epub.writestr("META-INF/container.xml", '''<?xml version="1.0"?>
<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0">
<rootfiles><rootfile full-path="book.opf" media-type="application/oebps-package+xml"/></rootfiles></container>''')
    epub.writestr("book.opf", '''<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="id">
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="id">verso-selection</dc:identifier>
<dc:title>Selection fixture</dc:title><dc:language>en</dc:language></metadata>
<manifest><item id="text" href="text.xhtml" media-type="application/xhtml+xml"/></manifest>
<spine><itemref idref="text"/></spine></package>''')
    epub.writestr("text.xhtml", '''<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Selection</title></head><body>
<p>“Hello,” can't reader’s punctuation.</p>
<p lang="ru">Привет, мир! Русский текст.</p>
<p>Format<strong>ting</strong> across inline markup.</p>
<p>A soft hyphen: extra&#173;ordinary.</p>
<p>''' + "selection foundation typography internationalization " * 150 + '''</p>
<p>Final boundary.</p></body></html>''')
