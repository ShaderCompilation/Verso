#!/usr/bin/env python3
"""Original, redistributable EPUB with the footnote and link patterns the
footnote rules handle."""
from pathlib import Path
from zipfile import ZipFile, ZIP_STORED

out = Path(__file__).resolve().parents[1] / "crates/verso/tests/fixtures/footnotes.epub"
head = '<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><head><title>Notes</title></head><body>'
with ZipFile(out, "w", ZIP_STORED) as epub:
    epub.writestr("mimetype", "application/epub+zip")
    epub.writestr("META-INF/container.xml", '''<?xml version="1.0"?>
<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0">
<rootfiles><rootfile full-path="book.opf" media-type="application/oebps-package+xml"/></rootfiles></container>''')
    epub.writestr("book.opf", '''<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="id">
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="id">verso-footnotes</dc:identifier>
<dc:title>Footnotes fixture</dc:title><dc:language>en</dc:language></metadata>
<manifest><item id="text" href="text.xhtml" media-type="application/xhtml+xml"/>
<item id="notes" href="notes.xhtml" media-type="application/xhtml+xml"/>
<item id="star" href="star.xhtml" media-type="application/xhtml+xml"/></manifest>
<spine><itemref idref="text"/><itemref idref="notes"/><itemref idref="star"/></spine></package>''')
    epub.writestr("text.xhtml", head + '''
<p><a href="#two">Chapter Two</a></p>
<p>A bracketed marker [<a id="m1" href="notes.xhtml#n1">1</a>] points at a note section.</p>
<p>A raised marker<span id="m2"><a href="#e2"><sup>2</sup></a></span> points at a list item.</p>
<p>A marked <a epub:type="noteref" href="#fn3">reference</a> points at an aside.</p>
<p>A starred marker<a id="m4" href="star.xhtml">*</a> points at a whole file.</p>
<p>A <span id="m5"><a href="#e5">linked phrase</a></span> points at a note that links back.</p>
<p>A cross-reference to <a href="#two">the second chapter</a> and <a href="http://example.com/">a web page</a>.</p>
<aside epub:type="footnote" id="fn3"><p>Third note, marked as a footnote.</p></aside>
<h1 id="two">Chapter Two</h1>
<p>Notes for this chapter follow.</p>
<ul><li><span id="e2"><a href="#m2">2</a></span> Second note, with <i>italic</i> and <b>bold</b> words.</li>
<li><span id="e5"><a href="#m5">linked phrase</a></span> Fifth note, about the phrase.</li></ul>
</body></html>''')
    epub.writestr("notes.xhtml", head + '''
<div id="n1"><h2>1</h2><p>First note, in a section of its own.</p><a href="text.xhtml#m1">back</a></div>
</body></html>''')
    epub.writestr("star.xhtml", head + '''
<p><a href="text.xhtml#m4">*</a> Fourth note, a file of its own.</p>
</body></html>''')
