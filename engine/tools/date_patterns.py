# generates engine/src/script/intl/date_patterns_data.rs, the date and time patterns and names
# Intl.DateTimeFormat formats with, from the CLDR in the ICU that Windows 10 and later ship
#
#     python -I engine/tools/date_patterns.py <output .rs file>
#
# patterns come from udatpg_getBestPatternWithOptions for every date skeleton and time
# skeleton V8 can build, as V8 asks for them; the glue between a date and a time is read off
# a combined skeleton, and names come from udat_getSymbols; strings shared between locales
# are stored once

import ctypes
import sys

icu = ctypes.CDLL("icu.dll")
icu.udatpg_open.restype = ctypes.c_void_p
icu.udat_open.restype = ctypes.c_void_p
icu.unumsys_open.restype = ctypes.c_void_p

LOCALES = [
    "en-US", "en-GB", "en-AU", "en-CA", "en-IN", "en-IE", "en-NZ", "en-ZA", "en-SG",
    "de-DE", "de-AT", "de-CH", "fr-FR", "fr-CA", "fr-CH", "fr-BE", "es-ES", "es-MX", "es-419",
    "es-AR", "es-CO", "es-US", "it-IT", "it-CH", "pt-BR", "pt-PT", "nl-NL", "nl-BE", "ru-RU",
    "uk-UA", "pl-PL", "cs-CZ", "sk-SK", "sv-SE", "da-DK", "nb-NO", "fi-FI", "hu-HU", "ro-RO",
    "bg-BG", "hr-HR", "sr-RS", "sl-SI", "el-GR", "tr-TR", "he-IL", "ar-EG", "ar-SA", "fa-IR",
    "hi-IN", "bn-BD", "th-TH", "vi-VN", "id-ID", "ms-MY", "ja-JP", "ko-KR", "zh-CN", "zh-TW",
    "zh-HK", "lt-LT", "lv-LV", "et-EE", "ca-ES", "fil-PH", "ur-PK", "ta-IN", "kk-KZ",
]

# zones whose names are kept per locale; others read as the localized GMT offset
ZONES = [
    "America/New_York", "America/Chicago", "America/Denver", "America/Phoenix",
    "America/Los_Angeles", "America/Anchorage", "Pacific/Honolulu", "America/Toronto",
    "America/Vancouver", "America/Mexico_City", "America/Sao_Paulo", "America/Halifax",
    "America/St_Johns", "Europe/London", "Europe/Dublin", "Europe/Lisbon", "Europe/Paris",
    "Europe/Berlin", "Europe/Madrid", "Europe/Rome", "Europe/Amsterdam", "Europe/Warsaw",
    "Europe/Kyiv", "Europe/Athens", "Europe/Istanbul", "Europe/Moscow", "Asia/Dubai",
    "Asia/Kolkata", "Asia/Shanghai", "Asia/Hong_Kong", "Asia/Tokyo", "Asia/Seoul",
    "Asia/Singapore", "Asia/Jakarta", "Australia/Sydney", "Australia/Perth",
    "Australia/Adelaide", "Pacific/Auckland", "Africa/Johannesburg", "Africa/Cairo", "UTC",
]

WEEKDAY_STYLES = ["", "EEEEE", "EEE", "EEEE"]
YEAR_STYLES = ["", "yy", "y"]
MONTH_STYLES = ["", "MM", "M", "MMMMM", "MMM", "MMMM"]
DAY_STYLES = ["", "dd", "d"]
HOUR_STYLES = ["", "h", "hh", "H", "HH"]
MINUTE_STYLES = ["", "m", "mm"]
SECOND_STYLES = ["", "s", "ss"]
ZONE_STYLES = ["", "z", "zzzz", "O", "OOOO", "v", "vvvv"]

MATCH_HOUR_FIELD_LENGTH = 1 << 11
JANUARY = 1704110400000.0  # 2024-01-01T12:00Z
JULY = 1720094400000.0  # 2024-07-04T12:00Z


def icu_locale(tag):
    return tag.replace("-", "_").encode()


def best(generator, skeleton, options=MATCH_HOUR_FIELD_LENGTH):
    if not skeleton:
        return ""
    buffer = ctypes.create_unicode_buffer(256)
    error = ctypes.c_int(0)
    length = icu.udatpg_getBestPatternWithOptions(
        ctypes.c_void_p(generator), skeleton, len(skeleton), options,
        buffer, 256, ctypes.byref(error))
    if error.value > 0:
        raise RuntimeError(f"no pattern for {skeleton}")
    # Chromium keeps an ordinary space where CLDR 42 put a narrow no-break space
    return buffer.value[:length].replace("\u202f", " ")


def formatter(tag, date_style, time_style, zone="UTC", pattern=None):
    error = ctypes.c_int(0)
    if pattern is None:
        handle = icu.udat_open(time_style, date_style, icu_locale(tag), zone, len(zone), None, 0, ctypes.byref(error))
    else:
        handle = icu.udat_open(-2, -2, icu_locale(tag), zone, len(zone), pattern, len(pattern), ctypes.byref(error))
    if error.value > 0 or not handle:
        raise RuntimeError(f"no formatter for {tag}")
    return ctypes.c_void_p(handle)


def to_pattern(handle):
    buffer = ctypes.create_unicode_buffer(256)
    error = ctypes.c_int(0)
    length = icu.udat_toPattern(handle, 0, buffer, 256, ctypes.byref(error))
    return buffer.value[:length].replace("\u202f", " ")


def format_at(handle, milliseconds):
    buffer = ctypes.create_unicode_buffer(256)
    error = ctypes.c_int(0)
    icu.udat_format.argtypes = [ctypes.c_void_p, ctypes.c_double, ctypes.c_wchar_p, ctypes.c_int, ctypes.c_void_p, ctypes.c_void_p]
    length = icu.udat_format(handle, milliseconds, buffer, 256, None, ctypes.byref(error))
    return buffer.value[:length]


def symbols(handle, kind, first, count):
    values = []
    for index in range(first, first + count):
        buffer = ctypes.create_unicode_buffer(128)
        error = ctypes.c_int(0)
        length = icu.udat_getSymbols(handle, kind, index, buffer, 128, ctypes.byref(error))
        values.append(buffer.value[:length])
    return values


def digits(tag):
    error = ctypes.c_int(0)
    system = icu.unumsys_open(icu_locale(tag), ctypes.byref(error))
    buffer = ctypes.create_unicode_buffer(64)
    length = icu.unumsys_getDescription(ctypes.c_void_p(system), buffer, 64, ctypes.byref(error))
    icu.unumsys_close(ctypes.c_void_p(system))
    text = buffer.value[:length]
    return text if len(text) == 10 else "0123456789"


def gmt_parts(tag):
    # the localized GMT format: what stands before and after the offset, the signs, and UTC
    # itself, read off formatted offsets
    parts = []
    for pattern in ["O", "OOOO"]:
        minus = format_at(formatter(tag, 0, 0, "Etc/GMT+4", pattern), JANUARY)
        plus = format_at(formatter(tag, 0, 0, "Asia/Kolkata", pattern), JANUARY)
        zero = format_at(formatter(tag, 0, 0, "UTC", pattern), JANUARY)
        digit = next(index for index, character in enumerate(minus) if character.isdigit() or character in "٠۰०০")
        sign_at = digit - 1
        number_end = len(minus)
        while number_end > digit and not (minus[number_end - 1].isdigit() or minus[number_end - 1] in "٠۰०০"):
            number_end -= 1
        prefix, minus_sign, suffix = minus[:sign_at], minus[sign_at], minus[number_end:]
        plus_sign = plus[len(prefix)]
        parts += [zero, prefix, suffix, minus_sign, plus_sign]
    return parts


def glue(generator, tag):
    # the date-time glue of each date style, read off a combined skeleton
    glues = []
    time = best(generator, "jmm")
    for date_skeleton in ["yMMMMEEEEd", "yMMMMd", "yMMMd", "yMd"]:
        date = best(generator, date_skeleton)
        combined = best(generator, date_skeleton + "jmm")
        if date in combined and time in combined:
            glues.append(combined.replace(date, "{1}", 1).replace(time, "{0}", 1))
        else:
            glues.append("{1}, {0}")
    return glues


def rust_string(text):
    escaped = []
    for character in text:
        if character in '\\"':
            escaped.append("\\" + character)
        elif ord(character) < 32:
            escaped.append(f"\\u{{{ord(character):x}}}")
        else:
            escaped.append(character)
    return '"' + "".join(escaped) + '"'


def main(output):
    table = []
    positions = {}

    def intern(text):
        if text not in positions:
            positions[text] = len(table)
            table.append(text)
        return positions[text]

    date_skeletons = [w + y + m + d for w in WEEKDAY_STYLES for y in YEAR_STYLES for m in MONTH_STYLES for d in DAY_STYLES]
    time_skeletons = [h + m + s + z for h in HOUR_STYLES for m in MINUTE_STYLES for s in SECOND_STYLES for z in ZONE_STYLES]
    rows = []
    for tag in LOCALES:
        error = ctypes.c_int(0)
        generator = icu.udatpg_open(icu_locale(tag), ctypes.byref(error))
        if error.value > 0 or not generator:
            raise RuntimeError(f"no pattern generator for {tag}")
        values = [best(generator, skeleton) for skeleton in date_skeletons]
        values += [best(generator, skeleton) for skeleton in time_skeletons]
        values += glue(generator, tag)
        values.append(best(generator, "j"))
        # the times toLocaleString shows when no time option is given keep the locale's own
        # hour width, so they are matched without holding the hour's length
        values += [best(generator, skeleton, 0) for skeleton in ["jms", "Hms", "hms"]]
        icu.udatpg_close(ctypes.c_void_p(generator))
        values += [to_pattern(formatter(tag, style, -1)) for style in range(4)]
        values += [to_pattern(formatter(tag, -1, style)) for style in range(4)]
        names = formatter(tag, 0, 0)
        # wide, abbreviated and narrow months, then the standalone forms
        for kind in [1, 2, 8, 10, 11, 12]:
            values += symbols(names, kind, 0, 12)
        # wide, abbreviated, short and narrow weekdays from Sunday, then standalone
        for kind in [3, 4, 20, 9, 13, 14, 21, 15]:
            values += symbols(names, kind, 1, 7)
        values += symbols(names, 5, 0, 2)
        values += symbols(names, 0, 0, 2) + symbols(names, 7, 0, 2)
        values.append(digits(tag))
        values += gmt_parts(tag)
        for zone in ZONES:
            for pattern in ["z", "zzzz", "v", "vvvv"]:
                handle = formatter(tag, 0, 0, zone, pattern)
                values += [format_at(handle, JANUARY), format_at(handle, JULY)]
        rows.append((tag, [intern(value) for value in values]))
    lines = [
        "// date and time patterns and names for Intl.DateTimeFormat from the CLDR that Windows ICU",
        "// carries; generated by engine/tools/date_patterns.py, regenerate instead of editing",
        "",
        f"pub(super) const DATE_SKELETONS: usize = {len(date_skeletons)};",
        f"pub(super) const TIME_SKELETONS: usize = {len(time_skeletons)};",
        f"pub(super) const PATTERN_ZONES: &[&str] = &[{', '.join(rust_string(zone) for zone in ZONES)}];",
        "",
        "// every string, each once, separated by U+001F",
        "pub(super) const STRINGS: &str = " + rust_string("\x1f".join(table)) + ";",
        "",
        "// per locale, the index of each of its strings in the order date_patterns.rs reads them",
        "pub(super) const DATE_LOCALES: &[(&str, &[u16])] = &[",
    ]
    for tag, indices in rows:
        lines.append(f'    ("{tag}", &[{", ".join(map(str, indices))}]),')
    lines += ["];", ""]
    with open(output, "w", encoding="utf-8", newline="\n") as file:
        file.write("\n".join(lines))
    print(f"{len(rows)} locales, {len(table)} strings")


if __name__ == "__main__":
    main(sys.argv[1])
