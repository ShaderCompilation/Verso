#!/usr/bin/env python3
"""Simulator screenshots of search in book, using the original search fixture.

Run cargo build first. Outputs live in gitignored screenshots/book-search/.
Coordinates assume the 1272 x 1696 simulator screen.
"""
import sim

OUT = sim.out_dir('book-search')


def main():
    books = sim.books(OUT / 'books', 'search.epub')
    opening = ['--tap', '636,1560', '--tap', '165,500']
    menu = [*opening, '--tap', '636,848']
    search = [*menu, '--tap', '895,77']
    results = [*search, '--text', 'ada', '--edit', 'submit']
    opened = [*results, '--swipe', '900,1000,300,1000', '--tap', '600,1000']
    passage = [*opening, '--hold-drag', '190,297,500,297']
    scenes = {
        'reader': opening,
        'menu': menu,
        'keyboard': [*search, '--text', 'new york'],
        'results': results,
        'variants': [*search, '--text', 'привет', '--edit', 'submit'],
        'word-start': [*search, '--text', 'vin', '--edit', 'submit'],
        'no-result': [*search, '--text', 'absent phrase', '--edit', 'submit'],
        'opened': opened,
        'next': [*opened, '--tap', '500,1625'],
        'results-again': [*opened, '--tap', '697,1625'],
        'returned': [*opened, '--tap', '960,1625'],
        'passage': passage,
        'passage-search': [*passage, '--tap', '338,1253'],
        # Dictionary → Actions → Search in book.
        'dictionary-search': [*opening, '--long-press', '190,297', '--tap', '1036,685', '--tap', '338,1253'],
    }
    for name, steps in scenes.items():
        state = sim.state(OUT / (name + '-state'), books)
        sim.run(OUT / (name + '.png'), steps, state, books, quiet=False)
    print(OUT)


if __name__ == '__main__':
    main()
