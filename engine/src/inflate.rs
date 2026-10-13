// zlib and deflate decoding (RFC 1950 and RFC 1951) with a hard output limit,
// so a small compressed stream cannot expand into unbounded memory

use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum InflateError {
    Truncated,
    Invalid(&'static str),
    TooLarge,
}

impl fmt::Display for InflateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => formatter.write_str("compressed data ends early"),
            Self::Invalid(detail) => write!(formatter, "invalid compressed data: {detail}"),
            Self::TooLarge => formatter.write_str("decompressed data exceeds its limit"),
        }
    }
}

const FAST_BITS: u32 = 10;
const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DISTANCE_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DISTANCE_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
const CODE_LENGTH_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

struct Bits<'a> {
    data: &'a [u8],
    position: usize,
    buffer: u64,
    count: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            position: 0,
            buffer: 0,
            count: 0,
        }
    }

    fn refill(&mut self) {
        while self.count <= 56 {
            let Some(&byte) = self.data.get(self.position) else {
                break;
            };
            self.buffer |= u64::from(byte) << self.count;
            self.position += 1;
            self.count += 8;
        }
    }

    fn take(&mut self, bits: u32) -> Result<u32, InflateError> {
        if bits == 0 {
            return Ok(0);
        }
        if self.count < bits {
            self.refill();
            if self.count < bits {
                return Err(InflateError::Truncated);
            }
        }
        let value = (self.buffer & ((1 << bits) - 1)) as u32;
        self.buffer >>= bits;
        self.count -= bits;
        Ok(value)
    }

    fn align_to_byte(&mut self) {
        let partial = self.count % 8;
        self.buffer >>= partial;
        self.count -= partial;
    }

    // bytes already pulled into the bit buffer are returned before the slice
    fn copy_bytes(&mut self, length: usize, output: &mut Vec<u8>) -> Result<(), InflateError> {
        let mut remaining = length;
        while remaining > 0 && self.count >= 8 {
            output.push(self.take(8)? as u8);
            remaining -= 1;
        }
        let end = self
            .position
            .checked_add(remaining)
            .filter(|end| *end <= self.data.len())
            .ok_or(InflateError::Truncated)?;
        output.extend_from_slice(&self.data[self.position..end]);
        self.position = end;
        Ok(())
    }

    fn consumed(&self) -> usize {
        self.position - (self.count / 8) as usize
    }
}

struct Huffman {
    counts: [u16; 16],
    symbols: Vec<u16>,
    // symbol << 4 | length for codes up to FAST_BITS long, zero otherwise
    fast: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Result<Self, InflateError> {
        let mut counts = [0_u16; 16];
        for &length in lengths {
            counts[usize::from(length)] += 1;
        }
        counts[0] = 0;
        let mut left = 1_i32;
        for &count in &counts[1..] {
            left = (left << 1) - i32::from(count);
            if left < 0 {
                return Err(InflateError::Invalid("over-subscribed code"));
            }
        }
        let mut offsets = [0_u16; 16];
        for length in 1..15 {
            offsets[length + 1] = offsets[length] + counts[length];
        }
        let mut symbols = vec![0_u16; lengths.len()];
        for (symbol, &length) in lengths.iter().enumerate() {
            if length != 0 {
                symbols[usize::from(offsets[usize::from(length)])] = symbol as u16;
                offsets[usize::from(length)] += 1;
            }
        }
        let mut fast = vec![0_u16; 1 << FAST_BITS];
        let mut code = 0_u32;
        let mut index = 0;
        for length in 1..=FAST_BITS {
            for _ in 0..counts[length as usize] {
                let reversed = reverse_bits(code, length);
                let entry = (symbols[index] << 4) | length as u16;
                let mut slot = reversed;
                while slot < (1 << FAST_BITS) {
                    fast[slot as usize] = entry;
                    slot += 1 << length;
                }
                code += 1;
                index += 1;
            }
            code <<= 1;
        }
        Ok(Self {
            counts,
            symbols,
            fast,
        })
    }

    fn decode(&self, bits: &mut Bits<'_>) -> Result<u16, InflateError> {
        if bits.count < 16 {
            bits.refill();
        }
        let entry = self.fast[(bits.buffer & ((1 << FAST_BITS) - 1)) as usize];
        if entry != 0 {
            let length = u32::from(entry & 15);
            if bits.count < length {
                return Err(InflateError::Truncated);
            }
            bits.buffer >>= length;
            bits.count -= length;
            return Ok(entry >> 4);
        }
        // canonical decoding one bit at a time for codes longer than the table
        let mut code = 0_i32;
        let mut first = 0_i32;
        let mut index = 0_i32;
        for length in 1..16 {
            code |= bits.take(1)? as i32;
            let count = i32::from(self.counts[length]);
            if code - count < first {
                return Ok(self.symbols[(index + code - first) as usize]);
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err(InflateError::Invalid("unknown code"))
    }
}

fn reverse_bits(code: u32, length: u32) -> u32 {
    code.reverse_bits() >> (32 - length)
}

fn fixed_tables() -> Result<(Huffman, Huffman), InflateError> {
    let mut lengths = [0_u8; 288];
    lengths[..144].fill(8);
    lengths[144..256].fill(9);
    lengths[256..280].fill(7);
    lengths[280..].fill(8);
    Ok((Huffman::new(&lengths)?, Huffman::new(&[5; 30])?))
}

fn dynamic_tables(bits: &mut Bits<'_>) -> Result<(Huffman, Huffman), InflateError> {
    let literal_count = bits.take(5)? as usize + 257;
    let distance_count = bits.take(5)? as usize + 1;
    let code_count = bits.take(4)? as usize + 4;
    if literal_count > 286 || distance_count > 30 {
        return Err(InflateError::Invalid("too many codes"));
    }
    let mut code_lengths = [0_u8; 19];
    for &slot in &CODE_LENGTH_ORDER[..code_count] {
        code_lengths[slot] = bits.take(3)? as u8;
    }
    let code_table = Huffman::new(&code_lengths)?;
    let mut lengths = vec![0_u8; literal_count + distance_count];
    let mut index = 0;
    while index < lengths.len() {
        let symbol = code_table.decode(bits)?;
        let (value, repeat) = match symbol {
            0..=15 => (symbol as u8, 1),
            16 => {
                let previous = *index
                    .checked_sub(1)
                    .and_then(|previous| lengths.get(previous))
                    .ok_or(InflateError::Invalid("repeat with no previous length"))?;
                (previous, 3 + bits.take(2)? as usize)
            }
            17 => (0, 3 + bits.take(3)? as usize),
            18 => (0, 11 + bits.take(7)? as usize),
            _ => return Err(InflateError::Invalid("bad code length symbol")),
        };
        if index + repeat > lengths.len() {
            return Err(InflateError::Invalid("code lengths overflow"));
        }
        lengths[index..index + repeat].fill(value);
        index += repeat;
    }
    if lengths[256] == 0 {
        return Err(InflateError::Invalid("missing end of block code"));
    }
    Ok((
        Huffman::new(&lengths[..literal_count])?,
        Huffman::new(&lengths[literal_count..])?,
    ))
}

fn inflate_block(
    bits: &mut Bits<'_>,
    literals: &Huffman,
    distances: &Huffman,
    output: &mut Vec<u8>,
    limit: usize,
) -> Result<(), InflateError> {
    loop {
        let symbol = literals.decode(bits)?;
        match symbol {
            0..=255 => {
                if output.len() >= limit {
                    return Err(InflateError::TooLarge);
                }
                output.push(symbol as u8);
            }
            256 => return Ok(()),
            257..=285 => {
                let slot = usize::from(symbol - 257);
                let length = usize::from(LENGTH_BASE[slot])
                    + bits.take(u32::from(LENGTH_EXTRA[slot]))? as usize;
                let distance_symbol = usize::from(distances.decode(bits)?);
                if distance_symbol >= 30 {
                    return Err(InflateError::Invalid("bad distance symbol"));
                }
                let distance = usize::from(DISTANCE_BASE[distance_symbol])
                    + bits.take(u32::from(DISTANCE_EXTRA[distance_symbol]))? as usize;
                if distance > output.len() {
                    return Err(InflateError::Invalid("distance before the start"));
                }
                if output.len() + length > limit {
                    return Err(InflateError::TooLarge);
                }
                let start = output.len() - distance;
                if distance >= length {
                    output.extend_from_within(start..start + length);
                } else {
                    for index in 0..length {
                        output.push(output[start + index]);
                    }
                }
            }
            _ => return Err(InflateError::Invalid("bad length symbol")),
        }
    }
}

// raw deflate data; returns the output and how many input bytes the stream used
pub(crate) fn inflate(data: &[u8], limit: usize) -> Result<(Vec<u8>, usize), InflateError> {
    let mut bits = Bits::new(data);
    let mut output = Vec::new();
    let mut fixed = None;
    loop {
        let last = bits.take(1)? == 1;
        match bits.take(2)? {
            0 => {
                bits.align_to_byte();
                let length = bits.take(16)?;
                let complement = bits.take(16)?;
                if length != !complement & 0xffff {
                    return Err(InflateError::Invalid("stored length mismatch"));
                }
                if output.len() + length as usize > limit {
                    return Err(InflateError::TooLarge);
                }
                bits.copy_bytes(length as usize, &mut output)?;
            }
            1 => {
                if fixed.is_none() {
                    fixed = Some(fixed_tables()?);
                }
                let (literals, distances) = fixed.as_ref().expect("fixed tables were built");
                inflate_block(&mut bits, literals, distances, &mut output, limit)?;
            }
            2 => {
                let (literals, distances) = dynamic_tables(&mut bits)?;
                inflate_block(&mut bits, &literals, &distances, &mut output, limit)?;
            }
            _ => return Err(InflateError::Invalid("reserved block type")),
        }
        if last {
            bits.align_to_byte();
            return Ok((output, bits.consumed()));
        }
    }
}

pub(crate) fn zlib_decompress(data: &[u8], limit: usize) -> Result<Vec<u8>, InflateError> {
    let [method, flags, ..] = *data else {
        return Err(InflateError::Truncated);
    };
    if method & 0x0f != 8
        || method >> 4 > 7
        || (u16::from(method) << 8 | u16::from(flags)) % 31 != 0
    {
        return Err(InflateError::Invalid("bad zlib header"));
    }
    if flags & 0x20 != 0 {
        return Err(InflateError::Invalid(
            "preset dictionaries are not supported",
        ));
    }
    let (output, used) = inflate(&data[2..], limit)?;
    let trailer = data
        .get(2 + used..2 + used + 4)
        .ok_or(InflateError::Truncated)?;
    let expected = u32::from_be_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]);
    if adler32(&output) != expected {
        return Err(InflateError::Invalid("checksum mismatch"));
    }
    Ok(output)
}

fn adler32(data: &[u8]) -> u32 {
    let (mut low, mut high) = (1_u32, 0_u32);
    for chunk in data.chunks(5552) {
        for &byte in chunk {
            low += u32::from(byte);
            high += low;
        }
        low %= 65521;
        high %= 65521;
    }
    (high << 16) | low
}

#[cfg(test)]
mod tests {
    use super::*;

    // produced by zlib at level 1, which emits a fixed-Huffman block
    const HELLO_FIXED: &[u8] = &[
        0x78, 0x01, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0x40, 0x22, 0x01, 0x3a, 0x2e, 0x06, 0x7d,
    ];

    #[test]
    fn inflates_stored_blocks() {
        let stream = [
            0x78, 0x01, 0x01, 0x05, 0x00, 0xfa, 0xff, 0x68, 0x65, 0x6c, 0x6c, 0x6f, 0x06, 0x2c,
            0x02, 0x15,
        ];
        assert_eq!(zlib_decompress(&stream, 64).unwrap(), b"hello");
    }

    #[test]
    fn inflates_fixed_huffman_with_back_references() {
        assert_eq!(
            zlib_decompress(HELLO_FIXED, 64).unwrap(),
            b"hello hello hello"
        );
    }

    #[test]
    fn inflates_dynamic_huffman_blocks() {
        let compressed = include_bytes!("../tests/fixtures/words.zlib");
        let expected = include_bytes!("../tests/fixtures/words.txt");
        assert_eq!(
            zlib_decompress(compressed, expected.len()).unwrap(),
            expected
        );
    }

    #[test]
    fn rejects_bad_checksums_truncation_and_oversized_output() {
        let mut corrupt = HELLO_FIXED.to_vec();
        *corrupt.last_mut().unwrap() ^= 1;
        assert_eq!(
            zlib_decompress(&corrupt, 64),
            Err(InflateError::Invalid("checksum mismatch"))
        );
        for end in 0..HELLO_FIXED.len() {
            assert!(zlib_decompress(&HELLO_FIXED[..end], 64).is_err());
        }
        assert_eq!(zlib_decompress(HELLO_FIXED, 8), Err(InflateError::TooLarge));
    }
}
