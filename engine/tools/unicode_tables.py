# generates engine/src/unicode_tables.rs from a Unicode Character Database directory
#
#     python -I engine/tools/unicode_tables.py <ucd directory> <output .rs file>
#
# the UCD files are read as untrusted text: every field is parsed as hex or an integer, and
# anything malformed stops the generator

import sys
from pathlib import Path

VERSION = "17.0.0"
HANGUL_FIRST, HANGUL_LAST = 0xAC00, 0xD7A3


def hex_point(text):
    value = int(text, 16)
    if not 0 <= value <= 0x10FFFF:
        raise ValueError(f"code point out of range: {text}")
    return value


def read_unicode_data(path):
    classes, canonical, compatibility = {}, {}, {}
    for line in path.read_text(encoding="utf-8").splitlines():
        fields = line.split(";")
        if len(fields) != 15:
            raise ValueError(f"unexpected UnicodeData line: {line!r}")
        point = hex_point(fields[0])
        combining = int(fields[3])
        if not 0 <= combining <= 254:
            raise ValueError(f"combining class out of range: {line!r}")
        if combining:
            classes[point] = combining
        mapping = fields[5].split()
        if not mapping:
            continue
        if mapping[0].startswith("<"):
            compatibility[point] = [hex_point(part) for part in mapping[1:]]
        else:
            canonical[point] = [hex_point(part) for part in mapping]
    return classes, canonical, compatibility


def read_exclusions(path):
    excluded = set()
    for line in path.read_text(encoding="utf-8").splitlines():
        data = line.split("#", 1)[0].strip()
        if data:
            excluded.add(hex_point(data))
    return excluded


def read_identifiers(path):
    starts, continues = [], []
    for line in path.read_text(encoding="utf-8").splitlines():
        data = line.split("#", 1)[0].strip()
        if not data:
            continue
        fields = data.split(";")
        if len(fields) == 3 and fields[1].strip() == "InCB":
            # the one property with a value after its name
            continue
        if len(fields) != 2:
            raise ValueError(f"unexpected DerivedCoreProperties line: {line!r}")
        points = fields[0].split("..")
        if len(points) == 1:
            first = last = hex_point(points[0])
        elif len(points) == 2:
            first, last = hex_point(points[0]), hex_point(points[1])
        else:
            raise ValueError(f"unexpected code point range: {line!r}")
        if first > last:
            raise ValueError(f"reversed code point range: {line!r}")
        name = fields[1].strip()
        if name == "ID_Start":
            starts.append((first, last))
        elif name == "ID_Continue":
            continues.append((first, last))
    for ranges in (starts, continues):
        for (first, last), (next_first, _) in zip(ranges, ranges[1:]):
            if next_first <= last:
                raise ValueError(f"overlapping identifier ranges near {first:04X}")
    return starts, continues


def expand(point, canonical, compatibility, compat, seen=()):
    if point in seen:
        raise ValueError(f"decomposition cycle at {point:04X}")
    mapping = canonical.get(point)
    if mapping is None and compat:
        mapping = compatibility.get(point)
    if mapping is None:
        return [point]
    expanded = []
    for part in mapping:
        expanded.extend(expand(part, canonical, compatibility, compat, seen + (point,)))
    return expanded


def ranges(classes):
    result = []
    for point in sorted(classes):
        value = classes[point]
        if result and result[-1][1] == point - 1 and result[-1][2] == value:
            result[-1][1] = point
        else:
            result.append([point, point, value])
    return result


def main():
    if len(sys.argv) != 3:
        raise SystemExit("usage: unicode_tables.py <ucd directory> <output .rs file>")
    ucd, output = Path(sys.argv[1]), Path(sys.argv[2])
    classes, canonical, compatibility = read_unicode_data(ucd / "UnicodeData.txt")
    exclusions = read_exclusions(ucd / "CompositionExclusions.txt")
    identifier_starts, identifier_continues = read_identifiers(
        ucd / "DerivedCoreProperties.txt")

    decomposed, entries = [], []
    for point in sorted(set(canonical) | set(compatibility)):
        if HANGUL_FIRST <= point <= HANGUL_LAST:
            continue
        spans = []
        for compat in (False, True):
            expansion = expand(point, canonical, compatibility, compat)
            if expansion == [point]:
                spans.append((0, 0))
                continue
            spans.append((len(decomposed), len(expansion)))
            decomposed.extend(expansion)
        entries.append((point, spans[0], spans[1]))
    if len(decomposed) > 0xFFFF:
        raise ValueError("decomposition table outgrew u16 offsets")

    compositions = []
    for point, mapping in canonical.items():
        if point in exclusions or len(mapping) != 2:
            continue
        if classes.get(point, 0) or classes.get(mapping[0], 0):
            continue
        compositions.append((mapping[0], mapping[1], point))
    compositions.sort()

    lines = [
        f"// generated by engine/tools/unicode_tables.py from the Unicode {VERSION} character",
        "// database; regenerate instead of editing, see UNICODE-LICENSE.txt beside this file",
        "",
        "// (first, last, class) for code points whose canonical combining class is not zero",
        "pub(crate) static COMBINING_CLASSES: &[(u32, u32, u8)] = &[",
    ]
    lines += [f"    (0x{first:X}, 0x{last:X}, {value})," for first, last, value in ranges(classes)]
    lines += [
        "];",
        "",
        "// a start and length in DECOMPOSED",
        "pub(crate) type Span = (u16, u8);",
        "",
        "// (code point, canonical span, compatibility span) into DECOMPOSED, fully expanded;",
        "// an empty span means the form leaves the code point alone",
        "pub(crate) static DECOMPOSITIONS: &[(u32, Span, Span)] = &[",
    ]
    lines += [
        f"    (0x{point:X}, ({canon[0]}, {canon[1]}), ({compat[0]}, {compat[1]})),"
        for point, canon, compat in entries
    ]
    lines += ["];", "", "pub(crate) static DECOMPOSED: &[u32] = &["]
    for start in range(0, len(decomposed), 8):
        chunk = ", ".join(f"0x{point:X}" for point in decomposed[start:start + 8])
        lines.append(f"    {chunk},")
    lines += [
        "];",
        "",
        "// (first, second, composite) for canonical pairs that recompose, sorted by pair",
        "pub(crate) static COMPOSITIONS: &[(u32, u32, u32)] = &[",
    ]
    lines += [f"    (0x{a:X}, 0x{b:X}, 0x{c:X})," for a, b, c in compositions]
    lines += [
        "];",
        "",
        "// (first, last) for code points an identifier can start with; '$' and '_' are the",
        "// only additions the lexer makes",
        "pub(crate) static ID_START: &[(u32, u32)] = &[",
    ]
    lines += [f"    (0x{first:X}, 0x{last:X})," for first, last in identifier_starts]
    lines += [
        "];",
        "",
        "// (first, last) for code points an identifier can continue with; '$' and '_' are the",
        "// only additions the lexer makes",
        "pub(crate) static ID_CONTINUE: &[(u32, u32)] = &[",
    ]
    lines += [f"    (0x{first:X}, 0x{last:X})," for first, last in identifier_continues]
    lines += ["];", ""]
    output.write_text("\n".join(lines), encoding="utf-8", newline="\n")
    print(f"{len(entries)} decompositions, {len(compositions)} compositions, "
          f"{len(ranges(classes))} class ranges, {len(identifier_starts)} identifier starts, "
          f"{len(identifier_continues)} identifier continues")


if __name__ == "__main__":
    main()
