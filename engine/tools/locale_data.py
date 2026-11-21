# generates engine/src/script/locale_data.rs from the CLDR data in the ICU that Windows 10 and
# later ship as icu.dll
#
#     python -I engine/tools/locale_data.py <output .rs file>
#
# every table is asked of ICU itself: likely subtags through uloc_addLikelySubtags, week data
# through ucal, hour cycles through the best pattern for the skeleton j, time zones through
# ucal_openTimeZoneIDEnumeration and currencies through ucurr_openISOCurrencies

import ctypes
import re
import sys

icu = ctypes.CDLL("icu.dll")
icu.uloc_getAvailable.restype = ctypes.c_char_p
icu.uenum_next.restype = ctypes.c_void_p
icu.ucal_open.restype = ctypes.c_void_p
icu.udatpg_open.restype = ctypes.c_void_p
icu.ucal_openTimeZoneIDEnumeration.restype = ctypes.c_void_p
icu.ucurr_openISOCurrencies.restype = ctypes.c_void_p

# ECMA-402 names zones by their primary IANA identifier where CLDR still keeps an older one
PRIMARY_ZONES = {
    "Africa/Asmera": "Africa/Asmara",
    "America/Buenos_Aires": "America/Argentina/Buenos_Aires",
    "America/Catamarca": "America/Argentina/Catamarca",
    "America/Coral_Harbour": "America/Atikokan",
    "America/Cordoba": "America/Argentina/Cordoba",
    "America/Godthab": "America/Nuuk",
    "America/Indianapolis": "America/Indiana/Indianapolis",
    "America/Jujuy": "America/Argentina/Jujuy",
    "America/Louisville": "America/Kentucky/Louisville",
    "America/Mendoza": "America/Argentina/Mendoza",
    "Asia/Calcutta": "Asia/Kolkata",
    "Asia/Katmandu": "Asia/Kathmandu",
    "Asia/Rangoon": "Asia/Yangon",
    "Asia/Saigon": "Asia/Ho_Chi_Minh",
    "Atlantic/Faeroe": "Atlantic/Faroe",
    "Europe/Kiev": "Europe/Kyiv",
    "Pacific/Enderbury": "Pacific/Kanton",
    "Pacific/Ponape": "Pacific/Pohnpei",
    "Pacific/Truk": "Pacific/Chuuk",
}


def maximize(tag):
    out = ctypes.create_string_buffer(160)
    error = ctypes.c_int(0)
    icu.uloc_addLikelySubtags(tag.encode(), out, 160, ctypes.byref(error))
    return None if error.value > 0 else out.value.decode()


def split(tag):
    parts = tag.split("_")
    script = region = ""
    for part in parts[1:]:
        if len(part) == 4 and part.isalpha():
            script = part
        elif re.fullmatch(r"[A-Z]{2}|\d{3}", part):
            region = part
    return parts[0], script, region


def enumeration(handle):
    values = []
    while handle:
        length = ctypes.c_int(0)
        error = ctypes.c_int(0)
        pointer = icu.uenum_next(ctypes.c_void_p(handle), ctypes.byref(length), ctypes.byref(error))
        if not pointer or error.value > 0:
            break
        values.append(ctypes.string_at(pointer, length.value).decode())
    if handle:
        icu.uenum_close(ctypes.c_void_p(handle))
    return values


def week(locale):
    error = ctypes.c_int(0)
    calendar = icu.ucal_open("UTC", 3, locale.encode(), 1, ctypes.byref(error))
    if error.value > 0 or not calendar:
        return None
    calendar = ctypes.c_void_p(calendar)
    first = icu.ucal_getAttribute(calendar, 1)
    minimal = icu.ucal_getAttribute(calendar, 2)
    weekend = 0
    for day in range(1, 8):
        error = ctypes.c_int(0)
        if icu.ucal_getDayOfWeekType(calendar, day, ctypes.byref(error)) != 0:
            # ICU counts from Sunday 1, ECMA-402 from Monday 1
            weekend |= 1 << ((7 if day == 1 else day - 1) - 1)
    icu.ucal_close(calendar)
    return (7 if first == 1 else first - 1, minimal, weekend)


def hour_cycle(locale):
    error = ctypes.c_int(0)
    generator = icu.udatpg_open(locale.encode(), ctypes.byref(error))
    if error.value > 0 or not generator:
        return None
    pattern = ctypes.create_unicode_buffer(64)
    icu.udatpg_getBestPattern(ctypes.c_void_p(generator), "j", 1, pattern, 64, ctypes.byref(error))
    icu.udatpg_close(ctypes.c_void_p(generator))
    quoted = False
    for character in pattern.value:
        if character == "'":
            quoted = not quoted
        elif not quoted and character in "hHKk":
            return {"h": "h12", "H": "h23", "K": "h11", "k": "h24"}[character]
    return None


def zones(region):
    error = ctypes.c_int(0)
    handle = icu.ucal_openTimeZoneIDEnumeration(1, region.encode(), None, ctypes.byref(error))
    if error.value > 0:
        return []
    return sorted(PRIMARY_ZONES.get(zone, zone) for zone in enumeration(handle))


def main(output):
    available = [icu.uloc_getAvailable(index).decode() for index in range(icu.uloc_countAvailable())]
    languages, regions, scripts = set(), set(), set()
    for tag in available:
        language, script, region = split(tag)
        languages.add(language)
        if region:
            regions.add(region)
        if script:
            scripts.add(script)
    likely = {}
    for language in sorted(languages):
        maximal = maximize(language)
        if maximal:
            likely[language] = split(maximal)
            if likely[language][1]:
                scripts.add(likely[language][1])
    lines = [
        "// likely subtags, week data, hour cycles and time zones from the CLDR that Windows ICU",
        "// carries; generated by engine/tools/locale_data.py, regenerate instead of editing",
        "",
        "// language: its likely script and region",
        "pub(super) const LIKELY_LANGUAGES: &[(&str, &str, &str)] = &[",
    ]
    lines += [f'    ("{language}", "{script}", "{region}"),' for language, (_, script, region) in sorted(likely.items())]
    lines += ["];", "", "// language and region whose likely script differs from the language's own",
              "pub(super) const LIKELY_LANGUAGE_REGIONS: &[(&str, &str, &str)] = &["]
    for language in sorted(likely):
        for region in sorted(regions):
            maximal = maximize(f"{language}_{region}")
            if maximal and split(maximal)[1] != likely[language][1]:
                lines.append(f'    ("{language}", "{region}", "{split(maximal)[1]}"),')
    lines += ["];", "", "// language and script whose likely region differs from the language's own",
              "pub(super) const LIKELY_LANGUAGE_SCRIPTS: &[(&str, &str, &str)] = &["]
    for language in sorted(likely):
        for script in sorted(scripts):
            maximal = maximize(f"{language}_{script}")
            if maximal and split(maximal)[2] != likely[language][2]:
                lines.append(f'    ("{language}", "{script}", "{split(maximal)[2]}"),')
    lines += ["];", "", "// region alone: its likely language and script",
              "pub(super) const LIKELY_REGIONS: &[(&str, &str, &str)] = &["]
    for region in sorted(regions):
        maximal = maximize(f"und_{region}")
        if maximal:
            language, script, _ = split(maximal)
            lines.append(f'    ("{region}", "{language}", "{script}"),')
    lines += ["];", "", "// script alone: its likely language and region",
              "pub(super) const LIKELY_SCRIPTS: &[(&str, &str, &str)] = &["]
    for script in sorted(scripts):
        maximal = maximize(f"und_{script}")
        if maximal:
            language, _, region = split(maximal)
            lines.append(f'    ("{script}", "{language}", "{region}"),')
    world = week("und_001")
    lines += ["];", "",
              "// region: first day of the week (1 Monday to 7 Sunday), minimal days in the first week, and",
              "// the weekend as a bit per day from Monday; regions left out follow the world default",
              "pub(super) const WEEK_DATA: &[(&str, u8, u8, u8)] = &[",
              f'    ("001", {world[0]}, {world[1]}, {world[2]}),']
    for region in sorted(regions):
        data = week(f"und_{region}")
        if data and data != world:
            lines.append(f'    ("{region}", {data[0]}, {data[1]}, {data[2]}),')
    lines += ["];", "", "// region: the hour cycle its times use, where it is not h23",
              "pub(super) const HOUR_CYCLES: &[(&str, &str)] = &["]
    for region in sorted(regions):
        maximal = maximize(f"und_{region}")
        cycle = maximal and hour_cycle(maximal)
        if cycle and cycle != "h23":
            lines.append(f'    ("{region}", "{cycle}"),')
    lines += ["];", "", "// region: its canonical time zones, space separated",
              "pub(super) const REGION_ZONES: &[(&str, &str)] = &["]
    for region in sorted(regions):
        listed = zones(region)
        if listed:
            lines.append(f'    ("{region}", "{" ".join(listed)}"),')
    error = ctypes.c_int(0)
    # common and not deprecated
    currencies = sorted(enumeration(icu.ucurr_openISOCurrencies(1 | 8, ctypes.byref(error))))
    lines += ["];", "", "// the ISO 4217 currencies in common use",
              f'pub(super) const CURRENCIES: &str = "{" ".join(currencies)}";', ""]
    with open(output, "w", encoding="utf-8", newline="\n") as file:
        file.write("\n".join(lines))


if __name__ == "__main__":
    main(sys.argv[1])
