#!/usr/bin/env python3
"""Simulator screenshots for Phase 2 Markdown rendering; build the simulator first."""
import sim

OUT = sim.out_dir('markdown', clean=True)


def main():
    book = str(sim.FIXTURES / 'rich.md')
    sim.run(OUT / 'page1.png', ['--book', book], sim.state(OUT / 'state-p1'), quiet=False)
    sim.run(OUT / 'page2.png', ['--book', book, '--key', 'next'], sim.state(OUT / 'state-p2'), quiet=False)
    # Footnote marker [1] on page 1 of rich.md at pw6 portrait (1272x1696).
    sim.run(OUT / 'footnote.png', ['--book', book, '--tap', '435,320'], sim.state(OUT / 'state-fn'), quiet=False)
    print(OUT)


if __name__ == '__main__':
    main()
