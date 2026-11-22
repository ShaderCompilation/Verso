#!/usr/bin/env bash
# Save a PNG of what the Kindle screen shows right now (read-only: reads /dev/fb0 over SSH).
# Usage: scripts/kindle-screenshot.sh [output.png]
# Default output: screenshots/kindle/<timestamp>.png (gitignored). Needs python3, no extra packages.
set -euo pipefail

cd "$(dirname "$0")/.."
out="${1:-screenshots/kindle/$(date +%Y%m%d-%H%M%S).png}"
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
