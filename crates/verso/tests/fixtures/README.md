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

dictionary-words.epub is an original test book of selectable single words and
short passages for manual dictionary and base-form checks: exact entries,
regular and irregular English inflections, possessives, function words,
exact-only inputs, and selection across markup, punctuation and special
characters. Each group says what it exercises. Regenerate with
`python3 scripts/gen-dictionary-fixture.py`. Licensed with Verso.
