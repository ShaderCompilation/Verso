#!/usr/bin/env python3
"""Check every StarDict .idx/.idx.gz and .syn under a folder, read-only.

Copy installed .ifo[.gz], .idx[.gz] and .syn files from the Kindle first.
Reports counts, sizes, longest headwords and every invalid record with its
zero-based record number and decompressed byte offset. Exit 1 means errors.
"""
import argparse
import gzip
import unicodedata
from pathlib import Path

LIMIT = 4096  # Headword and folded key bytes, excluding the NUL.


def is_cyrillic(ch):
    code = ord(ch)
    return 0x0400 <= code <= 0x052F or 0x1C80 <= code <= 0x1C8F or 0xA640 <= code <= 0xA69F


def fold(text):
    """Matches verso_wordforms::fold: NFD, casefold, drop Cyrillic stress/ё marks, NFC."""
    base = '\0'
    out = []
    for ch in unicodedata.normalize('NFD', text).casefold():
        if ch in ('\u0301', '\u0300'):
            drop = is_cyrillic(base)
        elif ch == '\u0308':
            drop = base == 'е'
        else:
            drop = False
        if not unicodedata.category(ch).startswith('M'):
            base = ch
        if not drop:
            out.append(ch)
    return unicodedata.normalize('NFC', ''.join(out))


def read(path):
    return gzip.open(path, 'rb').read() if path.suffix == '.gz' else path.read_bytes()


def records(path, tail, expected_count, expected_size=None, targets=None):
    data = read(path)
    pos = count = errors = 0
    longest = (0, 0)

    def error(message):
        nonlocal errors
        errors += 1
        print(f'   ERROR: {path}: record {count} at byte offset {pos}: {message}')

    while pos < len(data):
        end = data.find(b'\0', pos)
        if end < 0:
            error('Unterminated headword' if len(data) - pos <= LIMIT else 'Headword too long (limit 4096 bytes)')
            break
        word = data[pos:end]
        if len(word) > longest[0]:
            longest = (len(word), pos)
        if len(word) > LIMIT:
            error(f'Headword too long: {len(word)} bytes (limit 4096 bytes)')
        elif not word:
            error('Empty headword')
        else:
            try:
                text = word.decode('utf-8')
                key = fold(text)
                if len(key.encode('utf-8')) > LIMIT:
                    error('Folded headword too long (limit 4096 bytes)')
            except UnicodeDecodeError:
                error('Invalid UTF-8 headword')
        if end + 1 + tail > len(data):
            error('Truncated offset/size record' if targets is None else 'Truncated synonym target record')
            break
        if targets is not None and int.from_bytes(data[end + 1:end + 5], 'big') >= targets:
            error('Invalid synonym target')
        pos = end + 1 + tail
        count += 1
    if count != expected_count or expected_size is not None and len(data) != expected_size:
        error(f'Count/size differs from .ifo: {count}/{expected_count} records, {len(data)}/{expected_size} bytes')
    print(f'   {path.name}: {count} records, {len(data)} bytes; longest headword {longest[0]} bytes at offset {longest[1]}')
    return errors


def scan(path):
    base = Path(str(path)[:-7] if path.name.endswith('.ifo.gz') else str(path)[:-4])
    info = dict(line.split('=', 1) for line in read(path).decode('utf-8').splitlines() if '=' in line)
    idx = Path(str(base) + '.idx')
    if not idx.exists():
        idx = Path(str(idx) + '.gz')
    print(f'== {path}')
    count = int(info['wordcount'])
    errors = records(idx, 12 if info.get('idxoffsetbits') == '64' else 8, count, int(info['idxfilesize']))
    syn = Path(str(base) + '.syn')
    if syn.exists():
        errors += records(syn, 4, int(info['synwordcount']), targets=count)
    elif int(info.get('synwordcount', 0)):
        print(f'   ERROR: {syn}: Missing synonym file')
        errors += 1
    print('   CLEAN' if not errors else f'   {errors} error(s)')
    return errors


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('folder', type=Path)
    args = parser.parse_args()
    errors = count = 0
    for path in sorted(args.folder.rglob('*')):
        if path.name.endswith('.ifo.gz') and path.with_suffix('').is_file():
            continue  # Same precedence as Verso: plain metadata wins.
        if not path.name.endswith(('.ifo', '.ifo.gz')):
            continue
        count += 1
        try:
            errors += scan(path)
        except (OSError, EOFError, ValueError, KeyError) as e:
            print(f'   ERROR: {path}: {e}')
            errors += 1
    if not count:
        parser.error('No dictionary metadata found')
    print(f'{count} dictionaries checked, {errors} error(s)')
    return bool(errors)


if __name__ == '__main__':
    raise SystemExit(main())
