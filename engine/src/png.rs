// PNG decoding: every color type and bit depth, palettes with transparency,
// all five scanline filters and Adam7 interlacing, checked against image limits
// before anything is allocated

use crate::css::Color;
use crate::image::{Image, ImageError, check_dimensions};
use crate::inflate::zlib_decompress;

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
const ADAM7: [(usize, usize, usize, usize); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];

pub(crate) fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(&SIGNATURE)
}

struct Header {
    width: usize,
    height: usize,
    depth: u8,
    color_type: u8,
    interlaced: bool,
}

impl Header {
    fn channels(&self) -> usize {
        match self.color_type {
            0 | 3 => 1,
            2 => 3,
            4 => 2,
            _ => 4,
        }
    }

    fn bits_per_pixel(&self) -> usize {
        self.channels() * usize::from(self.depth)
    }

    fn row_bytes(&self, width: usize) -> usize {
        (width * self.bits_per_pixel()).div_ceil(8)
    }

    // filtered size of one pass: a filter byte plus packed samples per row
    fn pass_bytes(&self, width: usize, height: usize) -> usize {
        if width == 0 || height == 0 {
            0
        } else {
            height * (1 + self.row_bytes(width))
        }
    }
}

fn error(detail: &str) -> ImageError {
    ImageError::new(format!("png: {detail}"))
}

pub(crate) fn decode_png(bytes: &[u8]) -> Result<Image, ImageError> {
    if !is_png(bytes) {
        return Err(error("missing signature"));
    }
    let mut position = SIGNATURE.len();
    let mut header = None;
    let mut palette: Vec<[u8; 3]> = Vec::new();
    let mut transparency: Vec<u8> = Vec::new();
    let mut data = Vec::new();
    let mut ended = false;
    while position < bytes.len() {
        let length = read_u32(bytes, position).ok_or_else(|| error("truncated chunk"))? as usize;
        let kind = bytes
            .get(position + 4..position + 8)
            .ok_or_else(|| error("truncated chunk"))?;
        let body_end = position
            .checked_add(8 + length)
            .filter(|end| end + 4 <= bytes.len())
            .ok_or_else(|| error("truncated chunk"))?;
        let body = &bytes[position + 8..body_end];
        let stored_crc = read_u32(bytes, body_end).ok_or_else(|| error("truncated chunk"))?;
        if crc32(&bytes[position + 4..body_end]) != stored_crc {
            return Err(error("chunk checksum mismatch"));
        }
        if header.is_none() && kind != b"IHDR" {
            return Err(error("IHDR must come first"));
        }
        match kind {
            b"IHDR" => header = Some(parse_header(body)?),
            b"PLTE" => {
                if !body.len().is_multiple_of(3) || body.len() > 768 {
                    return Err(error("bad palette"));
                }
                palette = body
                    .chunks_exact(3)
                    .map(|entry| [entry[0], entry[1], entry[2]])
                    .collect();
            }
            b"tRNS" => transparency = body.to_vec(),
            b"IDAT" => data.extend_from_slice(body),
            b"IEND" => {
                ended = true;
                break;
            }
            // critical chunks this decoder does not know would change the pixels
            _ if kind[0].is_ascii_uppercase() => return Err(error("unknown critical chunk")),
            _ => {}
        }
        position = body_end + 4;
    }
    let header = header.ok_or_else(|| error("missing IHDR"))?;
    if !ended {
        return Err(error("missing IEND"));
    }
    if header.color_type == 3 && palette.is_empty() {
        return Err(error("indexed image without a palette"));
    }
    let expected = if header.interlaced {
        ADAM7
            .iter()
            .map(|&(x0, y0, dx, dy)| {
                header.pass_bytes(
                    (header.width.saturating_sub(x0)).div_ceil(dx),
                    (header.height.saturating_sub(y0)).div_ceil(dy),
                )
            })
            .sum()
    } else {
        header.pass_bytes(header.width, header.height)
    };
    let raw = zlib_decompress(&data, expected).map_err(|cause| error(&cause.to_string()))?;
    if raw.len() < expected {
        return Err(error("image data ends early"));
    }
    let mut image = Image::new(header.width as u32, header.height as u32);
    let converter = Converter {
        header: &header,
        palette: &palette,
        transparency: &transparency,
    };
    if header.interlaced {
        let mut offset = 0;
        for &(x0, y0, dx, dy) in &ADAM7 {
            let width = header.width.saturating_sub(x0).div_ceil(dx);
            let height = header.height.saturating_sub(y0).div_ceil(dy);
            let size = header.pass_bytes(width, height);
            let rows = unfilter(&header, &raw[offset..offset + size], width, height)?;
            for (row_index, row) in rows
                .chunks_exact(header.row_bytes(width).max(1))
                .enumerate()
            {
                for column in 0..width {
                    let x = x0 + column * dx;
                    let y = y0 + row_index * dy;
                    image.pixels[y * header.width + x] = converter.pixel(row, column);
                }
            }
            offset += size;
        }
    } else {
        let rows = unfilter(&header, &raw, header.width, header.height)?;
        let opaque_rgb = header.color_type == 2 && transparency.is_empty();
        match (header.depth, header.color_type) {
            // the common 8-bit layouts skip per-sample unpacking
            (8, 6) => {
                for (pixel, rgba) in image.pixels.iter_mut().zip(rows.chunks_exact(4)) {
                    *pixel = Color::rgba(rgba[0], rgba[1], rgba[2], rgba[3]);
                }
            }
            (8, 2) if opaque_rgb => {
                for (pixel, rgb) in image.pixels.iter_mut().zip(rows.chunks_exact(3)) {
                    *pixel = Color::rgba(rgb[0], rgb[1], rgb[2], 255);
                }
            }
            _ => {
                for (y, row) in rows
                    .chunks_exact(header.row_bytes(header.width))
                    .enumerate()
                {
                    for x in 0..header.width {
                        image.pixels[y * header.width + x] = converter.pixel(row, x);
                    }
                }
            }
        }
    }
    Ok(image)
}

fn parse_header(body: &[u8]) -> Result<Header, ImageError> {
    if body.len() != 13 {
        return Err(error("bad IHDR"));
    }
    let width = read_u32(body, 0).unwrap_or_default();
    let height = read_u32(body, 4).unwrap_or_default();
    check_dimensions(width, height)?;
    let (depth, color_type) = (body[8], body[9]);
    let valid_depth = match color_type {
        0 => matches!(depth, 1 | 2 | 4 | 8 | 16),
        3 => matches!(depth, 1 | 2 | 4 | 8),
        2 | 4 | 6 => matches!(depth, 8 | 16),
        _ => false,
    };
    if !valid_depth {
        return Err(error("unsupported color type and depth"));
    }
    if body[10] != 0 || body[11] != 0 || body[12] > 1 {
        return Err(error("unsupported compression, filter or interlace method"));
    }
    Ok(Header {
        width: width as usize,
        height: height as usize,
        depth,
        color_type,
        interlaced: body[12] == 1,
    })
}

// reverses the per-row filters in place and returns the packed rows without filter bytes
fn unfilter(
    header: &Header,
    filtered: &[u8],
    width: usize,
    height: usize,
) -> Result<Vec<u8>, ImageError> {
    let stride = header.row_bytes(width);
    let step = header.bits_per_pixel().div_ceil(8).max(1);
    let mut rows = vec![0_u8; stride * height];
    for y in 0..height {
        let source = &filtered[y * (stride + 1)..(y + 1) * (stride + 1)];
        let (previous, current) = rows.split_at_mut(y * stride);
        let previous = if y == 0 {
            None
        } else {
            Some(&previous[(y - 1) * stride..])
        };
        let current = &mut current[..stride];
        current.copy_from_slice(&source[1..]);
        let above = |index: usize| previous.map_or(0, |row| row[index]);
        match source[0] {
            0 => {}
            1 => {
                for index in step..stride {
                    current[index] = current[index].wrapping_add(current[index - step]);
                }
            }
            2 => {
                if let Some(previous) = previous {
                    for (value, above) in current.iter_mut().zip(previous) {
                        *value = value.wrapping_add(*above);
                    }
                }
            }
            3 => {
                for index in 0..stride {
                    let left = if index >= step {
                        current[index - step]
                    } else {
                        0
                    };
                    let average = ((u16::from(left) + u16::from(above(index))) / 2) as u8;
                    current[index] = current[index].wrapping_add(average);
                }
            }
            4 => {
                for index in 0..stride {
                    let left = if index >= step {
                        current[index - step]
                    } else {
                        0
                    };
                    let upper_left = if index >= step {
                        above(index - step)
                    } else {
                        0
                    };
                    current[index] =
                        current[index].wrapping_add(paeth(left, above(index), upper_left));
                }
            }
            _ => return Err(error("bad filter type")),
        }
    }
    Ok(rows)
}

fn paeth(left: u8, above: u8, upper_left: u8) -> u8 {
    let estimate = i16::from(left) + i16::from(above) - i16::from(upper_left);
    let to_left = (estimate - i16::from(left)).abs();
    let to_above = (estimate - i16::from(above)).abs();
    let to_upper_left = (estimate - i16::from(upper_left)).abs();
    if to_left <= to_above && to_left <= to_upper_left {
        left
    } else if to_above <= to_upper_left {
        above
    } else {
        upper_left
    }
}

struct Converter<'a> {
    header: &'a Header,
    palette: &'a [[u8; 3]],
    transparency: &'a [u8],
}

impl Converter<'_> {
    // the full-precision sample, so tRNS keys compare exactly at 16 bits
    fn sample(&self, row: &[u8], column: usize, channel: usize) -> u16 {
        let depth = usize::from(self.header.depth);
        let index = column * self.header.channels() + channel;
        match depth {
            16 => u16::from_be_bytes([row[index * 2], row[index * 2 + 1]]),
            8 => u16::from(row[index]),
            _ => {
                let bit = index * depth;
                let byte = row[bit / 8];
                let shift = 8 - depth - bit % 8;
                u16::from((byte >> shift) & ((1 << depth) - 1) as u8)
            }
        }
    }

    fn to_byte(&self, sample: u16) -> u8 {
        match self.header.depth {
            16 => (sample >> 8) as u8,
            8 => sample as u8,
            depth => (u32::from(sample) * 255 / ((1 << depth) - 1)) as u8,
        }
    }

    fn key(&self, channel: usize) -> Option<u16> {
        let bytes = self.transparency.get(channel * 2..channel * 2 + 2)?;
        Some(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn pixel(&self, row: &[u8], column: usize) -> Color {
        let sample = |channel| self.sample(row, column, channel);
        match self.header.color_type {
            0 => {
                let gray = sample(0);
                let value = self.to_byte(gray);
                let alpha = if self.key(0) == Some(gray) { 0 } else { 255 };
                Color::rgba(value, value, value, alpha)
            }
            2 => {
                let (red, green, blue) = (sample(0), sample(1), sample(2));
                let keyed = self.key(0) == Some(red)
                    && self.key(1) == Some(green)
                    && self.key(2) == Some(blue);
                Color::rgba(
                    self.to_byte(red),
                    self.to_byte(green),
                    self.to_byte(blue),
                    if keyed { 0 } else { 255 },
                )
            }
            3 => {
                let index = usize::from(sample(0));
                let [red, green, blue] = self.palette.get(index).copied().unwrap_or([0, 0, 0]);
                let alpha = self.transparency.get(index).copied().unwrap_or(255);
                Color::rgba(red, green, blue, alpha)
            }
            4 => {
                let value = self.to_byte(sample(0));
                Color::rgba(value, value, value, self.to_byte(sample(1)))
            }
            _ => Color::rgba(
                self.to_byte(sample(0)),
                self.to_byte(sample(1)),
                self.to_byte(sample(2)),
                self.to_byte(sample(3)),
            ),
        }
    }
}

fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let slice = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn crc32(bytes: &[u8]) -> u32 {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut table = [0_u32; 256];
        for (index, entry) in table.iter_mut().enumerate() {
            let mut value = index as u32;
            for _ in 0..8 {
                value = if value & 1 == 1 {
                    0xedb8_8320 ^ (value >> 1)
                } else {
                    value >> 1
                };
            }
            *entry = value;
        }
        table
    });
    !bytes.iter().fold(!0_u32, |crc, &byte| {
        table[((crc ^ u32::from(byte)) & 0xff) as usize] ^ (crc >> 8)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(format!(
            "{}/tests/fixtures/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .expect("fixture should exist")
    }

    fn rgba(color: Color) -> [u8; 4] {
        [color.red, color.green, color.blue, color.alpha]
    }

    #[test]
    fn decodes_every_color_type_depth_filter_and_interlace() {
        let names = String::from_utf8(fixture("png-cases.txt")).expect("case list is text");
        for name in names.lines().filter(|line| !line.is_empty()) {
            let image =
                decode_png(&fixture(name)).unwrap_or_else(|cause| panic!("{name}: {cause}"));
            assert_eq!((image.width, image.height), (13, 7), "{name}");
            let reference = fixture(&name.replace(".png", ".rgba"));
            for (index, pixel) in image.pixels.iter().enumerate() {
                let want = &reference[index * 4..index * 4 + 4];
                assert_eq!(rgba(*pixel), want, "{name} pixel {index}");
            }
        }
    }

    #[test]
    fn rejects_corrupt_and_oversized_files_without_panicking() {
        let bytes = fixture("rgba8.png");
        for end in 0..bytes.len() {
            assert!(decode_png(&bytes[..end]).is_err());
        }
        let mut corrupt = bytes.clone();
        corrupt[40] ^= 0x55;
        assert!(decode_png(&corrupt).is_err());
        let mut huge = bytes;
        huge[16..20].copy_from_slice(&100_000_u32.to_be_bytes());
        let crc = crc32(&huge[12..29]);
        huge[29..33].copy_from_slice(&crc.to_be_bytes());
        assert!(
            decode_png(&huge)
                .unwrap_err()
                .to_string()
                .contains("exceeds")
        );
    }
}
