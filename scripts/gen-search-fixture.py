#!/usr/bin/env python3
"""Original, redistributable EPUB exercising book search across pages and text variants."""
from pathlib import Path
from zipfile import ZipFile, ZIP_STORED

out = Path(__file__).resolve().parents[1] / "crates/verso/tests/fixtures/search.epub"


def chapter(title, paragraphs, extra=""):
    body = "".join(
        f"<p>Paragraph {i}. Ada watched the tide while the lighthouse keeper counted ships.</p>\n"
        for i in paragraphs
    )
    return f'''<html xmlns="http://www.w3.org/1999/xhtml"><head><title>{title}</title></head><body>
<h1>{title}</h1>
{extra}{body}</body></html>'''


# Each variant paragraph is matched by the query noted in the search test.
variants = (
    "<p>Love<em>lace</em> wrote notes.</p>\n"  # lovelace: across inline markup
    "<p>The CAFE\u0301 opened; the café closed.</p>\n"  # café: case and canonical forms
    "<p>She said don't and don\u2019t.</p>\n"  # don't: apostrophes
    "<p>New\u00a0York and New  York.</p>\n"  # new york: spaces
    '<p lang="ru">ПРИВЕТ, привет и Привет.</p>\n'  # привет: Cyrillic case
    "<p>A soft hyphen: extra&#173;ordinary.</p>\n"  # extraordinary: soft hyphen
    "<p>Literal a.b and axb.</p>\n"  # a.b: not a regular expression
    "<p>The o\ufb03ce and the office.</p>\n"  # office: compatibility ligature
    "<p>Moving and living near Vin\u2019s vine<em>yard</em>.</p>\n"  # vin: word starts only
)

with ZipFile(out, "w", ZIP_STORED) as epub:
    epub.writestr("mimetype", "application/epub+zip")
    epub.writestr("META-INF/container.xml", '''<?xml version="1.0"?>
<container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0">
<rootfiles><rootfile full-path="book.opf" media-type="application/oebps-package+xml"/></rootfiles></container>''')
    epub.writestr("book.opf", '''<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="id">
<metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="id">verso-search</dc:identifier>
<dc:title>Search fixture</dc:title><dc:language>en</dc:language></metadata>
<manifest><item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
<item id="one" href="one.xhtml" media-type="application/xhtml+xml"/>
<item id="two" href="two.xhtml" media-type="application/xhtml+xml"/></manifest>
<spine toc="ncx"><itemref idref="one"/><itemref idref="two"/></spine></package>''')
    epub.writestr("toc.ncx", '''<?xml version="1.0"?><ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1">
<head><meta name="dtb:uid" content="verso-search"/></head><docTitle><text>Search fixture</text></docTitle><navMap>
<navPoint id="one" playOrder="1"><navLabel><text>The harbour</text></navLabel><content src="one.xhtml"/></navPoint>
<navPoint id="two" playOrder="2"><navLabel><text>The return</text></navLabel><content src="two.xhtml"/></navPoint>
</navMap></ncx>''')
    epub.writestr("one.xhtml", chapter("The harbour", range(1, 121), variants))
    epub.writestr("two.xhtml", chapter("The return", range(121, 201)))
