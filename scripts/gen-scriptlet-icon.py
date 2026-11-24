#!/usr/bin/env python3
"""Generate the monochrome Kindle library cover for dist/kindle/documents/Verso.sh.

Uses the bundled Literata SemiBold font so the cover is reproducible without
checking in a binary image. Run with --embed to update Verso.sh in place, or
--png PATH to write a standalone PNG for inspection.
"""
import argparse
import base64
import io
from pathlib import Path
from PIL import Image, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parent.parent
FONT_PATH = ROOT / "assets" / "fonts" / "Literata-SemiBold.ttf"
SCRIPTLET = ROOT / "dist" / "kindle" / "documents" / "Verso.sh"
WIDTH, HEIGHT = 390, 600


def render_png() -> bytes:
    img = Image.new("L", (WIDTH, HEIGHT), 255)
    draw = ImageDraw.Draw(img)
    font = ImageFont.truetype(str(FONT_PATH), 88)
    text = "Verso"
    bbox = draw.textbbox((0, 0), text, font=font)
    tw, th = bbox[2] - bbox[0], bbox[3] - bbox[1]
    # Place the wordmark at optical center (42% down), clear of Amazon's corner badges.
    x = (WIDTH - tw) / 2 - bbox[0]
    y = HEIGHT * 0.42 - th / 2 - bbox[1]
    draw.text((x, y), text, fill=0, font=font)
    rule_w = 76
    rule_y = int(HEIGHT * 0.42 + th / 2 + 26)
    draw.rectangle([(WIDTH - rule_w) // 2, rule_y, (WIDTH + rule_w) // 2, rule_y + 2], fill=96)
    buf = io.BytesIO()
    img.save(buf, format="PNG", optimize=True)
    return buf.getvalue()


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--png", type=Path, help="Write the rendered PNG to this path.")
    ap.add_argument("--embed", type=Path, nargs="?", const=SCRIPTLET,
                    help=f"Update the '# Icon:' header in a scriptlet (default: {SCRIPTLET}).")
    args = ap.parse_args()
    png = render_png()
    if args.png:
        args.png.write_bytes(png)
    if args.embed:
        b64 = base64.b64encode(png).decode("ascii")
        icon_line = f"# Icon: data:image/png;base64,{b64}"
        lines = args.embed.read_text(encoding="utf-8").splitlines()
        for i, line in enumerate(lines[:6]):
            if line.startswith("# Icon: "):
                lines[i] = icon_line
                break
        else:
            raise SystemExit(f"No '# Icon: ' header in first 6 lines of {args.embed}")
        args.embed.write_text("\n".join(lines) + "\n", encoding="utf-8")
    if not args.png and not args.embed:
        print(f"data:image/png;base64,{base64.b64encode(png).decode('ascii')}")


if __name__ == "__main__":
    main()
