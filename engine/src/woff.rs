// WOFF and WOFF2 web fonts unpacked back into plain sfnt files, which FontFace then reads

use crate::inflate::zlib_decompress;
use std::io::Read;

// fonts larger than this are refused rather than unpacked
const MAX_SFNT_BYTES: usize = 64 * 1024 * 1024;

pub(crate) fn is_woff(data: &[u8]) -> bool {
    data.starts_with(b"wOFF") || data.starts_with(b"wOF2")
}

pub(crate) fn decode(data: &[u8]) -> Result<Vec<u8>, String> {
    if data.starts_with(b"wOFF") {
        decode_woff(data)
    } else if data.starts_with(b"wOF2") {
        decode_woff2(data)
    } else {
        Err("not a woff font".to_string())
    }
}

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, at: 0 }
    }

    fn bytes(&mut self, count: usize) -> Result<&'a [u8], String> {
        let end = self
            .at
            .checked_add(count)
            .filter(|end| *end <= self.data.len());
        let end = end.ok_or("font data is truncated")?;
        let slice = &self.data[self.at..end];
        self.at = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.bytes(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, String> {
        let bytes = self.bytes(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn i16(&mut self) -> Result<i16, String> {
        Ok(self.u16()? as i16)
    }

    fn u32(&mut self) -> Result<u32, String> {
        let bytes = self.bytes(4)?;
        Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    // 255UInt16, the variable-length number of WOFF2 glyph data
    fn u255(&mut self) -> Result<u16, String> {
        Ok(match self.u8()? {
            253 => self.u16()?,
            254 => u16::from(self.u8()?) + 506,
            255 => u16::from(self.u8()?) + 253,
            code => u16::from(code),
        })
    }

    // UIntBase128, the variable-length number of the WOFF2 table directory
    fn base128(&mut self) -> Result<u32, String> {
        let mut value = 0_u32;
        for index in 0..5 {
            let byte = self.u8()?;
            if index == 0 && byte == 0x80 {
                return Err("a woff2 number has a leading zero".to_string());
            }
            if value & 0xFE00_0000 != 0 {
                return Err("a woff2 number overflows".to_string());
            }
            value = (value << 7) | u32::from(byte & 0x7F);
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err("a woff2 number runs past five bytes".to_string())
    }
}

// a plain sfnt from its tables, sorted by tag, each padded to four bytes
fn build_sfnt(flavor: u32, mut tables: Vec<([u8; 4], Vec<u8>)>) -> Result<Vec<u8>, String> {
    tables.sort_by(|left, right| left.0.cmp(&right.0));
    let count = tables.len();
    let mut output = Vec::new();
    output.extend_from_slice(&flavor.to_be_bytes());
    output.extend_from_slice(&(count as u16).to_be_bytes());
    let mut power = 1_u16;
    let mut log = 0_u16;
    while usize::from(power) * 2 <= count {
        power *= 2;
        log += 1;
    }
    output.extend_from_slice(&(power * 16).to_be_bytes());
    output.extend_from_slice(&log.to_be_bytes());
    output.extend_from_slice(&((count as u16 * 16).saturating_sub(power * 16)).to_be_bytes());
    let mut offset = 12 + count * 16;
    for (tag, table) in &tables {
        output.extend_from_slice(tag);
        output.extend_from_slice(&0_u32.to_be_bytes());
        output.extend_from_slice(&(offset as u32).to_be_bytes());
        output.extend_from_slice(&(table.len() as u32).to_be_bytes());
        offset += table.len().div_ceil(4) * 4;
    }
    if offset > MAX_SFNT_BYTES {
        return Err("the unpacked font is too large".to_string());
    }
    for (_, table) in &tables {
        output.extend_from_slice(table);
        output.resize(output.len().div_ceil(4) * 4, 0);
    }
    Ok(output)
}

fn decode_woff(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut header = Reader::new(data);
    header.bytes(4)?;
    let flavor = header.u32()?;
    header.u32()?;
    let count = header.u16()?;
    header.at = 44;
    let mut tables = Vec::new();
    let mut total = 0_usize;
    for _ in 0..count {
        let tag = header.bytes(4)?;
        let offset = header.u32()? as usize;
        let compressed = header.u32()? as usize;
        let original = header.u32()? as usize;
        header.u32()?;
        total = total.saturating_add(original);
        if total > MAX_SFNT_BYTES {
            return Err("the unpacked font is too large".to_string());
        }
        let stored = data
            .get(offset..offset.saturating_add(compressed))
            .ok_or("a woff table runs past the file")?;
        let table = if compressed < original {
            zlib_decompress(stored, original)
                .map_err(|error| format!("a woff table does not inflate: {error:?}"))?
        } else {
            stored.to_vec()
        };
        if table.len() != original {
            return Err("a woff table has the wrong length".to_string());
        }
        tables.push(([tag[0], tag[1], tag[2], tag[3]], table));
    }
    build_sfnt(flavor, tables)
}

const KNOWN_TAGS: [&[u8; 4]; 63] = [
    b"cmap", b"head", b"hhea", b"hmtx", b"maxp", b"name", b"OS/2", b"post", b"cvt ", b"fpgm",
    b"glyf", b"loca", b"prep", b"CFF ", b"VORG", b"EBDT", b"EBLC", b"gasp", b"hdmx", b"kern",
    b"LTSH", b"PCLT", b"VDMX", b"vhea", b"vmtx", b"BASE", b"GDEF", b"GPOS", b"GSUB", b"EBSC",
    b"JSTF", b"MATH", b"CBDT", b"CBLC", b"COLR", b"CPAL", b"SVG ", b"sbix", b"acnt", b"avar",
    b"bdat", b"bloc", b"bsln", b"cvar", b"fdsc", b"feat", b"fmtx", b"fvar", b"gvar", b"hsty",
    b"just", b"lcar", b"mort", b"morx", b"opbd", b"prop", b"trak", b"Zapf", b"Silf", b"Glat",
    b"Gloc", b"Feat", b"Sill",
];

struct Woff2Table {
    tag: [u8; 4],
    transformed: bool,
    // the length in the decompressed stream
    stored: usize,
}

fn decode_woff2(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut header = Reader::new(data);
    header.bytes(4)?;
    let flavor = header.u32()?;
    if flavor == u32::from_be_bytes(*b"ttcf") {
        return Err("woff2 font collections are not supported".to_string());
    }
    header.u32()?;
    let count = header.u16()?;
    header.u16()?;
    header.u32()?;
    let compressed_size = header.u32()? as usize;
    header.at = 48;
    let mut tables = Vec::new();
    for _ in 0..count {
        let flags = header.u8()?;
        let tag = match flags & 0x3F {
            63 => {
                let tag = header.bytes(4)?;
                [tag[0], tag[1], tag[2], tag[3]]
            }
            index => *KNOWN_TAGS[usize::from(index)],
        };
        let version = flags >> 6;
        let original = header.base128()? as usize;
        // glyf and loca are transformed at version 0, every other table at any other version
        let transformed = if matches!(&tag, b"glyf" | b"loca") {
            version == 0
        } else {
            version != 0
        };
        let stored = if transformed {
            header.base128()? as usize
        } else {
            original
        };
        tables.push(Woff2Table {
            tag,
            transformed,
            stored,
        });
    }
    let compressed = data
        .get(header.at..header.at.saturating_add(compressed_size))
        .ok_or("the woff2 stream runs past the file")?;
    let expected = tables.iter().map(|table| table.stored).sum::<usize>();
    if expected > MAX_SFNT_BYTES {
        return Err("the unpacked font is too large".to_string());
    }
    let mut stream = Vec::with_capacity(expected);
    brotli_decompressor::Decompressor::new(compressed, 4096)
        .take(MAX_SFNT_BYTES as u64 + 1)
        .read_to_end(&mut stream)
        .map_err(|error| format!("the woff2 stream does not decompress: {error}"))?;
    if stream.len() < expected {
        return Err("the woff2 stream is shorter than its tables".to_string());
    }

    let mut offset = 0;
    let mut raw = Vec::new();
    for table in &tables {
        raw.push((table, &stream[offset..offset + table.stored]));
        offset += table.stored;
    }
    let find = |tag: &[u8; 4]| {
        raw.iter()
            .find(|(table, _)| &table.tag == tag)
            .map(|(table, bytes)| (*table, *bytes))
    };
    let mut output = Vec::new();
    let mut glyph_bounds = Vec::new();
    if let Some((glyf, bytes)) = find(b"glyf")
        && glyf.transformed
    {
        let (glyf_table, loca_table, bounds) = reconstruct_glyf(bytes)?;
        glyph_bounds = bounds;
        output.push((*b"glyf", glyf_table));
        output.push((*b"loca", loca_table));
    }
    for (table, bytes) in &raw {
        match &table.tag {
            b"glyf" | b"loca" if !glyph_bounds.is_empty() || table.transformed => {}
            b"head" if !glyph_bounds.is_empty() => {
                // the rebuilt loca uses long offsets
                let mut head = bytes.to_vec();
                if head.len() >= 52 {
                    head[50..52].copy_from_slice(&1_u16.to_be_bytes());
                }
                output.push((table.tag, head));
            }
            b"hmtx" if table.transformed => {
                let hhea = find(b"hhea").ok_or("a woff2 font has hmtx without hhea")?.1;
                let metrics = u16::from_be_bytes([
                    *hhea.get(34).ok_or("hhea is short")?,
                    *hhea.get(35).ok_or("hhea is short")?,
                ]);
                output.push((table.tag, reconstruct_hmtx(bytes, metrics, &glyph_bounds)?));
            }
            _ => output.push((table.tag, bytes.to_vec())),
        }
    }
    build_sfnt(flavor, output)
}

// one woff2 coordinate pair from its flag and the bytes after it
fn triplet(flag: u8, data: &[u8]) -> Result<(i32, i32, usize), String> {
    let signed = |flag: u8, value: i32| if flag & 1 != 0 { value } else { -value };
    let byte = |index: usize| -> Result<i32, String> {
        data.get(index)
            .map(|byte| i32::from(*byte))
            .ok_or_else(|| "a woff2 glyph point is truncated".to_string())
    };
    let flag_value = i32::from(flag);
    Ok(match flag {
        0..10 => (0, signed(flag, ((flag_value & 14) << 7) + byte(0)?), 1),
        10..20 => (
            signed(flag, (((flag_value - 10) & 14) << 7) + byte(0)?),
            0,
            1,
        ),
        20..84 => {
            let base = flag_value - 20;
            let next = byte(0)?;
            (
                signed(flag, 1 + (base & 0x30) + (next >> 4)),
                signed(flag >> 1, 1 + ((base & 0x0C) << 2) + (next & 0x0F)),
                1,
            )
        }
        84..120 => {
            let base = flag_value - 84;
            (
                signed(flag, 1 + ((base / 12) << 8) + byte(0)?),
                signed(flag >> 1, 1 + (((base % 12) >> 2) << 8) + byte(1)?),
                2,
            )
        }
        120..124 => {
            let middle = byte(1)?;
            (
                signed(flag, (byte(0)? << 4) + (middle >> 4)),
                signed(flag >> 1, ((middle & 0x0F) << 8) + byte(2)?),
                3,
            )
        }
        _ => (
            signed(flag, (byte(0)? << 8) + byte(1)?),
            signed(flag >> 1, (byte(2)? << 8) + byte(3)?),
            4,
        ),
    })
}

// the glyf and loca tables from the woff2 glyf transform, with each glyph's xMin for hmtx
// rebuilt glyf and loca tables, and each glyph's xMin
type RebuiltGlyphs = (Vec<u8>, Vec<u8>, Vec<i16>);

fn reconstruct_glyf(data: &[u8]) -> Result<RebuiltGlyphs, String> {
    let mut header = Reader::new(data);
    header.u16()?;
    header.u16()?;
    let glyph_count = usize::from(header.u16()?);
    header.u16()?;
    let mut sizes = [0_usize; 7];
    for size in &mut sizes {
        *size = header.u32()? as usize;
    }
    let mut start = header.at;
    let mut stream = |size: usize| -> Result<Reader<'_>, String> {
        let slice = data
            .get(start..start.saturating_add(size))
            .ok_or("a woff2 glyf stream runs past its table")?;
        start += size;
        Ok(Reader::new(slice))
    };
    let mut contours = stream(sizes[0])?;
    let mut points = stream(sizes[1])?;
    let mut flags = stream(sizes[2])?;
    let mut glyphs = stream(sizes[3])?;
    let mut composites = stream(sizes[4])?;
    let bbox_data = stream(sizes[5])?.data;
    let mut instructions = stream(sizes[6])?;
    let bitmap_length = glyph_count.div_ceil(32) * 4;
    let bitmap = bbox_data
        .get(..bitmap_length)
        .ok_or("the woff2 bbox bitmap is truncated")?;
    let mut bboxes = Reader::new(&bbox_data[bitmap_length..]);
    let has_bbox = |glyph: usize| bitmap[glyph / 8] & (0x80 >> (glyph % 8)) != 0;

    let mut glyf = Vec::new();
    let mut loca = Vec::with_capacity((glyph_count + 1) * 4);
    let mut x_minimums = Vec::with_capacity(glyph_count);
    for glyph in 0..glyph_count {
        loca.extend_from_slice(&(glyf.len() as u32).to_be_bytes());
        let contour_count = contours.i16()?;
        if contour_count == 0 {
            x_minimums.push(0);
            continue;
        }
        if contour_count < 0 {
            // a composite: its records are copied, and its box always comes from the stream
            let bounds = [bboxes.i16()?, bboxes.i16()?, bboxes.i16()?, bboxes.i16()?];
            let record_start = composites.at;
            let mut instructions_follow = false;
            loop {
                let component = composites.u16()?;
                composites.u16()?;
                let mut size = if component & 0x0001 != 0 { 4 } else { 2 };
                if component & 0x0008 != 0 {
                    size += 2;
                } else if component & 0x0040 != 0 {
                    size += 4;
                } else if component & 0x0080 != 0 {
                    size += 8;
                }
                composites.bytes(size)?;
                instructions_follow |= component & 0x0100 != 0;
                if component & 0x0020 == 0 {
                    break;
                }
            }
            glyf.extend_from_slice(&(-1_i16).to_be_bytes());
            for value in bounds {
                glyf.extend_from_slice(&value.to_be_bytes());
            }
            glyf.extend_from_slice(&composites.data[record_start..composites.at]);
            if instructions_follow {
                let length = glyphs.u255()?;
                glyf.extend_from_slice(&length.to_be_bytes());
                glyf.extend_from_slice(instructions.bytes(usize::from(length))?);
            }
            x_minimums.push(bounds[0]);
        } else {
            let mut ends = Vec::with_capacity(contour_count as usize);
            let mut total = 0_usize;
            for _ in 0..contour_count {
                total += usize::from(points.u255()?);
                ends.push(
                    total
                        .checked_sub(1)
                        .ok_or("a woff2 contour has no points")?,
                );
            }
            let mut coordinates = Vec::with_capacity(total);
            let (mut x, mut y) = (0_i32, 0_i32);
            for _ in 0..total {
                let flag = flags.u8()?;
                let (dx, dy, used) = triplet(flag & 0x7F, &glyphs.data[glyphs.at..])?;
                glyphs.at += used;
                x += dx;
                y += dy;
                coordinates.push((x, y, flag & 0x80 == 0));
            }
            let instruction_length = glyphs.u255()?;
            let bounds = if has_bbox(glyph) {
                [bboxes.i16()?, bboxes.i16()?, bboxes.i16()?, bboxes.i16()?]
            } else {
                let clamp =
                    |value: i32| value.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
                [
                    clamp(coordinates.iter().map(|point| point.0).min().unwrap_or(0)),
                    clamp(coordinates.iter().map(|point| point.1).min().unwrap_or(0)),
                    clamp(coordinates.iter().map(|point| point.0).max().unwrap_or(0)),
                    clamp(coordinates.iter().map(|point| point.1).max().unwrap_or(0)),
                ]
            };
            glyf.extend_from_slice(&contour_count.to_be_bytes());
            for value in bounds {
                glyf.extend_from_slice(&value.to_be_bytes());
            }
            for end in &ends {
                glyf.extend_from_slice(
                    &u16::try_from(*end)
                        .map_err(|_| "a woff2 glyph has too many points")?
                        .to_be_bytes(),
                );
            }
            glyf.extend_from_slice(&instruction_length.to_be_bytes());
            glyf.extend_from_slice(instructions.bytes(usize::from(instruction_length))?);
            // every coordinate is a full 16-bit delta, so each flag only marks on-curve points
            for point in &coordinates {
                glyf.push(u8::from(point.2));
            }
            let mut previous = (0_i32, 0_i32);
            let mut x_deltas = Vec::with_capacity(total * 2);
            let mut y_deltas = Vec::with_capacity(total * 2);
            for point in &coordinates {
                let dx = i16::try_from(point.0 - previous.0)
                    .map_err(|_| "a woff2 glyph point is out of range")?;
                let dy = i16::try_from(point.1 - previous.1)
                    .map_err(|_| "a woff2 glyph point is out of range")?;
                x_deltas.extend_from_slice(&dx.to_be_bytes());
                y_deltas.extend_from_slice(&dy.to_be_bytes());
                previous = (point.0, point.1);
            }
            glyf.extend_from_slice(&x_deltas);
            glyf.extend_from_slice(&y_deltas);
            x_minimums.push(bounds[0]);
        }
        glyf.resize(glyf.len().div_ceil(4) * 4, 0);
        if glyf.len() > MAX_SFNT_BYTES {
            return Err("the unpacked font is too large".to_string());
        }
    }
    loca.extend_from_slice(&(glyf.len() as u32).to_be_bytes());
    Ok((glyf, loca, x_minimums))
}

// hmtx from the woff2 transform, where left side bearings may be left to the glyph boxes
fn reconstruct_hmtx(data: &[u8], metrics: u16, x_minimums: &[i16]) -> Result<Vec<u8>, String> {
    let mut reader = Reader::new(data);
    let flags = reader.u8()?;
    let metrics = usize::from(metrics);
    let glyph_count = x_minimums.len().max(metrics);
    let mut advances = Vec::with_capacity(metrics);
    for _ in 0..metrics {
        advances.push(reader.u16()?);
    }
    let mut bearing = |index: usize, stored: bool| -> Result<i16, String> {
        if stored {
            reader.i16()
        } else {
            Ok(x_minimums.get(index).copied().unwrap_or(0))
        }
    };
    let mut output = Vec::with_capacity(metrics * 4 + (glyph_count - metrics) * 2);
    let mut bearings = Vec::with_capacity(glyph_count);
    for index in 0..metrics {
        bearings.push(bearing(index, flags & 1 == 0)?);
    }
    for index in metrics..glyph_count {
        bearings.push(bearing(index, flags & 2 == 0)?);
    }
    for index in 0..metrics {
        output.extend_from_slice(&advances[index].to_be_bytes());
        output.extend_from_slice(&bearings[index].to_be_bytes());
    }
    for value in &bearings[metrics..] {
        output.extend_from_slice(&value.to_be_bytes());
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_woff2_numbers_and_point_triplets() {
        assert_eq!(Reader::new(&[0x3F]).base128(), Ok(63));
        assert_eq!(Reader::new(&[0x81, 0x00]).base128(), Ok(128));
        assert!(Reader::new(&[0x80, 0x01]).base128().is_err());
        assert_eq!(Reader::new(&[252]).u255(), Ok(252));
        assert_eq!(Reader::new(&[255, 3]).u255(), Ok(256));
        assert_eq!(Reader::new(&[254, 0]).u255(), Ok(506));
        assert_eq!(Reader::new(&[253, 1, 0]).u255(), Ok(256));
        // flag 1: no x movement and a positive y from the next byte
        assert_eq!(triplet(1, &[5]), Ok((0, 5, 1)));
        assert_eq!(triplet(0, &[5]), Ok((0, -5, 1)));
        assert_eq!(triplet(11, &[7]), Ok((7, 0, 1)));
        assert_eq!(triplet(127, &[1, 0, 0, 2]), Ok((256, 2, 4)));
    }

    #[test]
    fn unpacks_a_woff_wrapping_of_stored_tables() {
        let table = vec![1_u8, 2, 3, 4];
        let mut woff = Vec::new();
        woff.extend_from_slice(b"wOFF");
        woff.extend_from_slice(&0x0001_0000_u32.to_be_bytes());
        woff.extend_from_slice(&0_u32.to_be_bytes());
        woff.extend_from_slice(&1_u16.to_be_bytes());
        woff.resize(44, 0);
        woff.extend_from_slice(b"test");
        woff.extend_from_slice(&64_u32.to_be_bytes());
        woff.extend_from_slice(&4_u32.to_be_bytes());
        woff.extend_from_slice(&4_u32.to_be_bytes());
        woff.extend_from_slice(&0_u32.to_be_bytes());
        woff.extend_from_slice(&table);
        let sfnt = decode(&woff).expect("woff should unpack");
        assert_eq!(&sfnt[..4], &0x0001_0000_u32.to_be_bytes());
        assert_eq!(&sfnt[12..16], b"test");
        assert_eq!(&sfnt[28..32], &table[..]);
    }
}
