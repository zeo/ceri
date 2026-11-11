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
    for row in block.as_chunks_mut::<8>().0.iter_mut() {
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

const LUMA_QUANT: [u16; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56,
    14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113,
    92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
];
const CHROMA_QUANT: [u16; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99, 99,
    47, 66, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
];
// the example tables of ITU T.81 annex K.3, as counts per code length and then values
const LUMA_DC: ([u8; 16], &[u8]) = (
    [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0],
    &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
);
const CHROMA_DC: ([u8; 16], &[u8]) = (
    [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0],
    &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
);
const LUMA_AC: ([u8; 16], &[u8]) = (
    [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d],
    &[
        0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61,
        0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52,
        0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25,
        0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45,
        0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64,
        0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83,
        0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99,
        0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6,
        0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3,
        0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8,
        0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
    ],
);
const CHROMA_AC: ([u8; 16], &[u8]) = (
    [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77],
    &[
        0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61,
        0x71, 0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33,
        0x52, 0xf0, 0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18,
        0x19, 0x1a, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44,
        0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63,
        0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a,
        0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97,
        0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4,
        0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca,
        0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7,
        0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
    ],
);

// baseline YCbCr the way Chromium has libjpeg-turbo write canvas images: its fixed-point
// color conversion, 4:2:0 with its biased box downsampling below quality 100 and 4:4:4 at it,
// the islow integer DCT and the scaled annex K quantization tables; `rgb` is tightly packed
pub(crate) fn encode_jpeg(width: u32, height: u32, rgb: &[u8], quality: u8) -> Option<Vec<u8>> {
    if width == 0 || height == 0 || width > 65535 || height > 65535 {
        return None;
    }
    let (width, height) = (width as usize, height as usize);
    let quality = u32::from(quality.clamp(1, 100));
    let factor = if quality == 100 { 1 } else { 2 };
    let mcu = 8 * factor;
    let (padded_width, padded_height) = (width.div_ceil(mcu) * mcu, height.div_ceil(mcu) * mcu);
    let fix = |value: f64| (value * 65536.0 + 0.5) as i32;
    let chroma_offset = (128 << 16) + (1 << 15) - 1;
    let mut planes = [(); 3].map(|_| vec![0_u8; padded_width * padded_height]);
    // samples past the edges repeat the last column and row, as libjpeg pads them
    for y in 0..padded_height {
        for x in 0..padded_width {
            let at = (y.min(height - 1) * width + x.min(width - 1)) * 3;
            let [red, green, blue] = [rgb[at], rgb[at + 1], rgb[at + 2]].map(i32::from);
            let index = y * padded_width + x;
            planes[0][index] =
                ((fix(0.299) * red + fix(0.587) * green + fix(0.114) * blue + (1 << 15)) >> 16)
                    as u8;
            planes[1][index] =
                ((-fix(0.16874) * red - fix(0.33126) * green + fix(0.5) * blue + chroma_offset)
                    >> 16) as u8;
            planes[2][index] = ((fix(0.5) * red - fix(0.41869) * green - fix(0.08131) * blue
                + chroma_offset)
                >> 16) as u8;
        }
    }
    let (chroma_width, chroma_height) = (padded_width / factor, padded_height / factor);
    let [luma, blue, red] = planes;
    let [blue, red] = [blue, red].map(|plane| {
        if factor == 1 {
            return plane;
        }
        let mut half = Vec::with_capacity(chroma_width * chroma_height);
        for y in 0..chroma_height {
            let (top, bottom) = (2 * y * padded_width, (2 * y + 1) * padded_width);
            for x in 0..chroma_width {
                let sum: u32 = [top, bottom]
                    .iter()
                    .flat_map(|row| [plane[row + 2 * x], plane[row + 2 * x + 1]])
                    .map(u32::from)
                    .sum();
                // the rounding bias alternates 1, 2 along the row
                half.push(((sum + 1 + (x as u32 & 1)) >> 2) as u8);
            }
        }
        half
    });

    let scale = if quality < 50 {
        5000 / quality
    } else {
        200 - 2 * quality
    };
    let tables = [LUMA_QUANT, CHROMA_QUANT]
        .map(|table| table.map(|base| ((u32::from(base) * scale + 50) / 100).clamp(1, 255) as u16));

    let mut file = vec![0xff, 0xd8];
    let mut segment = |marker: u8, body: &[u8]| {
        file.extend_from_slice(&[0xff, marker]);
        file.extend_from_slice(&(body.len() as u16 + 2).to_be_bytes());
        file.extend_from_slice(body);
    };
    segment(0xe0, b"JFIF\0\x01\x01\0\0\x01\0\x01\0\0");
    for (id, table) in tables.iter().enumerate() {
        let mut body = vec![id as u8];
        body.extend(ZIGZAG.iter().map(|&natural| table[natural] as u8));
        segment(0xdb, &body);
    }
    let mut frame = vec![8];
    frame.extend_from_slice(&(height as u16).to_be_bytes());
    frame.extend_from_slice(&(width as u16).to_be_bytes());
    frame.extend_from_slice(&[3, 1, 0x11 * factor as u8, 0, 2, 0x11, 1, 3, 0x11, 1]);
    segment(0xc0, &frame);
    let huffman = [LUMA_DC, LUMA_AC, CHROMA_DC, CHROMA_AC];
    for ((counts, values), class_id) in huffman.iter().zip([0x00, 0x10, 0x01, 0x11]) {
        let mut body = vec![class_id];
        body.extend_from_slice(counts);
        body.extend_from_slice(values);
        segment(0xc4, &body);
    }
    segment(0xda, &[3, 1, 0x00, 2, 0x11, 3, 0x11, 0, 63, 0]);

    let codes = huffman.map(|(counts, values)| {
        let mut codes = [(0_u16, 0_u8); 256];
        let mut code = 0_u16;
        let mut values = values.iter();
        for (length, &count) in (1..=16).zip(&counts) {
            for &value in values.by_ref().take(usize::from(count)) {
                codes[usize::from(value)] = (code, length);
                code += 1;
            }
            code <<= 1;
        }
        codes
    });
    let mut writer = EntropyWriter::default();
    let mut previous_dc = [0_i32; 3];
    for mcu_y in 0..padded_height / mcu {
        for mcu_x in 0..padded_width / mcu {
            let mut blocks = Vec::with_capacity(6);
            for (dy, dx) in (0..factor).flat_map(|dy| (0..factor).map(move |dx| (dy, dx))) {
                let (top, left) = (mcu_y * mcu + dy * 8, mcu_x * mcu + dx * 8);
                blocks.push((0, &luma, padded_width, top, left));
            }
            blocks.push((1, &blue, chroma_width, mcu_y * 8, mcu_x * 8));
            blocks.push((2, &red, chroma_width, mcu_y * 8, mcu_x * 8));
            for (component, plane, stride, top, left) in blocks {
                let mut block = [0_i32; 64];
                for (index, sample) in block.iter_mut().enumerate() {
                    *sample = i32::from(plane[(top + index / 8) * stride + left + index % 8]) - 128;
                }
                forward_dct(&mut block);
                let table = &tables[usize::from(component > 0)];
                for (coefficient, &step) in block.iter_mut().zip(table) {
                    let divisor = i32::from(step) << 3;
                    *coefficient =
                        coefficient.signum() * ((coefficient.abs() + divisor / 2) / divisor);
                }
                let (dc_codes, ac_codes) = (
                    &codes[usize::from(component > 0) * 2],
                    &codes[usize::from(component > 0) * 2 + 1],
                );
                writer.value(block[0] - previous_dc[component], 0, dc_codes);
                previous_dc[component] = block[0];
                let mut run = 0;
                for &natural in &ZIGZAG[1..] {
                    if block[natural] == 0 {
                        run += 1;
                        continue;
                    }
                    while run > 15 {
                        writer.code(ac_codes[0xf0]);
                        run -= 16;
                    }
                    writer.value(block[natural], run, ac_codes);
                    run = 0;
                }
                if run > 0 {
                    writer.code(ac_codes[0]);
                }
            }
        }
    }
    writer.flush();
    file.extend_from_slice(&writer.bytes);
    file.extend_from_slice(&[0xff, 0xd9]);
    Some(file)
}

#[derive(Default)]
struct EntropyWriter {
    bytes: Vec<u8>,
    bits: u32,
    count: u32,
}

impl EntropyWriter {
    fn put(&mut self, value: u32, length: u8) {
        self.bits = (self.bits << length) | (value & ((1 << length) - 1));
        self.count += u32::from(length);
        while self.count >= 8 {
            let byte = (self.bits >> (self.count - 8)) as u8;
            self.bytes.push(byte);
            if byte == 0xff {
                self.bytes.push(0);
            }
            self.count -= 8;
        }
    }

    fn code(&mut self, (code, length): (u16, u8)) {
        self.put(u32::from(code), length);
    }

    // a coefficient as its run and size category, then its low bits in one's complement
    fn value(&mut self, value: i32, run: u8, codes: &[(u16, u8); 256]) {
        let size = (32 - value.unsigned_abs().leading_zeros()) as u8;
        self.code(codes[usize::from(run << 4 | size)]);
        let bits = if value < 0 { value - 1 } else { value };
        self.put(bits as u32, size);
    }

    fn flush(&mut self) {
        let pad = (8 - self.count % 8) % 8;
        self.put((1 << pad) - 1, pad as u8);
    }
}

// the jfdctint transform of libjpeg: Loeffler, Ligtenberg and Moschytz in 13-bit fixed point,
// with outputs scaled up by 8
fn forward_dct(block: &mut [i32; 64]) {
    const CONST_BITS: u32 = 13;
    const PASS1_BITS: u32 = 2;
    let descale = |value: i32, shift: u32| (value + (1 << (shift - 1))) >> shift;
    for pass in 0..2 {
        let odd_shift = if pass == 0 {
            CONST_BITS - PASS1_BITS
        } else {
            CONST_BITS + PASS1_BITS
        };
        for line in 0..8 {
            let at = |index: usize| {
                if pass == 0 {
                    line * 8 + index
                } else {
                    index * 8 + line
                }
            };
            let sample = |index: usize| block[at(index)];
            let (tmp0, tmp7) = (sample(0) + sample(7), sample(0) - sample(7));
            let (tmp1, tmp6) = (sample(1) + sample(6), sample(1) - sample(6));
            let (tmp2, tmp5) = (sample(2) + sample(5), sample(2) - sample(5));
            let (tmp3, tmp4) = (sample(3) + sample(4), sample(3) - sample(4));
            let (tmp10, tmp13) = (tmp0 + tmp3, tmp0 - tmp3);
            let (tmp11, tmp12) = (tmp1 + tmp2, tmp1 - tmp2);
            let mut output = [0_i32; 8];
            if pass == 0 {
                output[0] = (tmp10 + tmp11) << PASS1_BITS;
                output[4] = (tmp10 - tmp11) << PASS1_BITS;
            } else {
                output[0] = descale(tmp10 + tmp11, PASS1_BITS);
                output[4] = descale(tmp10 - tmp11, PASS1_BITS);
            }
            let z1 = (tmp12 + tmp13) * 4433;
            output[2] = descale(z1 + tmp13 * 6270, odd_shift);
            output[6] = descale(z1 - tmp12 * 15137, odd_shift);
            let z5 = (tmp4 + tmp5 + tmp6 + tmp7) * 9633;
            let z1 = (tmp4 + tmp7) * -7373;
            let z2 = (tmp5 + tmp6) * -20995;
            let z3 = (tmp4 + tmp6) * -16069 + z5;
            let z4 = (tmp5 + tmp7) * -3196 + z5;
            output[7] = descale(tmp4 * 2446 + z1 + z3, odd_shift);
            output[5] = descale(tmp5 * 16819 + z2 + z4, odd_shift);
            output[3] = descale(tmp6 * 25172 + z2 + z3, odd_shift);
            output[1] = descale(tmp7 * 12299 + z1 + z4, odd_shift);
            for (index, value) in output.into_iter().enumerate() {
                block[at(index)] = value;
            }
        }
    }
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

    // byte-for-byte pixel agreement with libjpeg-turbo was checked outside the test suite,
    // so this only guards that the stream stays decodable and close to its source
    #[test]
    fn encoded_images_decode_close_to_their_source() {
        let (width, height) = (37, 21);
        let rgb = (0..height)
            .flat_map(|y| (0..width).flat_map(move |x| [x * 6, y * 11, 200 - x * 3]))
            .map(|value| value as u8)
            .collect::<Vec<_>>();
        for quality in [100, 92, 30] {
            let image = decode_jpeg(&encode_jpeg(width, height, &rgb, quality).expect("encodes"))
                .expect("decodes");
            assert_eq!((image.width, image.height), (width, height));
            let worst = image
                .pixels
                .iter()
                .zip(rgb.as_chunks::<3>().0.iter())
                .flat_map(|(pixel, source)| {
                    [pixel.red, pixel.green, pixel.blue]
                        .into_iter()
                        .zip(source)
                        .map(|(decoded, &source)| decoded.abs_diff(source))
                })
                .max()
                .unwrap_or(0);
            assert!(
                worst <= if quality == 30 { 24 } else { 8 },
                "q{quality} off by {worst}"
            );
        }
        assert!(encode_jpeg(0, 4, &[], 92).is_none());
        assert!(encode_jpeg(70_000, 1, &[0; 210_000], 92).is_none());
    }
}
