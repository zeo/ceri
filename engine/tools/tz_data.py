# generates engine/src/script/tz_data.bin, the IANA time zone database the engine carries, from
# a compiled tzdata: the zone and link names of tzdata.zi and each zone's TZif file
#
#     python3 -I engine/tools/tz_data.py /usr/share/zoneinfo engine/src/script/tz_data.bin
#
# layout, little-endian: "CTZ1", the tzdata version, the zones by name with where their data
# starts, the links with the zone each names, then per zone its local time types (UTC offset in
# seconds and whether it is daylight time), its transitions (seconds since the epoch and the
# type each starts) and the POSIX TZ rule that holds after the last transition

import os
import struct
import sys


def tzif(path):
    with open(path, "rb") as file:
        data = file.read()
    if data[:4] != b"TZif" or data[4] < ord("2"):
        raise ValueError(f"{path} is not TZif version 2 or later")

    def header(at):
        return struct.unpack(">6I", data[at + 20 : at + 44])

    isutcnt, isstdcnt, leapcnt, timecnt, typecnt, charcnt = header(0)
    skip = 44 + timecnt * 5 + typecnt * 6 + charcnt + leapcnt * 8 + isstdcnt + isutcnt
    isutcnt, isstdcnt, leapcnt, timecnt, typecnt, charcnt = header(skip)
    at = skip + 44
    times = list(struct.unpack(f">{timecnt}q", data[at : at + timecnt * 8]))
    at += timecnt * 8
    indices = list(data[at : at + timecnt])
    at += timecnt
    types = []
    for _ in range(typecnt):
        utoff, isdst, _ = struct.unpack(">iBB", data[at : at + 6])
        types.append((utoff, isdst))
        at += 6
    at += charcnt + leapcnt * 12 + isstdcnt + isutcnt
    footer = data[at:].strip(b"\n")
    return types, times, indices, footer


def main():
    root, output = sys.argv[1], sys.argv[2]
    zones, links, version = [], [], b""
    with open(os.path.join(root, "tzdata.zi"), "rb") as source:
        for line in source:
            fields = line.split()
            if line.startswith(b"# version"):
                version = fields[2]
            elif fields[:1] == [b"Z"]:
                zones.append(fields[1].decode())
            elif fields[:1] == [b"L"]:
                links.append((fields[2].decode(), fields[1].decode()))
    zones.sort()
    position = {name: index for index, name in enumerate(zones)}
    blobs = []
    for name in zones:
        types, times, indices, footer = tzif(os.path.join(root, name))
        blob = struct.pack("<B", len(types))
        blob += b"".join(struct.pack("<iB", utoff, isdst) for utoff, isdst in types)
        blob += struct.pack("<I", len(times))
        blob += b"".join(struct.pack("<q", time) for time in times)
        blob += bytes(indices)
        blob += struct.pack("<B", len(footer)) + footer
        blobs.append(blob)
    out = b"CTZ1" + struct.pack("<B", len(version)) + version
    out += struct.pack("<H", len(zones))
    offset = 0
    for name, blob in zip(zones, blobs):
        encoded = name.encode()
        out += struct.pack("<B", len(encoded)) + encoded + struct.pack("<I", offset)
        offset += len(blob)
    links = sorted(link for link in links if link[1] in position)
    out += struct.pack("<H", len(links))
    for name, target in links:
        encoded = name.encode()
        out += struct.pack("<B", len(encoded)) + encoded + struct.pack("<H", position[target])
    out += b"".join(blobs)
    with open(output, "wb") as file:
        file.write(out)
    print(f"{len(zones)} zones, {len(links)} links, {len(out)} bytes, tzdata {version.decode()}")


main()
