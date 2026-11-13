// zlib streams at about zlib's default level: hash-chained LZ77 with one step of lazy
// matching, and each block coded as the cheapest of stored, fixed and dynamic Huffman

use crate::inflate::{
    CODE_LENGTH_ORDER, DISTANCE_BASE, DISTANCE_EXTRA, LENGTH_BASE, LENGTH_EXTRA, adler32,
};

const WINDOW: usize = 32 * 1024;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
const HASH_BITS: u32 = 15;
// zlib's level 6 tuning
const MAX_CHAIN: usize = 128;
const GOOD_LENGTH: usize = 8;
const LAZY_LENGTH: usize = 16;
const NICE_LENGTH: usize = 128;
const TOO_FAR: usize = 4096;
const BLOCK_SYMBOLS: usize = 16 * 1024 - 1;

#[derive(Clone, Copy)]
enum Symbol {
    Literal(u8),
    Match { length: u16, distance: u16 },
}

#[derive(Default)]
struct Writer {
    bytes: Vec<u8>,
    bits: u64,
    count: u32,
}

impl Writer {
    fn put(&mut self, value: u32, length: u8) {
        self.bits |= u64::from(value) << self.count;
        self.count += u32::from(length);
        while self.count >= 8 {
            self.bytes.push(self.bits as u8);
            self.bits >>= 8;
            self.count -= 8;
        }
    }

    // Huffman codes go out from their most significant bit
    fn code(&mut self, (code, length): (u16, u8)) {
        let reversed = u32::from(code.reverse_bits()) >> (16 - u32::from(length.max(1)));
        self.put(reversed, length);
    }

    fn align(&mut self) {
        if self.count > 0 {
            self.bytes.push(self.bits as u8);
            self.bits = 0;
            self.count = 0;
        }
    }
}

pub fn zlib_compress(data: &[u8]) -> Vec<u8> {
    let mut writer = Writer::default();
    writer.bytes.extend_from_slice(&[0x78, 0x9c]);
    let mut block = Vec::with_capacity(BLOCK_SYMBOLS);
    let mut block_start = 0;
    parse(data, |symbol, position| {
        block.push(symbol);
        if block.len() == BLOCK_SYMBOLS {
            write_block(&mut writer, &block, &data[block_start..position], false);
            block.clear();
            block_start = position;
        }
    });
    write_block(&mut writer, &block, &data[block_start..], true);
    writer.align();
    writer.bytes.extend_from_slice(&adler32(data).to_be_bytes());
    writer.bytes
}

// a gzip member (RFC 1952) around the same deflate stream, with no name or timestamp
pub fn gzip(data: &[u8]) -> Vec<u8> {
    let zlib = zlib_compress(data);
    let mut member = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3];
    member.extend_from_slice(&zlib[2..zlib.len() - 4]);
    member.extend(crate::png::crc32(data).to_le_bytes());
    member.extend((data.len() as u32).to_le_bytes());
    member
}

fn hash(data: &[u8], at: usize) -> usize {
    let word = u32::from_le_bytes([data[at], data[at + 1], data[at + 2], 0]);
    (word.wrapping_mul(0x9e37_79b1) >> (32 - HASH_BITS)) as usize
}

// emits symbols in order, each with the input position just past what it covers
fn parse(data: &[u8], mut emit: impl FnMut(Symbol, usize)) {
    let mut head = vec![usize::MAX; 1 << HASH_BITS];
    let mut previous = vec![usize::MAX; WINDOW];
    let mut insert = |previous: &mut [usize], at: usize| -> usize {
        let slot = hash(data, at);
        let candidate = head[slot];
        previous[at % WINDOW] = candidate;
        head[slot] = at;
        candidate
    };
    let mut at = 0;
    // a match found at the position before this one, waiting to see whether this one does better
    let mut waiting: Option<(usize, usize)> = None;
    while at < data.len() {
        let current = if at + MIN_MATCH <= data.len() {
            let candidate = insert(&mut previous, at);
            match waiting {
                Some((length, _)) if length >= LAZY_LENGTH => None,
                _ => longest_match(
                    data,
                    at,
                    candidate,
                    &previous,
                    waiting.map_or(MIN_MATCH - 1, |(length, _)| length),
                ),
            }
        } else {
            None
        };
        match (waiting, current) {
            (Some((length, distance)), None) => {
                let end = at - 1 + length;
                emit(
                    Symbol::Match {
                        length: length as u16,
                        distance: distance as u16,
                    },
                    end,
                );
                for skipped in at + 1..end {
                    if skipped + MIN_MATCH <= data.len() {
                        insert(&mut previous, skipped);
                    }
                }
                at = end;
                waiting = None;
            }
            (Some(_), Some(found)) => {
                emit(Symbol::Literal(data[at - 1]), at);
                waiting = Some(found);
                at += 1;
            }
            (None, found) => {
                if found.is_none() {
                    emit(Symbol::Literal(data[at]), at + 1);
                }
                waiting = found;
                at += 1;
            }
        }
    }
}

// the longest earlier match for the bytes at `at` that beats `floor`, as (length, distance)
fn longest_match(
    data: &[u8],
    at: usize,
    mut candidate: usize,
    previous: &[usize],
    floor: usize,
) -> Option<(usize, usize)> {
    let limit = (data.len() - at).min(MAX_MATCH);
    let mut best = (floor, 0);
    let mut chain = if floor >= GOOD_LENGTH {
        MAX_CHAIN / 4
    } else {
        MAX_CHAIN
    };
    while candidate != usize::MAX && candidate < at && at - candidate < WINDOW && chain > 0 {
        if best.0 < limit && data[candidate + best.0] == data[at + best.0] {
            let length = data[candidate..candidate + limit]
                .iter()
                .zip(&data[at..at + limit])
                .take_while(|(a, b)| a == b)
                .count();
            if length > best.0 {
                best = (length, at - candidate);
                if length >= NICE_LENGTH.min(limit) {
                    break;
                }
            }
        }
        let next = previous[candidate % WINDOW];
        if next == usize::MAX || next >= candidate {
            break;
        }
        candidate = next;
        chain -= 1;
    }
    let (length, distance) = best;
    (length > floor && length >= MIN_MATCH && !(length == MIN_MATCH && distance > TOO_FAR))
        .then_some(best)
}

fn length_code(length: usize) -> usize {
    LENGTH_BASE.partition_point(|&base| usize::from(base) <= length) - 1
}

fn distance_code(distance: usize) -> usize {
    DISTANCE_BASE.partition_point(|&base| usize::from(base) <= distance) - 1
}

// code lengths no longer than `limit` for the given frequencies; any alphabet in use gets
// at least two codes, which every decoder accepts
fn code_lengths(frequencies: &[u32], limit: u8) -> Vec<u8> {
    let mut weights = frequencies.to_vec();
    loop {
        let mut used = weights
            .iter()
            .enumerate()
            .filter(|(_, weight)| **weight > 0)
            .map(|(symbol, &weight)| (weight, symbol))
            .collect::<Vec<_>>();
        let mut lengths = vec![0_u8; weights.len()];
        if used.len() < 2 {
            let first = used.first().map_or(0, |&(_, symbol)| symbol);
            lengths[first] = 1;
            lengths[usize::from(first == 0)] = 1;
            return lengths;
        }
        used.sort_unstable();
        let leaves = used.len();
        // leaves first in ascending weight, then the internal nodes as they are made
        let mut weight = used
            .iter()
            .map(|&(weight, _)| u64::from(weight))
            .collect::<Vec<_>>();
        let mut parent = vec![0_usize; 2 * leaves - 1];
        let (mut next_leaf, mut next_node) = (0, leaves);
        for node in leaves..2 * leaves - 1 {
            let mut take = || {
                if next_leaf < leaves
                    && (next_node >= node || weight[next_leaf] <= weight[next_node])
                {
                    next_leaf += 1;
                    next_leaf - 1
                } else {
                    next_node += 1;
                    next_node - 1
                }
            };
            let (first, second) = (take(), take());
            weight.push(weight[first] + weight[second]);
            parent[first] = node;
            parent[second] = node;
        }
        let mut depth = vec![0_u8; 2 * leaves - 1];
        for node in (0..2 * leaves - 2).rev() {
            depth[node] = depth[parent[node]] + 1;
        }
        if depth[..leaves].iter().all(|&depth| depth <= limit) {
            for (index, &(_, symbol)) in used.iter().enumerate() {
                lengths[symbol] = depth[index];
            }
            return lengths;
        }
        // flattening the weights shortens the deepest codes until they fit
        for weight in weights.iter_mut().filter(|weight| **weight > 0) {
            *weight = (*weight >> 1).max(1);
        }
    }
}

// canonical codes for the lengths, per RFC 1951 section 3.2.2
fn canonical(lengths: &[u8]) -> Vec<(u16, u8)> {
    let mut counts = [0_u16; 16];
    for &length in lengths.iter().filter(|&&length| length > 0) {
        counts[usize::from(length)] += 1;
    }
    let mut next = [0_u16; 16];
    let mut code = 0_u16;
    for bits in 1..16 {
        code = (code + counts[bits - 1]) << 1;
        next[bits] = code;
    }
    lengths
        .iter()
        .map(|&length| {
            if length == 0 {
                return (0, 0);
            }
            let code = next[usize::from(length)];
            next[usize::from(length)] += 1;
            (code, length)
        })
        .collect()
}

// the code length sequence with runs folded into codes 16, 17 and 18, as (symbol, extra)
fn run_lengths(lengths: &[u8]) -> Vec<(u8, u8)> {
    let mut runs = Vec::new();
    let mut index = 0;
    while index < lengths.len() {
        let value = lengths[index];
        let mut run = lengths[index..]
            .iter()
            .take_while(|&&length| length == value)
            .count();
        index += run;
        if value == 0 {
            while run >= 11 {
                let take = run.min(138);
                runs.push((18, (take - 11) as u8));
                run -= take;
            }
            if run >= 3 {
                runs.push((17, (run - 3) as u8));
                run = 0;
            }
        } else {
            runs.push((value, 0));
            run -= 1;
            while run >= 3 {
                let take = run.min(6);
                runs.push((16, (take - 3) as u8));
                run -= take;
            }
        }
        runs.extend(std::iter::repeat_n((value, 0), run));
    }
    runs
}

fn fixed_lengths() -> (Vec<u8>, Vec<u8>) {
    let literals = (0..288)
        .map(|symbol| match symbol {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        })
        .collect();
    (literals, vec![5; 30])
}

fn write_block(writer: &mut Writer, symbols: &[Symbol], raw: &[u8], last: bool) {
    let mut literal_counts = vec![0_u32; 286];
    let mut distance_counts = vec![0_u32; 30];
    literal_counts[256] = 1;
    let mut extra_bits = 0_u64;
    for symbol in symbols {
        match *symbol {
            Symbol::Literal(byte) => literal_counts[usize::from(byte)] += 1,
            Symbol::Match { length, distance } => {
                let length_index = length_code(usize::from(length));
                let distance_index = distance_code(usize::from(distance));
                literal_counts[257 + length_index] += 1;
                distance_counts[distance_index] += 1;
                extra_bits +=
                    u64::from(LENGTH_EXTRA[length_index] + DISTANCE_EXTRA[distance_index]);
            }
        }
    }
    let cost = |literals: &[u8], distances: &[u8]| {
        let sum = |counts: &[u32], lengths: &[u8]| {
            counts
                .iter()
                .zip(lengths)
                .map(|(&count, &length)| u64::from(count) * u64::from(length))
                .sum::<u64>()
        };
        sum(&literal_counts, literals) + sum(&distance_counts, distances) + extra_bits
    };
    let literal_lengths = code_lengths(&literal_counts, 15);
    let distance_lengths = code_lengths(&distance_counts, 15);
    let literal_used = literal_lengths
        .iter()
        .rposition(|&length| length > 0)
        .map_or(257, |last| last + 1)
        .max(257);
    let distance_used = distance_lengths
        .iter()
        .rposition(|&length| length > 0)
        .map_or(1, |last| last + 1);
    let mut sequence = literal_lengths[..literal_used].to_vec();
    sequence.extend_from_slice(&distance_lengths[..distance_used]);
    let runs = run_lengths(&sequence);
    let mut run_counts = vec![0_u32; 19];
    for &(symbol, _) in &runs {
        run_counts[usize::from(symbol)] += 1;
    }
    let run_lengths_used = code_lengths(&run_counts, 7);
    let run_codes_used = CODE_LENGTH_ORDER
        .iter()
        .rposition(|&symbol| run_lengths_used[symbol] > 0)
        .map_or(4, |last| last + 1)
        .max(4);
    let header_bits = 5
        + 5
        + 4
        + 3 * run_codes_used as u64
        + runs
            .iter()
            .map(|&(symbol, _)| {
                u64::from(run_lengths_used[usize::from(symbol)])
                    + match symbol {
                        16 => 2,
                        17 => 3,
                        18 => 7,
                        _ => 0,
                    }
            })
            .sum::<u64>();
    let dynamic = header_bits + cost(&literal_lengths, &distance_lengths);
    let (fixed_literals, fixed_distances) = fixed_lengths();
    let fixed = cost(&fixed_literals, &fixed_distances);
    let stored_blocks = raw.len().div_ceil(0xffff).max(1) as u64;
    let stored = (raw.len() as u64 + 4 * stored_blocks) * 8 + 8 * stored_blocks;
    if stored < dynamic.min(fixed) {
        let mut chunks = raw.chunks(0xffff).peekable();
        if raw.is_empty() {
            writer.put(u32::from(last), 3);
            writer.align();
            writer.bytes.extend_from_slice(&[0, 0, 0xff, 0xff]);
        }
        while let Some(chunk) = chunks.next() {
            writer.put(u32::from(last && chunks.peek().is_none()), 3);
            writer.align();
            let length = chunk.len() as u16;
            writer.bytes.extend_from_slice(&length.to_le_bytes());
            writer.bytes.extend_from_slice(&(!length).to_le_bytes());
            writer.bytes.extend_from_slice(chunk);
        }
        return;
    }
    let (literal_codes, distance_codes) = if fixed <= dynamic {
        writer.put(u32::from(last) | 0b010, 3);
        (canonical(&fixed_literals), canonical(&fixed_distances))
    } else {
        writer.put(u32::from(last) | 0b100, 3);
        writer.put((literal_used - 257) as u32, 5);
        writer.put((distance_used - 1) as u32, 5);
        writer.put((run_codes_used - 4) as u32, 4);
        for &symbol in &CODE_LENGTH_ORDER[..run_codes_used] {
            writer.put(u32::from(run_lengths_used[symbol]), 3);
        }
        let run_codes = canonical(&run_lengths_used);
        for (symbol, extra) in runs {
            writer.code(run_codes[usize::from(symbol)]);
            match symbol {
                16 => writer.put(u32::from(extra), 2),
                17 => writer.put(u32::from(extra), 3),
                18 => writer.put(u32::from(extra), 7),
                _ => {}
            }
        }
        (canonical(&literal_lengths), canonical(&distance_lengths))
    };
    for symbol in symbols {
        match *symbol {
            Symbol::Literal(byte) => writer.code(literal_codes[usize::from(byte)]),
            Symbol::Match { length, distance } => {
                let (length, distance) = (usize::from(length), usize::from(distance));
                let length_index = length_code(length);
                writer.code(literal_codes[257 + length_index]);
                writer.put(
                    (length - usize::from(LENGTH_BASE[length_index])) as u32,
                    LENGTH_EXTRA[length_index],
                );
                let distance_index = distance_code(distance);
                writer.code(distance_codes[distance_index]);
                writer.put(
                    (distance - usize::from(DISTANCE_BASE[distance_index])) as u32,
                    DISTANCE_EXTRA[distance_index],
                );
            }
        }
    }
    writer.code(literal_codes[256]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gzip_member_wraps_the_deflate_stream_with_its_crc_and_length() {
        let text = b"debian packages are gzipped tars, ".repeat(300);
        let member = gzip(&text);
        assert_eq!(member[..4], [0x1f, 0x8b, 8, 0]);
        let (inflated, used) = crate::inflate::inflate(&member[10..], text.len()).unwrap();
        assert_eq!(inflated, text);
        let trailer = &member[10 + used..];
        assert_eq!(trailer[..4], crate::png::crc32(&text).to_le_bytes());
        assert_eq!(trailer[4..], (text.len() as u32).to_le_bytes());
    }

    #[test]
    fn streams_inflate_back_to_their_input() {
        let mut noise = 0x1234_5678_u32;
        let random = (0..70_000)
            .map(|_| {
                noise ^= noise << 13;
                noise ^= noise >> 17;
                noise ^= noise << 5;
                noise as u8
            })
            .collect::<Vec<_>>();
        let text = b"canvas pixels repeat, canvas pixels repeat often. ".repeat(4000);
        let mixed = random[..5000]
            .iter()
            .chain(&text[..90_000])
            .chain(&[7; 300])
            .copied()
            .collect::<Vec<_>>();
        for input in [&[][..], &[42][..], &[1, 2][..], &random, &text, &mixed] {
            let stream = zlib_compress(input);
            let output = crate::inflate::zlib_decompress(&stream, input.len() + 1)
                .expect("the stream should inflate");
            assert_eq!(output, input);
        }
        assert!(zlib_compress(&text).len() < text.len() / 50);
        assert!(zlib_compress(&random).len() < random.len() + 64);
    }
}
