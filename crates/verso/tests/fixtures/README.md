reader.epub is an original, generated two-chapter test book (100 numbered paragraphs per chapter), licensed with Verso. It contains no third-party book text or fonts. Used to check TOC, layout, positions and progress without private testbooks/.

selection.epub is original English/Russian text with punctuation, apostrophes,
inline formatting, a soft hyphen, and enough text to span pages. Regenerate with
`python3 scripts/gen-selection-fixture.py`. Licensed with Verso.

library2.epub and library3.epub are tiny original EPUB 2/3 package/cover fixtures,
licensed with Verso. Regenerate with `python3 scripts/gen-library-fixtures.py`.
They contain a nested OPF, multiple creators, escaped text, a percent-encoded PNG
path (EPUB 2) and a self-contained SVG (EPUB 3). Tests also generate malformed and
oversized inputs in temporary directories.

search.epub is original text with a name recurring in 200 paragraphs over two
chapters, plus inline markup, case, Unicode canonical/compatibility forms,
apostrophes, spaces, Cyrillic, a soft hyphen and regular-expression characters.
Regenerate with `python3 scripts/gen-search-fixture.py`. Licensed with Verso.

footnotes.epub is original text with one link of each kind the footnote rules
handle: a bracketed marker to a note section in another file, a raised marker
to an endnote list item, an `epub:type` noteref to an aside, a starred marker to
a whole file, a phrase to a note that links back, a contents entry, a
cross-reference to a chapter heading and a web link. Regenerate with
`python3 scripts/gen-footnotes-fixture.py`. Licensed with Verso.

dictionary-words.epub is an original test book of selectable single words and
short passages for manual dictionary and base-form checks: exact entries,
regular and irregular English inflections, possessives, function words,
Regular and irregular English inflections, possessives, function words,
exact-only inputs, and selection across markup, punctuation and special
characters. Each group says what it exercises. Regenerate with
`python3 scripts/gen-dictionary-fixture.py`. Licensed with Verso.

rich.md, prose.markdown, and markdown-cover.png are small original Markdown
fixtures licensed with Verso. rich.md exercises YAML frontmatter, GFM tables,
strikethrough, task lists, footnotes, code blocks, and a relative image;
prose.markdown exercises a minimal `.markdown` document without frontmatter.
