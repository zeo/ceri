// JPEG decoding: baseline and progressive Huffman coding with 8-bit samples,
// any sampling factors, restart intervals, Adobe CMYK and EXIF orientation

use crate::css::Color;
use crate::image::{Image, ImageError, check_dimensions};

const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];
const FAST_BITS: u32 = 9;

pub(crate) fn is_jpeg(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xff, 0xd8, 0xff])
}

fn error(detail: &str) -> ImageError {
    ImageError::new(format!("jpeg: {detail}"))
}

#[derive(Clone, Default)]
struct Huffman {
    // value << 8 | length for codes up to FAST_BITS long
    fast: Vec<u16>,
    max_code: [i32; 18],
    value_offset: [i32; 17],
    values: Vec<u8>,
}

impl Huffman {
    fn new(counts: &[u8; 16], values: Vec<u8>) -> Result<Self, ImageError> {
        let mut fast = vec![0_u16; 1 << FAST_BITS];
        let mut max_code = [-1_i32; 18];
        let mut value_offset = [0_i32; 17];
        let mut code = 0_i32;
        let mut index = 0_i32;
        for length in 1..=16 {
            let count = i32::from(counts[length - 1]);
            value_offset[length] = index - code;
            for _ in 0..count {
                if length as u32 <= FAST_BITS {
                    let shift = FAST_BITS - length as u32;
                    let value = u16::from(
                        *values
                            .get(index as usize)
                            .ok_or_else(|| error("Huffman table lists more codes than values"))?,
                    );
                    for slot in 0..(1 << shift) {
                        fast[((code << shift) | slot) as usize] = (value << 8) | length as u16;
                    }
                }
                code += 1;
                index += 1;
            }
            if code > 1 << length {
                return Err(error("over-subscribed Huffman table"));
            }
            max_code[length] = if count > 0 { code - 1 } else { -1 };
            code <<= 1;
        }
        max_code[17] = i32::MAX;
        if (index as usize) > values.len() {
            return Err(error("Huffman table lists more codes than values"));
        }
        Ok(Self {
            fast,
            max_code,
            value_offset,
            values,
        })
    }
}

struct Bits<'a> {
    data: &'a [u8],
    position: usize,
    buffer: u64,
    count: u32,
    // a marker stops the entropy-coded data; later reads see zero bits
    hit_marker: bool,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8], position: usize) -> Self {
        Self {
            data,
            position,
            buffer: 0,
            count: 0,
            hit_marker: false,
        }
    }

    fn fill(&mut self) {
        while self.count <= 56 {
            let mut byte = 0;
            if !self.hit_marker {
                match self.data.get(self.position) {
                    Some(0xff) => match self.data.get(self.position + 1) {
                        Some(0x00) => {
                            byte = 0xff;
                            self.position += 2;
                        }
                        _ => self.hit_marker = true,
                    },
                    Some(&value) => {
                        byte = value;
                        self.position += 1;
                    }
                    None => self.hit_marker = true,
                }
            }
            self.buffer |= u64::from(byte) << (56 - self.count);
            self.count += 8;
        }
    }

    fn take(&mut self, bits: u32) -> u32 {
        if bits == 0 {
            return 0;
        }
        if self.count < bits {
            self.fill();
        }
        let value = (self.buffer >> (64 - bits)) as u32;
        self.buffer <<= bits;
        self.count -= bits;
        value
    }

    fn bit(&mut self) -> bool {
        self.take(1) == 1
    }

    fn extend(&mut self, bits: u32) -> i32 {
        if bits == 0 {
            return 0;
        }
        let value = self.take(bits) as i32;
        if value < 1 << (bits - 1) {
            value - (1 << bits) + 1
        } else {
            value
        }
    }

    fn decode(&mut self, table: &Huffman) -> Result<u8, ImageError> {
        if self.count < 16 {
            self.fill();
        }
        let entry = table.fast[(self.buffer >> (64 - FAST_BITS)) as usize];
        if entry != 0 {
            let length = u32::from(entry & 0xff);
            self.buffer <<= length;
            self.count -= length;
            return Ok((entry >> 8) as u8);
        }
        let mut code = 0_i32;
        for length in 1..=16 {
            code = (code << 1) | self.take(1) as i32;
            if code <= table.max_code[length] {
                let index = (code + table.value_offset[length]) as usize;
                return table
                    .values
                    .get(index)
                    .copied()
                    .ok_or_else(|| error("bad Huffman code"));
            }
        }
        Err(error("bad Huffman code"))
    }

    // skips to just past the restart marker the encoder placed at an interval boundary
    fn restart(&mut self) -> Result<(), ImageError> {
        self.buffer = 0;
        self.count = 0;
        self.hit_marker = false;
        while self.position + 1 < self.data.len() {
            if self.data[self.position] == 0xff
                && (0xd0..=0xd7).contains(&self.data[self.position + 1])
            {
                self.position += 2;
                return Ok(());
            }
            self.position += 1;
        }
        Err(error("missing restart marker"))
    }
}

#[derive(Clone)]
struct Component {
    id: u8,
    horizontal: usize,
    vertical: usize,
    table: usize,
    blocks_wide: usize,
    blocks_high: usize,
    coefficients: Vec<i16>,
    dc_table: usize,
    ac_table: usize,
    prediction: i32,
}

impl Component {
    fn block(&mut self, row: usize, column: usize) -> &mut [i16] {
        let start = (row * self.blocks_wide + column) * 64;
        &mut self.coefficients[start..start + 64]
    }
}

struct Frame {
    width: usize,
    height: usize,
    progressive: bool,
    components: Vec<Component>,
    max_horizontal: usize,
    max_vertical: usize,
    mcus_wide: usize,
    mcus_high: usize,
}

struct Scan {
    components: Vec<usize>,
    spectral_start: usize,
    spectral_end: usize,
    high: u32,
    low: u32,
}

struct Decoder {
    quantization: [[u16; 64]; 4],
    dc_tables: [Huffman; 4],
    ac_tables: [Huffman; 4],
    restart_interval: usize,
    frame: Option<Frame>,
    adobe_transform: Option<u8>,
    orientation: u16,
}

pub(crate) fn decode_jpeg(bytes: &[u8]) -> Result<Image, ImageError> {
    if !is_jpeg(bytes) {
        return Err(error("missing start of image"));
    }
    let mut decoder = Decoder {
        quantization: [[1; 64]; 4],
        dc_tables: Default::default(),
        ac_tables: Default::default(),
        restart_interval: 0,
        frame: None,
        adobe_transform: None,
        orientation: 1,
    };
    let mut position = 2;
    loop {
        while bytes.get(position) == Some(&0xff) && bytes.get(position + 1) == Some(&0xff) {
            position += 1;
        }
        let (Some(0xff), Some(&marker)) = (bytes.get(position), bytes.get(position + 1)) else {
            return Err(error("expected a marker"));
        };
        position += 2;
        match marker {
            0xd9 => break,
            0xd0..=0xd7 | 0x01 => continue,
            _ => {}
        }
        let length = read_u16(bytes, position)
            .filter(|length| *length >= 2)
            .ok_or_else(|| error("truncated segment"))? as usize;
        let segment = bytes
            .get(position + 2..position + length)
            .ok_or_else(|| error("truncated segment"))?;
        position += length;
        match marker {
            0xc0..=0xc2 => decoder.read_frame(segment, marker == 0xc2)?,
            0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf => {
                return Err(error(
                    "lossless, hierarchical and arithmetic coding are not supported",
                ));
            }
            0xc4 => decoder.read_huffman(segment)?,
            0xdb => decoder.read_quantization(segment)?,
            0xdd => {
                decoder.restart_interval =
                    read_u16(segment, 0).ok_or_else(|| error("bad restart interval"))? as usize;
            }
            0xda => {
                let scan = decoder.read_scan(segment)?;
                position = decoder.decode_scan(bytes, position, &scan)?;
            }
            0xe1 if segment.starts_with(b"Exif\0\0") => {
                decoder.orientation = exif_orientation(&segment[6..]).unwrap_or(1);
            }
            0xee if segment.starts_with(b"Adobe") && segment.len() >= 12 => {
                decoder.adobe_transform = Some(segment[11]);
            }
            _ => {}
        }
    }
    decoder.finish()
}

impl Decoder {
    fn read_quantization(&mut self, mut segment: &[u8]) -> Result<(), ImageError> {
        while let Some(&info) = segment.first() {
            let precision = info >> 4;
            let slot = usize::from(info & 15);
            if slot > 3 || precision > 1 {
                return Err(error("bad quantization table"));
            }
            let size = if precision == 0 { 64 } else { 128 };
            let values = segment
                .get(1..1 + size)
                .ok_or_else(|| error("truncated quantization table"))?;
            for (index, &natural) in ZIGZAG.iter().enumerate() {
                self.quantization[slot][natural] = if precision == 0 {
                    u16::from(values[index])
                } else {
                    u16::from_be_bytes([values[index * 2], values[index * 2 + 1]])
                };
            }
            segment = &segment[1 + size..];
        }
        Ok(())
    }

    fn read_huffman(&mut self, mut segment: &[u8]) -> Result<(), ImageError> {
        while let Some(&info) = segment.first() {
            let class = info >> 4;
            let slot = usize::from(info & 15);
            if class > 1 || slot > 3 {
                return Err(error("bad Huffman table"));
            }
            let counts: [u8; 16] = segment
                .get(1..17)
                .and_then(|counts| counts.try_into().ok())
                .ok_or_else(|| error("truncated Huffman table"))?;
            let total = counts
                .iter()
                .map(|&count| usize::from(count))
                .sum::<usize>();
            let values = segment
                .get(17..17 + total)
                .ok_or_else(|| error("truncated Huffman table"))?
                .to_vec();
            let table = Huffman::new(&counts, values)?;
            if class == 0 {
                self.dc_tables[slot] = table;
            } else {
                self.ac_tables[slot] = table;
            }
            segment = &segment[17 + total..];
        }
        Ok(())
    }

    fn read_frame(&mut self, segment: &[u8], progressive: bool) -> Result<(), ImageError> {
        if self.frame.is_some() {
            return Err(error("more than one frame"));
        }
        if segment.first() != Some(&8) {
            return Err(error("only 8-bit samples are supported"));
        }
        let height = read_u16(segment, 1).unwrap_or_default();
        let width = read_u16(segment, 3).unwrap_or_default();
        check_dimensions(u32::from(width), u32::from(height))?;
        let count = usize::from(*segment.get(5).unwrap_or(&0));
        if !matches!(count, 1 | 3 | 4) || segment.len() < 6 + count * 3 {
            return Err(error("unsupported component count"));
        }
        let mut components = Vec::with_capacity(count);
        for index in 0..count {
            let entry = &segment[6 + index * 3..9 + index * 3];
            let horizontal = usize::from(entry[1] >> 4);
            let vertical = usize::from(entry[1] & 15);
            if !(1..=4).contains(&horizontal) || !(1..=4).contains(&vertical) || entry[2] > 3 {
                return Err(error("bad component"));
            }
            components.push(Component {
                id: entry[0],
                horizontal,
                vertical,
                table: usize::from(entry[2]),
                blocks_wide: 0,
                blocks_high: 0,
                coefficients: Vec::new(),
                dc_table: 0,
                ac_table: 0,
                prediction: 0,
            });
        }
        let max_horizontal = components.iter().map(|c| c.horizontal).max().unwrap_or(1);
        let max_vertical = components.iter().map(|c| c.vertical).max().unwrap_or(1);
        let (width, height) = (usize::from(width), usize::from(height));
        let mcus_wide = width.div_ceil(8 * max_horizontal);
        let mcus_high = height.div_ceil(8 * max_vertical);
        for component in &mut components {
            component.blocks_wide = mcus_wide * component.horizontal;
            component.blocks_high = mcus_high * component.vertical;
            component.coefficients = vec![0; component.blocks_wide * component.blocks_high * 64];
        }
        self.frame = Some(Frame {
            width,
            height,
            progressive,
            components,
            max_horizontal,
            max_vertical,
            mcus_wide,
            mcus_high,
        });
        Ok(())
    }

    fn read_scan(&mut self, segment: &[u8]) -> Result<Scan, ImageError> {
        let frame = self
            .frame
            .as_mut()
            .ok_or_else(|| error("scan before frame"))?;
        let count = usize::from(*segment.first().unwrap_or(&0));
        if count == 0 || count > 4 || segment.len() < 4 + count * 2 {
            return Err(error("bad scan header"));
        }
        let mut components = Vec::with_capacity(count);
        for index in 0..count {
            let id = segment[1 + index * 2];
            let tables = segment[2 + index * 2];
            let position = frame
                .components
                .iter()
                .position(|component| component.id == id)
                .ok_or_else(|| error("scan names an unknown component"))?;
            let component = &mut frame.components[position];
            component.dc_table = usize::from(tables >> 4) & 3;
            component.ac_table = usize::from(tables & 15) & 3;
            components.push(position);
        }
        let tail = &segment[1 + count * 2..];
        let scan = Scan {
            components,
            spectral_start: usize::from(tail[0]),
            spectral_end: usize::from(tail[1]),
            high: u32::from(tail[2] >> 4),
            low: u32::from(tail[2] & 15),
        };
        let valid = if frame.progressive {
            scan.spectral_start <= scan.spectral_end
                && scan.spectral_end < 64
                && (scan.spectral_start == 0) == (scan.spectral_end == 0)
                && (scan.spectral_start == 0 || scan.components.len() == 1)
                && scan.low < 14
        } else {
            scan.spectral_start == 0 && scan.spectral_end == 63
        };
        if !valid {
            return Err(error("bad scan parameters"));
        }
        Ok(scan)
    }

    // decodes one scan's entropy-coded data and returns the position just after it
    fn decode_scan(
        &mut self,
        bytes: &[u8],
        start: usize,
        scan: &Scan,
    ) -> Result<usize, ImageError> {
        let mut frame = self
            .frame
            .take()
            .ok_or_else(|| error("scan before frame"))?;
        for &index in &scan.components {
            frame.components[index].prediction = 0;
        }
        let mut eob_run = 0;
        let mut bits = Bits::new(bytes, start);
        let single = scan.components.len() == 1;
        let (units_wide, units_high) = if single {
            let component = &frame.components[scan.components[0]];
            (
                (frame.width * component.horizontal).div_ceil(8 * frame.max_horizontal),
                (frame.height * component.vertical).div_ceil(8 * frame.max_vertical),
            )
        } else {
            (frame.mcus_wide, frame.mcus_high)
        };
        let total = units_wide * units_high;
        let mut result = Ok(());
        for unit in 0..total {
            if self.restart_interval > 0 && unit > 0 && unit % self.restart_interval == 0 {
                if let Err(cause) = bits.restart() {
                    result = Err(cause);
                    break;
                }
                for &index in &scan.components {
                    frame.components[index].prediction = 0;
                }
                eob_run = 0;
            }
            let (row, column) = (unit / units_wide, unit % units_wide);
            for &index in &scan.components {
                let component = &frame.components[index];
                let (repeat_x, repeat_y) = if single {
                    (1, 1)
                } else {
                    (component.horizontal, component.vertical)
                };
                for y in 0..repeat_y {
                    for x in 0..repeat_x {
                        let block_row = row * repeat_y + y;
                        let block_column = column * repeat_x + x;
                        let step = self.decode_block(
                            &mut bits,
                            &mut frame.components[index],
                            block_row,
                            block_column,
                            scan,
                            frame.progressive,
                            &mut eob_run,
                        );
                        if let Err(cause) = step {
                            result = Err(cause);
                        }
                    }
                }
            }
            if result.is_err() {
                break;
            }
        }
        let position = bits.position;
        self.frame = Some(frame);
        result?;
        // the scan ends at the next marker, which the caller reads
        let mut position = position;
        while position + 1 < bytes.len()
            && !(bytes[position] == 0xff
                && bytes[position + 1] != 0
                && !(0xd0..=0xd7).contains(&bytes[position + 1]))
        {
            position += 1;
        }
        Ok(position)
    }

    #[allow(clippy::too_many_arguments)]
    fn decode_block(
        &self,
        bits: &mut Bits<'_>,
        component: &mut Component,
        row: usize,
        column: usize,
        scan: &Scan,
        progressive: bool,
        eob_run: &mut u32,
    ) -> Result<(), ImageError> {
        let dc_table = &self.dc_tables[component.dc_table];
        let ac_table = &self.ac_tables[component.ac_table];
        let mut prediction = component.prediction;
        let block = component.block(row, column);
        if !progressive {
            let size = bits.decode(dc_table)?;
            prediction += bits.extend(u32::from(size));
            block[0] = prediction as i16;
            let mut index = 1;
            while index < 64 {
                let symbol = bits.decode(ac_table)?;
                let (run, size) = (usize::from(symbol >> 4), u32::from(symbol & 15));
                if size == 0 {
                    if run != 15 {
                        break;
                    }
                    index += 16;
                    continue;
                }
                index += run;
                if index >= 64 {
                    return Err(error("coefficient index out of range"));
                }
                block[ZIGZAG[index]] = bits.extend(size) as i16;
                index += 1;
            }
            component.prediction = prediction;
            return Ok(());
        }
        if scan.spectral_start == 0 {
            if scan.high == 0 {
                let size = bits.decode(dc_table)?;
                prediction += bits.extend(u32::from(size));
                block[0] = (prediction << scan.low) as i16;
                component.prediction = prediction;
            } else if bits.bit() {
                block[0] |= 1 << scan.low;
            }
            return Ok(());
        }
        if scan.high == 0 {
            if *eob_run > 0 {
                *eob_run -= 1;
                return Ok(());
            }
            let mut index = scan.spectral_start;
            while index <= scan.spectral_end {
                let symbol = bits.decode(ac_table)?;
                let (run, size) = (u32::from(symbol >> 4), u32::from(symbol & 15));
                if size == 0 {
                    if run < 15 {
                        *eob_run = (1 << run) - 1 + bits.take(run);
                        break;
                    }
                    index += 16;
                    continue;
                }
                index += run as usize;
                if index > scan.spectral_end {
                    return Err(error("coefficient index out of range"));
                }
                block[ZIGZAG[index]] = (bits.extend(size) * (1 << scan.low)) as i16;
                index += 1;
            }
            return Ok(());
        }
        // successive approximation refinement of AC coefficients
        let positive = 1_i16 << scan.low;
        let negative = -positive;
        let mut index = scan.spectral_start;
        if *eob_run == 0 {
            while index <= scan.spectral_end {
                let symbol = bits.decode(ac_table)?;
                let (mut run, size) = (i32::from(symbol >> 4), symbol & 15);
                let mut value = 0;
                if size == 0 {
                    if run < 15 {
                        *eob_run = (1 << run) + bits.take(run as u32);
                        break;
                    }
                } else {
                    if size != 1 {
                        return Err(error("bad refinement size"));
                    }
                    value = if bits.bit() { positive } else { negative };
                }
                while index <= scan.spectral_end {
                    let coefficient = &mut block[ZIGZAG[index]];
                    if *coefficient != 0 {
                        if bits.bit() && (*coefficient & positive) == 0 {
                            *coefficient += if *coefficient >= 0 {
                                positive
                            } else {
                                negative
                            };
                        }
                    } else {
                        if run == 0 {
                            if value != 0 {
                                *coefficient = value;
                            }
                            index += 1;
                            break;
                        }
                        run -= 1;
                    }
                    index += 1;
                }
            }
        }
        if *eob_run > 0 {
            while index <= scan.spectral_end {
                let coefficient = &mut block[ZIGZAG[index]];
                if *coefficient != 0 && bits.bit() && (*coefficient & positive) == 0 {
                    *coefficient += if *coefficient >= 0 {
                        positive
                    } else {
                        negative
                    };
                }
                index += 1;
            }
            *eob_run -= 1;
        }
        Ok(())
    }

    fn finish(self) -> Result<Image, ImageError> {
        let frame = self.frame.ok_or_else(|| error("no frame"))?;
        let planes = frame
            .components
            .iter()
            .map(|component| {
                let plane = transform_plane(component, &self.quantization[component.table]);
                upsample(
                    &plane,
                    component.blocks_wide * 8,
                    (frame.width, frame.height),
                    (component.horizontal, frame.max_horizontal),
                    (component.vertical, frame.max_vertical),
                )
            })
            .collect::<Vec<_>>();
        let mut image = Image::new(frame.width as u32, frame.height as u32);
        let transform = self.adobe_transform;
        let rgb_ids = frame.components.len() == 3
            && frame.components[0].id == b'R'
            && frame.components[1].id == b'G'
            && frame.components[2].id == b'B';
        for (index, pixel) in image.pixels.iter_mut().enumerate() {
            let sample = |plane: usize| planes[plane][index];
            *pixel = match planes.len() {
                1 => Color::rgba(sample(0), sample(0), sample(0), 255),
                3 if rgb_ids || transform == Some(0) => {
                    Color::rgba(sample(0), sample(1), sample(2), 255)
                }
                3 => ycbcr(sample(0), sample(1), sample(2)),
                _ => {
                    // Adobe stores CMYK inverted; YCCK converts its first three planes first
                    let (cyan, magenta, yellow) = if transform == Some(2) {
                        let color = ycbcr(sample(0), sample(1), sample(2));
                        (255 - color.red, 255 - color.green, 255 - color.blue)
                    } else {
                        (sample(0), sample(1), sample(2))
                    };
                    let key = u32::from(sample(3));
                    let scale = |value: u8| ((u32::from(value) * key + 127) / 255) as u8;
                    if transform.is_some() {
                        Color::rgba(scale(cyan), scale(magenta), scale(yellow), 255)
                    } else {
                        let invert =
                            |value: u8| ((u32::from(255 - value) * (255 - key) + 127) / 255) as u8;
                        Color::rgba(invert(cyan), invert(magenta), invert(yellow), 255)
                    }
                }
            };
        }
        Ok(orient(image, self.orientation))
    }
}

// dequantizes and inverse-transforms every block of a component into a padded sample plane
fn transform_plane(component: &Component, quantization: &[u16; 64]) -> Vec<u8> {
    let scales = aan_scales();
    let mut table = [0_f32; 64];
    for (index, value) in table.iter_mut().enumerate() {
        *value = f32::from(quantization[index]) * scales[index / 8] * scales[index % 8];
    }
    let stride = component.blocks_wide * 8;
    let mut plane = vec![0_u8; stride * component.blocks_high * 8];
    let mut block = [0_f32; 64];
    for row in 0..component.blocks_high {
        for column in 0..component.blocks_wide {
            let start = (row * component.blocks_wide + column) * 64;
            let coefficients = &component.coefficients[start..start + 64];
            for ((value, &coefficient), &factor) in
                block.iter_mut().zip(coefficients).zip(table.iter())
            {
                *value = f32::from(coefficient) * factor;
            }
            idct(&mut block);
            for y in 0..8 {
                let offset = (row * 8 + y) * stride + column * 8;
                for (target, value) in plane[offset..offset + 8]
                    .iter_mut()
                    .zip(&block[y * 8..y * 8 + 8])
                {
                    *target = (value + 128.5).clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
    plane
}

// the AAN transform folds these per-frequency factors into the dequantization table
fn aan_scales() -> [f32; 8] {
    let mut scales = [1.0_f32; 8];
    for (index, scale) in scales.iter_mut().enumerate().skip(1) {
        *scale = (index as f32 * std::f32::consts::PI / 16.0).cos() * std::f32::consts::SQRT_2;
    }
    scales
}

// brings a subsampled plane up to frame size with bilinear filtering between sample centres
fn upsample(
    plane: &[u8],
    stride: usize,
    (width, height): (usize, usize),
    (horizontal, max_horizontal): (usize, usize),
    (vertical, max_vertical): (usize, usize),
) -> Vec<u8> {
    if horizontal == max_horizontal && vertical == max_vertical {
        let mut output = Vec::with_capacity(width * height);
        for row in plane.chunks_exact(stride).take(height) {
            output.extend_from_slice(&row[..width]);
        }
        return output;
    }
    let rows = plane.len() / stride;
    let taps = |count: usize, factor: usize, max: usize, limit: usize| {
        (0..count)
            .map(|position| {
                let source = ((position as f32 + 0.5) * factor as f32 / max as f32 - 0.5).max(0.0);
                let near = (source as usize).min(limit - 1);
                let far = (near + 1).min(limit - 1);
                let weight = ((source - near as f32) * 256.0) as u32;
                (near, far, weight)
            })
            .collect::<Vec<_>>()
    };
    let columns = taps(width, horizontal, max_horizontal, stride);
    let lines = taps(height, vertical, max_vertical, rows);
    let mut output = vec![0_u8; width * height];
    let mut blended = vec![0_u32; stride];
    for (y, &(top, bottom, weight_y)) in lines.iter().enumerate() {
        let upper = &plane[top * stride..top * stride + stride];
        let lower = &plane[bottom * stride..bottom * stride + stride];
        for ((target, &above), &below) in blended.iter_mut().zip(upper).zip(lower) {
            *target = u32::from(above) * (256 - weight_y) + u32::from(below) * weight_y;
        }
        let row = &mut output[y * width..(y + 1) * width];
        for (target, &(left, right, weight_x)) in row.iter_mut().zip(&columns) {
            let value = blended[left] * (256 - weight_x) + blended[right] * weight_x;
            *target = ((value + (1 << 15)) >> 16) as u8;
        }
    }
    output
}

fn ycbcr(luma: u8, blue: u8, red: u8) -> Color {
    // 16.16 fixed point forms of the JFIF conversion coefficients
    let luma = i32::from(luma) << 16;
    let (blue, red) = (i32::from(blue) - 128, i32::from(red) - 128);
    let channel = |value: i32| ((value + (1 << 15)) >> 16).clamp(0, 255) as u8;
    Color::rgba(
        channel(luma + 91_881 * red),
        channel(luma - 22_554 * blue - 46_802 * red),
        channel(luma + 116_130 * blue),
        255,
    )
}

// the AAN floating point inverse DCT from libjpeg's jidctflt, with the 1/8 output scale
fn idct(block: &mut [f32; 64]) {
    for column in 0..8 {
        let at = |row: usize| block[row * 8 + column];
        if (1..8).all(|row| at(row) == 0.0) {
            let value = at(0);
            for row in 0..8 {
                block[row * 8 + column] = value;
            }
            continue;
        }
        let output = idct_line([at(0), at(1), at(2), at(3), at(4), at(5), at(6), at(7)]);
        for (row, value) in output.into_iter().enumerate() {
            block[row * 8 + column] = value;
        }
    }
    for row in block.chunks_exact_mut(8) {
        let output = idct_line([
            row[0], row[1], row[2], row[3], row[4], row[5], row[6], row[7],
        ]);
        for (target, value) in row.iter_mut().zip(output) {
            *target = value / 8.0;
        }
    }
}

fn idct_line(input: [f32; 8]) -> [f32; 8] {
    let tmp10 = input[0] + input[4];
    let tmp11 = input[0] - input[4];
    let tmp13 = input[2] + input[6];
    let tmp12 = (input[2] - input[6]) * std::f32::consts::SQRT_2 - tmp13;
    let even0 = tmp10 + tmp13;
    let even3 = tmp10 - tmp13;
    let even1 = tmp11 + tmp12;
    let even2 = tmp11 - tmp12;
    let z13 = input[5] + input[3];
    let z10 = input[5] - input[3];
    let z11 = input[1] + input[7];
    let z12 = input[1] - input[7];
    let odd7 = z11 + z13;
    let odd11 = (z11 - z13) * std::f32::consts::SQRT_2;
    let z5 = (z10 + z12) * 1.847_759;
    let odd10 = 1.082_392_2 * z12 - z5;
    let odd12 = -2.613_126 * z10 + z5;
    let odd6 = odd12 - odd7;
    let odd5 = odd11 - odd6;
    let odd4 = odd10 + odd5;
    [
        even0 + odd7,
        even1 + odd6,
        even2 + odd5,
        even3 - odd4,
        even3 + odd4,
        even2 - odd5,
        even1 - odd6,
        even0 - odd7,
    ]
}

fn orient(image: Image, orientation: u16) -> Image {
    if !(2..=8).contains(&orientation) {
        return image;
    }
    let (width, height) = (image.width as usize, image.height as usize);
    let swapped = orientation >= 5;
    let (out_width, out_height) = if swapped {
        (height, width)
    } else {
        (width, height)
    };
    let mut output = Image::new(out_width as u32, out_height as u32);
    for y in 0..height {
        for x in 0..width {
            let (target_x, target_y) = match orientation {
                2 => (width - 1 - x, y),
                3 => (width - 1 - x, height - 1 - y),
                4 => (x, height - 1 - y),
                5 => (y, x),
                6 => (height - 1 - y, x),
                7 => (height - 1 - y, width - 1 - x),
                _ => (y, width - 1 - x),
            };
            output.pixels[target_y * out_width + target_x] = image.pixels[y * width + x];
        }
    }
    output
}

fn exif_orientation(tiff: &[u8]) -> Option<u16> {
    let little = match tiff.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let read16 = |at: usize| {
        let bytes = [*tiff.get(at)?, *tiff.get(at + 1)?];
        Some(if little {
            u16::from_le_bytes(bytes)
        } else {
            u16::from_be_bytes(bytes)
        })
    };
    let read32 = |at: usize| {
        let bytes = [
            *tiff.get(at)?,
            *tiff.get(at + 1)?,
            *tiff.get(at + 2)?,
            *tiff.get(at + 3)?,
        ];
        Some(if little {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        })
    };
    let directory = read32(4)? as usize;
    let entries = read16(directory)?;
    (0..usize::from(entries))
        .map(|entry| directory + 2 + entry * 12)
        .find(|&entry| read16(entry) == Some(0x0112))
        .and_then(|entry| read16(entry + 8))
}

fn read_u16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*bytes.get(at)?, *bytes.get(at + 1)?]))
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

    // libjpeg uses an integer transform and its own upsampling, so pixels agree
    // within a small error rather than exactly
    #[test]
    fn decodes_baseline_progressive_restart_cmyk_and_oriented_files() {
        let cases = String::from_utf8(fixture("jpeg-cases.txt")).expect("case list is text");
        for line in cases.lines().filter(|line| !line.is_empty()) {
            let mut fields = line.split(' ');
            let name = fields.next().expect("case name");
            let width: u32 = fields.next().and_then(|v| v.parse().ok()).expect("width");
            let height: u32 = fields.next().and_then(|v| v.parse().ok()).expect("height");
            let image =
                decode_jpeg(&fixture(name)).unwrap_or_else(|cause| panic!("{name}: {cause}"));
            assert_eq!((image.width, image.height), (width, height), "{name}");
            let reference = fixture(&format!("{name}.rgba"));
            let mut total = 0_u64;
            let mut worst = 0_u8;
            for (index, pixel) in image.pixels.iter().enumerate() {
                let want = &reference[index * 4..index * 4 + 3];
                for (got, want) in [pixel.red, pixel.green, pixel.blue].iter().zip(want) {
                    let difference = got.abs_diff(*want);
                    total += u64::from(difference);
                    worst = worst.max(difference);
                }
                assert_eq!(pixel.alpha, 255, "{name}");
            }
            let mean = total as f64 / (image.pixels.len() * 3) as f64;
            assert!(
                mean < 0.5 && worst <= 4,
                "{name}: mean {mean:.2}, worst {worst}"
            );
        }
    }

    #[test]
    fn rejects_truncated_and_unsupported_files_without_panicking() {
        let bytes = fixture("progressive-420.jpg");
        for end in 0..bytes.len() {
            let _ = decode_jpeg(&bytes[..end]);
        }
        let mut corrupt = fixture("baseline-420.jpg");
        let middle = corrupt.len() / 2;
        for byte in &mut corrupt[middle..middle + 40] {
            *byte ^= 0x5a;
        }
        let _ = decode_jpeg(&corrupt);
        let mut arithmetic = fixture("baseline-444.jpg");
        let frame = arithmetic
            .windows(2)
            .position(|pair| pair == [0xff, 0xc0])
            .expect("baseline frame marker");
        arithmetic[frame + 1] = 0xc9;
        assert!(decode_jpeg(&arithmetic).is_err());
    }
}
