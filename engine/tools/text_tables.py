# generates engine/src/text_tables.rs, the line breaking and bidi data, from a Unicode
# Character Database directory holding UnicodeData.txt, LineBreak.txt, BidiBrackets.txt,
# EastAsianWidth.txt and emoji/emoji-data.txt
#
#     python -I engine/tools/text_tables.py <ucd directory> <output .rs file>
#
# the UCD files are read as untrusted text: every field is parsed as hex or checked against a
# fixed set of names, and anything malformed stops the generator

import re
import sys
from pathlib import Path

VERSION = "17.0.0"

LINE_BREAK_CLASSES = [
    "AI", "AK", "AL", "AP", "AS", "B2", "BA", "BB", "BK", "CB", "CJ", "CL", "CM", "CP", "CR",
    "EB", "EM", "EX", "GL", "H2", "H3", "HH", "HL", "HY", "ID", "IN", "IS", "JL", "JT", "JV",
    "LF", "NL", "NS", "NU", "OP", "PO", "PR", "QU", "RI", "SA", "SG", "SP", "SY", "VF", "VI",
    "WJ", "XX", "ZW", "ZWJ",
]

BIDI_CLASSES = [
    "L", "R", "AL", "EN", "ES", "ET", "AN", "CS", "NSM", "BN", "B", "S", "WS", "ON", "LRE",
    "LRO", "RLE", "RLO", "PDF", "LRI", "RLI", "FSI", "PDI",
]

# defaults for unassigned code points, from the @missing lines of DerivedBidiClass.txt
BIDI_DEFAULTS = [
    (0x0590, 0x05FF, "R"), (0x0600, 0x07BF, "AL"), (0x07C0, 0x085F, "R"),
    (0x0860, 0x08FF, "AL"), (0x20A0, 0x20CF, "ET"), (0xFB1D, 0xFB4F, "R"),
    (0xFB50, 0xFDCF, "AL"), (0xFDF0, 0xFDFF, "AL"), (0xFE70, 0xFEFF, "AL"),
    (0x10800, 0x10CFF, "R"), (0x10D00, 0x10D3F, "AL"), (0x10D40, 0x10EBF, "R"),
    (0x10EC0, 0x10EFF, "AL"), (0x10F00, 0x10F2F, "R"), (0x10F30, 0x10F6F, "AL"),
    (0x10F70, 0x10FFF, "R"), (0x1E800, 0x1EC6F, "R"), (0x1EC70, 0x1ECBF, "AL"),
    (0x1ECC0, 0x1ECFF, "R"), (0x1ED00, 0x1ED4F, "AL"), (0x1ED50, 0x1EDFF, "R"),
    (0x1EE00, 0x1EEFF, "AL"), (0x1EF00, 0x1EFFF, "R"),
]

DATA_LINE = re.compile(r"^([0-9A-F]{4,6})(?:\.\.([0-9A-F]{4,6}))?\s*;\s*([A-Za-z_0-9]+)")
MISSING_LINE = re.compile(r"^#\s*@missing:\s*([0-9A-F]{4,6})\.\.([0-9A-F]{4,6})\s*;\s*([A-Za-z_0-9]+)")


def hex_point(text):
    value = int(text, 16)
    if not 0 <= value <= 0x10FFFF:
        raise ValueError(f"code point out of range: {text}")
    return value


def read_property(path, allowed=None, keep=None):
    # explicit lines win over @missing defaults, which are applied first
    defaults, values = [], {}
    for line in path.read_text(encoding="utf-8").splitlines():
        missing = MISSING_LINE.match(line)
        if missing:
            defaults.append((hex_point(missing[1]), hex_point(missing[2]), missing[3]))
            continue
        data = line.split("#", 1)[0].strip()
        if not data:
            continue
        match = DATA_LINE.match(data)
        if not match:
            raise ValueError(f"unexpected line in {path.name}: {line!r}")
        first = hex_point(match[1])
        last = hex_point(match[2]) if match[2] else first
        value = match[3]
        if allowed is not None and value not in allowed:
            raise ValueError(f"unknown value {value!r} in {path.name}")
        if keep is not None and value not in keep:
            continue
        for point in range(first, last + 1):
            values[point] = value
    return defaults, values


def variant(name):
    # three-letter class names are written as Rust variant names
    return name if len(name) < 3 else name[0] + name[1:].lower()


def ranges(values):
    result = []
    for point in sorted(values):
        value = values[point]
        if result and result[-1][1] == point - 1 and result[-1][2] == value:
            result[-1][1] = point
        else:
            result.append([point, point, value])
    return result


def main():
    if len(sys.argv) != 3:
        raise SystemExit("usage: text_tables.py <ucd directory> <output .rs file>")
    ucd, output = Path(sys.argv[1]), Path(sys.argv[2])

    # the header names the data's version; tables labelled with another would lie
    header = (ucd / "LineBreak.txt").read_text(encoding="utf-8").splitlines()[0]
    if header.strip() != f"# LineBreak-{VERSION}.txt":
        sys.exit(f"LineBreak.txt is {header.strip()!r}, expected version {VERSION}")
    defaults, breaks = read_property(ucd / "LineBreak.txt", set(LINE_BREAK_CLASSES))
    line_break = {}
    for first, last, value in defaults:
        if value not in LINE_BREAK_CLASSES:
            raise ValueError(f"unknown default line break class {value!r}")
        if value == "XX":
            continue
        for point in range(first, last + 1):
            line_break[point] = value
    line_break.update(breaks)
    line_break = {point: value for point, value in line_break.items() if value != "XX"}

    bidi = {}
    for first, last, value in BIDI_DEFAULTS:
        for point in range(first, last + 1):
            bidi[point] = value
    for line in (ucd / "UnicodeData.txt").read_text(encoding="utf-8").splitlines():
        fields = line.split(";")
        if len(fields) != 15:
            raise ValueError(f"unexpected UnicodeData line: {line!r}")
        if fields[4] not in BIDI_CLASSES:
            raise ValueError(f"unknown bidi class in {line!r}")
        point = hex_point(fields[0])
        bidi[point] = fields[4]
        # a range is listed as its first and last points
        if fields[1].endswith(", Last>"):
            start = max(candidate for candidate in bidi if candidate < point and bidi[candidate] == fields[4])
            for inner in range(start, point):
                bidi[inner] = fields[4]
    bidi = {point: value for point, value in bidi.items() if value != "L"}

    brackets = []
    for line in (ucd / "BidiBrackets.txt").read_text(encoding="utf-8").splitlines():
        data = line.split("#", 1)[0].strip()
        if not data:
            continue
        fields = [field.strip() for field in data.split(";")]
        if len(fields) != 3 or fields[2] not in ("o", "c"):
            raise ValueError(f"unexpected BidiBrackets line: {line!r}")
        brackets.append((hex_point(fields[0]), hex_point(fields[1]), fields[2] == "o"))

    _, widths = read_property(ucd / "EastAsianWidth.txt", {"A", "F", "H", "N", "Na", "W"}, {"F", "W", "H"})
    _, emoji = read_property(ucd / "emoji" / "emoji-data.txt", keep={"Extended_Pictographic"})

    joining = {}
    for line in (ucd / "ArabicShaping.txt").read_text(encoding="utf-8").splitlines():
        data = line.split("#", 1)[0].strip()
        if not data:
            continue
        fields = [field.strip() for field in data.split(";")]
        if len(fields) != 4 or fields[2] not in ("R", "L", "D", "C", "U", "T"):
            raise ValueError(f"unexpected ArabicShaping line: {line!r}")
        if fields[2] != "U":
            joining[hex_point(fields[0])] = fields[2]

    lines = [
        f"// generated by engine/tools/text_tables.py from the Unicode {VERSION} character",
        "// database; regenerate instead of editing, see UNICODE-LICENSE.txt beside this file",
        "",
        "use crate::bidi::BidiClass as B;",
        "use crate::linebreak::LineBreakClass as L;",
        "",
        "// (first, last, class), with every code point not listed in class XX",
        "pub(crate) static LINE_BREAK: &[(u32, u32, L)] = &[",
    ]
    lines += [f"    (0x{a:X}, 0x{b:X}, L::{variant(v)})," for a, b, v in ranges(line_break)]
    lines += [
        "];",
        "",
        "// (first, last, class), with every code point not listed in class L",
        "pub(crate) static BIDI_CLASS: &[(u32, u32, B)] = &[",
    ]
    lines += [f"    (0x{a:X}, 0x{b:X}, B::{variant(v)})," for a, b, v in ranges(bidi)]
    lines += [
        "];",
        "",
        "// (bracket, its pair, whether it opens), sorted by bracket",
        "pub(crate) static BIDI_BRACKETS: &[(u32, u32, bool)] = &[",
    ]
    lines += [f"    (0x{a:X}, 0x{b:X}, {'true' if o else 'false'})," for a, b, o in sorted(brackets)]
    lines += [
        "];",
        "",
        "// (first, last) of East Asian wide, fullwidth and halfwidth code points",
        "pub(crate) static EAST_ASIAN_WIDE: &[(u32, u32)] = &[",
    ]
    lines += [f"    (0x{a:X}, 0x{b:X})," for a, b, _ in ranges({p: 1 for p in widths})]
    lines += [
        "];",
        "",
        "// (first, last) of Extended_Pictographic code points",
        "pub(crate) static EXTENDED_PICTOGRAPHIC: &[(u32, u32)] = &[",
    ]
    lines += [f"    (0x{a:X}, 0x{b:X})," for a, b, _ in ranges({p: 1 for p in emoji})]
    lines += [
        "];",
        "",
        "// (first, last, joining type) for Arabic-style joining; unlisted code points do not join",
        "pub(crate) static JOINING_TYPE: &[(u32, u32, u8)] = &[",
    ]
    lines += [f"    (0x{a:X}, 0x{b:X}, b'{v}')," for a, b, v in ranges(joining)]
    lines += ["];", ""]
    output.write_text("\n".join(lines), encoding="utf-8", newline="\n")
    print(f"{len(ranges(line_break))} line break ranges, {len(ranges(bidi))} bidi ranges, "
          f"{len(brackets)} brackets")


if __name__ == "__main__":
    main()
