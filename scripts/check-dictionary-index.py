#!/usr/bin/env python3
"""Scan StarDict .idx files the way Verso's index builder reads them.

Usage: scripts/check-dictionary-index.py <folder>
Copy the dictionaries' .ifo/.idx files first, e.g. from the Kindle:
  ssh kindle 'cd /mnt/us && find koreader/data/dict -name "*.ifo" -o -name "*.idx" | tar cf - -T -' | tar xf -
Reports per dictionary: words and size against the .ifo, the longest
headword, headwords Verso rejects (256 bytes or more with the NUL), and the
first unterminated or truncated record, with byte offsets.
"""
import gzip, os, sys

LIMIT = 256  # Verso's read_word: take(256) including the NUL

def ifo(path):
    info = {}
    with open(path, encoding="utf-8", errors="replace") as f:
        for line in f:
            if "=" in line:
                k, v = line.rstrip("\n").split("=", 1)
                info[k.strip()] = v.strip()
    return info

def scan(ifo_path):
    base = ifo_path[: -len(".ifo")]
    info = ifo(ifo_path)
    idx = base + ".idx"
    data = open(idx, "rb").read() if os.path.exists(idx) else gzip.open(idx + ".gz").read()
    wide = info.get("idxoffsetbits") == "64"
    tail = 12 if wide else 8
    pos = count = 0
    longest = (0, "", 0)
    over = []
    err = None
    while pos < len(data):
        end = data.find(b"\0", pos)
        if end < 0:
            err = f"unterminated word at byte {pos} (record {count})"; break
        word = data[pos:end]
        if len(word) + 1 > LIMIT:
            over.append((count, pos, len(word), word[:80].decode("utf-8", "replace")))
        if len(word) > longest[0]:
            longest = (len(word), word[:100].decode("utf-8", "replace"), pos)
        if end + 1 + tail > len(data):
            err = f"truncated offset/size at byte {end + 1} (record {count})"; break
        pos = end + 1 + tail
        count += 1
    print(f"== {os.path.relpath(ifo_path)}")
    print(f"   words {count} / ifo {info.get('wordcount')}, bytes {len(data)} / ifo {info.get('idxfilesize')}, 64-bit={wide}")
    print(f"   longest headword {longest[0]} bytes at offset {longest[2]}: {longest[1]!r}")
    if over:
        o = over[0]
        print(f"   {len(over)} headword(s) >= 256 bytes incl. NUL; first: record {o[0]}, byte {o[1]}, {o[2]} bytes: {o[3]!r}")
        print(f"   lengths: {sorted(x[2] for x in over)[-10:]}")
    if err:
        print(f"   ERROR: {err}")

for dirpath, _, files in sorted(os.walk(sys.argv[1])):
    for name in sorted(files):
        if name.endswith(".ifo"):
            scan(os.path.join(dirpath, name))
