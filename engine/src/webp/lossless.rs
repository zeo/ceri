// VP8L, the lossless WebP bitstream of RFC 9649: prefix-coded ARGB with backward references,
// a color cache and the four transforms; and an encoder for canvas exports that uses the
// subtract-green transform and copies from the pixel to the left or above

use super::tables::DISTANCE_MAP;

const CODE_LENGTH_ORDER: [usize; 19] = [
    17, 18, 0, 1, 2, 3, 4, 5, 16, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
];
const FAST_BITS: u32 = 8;

pub(super) struct BitReader<'a> {
    bytes: &'a [u8],
    position: usize,
    bits: u64,
    count: u32,
}

impl<'a> BitReader<'a> {
    pub(super) fn new(bytes: &'a [u8]) -> Self {
        BitReader {
            bytes,
            position: 0,
            bits: 0,
            count: 0,
        }
    }

    fn fill(&mut self) {
        while self.count <= 56 {
            let byte = self.bytes.get(self.position).copied().unwrap_or(0);
            self.position += 1;
            self.bits |= u64::from(byte) << self.count;
            self.count += 8;
        }
    }

    pub(super) fn read(&mut self, count: u32) -> u32 {
        if count == 0 {
            return 0;
        }
        self.fill();
        let value = (self.bits & ((1 << count) - 1)) as u32;
        self.bits >>= count;
        self.count -= count;
        value
    }

    fn peek(&mut self, count: u32) -> u32 {
        self.fill();
        (self.bits & ((1 << count) - 1)) as u32
    }

    fn skip(&mut self, count: u32) {
        self.bits >>= count;
        self.count -= count;
    }

    // reading past the end yields zeros; this says whether the data ran out
    pub(super) fn overrun(&self) -> bool {
        self.position > self.bytes.len() + 8
    }
}

// a canonical prefix code: a table for codes of up to eight bits and the counts for longer ones
struct PrefixCode {
    // symbol and length for each eight-bit window, or length 0 when the code is longer
    fast: Vec<(u16, u8)>,
    counts: [u16; 16],
    symbols: Vec<u16>,
    // the only symbol of a code with one, which takes no bits
    single: Option<u16>,
}

impl PrefixCode {
    fn new(lengths: &[u8]) -> Option<PrefixCode> {
        let used: Vec<usize> = (0..lengths.len())
            .filter(|&symbol| lengths[symbol] > 0)
            .collect();
        if used.is_empty() {
            return None;
        }
        if used.len() == 1 {
            return Some(PrefixCode {
                fast: Vec::new(),
                counts: [0; 16],
                symbols: Vec::new(),
                single: Some(used[0] as u16),
            });
        }
        let mut counts = [0_u16; 16];
        for &symbol in &used {
            counts[usize::from(lengths[symbol])] += 1;
        }
        // a complete code fills exactly the space of every length
        let mut space = 1_i32;
        for &count in &counts[1..] {
            space = space * 2 - i32::from(count);
            if space < 0 {
                return None;
            }
        }
        if space != 0 {
            return None;
        }
        let mut symbols = used.clone();
        symbols.sort_by_key(|&symbol| (lengths[symbol], symbol));
        let mut fast = vec![(0_u16, 0_u8); 1 << FAST_BITS];
        let mut code = 0_u32;
        let mut previous = 0;
        for &symbol in &symbols {
            let length = u32::from(lengths[symbol]);
            code <<= length - previous;
            previous = length;
            if length <= FAST_BITS {
                let reversed = code.reverse_bits() >> (32 - length);
                let mut index = reversed;
                while index < 1 << FAST_BITS {
                    fast[index as usize] = (symbol as u16, length as u8);
                    index += 1 << length;
                }
            }
            code += 1;
        }
        Some(PrefixCode {
            fast,
            counts,
            symbols: symbols.into_iter().map(|symbol| symbol as u16).collect(),
            single: None,
        })
    }

    fn read(&self, reader: &mut BitReader) -> u16 {
        if let Some(symbol) = self.single {
            return symbol;
        }
        let (symbol, length) = self.fast[reader.peek(FAST_BITS) as usize];
        if length > 0 {
            reader.skip(u32::from(length));
            return symbol;
        }
        // one bit at a time past the table, as canonical codes allow
        let (mut code, mut first, mut index) = (0_i32, 0_i32, 0_i32);
        for length in 1..16 {
            code |= reader.read(1) as i32;
            let count = i32::from(self.counts[length]);
            if code - count < first {
                return self.symbols[(index + code - first) as usize];
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        0
    }
}

fn read_code(reader: &mut BitReader, alphabet: usize) -> Option<PrefixCode> {
    let mut lengths = vec![0_u8; alphabet];
    if reader.read(1) == 1 {
        // a simple code of one or two symbols
        let two = reader.read(1) == 1;
        let wide = reader.read(1) == 1;
        let first = reader.read(if wide { 8 } else { 1 }) as usize;
        *lengths.get_mut(first)? = 1;
        if two {
            let second = reader.read(8) as usize;
            *lengths.get_mut(second)? = 1;
        }
        return PrefixCode::new(&lengths);
    }
    let mut code_lengths = [0_u8; 19];
    let count = 4 + reader.read(4) as usize;
    for &symbol in CODE_LENGTH_ORDER.iter().take(count) {
        code_lengths[symbol] = reader.read(3) as u8;
    }
    let length_code = PrefixCode::new(&code_lengths)?;
    let mut remaining = if reader.read(1) == 1 {
        let bits = 2 + 2 * reader.read(3);
        2 + reader.read(bits) as usize
    } else {
        alphabet
    };
    let mut symbol = 0;
    let mut previous = 8_u8;
    while symbol < alphabet {
        if remaining == 0 {
            break;
        }
        remaining -= 1;
        let code = length_code.read(reader);
        match code {
            0..=15 => {
                lengths[symbol] = code as u8;
                if code != 0 {
                    previous = code as u8;
                }
                symbol += 1;
            }
            _ => {
                let (repeat, value) = match code {
                    16 => (3 + reader.read(2) as usize, previous),
                    17 => (3 + reader.read(3) as usize, 0),
                    _ => (11 + reader.read(7) as usize, 0),
                };
                if symbol + repeat > alphabet {
                    return None;
                }
                lengths[symbol..symbol + repeat].fill(value);
                symbol += repeat;
            }
        }
    }
    PrefixCode::new(&lengths)
}

fn subsample(size: u32, bits: u32) -> u32 {
    (size + (1 << bits) - 1) >> bits
}

fn prefix_value(reader: &mut BitReader, prefix: u32) -> u32 {
    if prefix < 4 {
        return prefix + 1;
    }
    let extra = (prefix - 2) >> 1;
    let offset = (2 + (prefix & 1)) << extra;
    offset + reader.read(extra) + 1
}

enum Transform {
    Predictor(u32, Vec<u32>),
    Color(u32, Vec<u32>),
    SubtractGreen,
    // the palette and how many pixels share a byte
    Indexing(Vec<u32>, u32),
}

#[inline]
fn add_pixels(a: u32, b: u32) -> u32 {
    let alpha_green = (a & 0xff00_ff00).wrapping_add(b & 0xff00_ff00);
    let red_blue = (a & 0x00ff_00ff).wrapping_add(b & 0x00ff_00ff);
    (alpha_green & 0xff00_ff00) | (red_blue & 0x00ff_00ff)
}

#[inline]
fn average(a: u32, b: u32) -> u32 {
    (((a ^ b) & 0xfefe_fefe) >> 1) + (a & b)
}

fn channel(pixel: u32, shift: u32) -> i32 {
    ((pixel >> shift) & 0xff) as i32
}

fn select(top: u32, left: u32, top_left: u32) -> u32 {
    let mut difference = 0;
    for shift in [24, 16, 8, 0] {
        let (a, b, c) = (
            channel(top, shift),
            channel(left, shift),
            channel(top_left, shift),
        );
        difference += (b - c).abs() - (a - c).abs();
    }
    if difference <= 0 { top } else { left }
}

fn clamp_add_subtract_full(a: u32, b: u32, c: u32) -> u32 {
    [24, 16, 8, 0].iter().fold(0, |pixel, &shift| {
        pixel
            | ((channel(a, shift) + channel(b, shift) - channel(c, shift)).clamp(0, 255) as u32)
                << shift
    })
}

fn clamp_add_subtract_half(a: u32, b: u32) -> u32 {
    [24, 16, 8, 0].iter().fold(0, |pixel, &shift| {
        let (x, y) = (channel(a, shift), channel(b, shift));
        pixel | ((x + (x - y) / 2).clamp(0, 255) as u32) << shift
    })
}

fn color_delta(transform: u8, value: u8) -> i32 {
    (i32::from(transform as i8) * i32::from(value as i8)) >> 5
}

impl Transform {
    fn undo(&self, pixels: &mut Vec<u32>, width: u32, height: u32) {
        let (width_usize, height_usize) = (width as usize, height as usize);
        match self {
            Transform::SubtractGreen => {
                for pixel in pixels.iter_mut() {
                    let green = (*pixel >> 8) & 0xff;
                    let red_blue = (*pixel & 0x00ff_00ff).wrapping_add((green << 16) | green);
                    *pixel = (*pixel & 0xff00_ff00) | (red_blue & 0x00ff_00ff);
                }
            }
            Transform::Color(bits, elements) => {
                let blocks = subsample(width, *bits) as usize;
                for y in 0..height_usize {
                    for x in 0..width_usize {
                        let element = elements[(y >> bits) * blocks + (x >> bits)];
                        let (green_to_red, green_to_blue, red_to_blue) =
                            (element as u8, (element >> 8) as u8, (element >> 16) as u8);
                        let pixel = &mut pixels[y * width_usize + x];
                        let green = (*pixel >> 8) as u8;
                        let red = ((((*pixel >> 16) & 0xff) as i32
                            + color_delta(green_to_red, green))
                            & 0xff) as u32;
                        let blue = (((*pixel & 0xff) as i32
                            + color_delta(green_to_blue, green)
                            + color_delta(red_to_blue, red as u8))
                            & 0xff) as u32;
                        *pixel = (*pixel & 0xff00_ff00) | (red << 16) | blue;
                    }
                }
            }
            Transform::Predictor(bits, modes) => {
                let blocks = subsample(width, *bits) as usize;
                for y in 0..height_usize {
                    for x in 0..width_usize {
                        let at = y * width_usize + x;
                        let predicted = if y == 0 {
                            if x == 0 { 0xff00_0000 } else { pixels[at - 1] }
                        } else if x == 0 {
                            pixels[at - width_usize]
                        } else {
                            let mode = (modes[(y >> bits) * blocks + (x >> bits)] >> 8) & 0xf;
                            let (left, top, top_left) = (
                                pixels[at - 1],
                                pixels[at - width_usize],
                                pixels[at - width_usize - 1],
                            );
                            // the top right of the last column is the first pixel of this row
                            let top_right = pixels[at - width_usize + 1];
                            match mode {
                                0 => 0xff00_0000,
                                1 => left,
                                2 => top,
                                3 => top_right,
                                4 => top_left,
                                5 => average(average(left, top_right), top),
                                6 => average(left, top_left),
                                7 => average(left, top),
                                8 => average(top_left, top),
                                9 => average(top, top_right),
                                10 => average(average(left, top_left), average(top, top_right)),
                                11 => select(top, left, top_left),
                                12 => clamp_add_subtract_full(left, top, top_left),
                                13 => clamp_add_subtract_half(average(left, top), top_left),
                                _ => 0xff00_0000,
                            }
                        };
                        pixels[at] = add_pixels(pixels[at], predicted);
                    }
                }
            }
            Transform::Indexing(palette, bits) => {
                let packed_width = subsample(width, *bits) as usize;
                let per_byte = 1 << bits;
                let bits_per_pixel = 8 >> bits;
                let mask = (1 << bits_per_pixel) - 1;
                let mut out = Vec::with_capacity(width_usize * height_usize);
                for y in 0..height_usize {
                    for x in 0..width_usize {
                        let packed = (pixels[y * packed_width + x / per_byte] >> 8) & 0xff;
                        let index = (packed >> ((x % per_byte) * bits_per_pixel)) & mask;
                        out.push(palette.get(index as usize).copied().unwrap_or(0));
                    }
                }
                *pixels = out;
            }
        }
    }
}

// an image stream: transforms and the color cache and meta codes at the top level, then the
// pixels; returns ARGB words
pub(super) fn decode_stream(
    reader: &mut BitReader,
    width: u32,
    height: u32,
    top: bool,
) -> Option<Vec<u32>> {
    // each transform with the width it works at: the width before it, which color indexing
    // packs for the transforms after it
    let mut transforms: Vec<(Transform, u32)> = Vec::new();
    let mut coded_width = width;
    if top {
        let mut seen = [false; 4];
        while reader.read(1) == 1 {
            let kind = reader.read(2) as usize;
            if std::mem::replace(&mut seen[kind], true) {
                return None;
            }
            let width_before = coded_width;
            let transform = match kind {
                0 | 1 => {
                    let bits = reader.read(3) + 2;
                    let data = decode_stream(
                        reader,
                        subsample(coded_width, bits),
                        subsample(height, bits),
                        false,
                    )?;
                    if kind == 0 {
                        Transform::Predictor(bits, data)
                    } else {
                        Transform::Color(bits, data)
                    }
                }
                2 => Transform::SubtractGreen,
                _ => {
                    let size = reader.read(8) + 1;
                    let mut palette = decode_stream(reader, size, 1, false)?;
                    for index in 1..palette.len() {
                        palette[index] = add_pixels(palette[index], palette[index - 1]);
                    }
                    let bits = match size {
                        0..=2 => 3,
                        3..=4 => 2,
                        5..=16 => 1,
                        _ => 0,
                    };
                    coded_width = subsample(coded_width, bits);
                    Transform::Indexing(palette, bits)
                }
            };
            transforms.push((transform, width_before));
        }
    }
    let cache_bits = if reader.read(1) == 1 {
        let bits = reader.read(4);
        if !(1..=11).contains(&bits) {
            return None;
        }
        bits
    } else {
        0
    };
    let cache_size = if cache_bits > 0 { 1 << cache_bits } else { 0 };
    let (meta_bits, meta) = if top && reader.read(1) == 1 {
        let bits = reader.read(3) + 2;
        (
            bits,
            Some(decode_stream(
                reader,
                subsample(coded_width, bits),
                subsample(height, bits),
                false,
            )?),
        )
    } else {
        (0, None)
    };
    let groups = meta.as_ref().map_or(1, |meta| {
        meta.iter()
            .map(|&value| ((value >> 8) & 0xffff) as usize)
            .max()
            .unwrap_or(0)
            + 1
    });
    let mut codes = Vec::with_capacity(groups);
    for _ in 0..groups {
        let alphabets = [256 + 24 + cache_size, 256, 256, 256, 40];
        let mut group = Vec::with_capacity(5);
        for alphabet in alphabets {
            group.push(read_code(reader, alphabet)?);
        }
        codes.push(group);
    }
    let total = coded_width as usize * height as usize;
    let mut pixels: Vec<u32> = Vec::with_capacity(total);
    let mut cache = vec![0_u32; cache_size];
    let mut cached = 0;
    let meta_width = subsample(coded_width, meta_bits) as usize;
    let stride = coded_width as usize;
    while pixels.len() < total {
        let at = pixels.len();
        let group = match &meta {
            Some(meta) => {
                &codes[((meta
                    [((at / stride) >> meta_bits) * meta_width + ((at % stride) >> meta_bits)]
                    >> 8)
                    & 0xffff) as usize]
            }
            None => &codes[0],
        };
        let green = u32::from(group[0].read(reader));
        if green < 256 {
            let red = u32::from(group[1].read(reader));
            let blue = u32::from(group[2].read(reader));
            let alpha = u32::from(group[3].read(reader));
            pixels.push((alpha << 24) | (red << 16) | (green << 8) | blue);
        } else if green < 256 + 24 {
            let length = prefix_value(reader, green - 256) as usize;
            let distance_symbol = u32::from(group[4].read(reader));
            let code = prefix_value(reader, distance_symbol) as usize;
            let distance = if code > 120 {
                code - 120
            } else {
                let (x, y) = DISTANCE_MAP[code - 1];
                (i64::from(x) + i64::from(y) * stride as i64).max(1) as usize
            };
            if distance > at || at + length > total {
                return None;
            }
            for index in 0..length {
                let pixel = pixels[at + index - distance];
                pixels.push(pixel);
            }
        } else {
            let index = (green - 256 - 24) as usize;
            pixels.push(*cache.get(index)?);
        }
        if cache_bits > 0 {
            for &pixel in &pixels[cached..] {
                cache[(0x1e35_a7bd_u32.wrapping_mul(pixel) >> (32 - cache_bits)) as usize] = pixel;
            }
        }
        cached = pixels.len();
        if reader.overrun() {
            return None;
        }
    }
    for (transform, width) in transforms.iter().rev() {
        transform.undo(&mut pixels, *width, height);
    }
    Some(pixels)
}

// a whole VP8L chunk: its header, then the top-level image stream; RGBA bytes
pub(super) fn decode(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    if bytes.first() != Some(&0x2f) {
        return None;
    }
    let mut reader = BitReader::new(&bytes[1..]);
    let width = reader.read(14) + 1;
    let height = reader.read(14) + 1;
    let _alpha = reader.read(1);
    if reader.read(3) != 0 {
        return None;
    }
    crate::image::check_dimensions(width, height).ok()?;
    let pixels = decode_stream(&mut reader, width, height, true)?;
    Some((width, height, argb_to_rgba(&pixels)))
}

pub(super) fn argb_to_rgba(pixels: &[u32]) -> Vec<u8> {
    pixels
        .iter()
        .flat_map(|&pixel| {
            [
                (pixel >> 16) as u8,
                (pixel >> 8) as u8,
                pixel as u8,
                (pixel >> 24) as u8,
            ]
        })
        .collect()
}

struct BitWriter {
    bytes: Vec<u8>,
    bits: u64,
    count: u32,
}

impl BitWriter {
    fn write(&mut self, value: u32, count: u32) {
        self.bits |= u64::from(value) << self.count;
        self.count += count;
        while self.count >= 8 {
            self.bytes.push(self.bits as u8);
            self.bits >>= 8;
            self.count -= 8;
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.count > 0 {
            self.bytes.push(self.bits as u8);
        }
        self.bytes
    }
}

// the prefix code for an alphabet's symbol counts, written as a normal code, with the codes
// to write each symbol by
fn write_code(writer: &mut BitWriter, frequencies: &[u32]) -> Vec<(u16, u8)> {
    let lengths = crate::deflate::code_lengths(frequencies, 15);
    let runs = crate::deflate::run_lengths(&lengths);
    let mut length_frequencies = [0_u32; 19];
    for &(symbol, _) in &runs {
        length_frequencies[usize::from(symbol)] += 1;
    }
    let length_lengths = crate::deflate::code_lengths(&length_frequencies, 7);
    let length_codes = crate::deflate::canonical(&length_lengths);
    let count = CODE_LENGTH_ORDER
        .iter()
        .rposition(|&symbol| length_lengths[symbol] > 0)
        .map_or(4, |last| (last + 1).max(4));
    writer.write(0, 1);
    writer.write((count - 4) as u32, 4);
    for &symbol in &CODE_LENGTH_ORDER[..count] {
        writer.write(u32::from(length_lengths[symbol]), 3);
    }
    writer.write(0, 1);
    for &(symbol, extra) in &runs {
        let (code, length) = length_codes[usize::from(symbol)];
        writer.write(reverse(code, length), u32::from(length));
        match symbol {
            16 => writer.write(u32::from(extra), 2),
            17 => writer.write(u32::from(extra), 3),
            18 => writer.write(u32::from(extra), 7),
            _ => {}
        }
    }
    crate::deflate::canonical(&lengths)
}

fn reverse(code: u16, length: u8) -> u32 {
    if length == 0 {
        0
    } else {
        u32::from(code).reverse_bits() >> (32 - u32::from(length))
    }
}

// a value of one or more as a prefix symbol and its extra bits
fn prefix_encode(value: u32) -> (u32, u32, u32) {
    let rest = value - 1;
    if rest < 4 {
        return (rest, 0, 0);
    }
    let high = 31 - rest.leading_zeros();
    let second = (rest >> (high - 1)) & 1;
    let extra = high - 1;
    (2 * high + second, extra, rest & ((1 << extra) - 1))
}

enum Token {
    Literal(u32),
    // a run of pixels and its distance code
    Copy(u32, u32),
}

// a VP8L chunk for RGBA pixels: subtract green, then literals and copies of runs that repeat
// the pixel to the left or the row above
pub(super) fn encode(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut pixels: Vec<u32> = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .map(|pixel| {
            (u32::from(pixel[3]) << 24)
                | (u32::from(pixel[0]) << 16)
                | (u32::from(pixel[1]) << 8)
                | u32::from(pixel[2])
        })
        .collect();
    for pixel in &mut pixels {
        // each channel on its own, so a blue below green borrows nothing from red
        let green = (*pixel >> 8) & 0xff;
        let red = ((*pixel >> 16) & 0xff).wrapping_sub(green) & 0xff;
        let blue = (*pixel & 0xff).wrapping_sub(green) & 0xff;
        *pixel = (*pixel & 0xff00_ff00) | (red << 16) | blue;
    }
    let stride = width as usize;
    let mut tokens = Vec::new();
    let mut at = 0;
    while at < pixels.len() {
        let run = |distance: usize| -> usize {
            if distance > at {
                return 0;
            }
            pixels[at..]
                .iter()
                .zip(&pixels[at - distance..])
                .take(4096)
                .take_while(|(a, b)| a == b)
                .count()
        };
        let (left, above) = (run(1), run(stride));
        let (length, code) = if above >= left { (above, 1) } else { (left, 2) };
        if length >= 3 {
            tokens.push(Token::Copy(length as u32, code));
            at += length;
        } else {
            tokens.push(Token::Literal(pixels[at]));
            at += 1;
        }
    }
    let mut frequencies = [
        vec![0_u32; 256 + 24],
        vec![0; 256],
        vec![0; 256],
        vec![0; 256],
        vec![0; 40],
    ];
    for token in &tokens {
        match *token {
            Token::Literal(pixel) => {
                frequencies[0][((pixel >> 8) & 0xff) as usize] += 1;
                frequencies[1][((pixel >> 16) & 0xff) as usize] += 1;
                frequencies[2][(pixel & 0xff) as usize] += 1;
                frequencies[3][(pixel >> 24) as usize] += 1;
            }
            Token::Copy(length, code) => {
                frequencies[0][256 + prefix_encode(length).0 as usize] += 1;
                frequencies[4][prefix_encode(code).0 as usize] += 1;
            }
        }
    }
    let alpha = pixels.iter().any(|&pixel| pixel >> 24 != 0xff);
    let mut writer = BitWriter {
        bytes: vec![0x2f],
        bits: 0,
        count: 0,
    };
    writer.write(width - 1, 14);
    writer.write(height - 1, 14);
    writer.write(u32::from(alpha), 1);
    writer.write(0, 3);
    // one transform, subtract green, then no color cache and no meta codes
    writer.write(1, 1);
    writer.write(2, 2);
    writer.write(0, 1);
    writer.write(0, 1);
    writer.write(0, 1);
    let codes: Vec<Vec<(u16, u8)>> = frequencies
        .iter()
        .map(|counts| write_code(&mut writer, counts))
        .collect();
    let put = |writer: &mut BitWriter, alphabet: usize, symbol: usize| {
        let (code, length) = codes[alphabet][symbol];
        writer.write(reverse(code, length), u32::from(length));
    };
    for token in &tokens {
        match *token {
            Token::Literal(pixel) => {
                put(&mut writer, 0, ((pixel >> 8) & 0xff) as usize);
                put(&mut writer, 1, ((pixel >> 16) & 0xff) as usize);
                put(&mut writer, 2, (pixel & 0xff) as usize);
                put(&mut writer, 3, (pixel >> 24) as usize);
            }
            Token::Copy(length, code) => {
                let (symbol, extra, value) = prefix_encode(length);
                put(&mut writer, 0, 256 + symbol as usize);
                writer.write(value, extra);
                let (symbol, extra, value) = prefix_encode(code);
                put(&mut writer, 4, symbol as usize);
                writer.write(value, extra);
            }
        }
    }
    writer.finish()
}
