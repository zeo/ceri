// VP8 key frames, the lossy WebP bitstream of RFC 6386: the boolean decoder, the frame header,
// intra modes and DCT tokens, prediction, the inverse transforms and the loop filter, with
// libwebp's integer arithmetic so the planes match what browsers decode

use super::tables::{AC_QUANT, BMODE_PROBS, BMODE_TREE, COEFF_PROBS, COEFF_UPDATE_PROBS, DC_QUANT};

const COEFF_BANDS: [usize; 17] = [0, 1, 2, 3, 6, 4, 5, 6, 6, 6, 6, 6, 6, 6, 6, 7, 0];
const ZIGZAG: [usize; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];
const CAT3: &[u8] = &[173, 148, 140];
const CAT4: &[u8] = &[176, 155, 140, 135];
const CAT5: &[u8] = &[180, 157, 141, 134, 130];
const CAT6: &[u8] = &[254, 254, 243, 230, 196, 177, 153, 140, 133, 130, 129];

// sub-block modes, numbered as the RFC and the tables number them
const B_DC: u8 = 0;
const B_TM: u8 = 1;
const B_VE: u8 = 2;
const B_HE: u8 = 3;
const B_LD: u8 = 4;
const B_RD: u8 = 5;
const B_VR: u8 = 6;
const B_VL: u8 = 7;
const B_HD: u8 = 8;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Dc,
    Vertical,
    Horizontal,
    TrueMotion,
}

struct BoolDecoder<'a> {
    data: &'a [u8],
    position: usize,
    value: u32,
    range: u32,
    count: u32,
}

impl<'a> BoolDecoder<'a> {
    fn new(data: &'a [u8]) -> Self {
        let byte = |index: usize| u32::from(data.get(index).copied().unwrap_or(0));
        BoolDecoder {
            data,
            position: 2,
            value: (byte(0) << 8) | byte(1),
            range: 255,
            count: 0,
        }
    }

    #[inline]
    fn read(&mut self, probability: u8) -> bool {
        let split = 1 + (((self.range - 1) * u32::from(probability)) >> 8);
        let big_split = split << 8;
        let bit = if self.value >= big_split {
            self.range -= split;
            self.value -= big_split;
            true
        } else {
            self.range = split;
            false
        };
        while self.range < 128 {
            self.value <<= 1;
            self.range <<= 1;
            self.count += 1;
            if self.count == 8 {
                self.count = 0;
                self.value |= u32::from(self.data.get(self.position).copied().unwrap_or(0));
                self.position += 1;
            }
        }
        bit
    }

    fn literal(&mut self, bits: u32) -> u32 {
        (0..bits).fold(0, |value, _| (value << 1) | u32::from(self.read(128)))
    }

    fn signed(&mut self, bits: u32) -> i32 {
        let value = self.literal(bits) as i32;
        if self.read(128) { -value } else { value }
    }

    fn flagged(&mut self, bits: u32) -> i32 {
        if self.read(128) { self.signed(bits) } else { 0 }
    }

    fn tree(&mut self, tree: &[i8], probabilities: &[u8]) -> u8 {
        let mut index = 0_i32;
        loop {
            index = i32::from(
                tree[index as usize + usize::from(self.read(probabilities[index as usize >> 1]))],
            );
            if index <= 0 {
                return (-index) as u8;
            }
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Quant {
    y1: [i32; 2],
    y2: [i32; 2],
    uv: [i32; 2],
}

#[derive(Clone, Copy, Default)]
struct Filter {
    // 2 * level + interior, or 0 for no filtering
    limit: i32,
    interior: i32,
    hev: i32,
}

struct Block {
    i4x4: bool,
    modes: [u8; 16],
    luma: Mode,
    chroma: Mode,
    segment: usize,
    // the coefficients of the 16 luma and 8 chroma blocks, in raster order within each block
    coefficients: Vec<[i32; 16]>,
    nonzero: bool,
}

pub(super) struct Planes {
    pub(super) width: usize,
    pub(super) height: usize,
    pub(super) y: Vec<u8>,
    pub(super) u: Vec<u8>,
    pub(super) v: Vec<u8>,
    pub(super) y_stride: usize,
    pub(super) uv_stride: usize,
}

fn clip8(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

// the inverse DCT of one block, added to the prediction already in place
fn idct_add(input: &[i32; 16], plane: &mut [u8], at: usize, stride: usize) {
    let mul1 = |a: i32| ((a * 20091) >> 16) + a;
    let mul2 = |a: i32| (a * 35468) >> 16;
    let mut columns = [0_i32; 16];
    for i in 0..4 {
        let a = input[i] + input[8 + i];
        let b = input[i] - input[8 + i];
        let c = mul2(input[4 + i]) - mul1(input[12 + i]);
        let d = mul1(input[4 + i]) + mul2(input[12 + i]);
        columns[i * 4] = a + d;
        columns[i * 4 + 1] = b + c;
        columns[i * 4 + 2] = b - c;
        columns[i * 4 + 3] = a - d;
    }
    for i in 0..4 {
        let dc = columns[i] + 4;
        let a = dc + columns[8 + i];
        let b = dc - columns[8 + i];
        let c = mul2(columns[4 + i]) - mul1(columns[12 + i]);
        let d = mul1(columns[4 + i]) + mul2(columns[12 + i]);
        let row = at + i * stride;
        for (x, value) in [a + d, b + c, b - c, a - d].into_iter().enumerate() {
            plane[row + x] = clip8(i32::from(plane[row + x]) + (value >> 3));
        }
    }
}

// the inverse Walsh-Hadamard transform of the second-order block into each luma block's DC
fn inverse_wht(input: &[i32; 16], blocks: &mut [[i32; 16]]) {
    let mut temp = [0_i32; 16];
    for i in 0..4 {
        let a0 = input[i] + input[12 + i];
        let a1 = input[4 + i] + input[8 + i];
        let a2 = input[4 + i] - input[8 + i];
        let a3 = input[i] - input[12 + i];
        temp[i] = a0 + a1;
        temp[8 + i] = a0 - a1;
        temp[4 + i] = a3 + a2;
        temp[12 + i] = a3 - a2;
    }
    for i in 0..4 {
        let dc = temp[i * 4] + 3;
        let a0 = dc + temp[3 + i * 4];
        let a1 = temp[1 + i * 4] + temp[2 + i * 4];
        let a2 = temp[1 + i * 4] - temp[2 + i * 4];
        let a3 = dc - temp[3 + i * 4];
        blocks[i * 4][0] = (a0 + a1) >> 3;
        blocks[i * 4 + 1][0] = (a3 + a2) >> 3;
        blocks[i * 4 + 2][0] = (a0 - a1) >> 3;
        blocks[i * 4 + 3][0] = (a3 - a2) >> 3;
    }
}

// the tokens of one block; returns the index after the last coefficient read
fn read_coefficients(
    decoder: &mut BoolDecoder,
    probabilities: &[[[u8; 11]; 3]; 8],
    context: usize,
    quant: [i32; 2],
    first: usize,
    out: &mut [i32; 16],
) -> usize {
    let mut n = first;
    let mut p = &probabilities[COEFF_BANDS[n]][context];
    while n < 16 {
        if !decoder.read(p[0]) {
            return n;
        }
        while !decoder.read(p[1]) {
            n += 1;
            if n == 16 {
                return 16;
            }
            p = &probabilities[COEFF_BANDS[n]][0];
        }
        let value = if !decoder.read(p[2]) {
            p = &probabilities[COEFF_BANDS[n + 1]][1];
            1
        } else {
            let value = large_value(decoder, p);
            p = &probabilities[COEFF_BANDS[n + 1]][2];
            value
        };
        let signed = if decoder.read(128) { -value } else { value };
        out[ZIGZAG[n]] = signed * quant[usize::from(n > 0)];
        n += 1;
    }
    16
}

fn large_value(decoder: &mut BoolDecoder, p: &[u8; 11]) -> i32 {
    if !decoder.read(p[3]) {
        if !decoder.read(p[4]) {
            2
        } else {
            3 + i32::from(decoder.read(p[5]))
        }
    } else if !decoder.read(p[6]) {
        if !decoder.read(p[7]) {
            5 + i32::from(decoder.read(159))
        } else {
            let high = 7 + 2 * i32::from(decoder.read(165));
            high + i32::from(decoder.read(145))
        }
    } else {
        let bit1 = usize::from(decoder.read(p[8]));
        let bit0 = usize::from(decoder.read(p[9 + bit1]));
        let category = 2 * bit1 + bit0;
        let table = [CAT3, CAT4, CAT5, CAT6][category];
        let extra = table.iter().fold(0, |value, &probability| {
            value + value + i32::from(decoder.read(probability))
        });
        extra + 3 + (8 << category)
    }
}

// the planes of a key frame
pub(super) fn decode(data: &[u8]) -> Option<Planes> {
    if data.len() < 10 {
        return None;
    }
    let tag = u32::from(data[0]) | (u32::from(data[1]) << 8) | (u32::from(data[2]) << 16);
    let key_frame = tag & 1 == 0;
    let first_size = (tag >> 5) as usize;
    if !key_frame || data[3..6] != [0x9d, 0x01, 0x2a] {
        return None;
    }
    let width = (u32::from(data[6]) | (u32::from(data[7]) << 8)) & 0x3fff;
    let height = (u32::from(data[8]) | (u32::from(data[9]) << 8)) & 0x3fff;
    crate::image::check_dimensions(width, height).ok()?;
    let rest = &data[10..];
    if first_size > rest.len() {
        return None;
    }
    let mut header = BoolDecoder::new(&rest[..first_size]);
    let _color_space = header.read(128);
    let _clamping = header.read(128);
    // segmentation
    let segmented = header.read(128);
    let (mut update_map, mut absolute) = (false, false);
    let mut segment_quant = [0_i32; 4];
    let mut segment_filter = [0_i32; 4];
    let mut segment_probabilities = [255_u8; 3];
    if segmented {
        update_map = header.read(128);
        let update_data = header.read(128);
        if update_data {
            absolute = header.read(128);
            for value in &mut segment_quant {
                *value = header.flagged(7);
            }
            for value in &mut segment_filter {
                *value = header.flagged(6);
            }
        }
        if update_map {
            for probability in &mut segment_probabilities {
                *probability = if header.read(128) {
                    header.literal(8) as u8
                } else {
                    255
                };
            }
        }
    }
    // the loop filter
    let simple = header.read(128);
    let level = header.literal(6) as i32;
    let sharpness = header.literal(3) as i32;
    let mut reference_delta = 0;
    let mut mode_delta = 0;
    let use_deltas = header.read(128);
    if use_deltas && header.read(128) {
        for index in 0..4 {
            let value = header.flagged(6);
            if index == 0 {
                reference_delta = value;
            }
        }
        for index in 0..4 {
            let value = header.flagged(6);
            if index == 0 {
                mode_delta = value;
            }
        }
    }
    let partitions = 1_usize << header.literal(2);
    // quantizers
    let base = header.literal(7) as i32;
    let deltas = [
        header.flagged(4),
        header.flagged(4),
        header.flagged(4),
        header.flagged(4),
        header.flagged(4),
    ];
    let _refresh = header.read(128);
    let mut probabilities = COEFF_PROBS;
    for (t, bands) in probabilities.iter_mut().enumerate() {
        for (b, contexts) in bands.iter_mut().enumerate() {
            for (c, values) in contexts.iter_mut().enumerate() {
                for (p, value) in values.iter_mut().enumerate() {
                    if header.read(COEFF_UPDATE_PROBS[t][b][c][p]) {
                        *value = header.literal(8) as u8;
                    }
                }
            }
        }
    }
    let skip_probability = if header.read(128) {
        Some(header.literal(8) as u8)
    } else {
        None
    };

    let quants: Vec<Quant> = (0..4)
        .map(|segment| {
            let q = if segmented {
                segment_quant[segment] + if absolute { 0 } else { base }
            } else {
                base
            };
            let index = |delta: i32, limit: i32| (q + delta).clamp(0, limit) as usize;
            let y2_ac = (AC_QUANT[index(deltas[2], 127)] * 101_581) >> 16;
            Quant {
                y1: [DC_QUANT[index(deltas[0], 127)], AC_QUANT[index(0, 127)]],
                y2: [DC_QUANT[index(deltas[1], 127)] * 2, y2_ac.max(8)],
                uv: [
                    DC_QUANT[index(deltas[3], 117)],
                    AC_QUANT[index(deltas[4], 127)],
                ],
            }
        })
        .collect();
    let filtering = level != 0;
    let filters: Vec<[Filter; 2]> = (0..4)
        .map(|segment| {
            let base_level = if segmented {
                segment_filter[segment] + if absolute { 0 } else { level }
            } else {
                level
            };
            std::array::from_fn(|i4x4| {
                let mut level = base_level;
                if use_deltas {
                    level += reference_delta;
                    if i4x4 == 1 {
                        level += mode_delta;
                    }
                }
                let level = level.clamp(0, 63);
                if level == 0 {
                    return Filter::default();
                }
                let mut interior = level;
                if sharpness > 0 {
                    interior >>= if sharpness > 4 { 2 } else { 1 };
                    interior = interior.min(9 - sharpness);
                }
                let interior = interior.max(1);
                Filter {
                    limit: 2 * level + interior,
                    interior,
                    hev: if level >= 40 {
                        2
                    } else if level >= 15 {
                        1
                    } else {
                        0
                    },
                }
            })
        })
        .collect();

    // the token partitions after the first
    let sizes_start = first_size;
    let sizes_length = 3 * (partitions - 1);
    if sizes_start + sizes_length > rest.len() {
        return None;
    }
    let mut start = sizes_start + sizes_length;
    let mut token_decoders = Vec::with_capacity(partitions);
    for index in 0..partitions {
        let size = if index + 1 < partitions {
            let at = sizes_start + 3 * index;
            (usize::from(rest[at])
                | (usize::from(rest[at + 1]) << 8)
                | (usize::from(rest[at + 2]) << 16))
                .min(rest.len() - start)
        } else {
            rest.len() - start
        };
        token_decoders.push(BoolDecoder::new(&rest[start..start + size]));
        start += size;
    }

    let (mb_width, mb_height) = (
        (width as usize).div_ceil(16),
        (height as usize).div_ceil(16),
    );
    let (y_stride, uv_stride) = (mb_width * 16, mb_width * 8);
    let mut planes = Planes {
        width: width as usize,
        height: height as usize,
        y: vec![0; y_stride * mb_height * 16],
        u: vec![0; uv_stride * mb_height * 8],
        v: vec![0; uv_stride * mb_height * 8],
        y_stride,
        uv_stride,
    };
    // nonzero flags of the blocks above, per macroblock column: four luma, two of each
    // chroma plane and the second-order block
    let mut top_nonzero = vec![[0_u8; 9]; mb_width];
    let mut top_modes = vec![[B_DC; 4]; mb_width];
    let mut filter_info: Vec<(Filter, bool)> = Vec::with_capacity(mb_width * mb_height);
    for mb_y in 0..mb_height {
        let mut left_nonzero = [0_u8; 9];
        let mut left_modes = [B_DC; 4];
        let tokens = &mut token_decoders[mb_y % partitions];
        for mb_x in 0..mb_width {
            let segment = if update_map {
                if !header.read(segment_probabilities[0]) {
                    usize::from(header.read(segment_probabilities[1]))
                } else {
                    usize::from(header.read(segment_probabilities[2])) + 2
                }
            } else {
                0
            };
            let skip = skip_probability.is_some_and(|probability| header.read(probability));
            let i4x4 = !header.read(145);
            let mut modes = [B_DC; 16];
            let luma = if i4x4 {
                for y in 0..4 {
                    let mut left = left_modes[y];
                    for x in 0..4 {
                        let above = top_modes[mb_x][x];
                        let mode = header.tree(
                            &BMODE_TREE,
                            &BMODE_PROBS[usize::from(above)][usize::from(left)],
                        );
                        modes[y * 4 + x] = mode;
                        top_modes[mb_x][x] = mode;
                        left = mode;
                    }
                    left_modes[y] = left;
                }
                Mode::Dc
            } else {
                let mode = if header.read(156) {
                    if header.read(128) {
                        Mode::TrueMotion
                    } else {
                        Mode::Horizontal
                    }
                } else if header.read(163) {
                    Mode::Vertical
                } else {
                    Mode::Dc
                };
                let implied = match mode {
                    Mode::Dc => B_DC,
                    Mode::Vertical => B_VE,
                    Mode::Horizontal => B_HE,
                    Mode::TrueMotion => B_TM,
                };
                top_modes[mb_x] = [implied; 4];
                left_modes = [implied; 4];
                mode
            };
            let chroma = if !header.read(142) {
                Mode::Dc
            } else if !header.read(114) {
                Mode::Vertical
            } else if header.read(183) {
                Mode::TrueMotion
            } else {
                Mode::Horizontal
            };
            let mut block = Block {
                i4x4,
                modes,
                luma,
                chroma,
                segment,
                coefficients: vec![[0; 16]; 24],
                nonzero: false,
            };
            let quant = quants[segment];
            let top = &mut top_nonzero[mb_x];
            if !skip {
                let mut first = 0;
                let mut luma_type = 3;
                if !i4x4 {
                    let mut second = [0_i32; 16];
                    let context = usize::from(top[8] + left_nonzero[8]);
                    let count = read_coefficients(
                        tokens,
                        &probabilities[1],
                        context,
                        quant.y2,
                        0,
                        &mut second,
                    );
                    let nonzero = u8::from(count > 0);
                    top[8] = nonzero;
                    left_nonzero[8] = nonzero;
                    if count > 1 {
                        inverse_wht(&second, &mut block.coefficients);
                    } else {
                        let dc = (second[0] + 3) >> 3;
                        for coefficients in block.coefficients.iter_mut().take(16) {
                            coefficients[0] = dc;
                        }
                    }
                    first = 1;
                    luma_type = 0;
                }
                for (y, left_slot) in left_nonzero.iter_mut().take(4).enumerate() {
                    let mut left = *left_slot;
                    for (x, top_slot) in top.iter_mut().take(4).enumerate() {
                        let context = usize::from(left + *top_slot);
                        let count = read_coefficients(
                            tokens,
                            &probabilities[luma_type],
                            context,
                            quant.y1,
                            first,
                            &mut block.coefficients[y * 4 + x],
                        );
                        left = u8::from(count > first);
                        *top_slot = left;
                    }
                    *left_slot = left;
                }
                for plane in 0..2 {
                    for y in 0..2 {
                        let mut left = left_nonzero[4 + plane * 2 + y];
                        for x in 0..2 {
                            let context = usize::from(left + top[4 + plane * 2 + x]);
                            let index = 16 + plane * 4 + y * 2 + x;
                            let count = read_coefficients(
                                tokens,
                                &probabilities[2],
                                context,
                                quant.uv,
                                0,
                                &mut block.coefficients[index],
                            );
                            left = u8::from(count > 0);
                            top[4 + plane * 2 + x] = left;
                        }
                        left_nonzero[4 + plane * 2 + y] = left;
                    }
                }
                block.nonzero = block
                    .coefficients
                    .iter()
                    .any(|coefficients| coefficients.iter().any(|&value| value != 0));
            } else {
                top[..8].fill(0);
                left_nonzero[..8].fill(0);
                if !i4x4 {
                    top[8] = 0;
                    left_nonzero[8] = 0;
                }
            }
            reconstruct(&mut planes, &block, mb_x, mb_y, mb_width);
            let filter = if filtering {
                filters[block.segment][usize::from(block.i4x4)]
            } else {
                Filter::default()
            };
            filter_info.push((filter, block.i4x4 || block.nonzero));
        }
    }
    if filtering {
        for mb_y in 0..mb_height {
            for mb_x in 0..mb_width {
                let (filter, inner) = filter_info[mb_y * mb_width + mb_x];
                if filter.limit > 0 {
                    loop_filter(&mut planes, mb_x, mb_y, filter, inner, simple);
                }
            }
        }
    }
    Some(planes)
}

// prediction and residuals of one macroblock, from the unfiltered pixels around it
fn reconstruct(planes: &mut Planes, block: &Block, mb_x: usize, mb_y: usize, mb_width: usize) {
    // a workspace one pixel taller and five wider than the luma block: the row above with the
    // four pixels above-right, and the column to the left
    const STRIDE: usize = 21;
    let mut work = [0_u8; STRIDE * 17];
    let (stride, x0, y0) = (planes.y_stride, mb_x * 16, mb_y * 16);
    if mb_y == 0 {
        work[..STRIDE].fill(127);
    } else {
        let above = (y0 - 1) * stride;
        work[0] = if mb_x == 0 {
            129
        } else {
            planes.y[above + x0 - 1]
        };
        work[1..17].copy_from_slice(&planes.y[above + x0..above + x0 + 16]);
        if mb_x + 1 < mb_width {
            work[17..21].copy_from_slice(&planes.y[above + x0 + 16..above + x0 + 20]);
        } else {
            work[17..21].fill(planes.y[above + x0 + 15]);
        }
    }
    for y in 0..16 {
        work[(y + 1) * STRIDE] = if mb_x == 0 {
            129
        } else {
            planes.y[(y0 + y) * stride + x0 - 1]
        };
    }
    let at = |x: usize, y: usize| (y + 1) * STRIDE + x + 1;
    if block.i4x4 {
        // the pixels above-right of the last column of sub-blocks repeat the macroblock's
        for row in [3, 7, 11] {
            let (from, to) = (at(16, 0) - STRIDE, at(16, row));
            work.copy_within(from..from + 4, to);
        }
        for n in 0..16 {
            let (bx, by) = ((n % 4) * 4, (n / 4) * 4);
            predict4(&mut work, at(bx, by), STRIDE, block.modes[n]);
            if block.coefficients[n].iter().any(|&value| value != 0) {
                idct_add(&block.coefficients[n], &mut work, at(bx, by), STRIDE);
            }
        }
    } else {
        predict(
            &mut work,
            at(0, 0),
            STRIDE,
            16,
            block.luma,
            mb_x == 0,
            mb_y == 0,
        );
        for n in 0..16 {
            if block.coefficients[n].iter().any(|&value| value != 0) {
                idct_add(
                    &block.coefficients[n],
                    &mut work,
                    at((n % 4) * 4, (n / 4) * 4),
                    STRIDE,
                );
            }
        }
    }
    for y in 0..16 {
        let row = (y0 + y) * stride + x0;
        planes.y[row..row + 16].copy_from_slice(&work[at(0, y)..at(0, y) + 16]);
    }
    let (stride, x0, y0) = (planes.uv_stride, mb_x * 8, mb_y * 8);
    for (plane, first) in [(&mut planes.u, 16), (&mut planes.v, 20)] {
        const CHROMA: usize = 9;
        let mut work = [0_u8; CHROMA * 9];
        if mb_y == 0 {
            work[..CHROMA].fill(127);
        } else {
            let above = (y0 - 1) * stride;
            work[0] = if mb_x == 0 {
                129
            } else {
                plane[above + x0 - 1]
            };
            work[1..9].copy_from_slice(&plane[above + x0..above + x0 + 8]);
        }
        for y in 0..8 {
            work[(y + 1) * CHROMA] = if mb_x == 0 {
                129
            } else {
                plane[(y0 + y) * stride + x0 - 1]
            };
        }
        let at = |x: usize, y: usize| (y + 1) * CHROMA + x + 1;
        predict(
            &mut work,
            at(0, 0),
            CHROMA,
            8,
            block.chroma,
            mb_x == 0,
            mb_y == 0,
        );
        for n in 0..4 {
            let coefficients = &block.coefficients[first + n];
            if coefficients.iter().any(|&value| value != 0) {
                idct_add(
                    coefficients,
                    &mut work,
                    at((n % 2) * 4, (n / 2) * 4),
                    CHROMA,
                );
            }
        }
        for y in 0..8 {
            let row = (y0 + y) * stride + x0;
            plane[row..row + 8].copy_from_slice(&work[at(0, y)..at(0, y) + 8]);
        }
    }
}

// whole-block prediction for 16x16 luma and 8x8 chroma; dc averages only the edges that exist
fn predict(
    work: &mut [u8],
    at: usize,
    stride: usize,
    size: usize,
    mode: Mode,
    no_left: bool,
    no_top: bool,
) {
    let top: Vec<i32> = (0..size)
        .map(|x| i32::from(work[at - stride + x]))
        .collect();
    let left: Vec<i32> = (0..size)
        .map(|y| i32::from(work[at + y * stride - 1]))
        .collect();
    let corner = i32::from(work[at - stride - 1]);
    let shift = size.trailing_zeros();
    for y in 0..size {
        for x in 0..size {
            work[at + y * stride + x] = match mode {
                Mode::Vertical => top[x] as u8,
                Mode::Horizontal => left[y] as u8,
                Mode::TrueMotion => clip8(top[x] + left[y] - corner),
                Mode::Dc => match (no_top, no_left) {
                    (true, true) => 128,
                    (true, false) => {
                        ((left.iter().sum::<i32>() + (1 << (shift - 1))) >> shift) as u8
                    }
                    (false, true) => {
                        ((top.iter().sum::<i32>() + (1 << (shift - 1))) >> shift) as u8
                    }
                    (false, false) => {
                        ((top.iter().sum::<i32>() + left.iter().sum::<i32>() + (1 << shift))
                            >> (shift + 1)) as u8
                    }
                },
            };
        }
    }
}

fn avg3(a: i32, b: i32, c: i32) -> u8 {
    ((a + 2 * b + c + 2) >> 2) as u8
}

fn avg2(a: i32, b: i32) -> u8 {
    ((a + b + 1) >> 1) as u8
}

// the ten 4x4 sub-block predictors of libwebp
fn predict4(work: &mut [u8], at: usize, stride: usize, mode: u8) {
    let top = |x: usize| i32::from(work[at - stride + x]);
    let left = |y: usize| i32::from(work[at + y * stride - 1]);
    let x_ = i32::from(work[at - stride - 1]);
    let (a, b, c, d, e, f, g, h) = (
        top(0),
        top(1),
        top(2),
        top(3),
        top(4),
        top(5),
        top(6),
        top(7),
    );
    let (i, j, k, l) = (left(0), left(1), left(2), left(3));
    let mut out = [[0_u8; 4]; 4];
    let mut set = |x: usize, y: usize, value: u8| out[y][x] = value;
    match mode {
        B_DC => {
            let dc = ((a + b + c + d + i + j + k + l + 4) >> 3) as u8;
            for y in 0..4 {
                for x in 0..4 {
                    set(x, y, dc);
                }
            }
        }
        B_TM => {
            for y in 0..4 {
                for x in 0..4 {
                    set(x, y, clip8(top(x) + left(y) - x_));
                }
            }
        }
        B_VE => {
            let values = [avg3(x_, a, b), avg3(a, b, c), avg3(b, c, d), avg3(c, d, e)];
            for y in 0..4 {
                for (x, value) in values.iter().enumerate() {
                    set(x, y, *value);
                }
            }
        }
        B_HE => {
            let values = [avg3(x_, i, j), avg3(i, j, k), avg3(j, k, l), avg3(k, l, l)];
            for (y, value) in values.iter().enumerate() {
                for x in 0..4 {
                    set(x, y, *value);
                }
            }
        }
        B_RD => {
            set(0, 3, avg3(j, k, l));
            let v = avg3(i, j, k);
            set(1, 3, v);
            set(0, 2, v);
            let v = avg3(x_, i, j);
            set(2, 3, v);
            set(1, 2, v);
            set(0, 1, v);
            let v = avg3(a, x_, i);
            set(3, 3, v);
            set(2, 2, v);
            set(1, 1, v);
            set(0, 0, v);
            let v = avg3(b, a, x_);
            set(3, 2, v);
            set(2, 1, v);
            set(1, 0, v);
            let v = avg3(c, b, a);
            set(3, 1, v);
            set(2, 0, v);
            set(3, 0, avg3(d, c, b));
        }
        B_LD => {
            set(0, 0, avg3(a, b, c));
            let v = avg3(b, c, d);
            set(1, 0, v);
            set(0, 1, v);
            let v = avg3(c, d, e);
            set(2, 0, v);
            set(1, 1, v);
            set(0, 2, v);
            let v = avg3(d, e, f);
            set(3, 0, v);
            set(2, 1, v);
            set(1, 2, v);
            set(0, 3, v);
            let v = avg3(e, f, g);
            set(3, 1, v);
            set(2, 2, v);
            set(1, 3, v);
            let v = avg3(f, g, h);
            set(3, 2, v);
            set(2, 3, v);
            set(3, 3, avg3(g, h, h));
        }
        B_VR => {
            let v = avg2(x_, a);
            set(0, 0, v);
            set(1, 2, v);
            let v = avg2(a, b);
            set(1, 0, v);
            set(2, 2, v);
            let v = avg2(b, c);
            set(2, 0, v);
            set(3, 2, v);
            set(3, 0, avg2(c, d));
            set(0, 3, avg3(k, j, i));
            set(0, 2, avg3(j, i, x_));
            let v = avg3(i, x_, a);
            set(0, 1, v);
            set(1, 3, v);
            let v = avg3(x_, a, b);
            set(1, 1, v);
            set(2, 3, v);
            let v = avg3(a, b, c);
            set(2, 1, v);
            set(3, 3, v);
            set(3, 1, avg3(b, c, d));
        }
        B_VL => {
            set(0, 0, avg2(a, b));
            let v = avg2(b, c);
            set(1, 0, v);
            set(0, 2, v);
            let v = avg2(c, d);
            set(2, 0, v);
            set(1, 2, v);
            let v = avg2(d, e);
            set(3, 0, v);
            set(2, 2, v);
            set(0, 1, avg3(a, b, c));
            let v = avg3(b, c, d);
            set(1, 1, v);
            set(0, 3, v);
            let v = avg3(c, d, e);
            set(2, 1, v);
            set(1, 3, v);
            let v = avg3(d, e, f);
            set(3, 1, v);
            set(2, 3, v);
            set(3, 2, avg3(e, f, g));
            set(3, 3, avg3(f, g, h));
        }
        B_HD => {
            let v = avg2(i, x_);
            set(0, 0, v);
            set(2, 1, v);
            let v = avg2(j, i);
            set(0, 1, v);
            set(2, 2, v);
            let v = avg2(k, j);
            set(0, 2, v);
            set(2, 3, v);
            set(0, 3, avg2(l, k));
            set(3, 0, avg3(a, b, c));
            set(2, 0, avg3(x_, a, b));
            let v = avg3(i, x_, a);
            set(1, 0, v);
            set(3, 1, v);
            let v = avg3(j, i, x_);
            set(1, 1, v);
            set(3, 2, v);
            let v = avg3(k, j, i);
            set(1, 2, v);
            set(3, 3, v);
            set(1, 3, avg3(l, k, j));
        }
        // horizontal-up
        _ => {
            set(0, 0, avg2(i, j));
            let v = avg2(j, k);
            set(2, 0, v);
            set(0, 1, v);
            let v = avg2(k, l);
            set(2, 1, v);
            set(0, 2, v);
            set(1, 0, avg3(i, j, k));
            let v = avg3(j, k, l);
            set(3, 0, v);
            set(1, 1, v);
            let v = avg3(k, l, l);
            set(3, 1, v);
            set(1, 2, v);
            for (x, y) in [(3, 2), (2, 2), (0, 3), (1, 3), (2, 3), (3, 3)] {
                set(x, y, l as u8);
            }
        }
    }
    for (y, row) in out.iter().enumerate() {
        work[at + y * stride..at + y * stride + 4].copy_from_slice(row);
    }
}

// libwebp's in-loop filters on one edge position, p at the first pixel past the edge
fn needs_filter(plane: &[u8], p: usize, step: usize, threshold: i32) -> bool {
    let at = |offset: isize| i32::from(plane[(p as isize + offset * step as isize) as usize]);
    4 * (at(-1) - at(0)).abs() + (at(-2) - at(1)).abs() <= threshold
}

fn needs_filter2(plane: &[u8], p: usize, step: usize, threshold: i32, interior: i32) -> bool {
    let at = |offset: isize| i32::from(plane[(p as isize + offset * step as isize) as usize]);
    let (p3, p2, p1, p0, q0, q1, q2, q3) =
        (at(-4), at(-3), at(-2), at(-1), at(0), at(1), at(2), at(3));
    if 4 * (p0 - q0).abs() + (p1 - q1).abs() > threshold {
        return false;
    }
    (p3 - p2).abs() <= interior
        && (p2 - p1).abs() <= interior
        && (p1 - p0).abs() <= interior
        && (q3 - q2).abs() <= interior
        && (q2 - q1).abs() <= interior
        && (q1 - q0).abs() <= interior
}

fn high_variance(plane: &[u8], p: usize, step: usize, threshold: i32) -> bool {
    let at = |offset: isize| i32::from(plane[(p as isize + offset * step as isize) as usize]);
    (at(-2) - at(-1)).abs() > threshold || (at(1) - at(0)).abs() > threshold
}

fn sclip1(value: i32) -> i32 {
    value.clamp(-128, 127)
}

fn sclip2(value: i32) -> i32 {
    value.clamp(-16, 15)
}

fn filter2(plane: &mut [u8], p: usize, step: usize) {
    let (p1, p0, q0, q1) = (
        i32::from(plane[p - 2 * step]),
        i32::from(plane[p - step]),
        i32::from(plane[p]),
        i32::from(plane[p + step]),
    );
    let a = 3 * (q0 - p0) + sclip1(p1 - q1);
    let a1 = sclip2((a + 4) >> 3);
    let a2 = sclip2((a + 3) >> 3);
    plane[p - step] = clip8(p0 + a2);
    plane[p] = clip8(q0 - a1);
}

fn filter4(plane: &mut [u8], p: usize, step: usize) {
    let (p1, p0, q0, q1) = (
        i32::from(plane[p - 2 * step]),
        i32::from(plane[p - step]),
        i32::from(plane[p]),
        i32::from(plane[p + step]),
    );
    let a = 3 * (q0 - p0);
    let a1 = sclip2((a + 4) >> 3);
    let a2 = sclip2((a + 3) >> 3);
    let a3 = (a1 + 1) >> 1;
    plane[p - 2 * step] = clip8(p1 + a3);
    plane[p - step] = clip8(p0 + a2);
    plane[p] = clip8(q0 - a1);
    plane[p + step] = clip8(q1 - a3);
}

fn filter6(plane: &mut [u8], p: usize, step: usize) {
    let (p2, p1, p0) = (
        i32::from(plane[p - 3 * step]),
        i32::from(plane[p - 2 * step]),
        i32::from(plane[p - step]),
    );
    let (q0, q1, q2) = (
        i32::from(plane[p]),
        i32::from(plane[p + step]),
        i32::from(plane[p + 2 * step]),
    );
    let a = sclip1(3 * (q0 - p0) + sclip1(p1 - q1));
    let a1 = (27 * a + 63) >> 7;
    let a2 = (18 * a + 63) >> 7;
    let a3 = (9 * a + 63) >> 7;
    plane[p - 3 * step] = clip8(p2 + a3);
    plane[p - 2 * step] = clip8(p1 + a2);
    plane[p - step] = clip8(p0 + a1);
    plane[p] = clip8(q0 - a1);
    plane[p + step] = clip8(q1 - a2);
    plane[p + 2 * step] = clip8(q2 - a3);
}

// one edge of `size` positions: `step` crosses the edge, `along` runs down it
#[allow(clippy::too_many_arguments)]
fn filter_edge(
    plane: &mut [u8],
    start: usize,
    step: usize,
    along: usize,
    size: usize,
    threshold: i32,
    interior: i32,
    hev: i32,
    edge: bool,
) {
    let threshold2 = 2 * threshold + 1;
    for index in 0..size {
        let p = start + index * along;
        if needs_filter2(plane, p, step, threshold2, interior) {
            if high_variance(plane, p, step, hev) {
                filter2(plane, p, step);
            } else if edge {
                filter6(plane, p, step);
            } else {
                filter4(plane, p, step);
            }
        }
    }
}

fn simple_edge(plane: &mut [u8], start: usize, step: usize, along: usize, threshold: i32) {
    let threshold2 = 2 * threshold + 1;
    for index in 0..16 {
        let p = start + index * along;
        if needs_filter(plane, p, step, threshold2) {
            filter2(plane, p, step);
        }
    }
}

fn loop_filter(
    planes: &mut Planes,
    mb_x: usize,
    mb_y: usize,
    filter: Filter,
    inner: bool,
    simple: bool,
) {
    let (stride, uv_stride) = (planes.y_stride, planes.uv_stride);
    let y = mb_y * 16 * stride + mb_x * 16;
    let (limit, interior, hev) = (filter.limit, filter.interior, filter.hev);
    if simple {
        if mb_x > 0 {
            simple_edge(&mut planes.y, y, 1, stride, limit + 4);
        }
        if inner {
            for offset in [4, 8, 12] {
                simple_edge(&mut planes.y, y + offset, 1, stride, limit);
            }
        }
        if mb_y > 0 {
            simple_edge(&mut planes.y, y, stride, 1, limit + 4);
        }
        if inner {
            for offset in [4, 8, 12] {
                simple_edge(&mut planes.y, y + offset * stride, stride, 1, limit);
            }
        }
        return;
    }
    let uv = mb_y * 8 * uv_stride + mb_x * 8;
    if mb_x > 0 {
        filter_edge(
            &mut planes.y,
            y,
            1,
            stride,
            16,
            limit + 4,
            interior,
            hev,
            true,
        );
        filter_edge(
            &mut planes.u,
            uv,
            1,
            uv_stride,
            8,
            limit + 4,
            interior,
            hev,
            true,
        );
        filter_edge(
            &mut planes.v,
            uv,
            1,
            uv_stride,
            8,
            limit + 4,
            interior,
            hev,
            true,
        );
    }
    if inner {
        for offset in [4, 8, 12] {
            filter_edge(
                &mut planes.y,
                y + offset,
                1,
                stride,
                16,
                limit,
                interior,
                hev,
                false,
            );
        }
        filter_edge(
            &mut planes.u,
            uv + 4,
            1,
            uv_stride,
            8,
            limit,
            interior,
            hev,
            false,
        );
        filter_edge(
            &mut planes.v,
            uv + 4,
            1,
            uv_stride,
            8,
            limit,
            interior,
            hev,
            false,
        );
    }
    if mb_y > 0 {
        filter_edge(
            &mut planes.y,
            y,
            stride,
            1,
            16,
            limit + 4,
            interior,
            hev,
            true,
        );
        filter_edge(
            &mut planes.u,
            uv,
            uv_stride,
            1,
            8,
            limit + 4,
            interior,
            hev,
            true,
        );
        filter_edge(
            &mut planes.v,
            uv,
            uv_stride,
            1,
            8,
            limit + 4,
            interior,
            hev,
            true,
        );
    }
    if inner {
        for offset in [4, 8, 12] {
            filter_edge(
                &mut planes.y,
                y + offset * stride,
                stride,
                1,
                16,
                limit,
                interior,
                hev,
                false,
            );
        }
        filter_edge(
            &mut planes.u,
            uv + 4 * uv_stride,
            uv_stride,
            1,
            8,
            limit,
            interior,
            hev,
            false,
        );
        filter_edge(
            &mut planes.v,
            uv + 4 * uv_stride,
            uv_stride,
            1,
            8,
            limit,
            interior,
            hev,
            false,
        );
    }
}
