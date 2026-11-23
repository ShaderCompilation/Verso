"""Shared helpers for the simulator capture scripts (`scripts/capture-*.py`).

Each scene runs the debug simulator headless (`--screenshot`) with its own
data folder, so captures never touch real state:

    import sim
    out = sim.out_dir('home')
    data = sim.state(out / 'state/home', books, {'orientation': 'landscape'})
    sim.run(out / 'home.png', sim.tap(sim.library_card_center((1696, 1272), 300)), data, books)

Taps are worked out from the theme's millimetre sizes (`ui/theme.rs`), so a
scene plays on every screen. When a layout changes, fix the position here
once. Run `cargo build` first.
"""
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess

ROOT = Path(__file__).resolve().parents[1]
VERSO = ROOT / 'target/debug/verso'
FIXTURES = ROOT / 'crates/verso/tests/fixtures'

# The simulator's `--device` presets: portrait width, height and dpi.
DEVICES = {
    'basic-167': (600, 800, 167),
    'basic-300': (1072, 1448, 300),
    'pw6': (1272, 1696, 300),
    'scribe': (1860, 2480, 300),
}


def mm(v, dpi):
    return round(v * dpi / 25.4)


class Screen:
    """Sizes for one device and orientation, in pixels, and its gestures."""

    def __init__(self, w, h, dpi):
        self.w, self.h, self.dpi = w, h, dpi
        self.size = (w, h)

    def mm(self, v):
        return mm(v, self.dpi)

    def tap(self, x, y):
        return ['--tap', f'{round(x)},{round(y)}']

    def long_press(self, x, y):
        return ['--long-press', f'{round(x)},{round(y)}']

    def swipe(self, x1, y1, x2, y2):
        return ['--swipe', f'{round(x1)},{round(y1)},{round(x2)},{round(y2)}']

    def open_book(self):
        """Home's Library card, then the first book in the grid."""
        return [*tap(library_card_center(self.size, self.dpi)), *self.tap(self.mm(5) + self.mm(9), self.mm(42))]


def device_screens(devices=('pw6', 'basic-167')):
    """(device, orientation, Screen) for each device in both orientations."""
    for device in devices:
        w, h, dpi = DEVICES[device]
        yield device, 'portrait', Screen(w, h, dpi)
        yield device, 'landscape', Screen(h, w, dpi)


def tap(point):
    return ['--tap', f'{round(point[0])},{round(point[1])}']


def hold(point):
    return ['--long-press', f'{round(point[0])},{round(point[1])}']


# Positions on Home and in Library, by screen size (w, h) and dpi.

def library_card_center(size, dpi):
    w, h = size
    margin, target = mm(5.0, dpi), mm(9.0, dpi)
    card_h = max(target, min(mm(13.2, dpi), h // 8))
    return w // 2, h - margin - card_h // 2


def grid_cell_center(size, dpi, col, row, search=False):
    """A Library grid cell; `search` for the grid under the search bar."""
    w, h = size
    margin, top_bar, target = mm(5.0, dpi), mm(13.0, dpi), mm(9.0, dpi)
    cols, rows = (6, 2) if w > h else ((3, 3) if w < mm(100.0, dpi) else (4, 3))
    gap_x = mm(2.6 if cols == 6 else (2.8 if cols == 3 else 3.4), dpi)
    gap_y = mm(2.0, dpi)
    if search:
        y, gap = top_bar + mm(2.0, dpi), mm(2.0, dpi)
        header_y = y + target + mm(1.8, dpi) if w > h else y + 2 * target + gap + mm(1.7, dpi)
        grid_top = header_y + mm(7.0, dpi) + mm(1.8, dpi)
    else:
        grid_top = top_bar + mm(2.2, dpi) + mm(11.0, dpi) + mm(2.9, dpi)
    cell_w = (w - 2 * margin - gap_x * (cols - 1)) // cols
    cell_h = (max(target, h - margin - grid_top) - gap_y * (rows - 1)) // rows
    return margin + col * (cell_w + gap_x) + cell_w // 2, grid_top + row * (cell_h + gap_y) + cell_h // 2


def _exit_cx(w, dpi):
    return w - mm(5.0, dpi) - mm(5.0, dpi) // 2, mm(10.0 if w < mm(100.0, dpi) else 10.5, dpi)


def settings_button_center(size, dpi):
    exit_cx, exit_step = _exit_cx(size[0], dpi)
    return exit_cx - exit_step, mm(13.0, dpi) // 2


def search_button_center(size, dpi):
    exit_cx, exit_step = _exit_cx(size[0], dpi)
    group_step = mm(10.0 if size[0] < mm(100.0, dpi) else 12.2, dpi)
    return exit_cx - exit_step - mm(9.0, dpi) - group_step - mm(9.0, dpi), mm(13.0, dpi) // 2


def menu_point(size, dpi, anchor, count, index):
    """Item `index` of a `count`-item PopupMenu opened at `anchor` (column-major)."""
    w, h = size
    margin, top = mm(5.0, dpi), mm(13.0, dpi)
    available = h - top - 2 * margin - 2 * mm(1.0, dpi)
    columns = 2 if available // count < mm(9.0, dpi) else 1
    rows = (count + columns - 1) // columns
    row = min(mm(12.0, dpi), available // rows)
    height = row * rows + 2 * mm(1.0, dpi)
    width = min(mm(70.0 * columns, dpi), w - 2 * margin)
    x, y = anchor
    centered = max(margin, min(x - width // 2, w - margin - width))
    below, above = y + mm(2.0, dpi), y - mm(2.0, dpi) - height
    if below + height <= h - margin:
        mx, my = centered, below
    elif above >= top + margin:
        mx, my = centered, above
    else:
        right = x + mm(2.0, dpi)
        mx = max(margin, min(right if right + width <= w - margin else x - mm(2.0, dpi) - width, w - margin - width))
        my = max(top + margin, min(y - height // 2, max(top + margin, h - margin - height)))
    col_w = (width - 2 * mm(0.35, dpi)) // columns
    return mx + mm(0.35, dpi) + (index // rows) * col_w + col_w // 2, my + mm(1.0, dpi) + (index % rows) * row + row // 2


# Folders, state and running.

def out_dir(name, clean=False):
    """screenshots/<name>/ (gitignored), emptied first if `clean`."""
    out = ROOT / 'screenshots' / name
    if clean:
        shutil.rmtree(out, ignore_errors=True)
    out.mkdir(parents=True, exist_ok=True)
    return out


def books(folder, *fixtures):
    """A folder holding copies of the named fixture EPUBs."""
    folder.mkdir(parents=True, exist_ok=True)
    for name in fixtures:
        shutil.copyfile(FIXTURES / name, folder / name)
    return folder


def state(path, library=None, settings=None, files=None):
    """A fresh data folder. `library` (a folder or a list) goes into
    settings.json's library_folders; `files` maps file names to JSON.
    First-time setup counts as done unless `settings` says otherwise."""
    shutil.rmtree(path, ignore_errors=True)
    path.mkdir(parents=True)
    folders = [str(f) for f in (library if isinstance(library, (list, tuple)) else [library]) if f]
    write_json(path / 'settings.json', {'setup_done': True,
                                        **({'library_folders': folders} if library is not None else {}),
                                        **(settings or {})})
    for name, value in (files or {}).items():
        write_json(path / name, value)
    return path


def run(png, steps=(), data=None, library=None, *, size=None, device=None, screen=None, env=None,
        timeout=120, quiet=True, codes=(0,)):
    """One headless simulator run: `steps` then a screenshot to `png`.
    `size` is ((w, h), dpi); `device` a DEVICES preset; `library` sets
    VERSO_BOOKS (a folder or a list). Returns `data`."""
    full = {k: v for k, v in os.environ.items() if not k.startswith('VERSO_')}
    full.update(VERSO_SIM_BATTERY='82', RUST_LOG='error')
    if data:
        full['VERSO_DATA'] = str(data)
    if library:
        full['VERSO_BOOKS'] = os.pathsep.join(map(str, library if isinstance(library, (list, tuple)) else [library]))
    if size:
        (w, h), dpi = size
        full.update(VERSO_SIM_SIZE=f'{w}x{h}', VERSO_SIM_DPI=str(dpi))
    full.update(env or {})
    args = [str(VERSO), *(['--device', device] if device else []), '--screenshot', str(png), *map(str, steps),
            *(['--screen', screen] if screen else [])]
    code = subprocess.run(args, env=full, cwd=ROOT, timeout=timeout,
                          stderr=subprocess.DEVNULL if quiet else None).returncode
    assert code in codes, f'{Path(png).name}: exit code {code}'
    return data


def read_json(path):
    return json.loads(Path(path).read_text())


def write_json(path, value):
    Path(path).write_text(json.dumps(value, ensure_ascii=False))


def dictionary(root, name, words):
    """An original StarDict (XDXF) dictionary in root/dictionaries/<name>;
    `words` is a list of (headword, definition markup)."""
    folder = root / 'dictionaries' / name
    folder.mkdir(parents=True, exist_ok=True)
    data, index = b'', b''
    for word, text in words:
        value = text.encode()
        index += word.encode() + b'\0' + struct.pack('>II', len(data), len(value))
        data += value
    (folder / 'test.ifo').write_text("StarDict's dict ifo file\nversion=3.0.0\n" +
                                     f'bookname={name}\nwordcount={len(words)}\nidxfilesize={len(index)}\n'
                                     'idxoffsetbits=32\nsametypesequence=x\n')
    (folder / 'test.idx').write_bytes(index)
    (folder / 'test.dict').write_bytes(data)
