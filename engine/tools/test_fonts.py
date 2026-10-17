# builds the small fonts the font tests read: one face drawn here from rectangles, written as
# TrueType, as OpenType CFF, and as WOFF and WOFF2 wrappings of the TrueType file
#
#     python engine/tools/test_fonts.py <output directory>
#
# needs fontTools with brotli support

import sys
from pathlib import Path

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.t2CharStringPen import T2CharStringPen
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib import TTFont

# each glyph is a list of (x0, y0, x1, y1) boxes on a 1000 unit em
SHAPES = {
    ".notdef": [(50, 0, 450, 700)],
    "space": [],
    "A": [(50, 0, 150, 700), (450, 0, 550, 700), (50, 600, 550, 700), (50, 300, 550, 400)],
    "B": [(50, 0, 150, 700), (50, 0, 500, 100), (50, 300, 450, 400), (50, 600, 500, 700), (450, 0, 550, 700)],
    "f": [(100, 0, 200, 700), (100, 600, 400, 700), (0, 350, 350, 450)],
    "i": [(100, 0, 200, 450), (100, 550, 200, 650)],
    "fi": [(100, 0, 200, 700), (100, 600, 600, 700), (500, 0, 600, 450)],
}
CODES = {"space": 0x20, "A": 0x41, "B": 0x42, "f": 0x66, "i": 0x69}
ADVANCE = {"fi": 700, "space": 300}


def draw(pen, boxes):
    for x0, y0, x1, y1 in boxes:
        pen.moveTo((x0, y0))
        pen.lineTo((x1, y0))
        pen.lineTo((x1, y1))
        pen.lineTo((x0, y1))
        pen.closePath()


def build(cff):
    order = list(SHAPES)
    builder = FontBuilder(1000, isTTF=not cff)
    builder.setupGlyphOrder(order)
    builder.setupCharacterMap({code: name for name, code in CODES.items()})
    metrics = {name: (ADVANCE.get(name, 600), min((box[0] for box in boxes), default=0)) for name, boxes in SHAPES.items()}
    if cff:
        charstrings = {}
        for name, boxes in SHAPES.items():
            pen = T2CharStringPen(metrics[name][0], None)
            draw(pen, boxes)
            charstrings[name] = pen.getCharString()
        builder.setupCFF("CeriTest", {"FullName": "Ceri Test"}, charstrings, {})
    else:
        glyphs = {}
        for name, boxes in SHAPES.items():
            pen = TTGlyphPen(None)
            draw(pen, boxes)
            glyphs[name] = pen.glyph()
        builder.setupGlyf(glyphs)
    builder.setupHorizontalMetrics(metrics)
    builder.setupHorizontalHeader(ascent=800, descent=-200)
    builder.setupNameTable({"familyName": "Ceri Test", "styleName": "Regular"})
    builder.setupOS2(sTypoAscender=800, sTypoDescender=-200, usWinAscent=800, usWinDescent=200, usWeightClass=400)
    builder.setupPost()
    # a ligature and a kerning pair for the shaping tests
    builder.addOpenTypeFeatures("feature liga { sub f i by fi; } liga; feature kern { pos A B -100; } kern;")
    return builder.font


def main():
    output = Path(sys.argv[1])
    output.mkdir(parents=True, exist_ok=True)
    build(cff=False).save(output / "ceri-test.ttf")
    build(cff=True).save(output / "ceri-test.otf")
    for flavor in ("woff", "woff2"):
        font = TTFont(output / "ceri-test.ttf")
        font.flavor = flavor
        font.save(output / f"ceri-test.{flavor}")


if __name__ == "__main__":
    main()
