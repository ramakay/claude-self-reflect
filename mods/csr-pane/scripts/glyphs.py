"""Writes hooks/glyphs.ts: the letters the pane's picture draws, as coverage bitmaps.

The mod paints its own picture and has no font engine, so the letters are drawn
here once, from two open fonts (SIL Open Font License 1.1):
  Inter       https://github.com/rsms/inter
  Newsreader  https://github.com/productiontype/Newsreader

Run:  python3 -I scripts/glyphs.py <Inter.ttf> <Newsreader.ttf>
"""
import base64
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

SANS_CHARS = "".join(chr(c) for c in range(32, 127)) + "·×…→’‘“”—–✓"
# The serif carries the card titles and the limits headline: the whole printable range.
SERIF_CHARS = "".join(chr(c) for c in range(32, 127)) + "·…’‘“”—–"


def face(path: str, weight: str, size: int, chars: str) -> dict:
    font = ImageFont.truetype(path, size)
    font.set_variation_by_name(weight)
    ascent, descent = font.getmetrics()
    height = ascent + descent
    advances = []
    data = bytearray()
    for ch in chars:
        advance = max(1, round(font.getlength(ch)))
        cell = Image.new("L", (advance, height), 0)
        ImageDraw.Draw(cell).text((0, 0), ch, font=font, fill=255)
        advances.append(advance)
        data += cell.tobytes()
    return {"em": size, "height": height, "ascent": ascent, "chars": chars, "advances": advances, "data": base64.b64encode(bytes(data)).decode()}


def emit(name: str, f: dict) -> str:
    chars = f["chars"].replace("\\", "\\\\").replace("'", "\\'")
    return (
        f"  {name}: {{\n"
        f"    em: {f['em']},\n"
        f"    height: {f['height']},\n"
        f"    ascent: {f['ascent']},\n"
        f"    chars: '{chars}',\n"
        f"    advances: [{', '.join(str(a) for a in f['advances'])}],\n"
        f"    data: '{f['data']}',\n"
        f"  }},\n"
    )


def main() -> None:
    inter, newsreader = sys.argv[1], sys.argv[2]
    out = Path(__file__).resolve().parent.parent / "hooks" / "glyphs.ts"
    body = (
        "// Made by scripts/glyphs.py. Do not edit by hand.\n"
        "// Coverage bitmaps of Inter (https://github.com/rsms/inter) and Newsreader (https://github.com/productiontype/Newsreader), both under the SIL Open Font License 1.1.\n"
        "// One block per letter, `advance` wide and `height` tall, one byte a pixel, row by row.\n"
        "export type FaceData = { em: number; height: number; ascent: number; chars: string; advances: number[]; data: string }\n\n"
        "export const FACES: { sans: FaceData; bold: FaceData; serif: FaceData } = {\n"
        + emit("sans", face(inter, "Medium", 26, SANS_CHARS))
        + emit("bold", face(inter, "Bold", 26, SANS_CHARS))
        + emit("serif", face(newsreader, "SemiBold", 48, SERIF_CHARS))
        + "}\n"
    )
    out.write_text(body, encoding="utf-8")
    print(out, len(body), "bytes")


main()
