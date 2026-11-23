#!/usr/bin/env python3
"""Captures first-time setup in portrait: Welcome, Library (folders found in
a fake Kindle storage), Turning pages (after trying the back zone on the
sketch) and Ready, on a 300 and a 167 dpi screen. Writes screenshots/setup/."""
import sim

PAGES = ('welcome', 'library', 'turning', 'ready')


def steps(size, page):
    """Taps from Welcome to `page` (an index into PAGES)."""
    (w, h), dpi = size
    mm = lambda v: sim.mm(v, dpi)
    margin, top_bar, button_h = mm(5.0), mm(13.0), mm(10.0)
    content_w = w - 2 * margin
    nav_w = min(mm(34.0), content_w // 3)
    start = (w // 2, h - margin - button_h - mm(3.0) - button_h // 2)
    nxt = (w - margin - nav_w // 2, h - margin - button_h // 2)
    out = [] if page == 0 else sim.tap(start)
    out += sim.tap(nxt) * (page - 1) if page > 1 else []
    if page == 2:
        # The sketch: below the zones, above the caption and the switch.
        body_top = top_bar + margin + mm(4.0)
        top = body_top + button_h + mm(5.0)
        bottom = h - margin - button_h - mm(6.0) - mm(14.0) - mm(12.0) - mm(2.0)
        sketch_w = (bottom - top) * w // h
        out += sim.tap(((w - sketch_w) // 2 + sketch_w // 8, (top + bottom) // 2))
    return out


def storage(root):
    """A Kindle storage with books in documents, My Library (KOReader's
    home folder) and a folder of the reader's own, plus an app folder."""
    sim.books(root / 'documents', 'reader.epub')
    sim.books(root / 'My Library', 'library2.epub', 'library3.epub')
    sim.books(root / 'Books/Novels', 'search.epub', 'footnotes.epub', 'selection.epub')
    sim.books(root / 'koreader', 'dictionary-words.epub')
    (root / 'koreader/settings.reader.lua').write_text(
        f'-- KOReader settings\nreturn {{\n    ["home_dir"] = "{root / "My Library"}",\n}}\n')
    return root


def main():
    out = sim.out_dir('setup', clean=True)
    root = storage(out / 'state/storage')
    for device in ('pw6', 'basic-167'):
        (w, h, dpi) = sim.DEVICES[device]
        for page, name in enumerate(PAGES):
            data = sim.state(out / f'state/{device}-{name}', settings={'setup_done': False})
            env = {'VERSO_SIM_UNTESTED': '1'} if page == 0 else None
            library = out / 'state/books'
            if name == 'library':
                env, library = {'VERSO_STORAGE': str(root), 'VERSO_KOREADER': str(root / 'koreader')}, None
            sim.run(out / f'{device}-{name}.png', steps(((w, h), dpi), page), data, library, device=device, env=env)
    print(out)


if __name__ == '__main__':
    main()
