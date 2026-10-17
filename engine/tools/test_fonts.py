# builds the small fonts the font tests read: one face drawn here from rectangles, written as
# TrueType, as OpenType CFF, and as WOFF and WOFF2 wrappings of the TrueType file, and a colour
# face with COLR layers and a CBDT bitmap
#
#     python engine/tools/test_fonts.py <output directory>
#
# needs fontTools with brotli support

import sys
from pathlib import Path

import struct
import zlib

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.t2CharStringPen import T2CharStringPen
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib import TTFont
from fontTools.ttLib.tables.DefaultTable import DefaultTable

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


def png(width, height, rgba):
    def chunk(tag, data):
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data))

    rows = b"".join(b"\x00" + bytes(rgba) * width for _ in range(height))
    header = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b"")


def build_color():
    # U+263A is two COLR layers, a red box under a box in the text colour; U+25CF is a green
    # 10 by 10 PNG in a 20 ppem CBDT strike; U+2605 a blue one in a 20 ppem sbix strike
    shapes = {".notdef": [], "smile": [], "dot": [], "under": [(0, 0, 1000, 1000)], "over": [(250, 250, 750, 750)], "star": []}
    builder = FontBuilder(1000, isTTF=True)
    builder.setupGlyphOrder(list(shapes))
    builder.setupCharacterMap({0x263A: "smile", 0x25CF: "dot", 0x2605: "star"})
    glyphs = {}
    for name, boxes in shapes.items():
        pen = TTGlyphPen(None)
        draw(pen, boxes)
        glyphs[name] = pen.glyph()
    builder.setupGlyf(glyphs)
    builder.setupHorizontalMetrics({name: (1000, 0) for name in shapes})
    builder.setupHorizontalHeader(ascent=800, descent=-200)
    builder.setupNameTable({"familyName": "Ceri Color", "styleName": "Regular"})
    builder.setupOS2(sTypoAscender=800, sTypoDescender=-200, usWinAscent=800, usWinDescent=200, usWeightClass=400)
    builder.setupPost()
    builder.setupCPAL([[(1.0, 0.0, 0.0, 1.0)]])
    builder.setupCOLR({"smile": [("under", 0), ("over", 0xFFFF)]}, version=0)
    dot = builder.font.getGlyphID("dot")
    image = png(10, 10, (0, 255, 0, 255))
    # format 17: small metrics (height, width, bearing x, bearing y, advance), length, png
    glyph_data = struct.pack(">BBbbBI", 10, 10, 0, 10, 10, len(image)) + image
    cbdt = DefaultTable("CBDT")
    cbdt.data = struct.pack(">HH", 3, 0) + glyph_data
    # index format 1, image format 17, offsets for one glyph
    subtable = struct.pack(">HHIII", 1, 17, 4, 0, len(glyph_data))
    array = struct.pack(">HHI", dot, dot, 8) + subtable
    line_metrics = struct.pack(">bbBbbbbbbbbb", 16, -4, 10, 0, 0, 0, 0, 0, 0, 0, 0, 0)
    size = struct.pack(">IIII", 8 + 48, len(array), 1, 0) + line_metrics * 2 + struct.pack(">HHBBBb", dot, dot, 20, 20, 32, 1)
    cblc = DefaultTable("CBLC")
    cblc.data = struct.pack(">HHI", 3, 0, 1) + size + array
    builder.font["CBDT"] = cbdt
    builder.font["CBLC"] = cblc
    star = builder.font.getGlyphID("star")
    record = struct.pack(">hh", 0, 0) + b"png " + png(10, 10, (0, 0, 255, 255))
    count = len(shapes)
    offsets = [4 + (count + 1) * 4 + (len(record) if index > star else 0) for index in range(count + 1)]
    sbix = DefaultTable("sbix")
    sbix.data = struct.pack(">HHII", 1, 1, 1, 12) + struct.pack(">HH", 20, 72) + struct.pack(f">{count + 1}I", *offsets) + record
    builder.font["sbix"] = sbix
    return builder.font


def main():
    output = Path(sys.argv[1])
    output.mkdir(parents=True, exist_ok=True)
    build(cff=False).save(output / "ceri-test.ttf")
    build(cff=True).save(output / "ceri-test.otf")
    build_color().save(output / "ceri-color.ttf")
    for flavor in ("woff", "woff2"):
        font = TTFont(output / "ceri-test.ttf")
        font.flavor = flavor
        font.save(output / f"ceri-test.{flavor}")


if __name__ == "__main__":
    main()
