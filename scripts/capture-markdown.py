#!/usr/bin/env python3
"""Simulator screenshots for Markdown rendering and Library UI; build the simulator first."""
import shutil

import sim

OUT = sim.out_dir('markdown', clean=True)


def main():
    book = str(sim.FIXTURES / 'rich.md')
    sim.run(OUT / 'page1.png', ['--book', book], sim.state(OUT / 'state-p1'), quiet=False)
    sim.run(OUT / 'page2.png', ['--book', book, '--key', 'next'], sim.state(OUT / 'state-p2'), quiet=False)
    # Footnote marker [1] on page 1 of rich.md at pw6 portrait (1272x1696).
    sim.run(OUT / 'footnote.png', ['--book', book, '--tap', '435,320'], sim.state(OUT / 'state-fn'), quiet=False)

    # Phase 3: Library grid with a cover: .md and title-card .md / .markdown, plus Details & Synopsis sheets.
    books = OUT / 'books'
    books.mkdir(parents=True, exist_ok=True)
    for name in ('rich.md', 'markdown-cover.png', 'prose.markdown'):
        shutil.copyfile(sim.FIXTURES / name, books / name)
    (books / 'notes.md').write_text(
        '# Quick Reading Notes\n\nA standalone `.md` file without frontmatter cover art.\n',
        encoding='utf-8',
    )

    s = sim.Screen(*sim.DEVICES['pw6'])
    open_library = sim.tap(sim.library_card_center(s.size, s.dpi))
    # Library defaults to Title sort, so cell (0, 0) is rich.md ("Field Notes on E-Ink Typography").
    cell0 = sim.grid_cell_center(s.size, s.dpi, 0, 0)
    details = sim.menu_point(s.size, s.dpi, cell0, 7, 1)
    synopsis = sim.menu_point(s.size, s.dpi, cell0, 7, 2)

    state = sim.state(OUT / 'state-library', books)
    sim.run(OUT / 'library.png', open_library, state, books, device='pw6', quiet=False)
    sim.run(OUT / 'details.png', [*open_library, *sim.hold(cell0), *sim.tap(details)], state, books, device='pw6', quiet=False)
    sim.run(OUT / 'synopsis.png', [*open_library, *sim.hold(cell0), *sim.tap(synopsis)], state, books, device='pw6', quiet=False)
    print(OUT)


if __name__ == '__main__':
    main()
