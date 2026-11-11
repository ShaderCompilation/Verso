reader.epub is an original, generated two-chapter test book (100 numbered paragraphs per chapter), licensed with Verso. It contains no third-party book text or fonts. Used to check TOC, layout, positions and progress without private testbooks/.

library2.epub and library3.epub are tiny original EPUB 2/3 package/cover fixtures,
licensed with Verso. Regenerate with `python3 scripts/gen-library-fixtures.py`.
They contain a nested OPF, multiple creators, escaped text, a percent-encoded PNG
path (EPUB 2) and a self-contained SVG (EPUB 3). Tests also generate malformed and
oversized inputs in temporary directories.
