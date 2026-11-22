#!/usr/bin/env bash
# Save a PNG of what the Kindle screen shows right now (read-only: reads /dev/fb0 over SSH).
# Usage: scripts/kindle-screenshot.sh [output.png | output-dir]
# A path ending in "/" (or naming an existing directory) gets a <timestamp>.png inside it;
# other paths are used as the file name. Paths are relative to where you run the script.
# Default output: screenshots/user/<timestamp>.png in the repo (gitignored). Needs python3, no extra packages.
set -euo pipefail

from="$PWD"
cd "$(dirname "$0")/.."
arg="${1:-}"
if [[ "$arg" == /* ]]; then
  dir="$arg"
elif [[ -n "$arg" ]]; then
  dir="$from/$arg"
fi
if [[ -z "$arg" ]]; then
  out="screenshots/user/$(date +%Y%m%d-%H%M%S).png"
elif [[ "$arg" == */ || -d "$dir" ]]; then
  mkdir -p "${dir%/}"
  out="$(cd "${dir%/}" && pwd)/$(date +%Y%m%d-%H%M%S).png"
else
  out="$dir"
fi
mkdir -p "$(dirname "$out")"

ssh -o ConnectTimeout=5 kindle 'head -c 2157312 /dev/fb0' | python3 -c '
import struct, sys, zlib
W, H = 1272, 1696
raw = sys.stdin.buffer.read()
if len(raw) != W * H:
    sys.exit(f"expected {W * H} bytes from /dev/fb0, got {len(raw)}")
rows = b"".join(b"\0" + raw[y * W:(y + 1) * W] for y in range(H))
def chunk(tag, data):
    body = tag + data
    return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))
png = (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", W, H, 8, 0, 0, 0, 0))
       + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))
open(sys.argv[1], "wb").write(png)
' "$out"
echo "$out"
