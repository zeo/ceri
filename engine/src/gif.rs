// GIF decoding of the first frame: LZW image data, global or local palettes,
// a transparent index from the graphic control extension and interlaced rows; and the frames
// of an animation, kept compressed until they are shown

use crate::css::Color;
use crate::image::{Image, ImageError, check_dimensions};

pub(crate) fn is_gif(bytes: &[u8]) -> bool {
    bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")
}

fn error(detail: &str) -> ImageError {
    ImageError::new(format!("gif: {detail}"))
}

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], ImageError> {
        let slice = self
            .bytes
            .get(self.position..self.position + count)
            .ok_or_else(|| error("file ends early"))?;
        self.position += count;
        Ok(slice)
    }

    fn byte(&mut self) -> Result<u8, ImageError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, ImageError> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn palette(&mut self, flags: u8) -> Result<Vec<[u8; 3]>, ImageError> {
        let size = 2_usize << (flags & 7);
        Ok(self
            .take(size * 3)?
            .as_chunks::<3>()
            .0
            .iter()
            .map(|entry| [entry[0], entry[1], entry[2]])
            .collect())
    }

    // concatenated data sub-blocks up to the zero-length terminator
    fn blocks(&mut self) -> Result<Vec<u8>, ImageError> {
        let mut data = Vec::new();
        loop {
            let length = usize::from(self.byte()?);
            if length == 0 {
                return Ok(data);
            }
            data.extend_from_slice(self.take(length)?);
        }
    }
}

pub(crate) fn decode_gif(bytes: &[u8]) -> Result<Image, ImageError> {
    if !is_gif(bytes) {
        return Err(error("missing signature"));
    }
    let mut reader = Reader { bytes, position: 6 };
    let width = reader.u16()?;
    let height = reader.u16()?;
    check_dimensions(u32::from(width), u32::from(height))?;
    let flags = reader.byte()?;
    reader.take(2)?;
    let global = if flags & 0x80 != 0 {
        reader.palette(flags)?
    } else {
        Vec::new()
    };
    let mut transparent = None;
    loop {
        match reader.byte()? {
            0x21 => {
                let label = reader.byte()?;
                let data = reader.blocks()?;
                if label == 0xf9 && data.len() >= 4 && data[0] & 1 == 1 {
                    transparent = Some(data[3]);
                }
            }
            0x2c => {
                let left = usize::from(reader.u16()?);
                let top = usize::from(reader.u16()?);
                let frame_width = usize::from(reader.u16()?);
                let frame_height = usize::from(reader.u16()?);
                let frame_flags = reader.byte()?;
                let local = if frame_flags & 0x80 != 0 {
                    Some(reader.palette(frame_flags)?)
                } else {
                    None
                };
                let palette = local.as_ref().unwrap_or(&global);
                let minimum = reader.byte()?;
                let data = reader.blocks()?;
                let indices = decompress(&data, minimum, frame_width * frame_height)?;
                let mut image = Image::new(u32::from(width), u32::from(height));
                let rows = row_order(frame_height, frame_flags & 0x40 != 0);
                for (source_row, &target_row) in rows.iter().enumerate() {
                    let y = top + target_row;
                    if y >= usize::from(height) {
                        continue;
                    }
                    for column in 0..frame_width {
                        let x = left + column;
                        let Some(&index) = indices.get(source_row * frame_width + column) else {
                            continue;
                        };
                        if x >= usize::from(width) || Some(index) == transparent {
                            continue;
                        }
                        if let Some(&[red, green, blue]) = palette.get(usize::from(index)) {
                            image.pixels[y * usize::from(width) + x] =
                                Color::rgba(red, green, blue, 255);
                        }
                    }
                }
                return Ok(image);
            }
            0x3b => return Err(error("no image frame")),
            _ => return Err(error("unknown block")),
        }
    }
}

// one frame of an animation: where it goes, how long it shows and what happens to it after
pub(crate) struct GifFrame {
    pub(crate) left: u32,
    pub(crate) top: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) delay: u32,
    // 2 clears the frame's area after it shows, 3 puts back what was there before it
    pub(crate) disposal: u8,
    transparent: Option<u8>,
    interlaced: bool,
    palette: Vec<[u8; 3]>,
    minimum: u8,
    data: Vec<u8>,
}

impl GifFrame {
    // the frame's pixels as straight RGBA, transparent where the index says so
    pub(crate) fn rgba(&self) -> Vec<u8> {
        let (width, height) = (self.width as usize, self.height as usize);
        let indices = decompress(&self.data, self.minimum, width * height).unwrap_or_default();
        let mut out = vec![0_u8; width * height * 4];
        for (source_row, &target_row) in row_order(height, self.interlaced).iter().enumerate() {
            for column in 0..width {
                let Some(&index) = indices.get(source_row * width + column) else {
                    continue;
                };
                if Some(index) == self.transparent {
                    continue;
                }
                if let Some(&[red, green, blue]) = self.palette.get(usize::from(index)) {
                    let at = (target_row * width + column) * 4;
                    out[at..at + 4].copy_from_slice(&[red, green, blue, 255]);
                }
            }
        }
        out
    }
}

// the frames of a GIF with its screen size and how many times it plays, None for ever
pub(crate) fn gif_frames(
    bytes: &[u8],
) -> Result<(u32, u32, Vec<GifFrame>, Option<u32>), ImageError> {
    if !is_gif(bytes) {
        return Err(error("missing signature"));
    }
    let mut reader = Reader { bytes, position: 6 };
    let width = u32::from(reader.u16()?);
    let height = u32::from(reader.u16()?);
    check_dimensions(width, height)?;
    let flags = reader.byte()?;
    reader.take(2)?;
    let global = if flags & 0x80 != 0 {
        reader.palette(flags)?
    } else {
        Vec::new()
    };
    let mut frames = Vec::new();
    // without the NETSCAPE extension an animation plays once
    let mut plays = Some(1);
    let (mut delay, mut disposal, mut transparent) = (0, 0, None);
    while let Ok(introducer) = reader.byte() {
        match introducer {
            0x21 => {
                let label = reader.byte()?;
                let data = reader.blocks()?;
                if label == 0xf9 && data.len() >= 4 {
                    disposal = (data[0] >> 2) & 7;
                    transparent = (data[0] & 1 == 1).then_some(data[3]);
                    delay = u32::from(u16::from_le_bytes([data[1], data[2]])) * 10;
                } else if label == 0xff
                    && data.len() >= 14
                    && &data[..11] == b"NETSCAPE2.0"
                    && data[11] == 1
                {
                    let count = u32::from(u16::from_le_bytes([data[12], data[13]]));
                    plays = (count != 0).then_some(count + 1);
                }
            }
            0x2c => {
                let left = u32::from(reader.u16()?);
                let top = u32::from(reader.u16()?);
                let frame_width = u32::from(reader.u16()?);
                let frame_height = u32::from(reader.u16()?);
                let frame_flags = reader.byte()?;
                let palette = if frame_flags & 0x80 != 0 {
                    reader.palette(frame_flags)?
                } else {
                    global.clone()
                };
                let minimum = reader.byte()?;
                let data = reader.blocks()?;
                frames.push(GifFrame {
                    left,
                    top,
                    width: frame_width,
                    height: frame_height,
                    // browsers stretch the briefest frames, which ads used to flash
                    delay: if delay <= 10 { 100 } else { delay },
                    disposal,
                    transparent,
                    interlaced: frame_flags & 0x40 != 0,
                    palette,
                    minimum,
                    data,
                });
                (delay, disposal, transparent) = (0, 0, None);
            }
            _ => break,
        }
    }
    if frames.is_empty() {
        return Err(error("no image frame"));
    }
    Ok((width, height, frames, plays))
}

// interlaced frames store every eighth row first, then the rows between
fn row_order(height: usize, interlaced: bool) -> Vec<usize> {
    if !interlaced {
        return (0..height).collect();
    }
    [(0, 8), (4, 8), (2, 4), (1, 2)]
        .iter()
        .flat_map(|&(start, step)| (start..height).step_by(step))
        .collect()
}

fn decompress(data: &[u8], minimum: u8, limit: usize) -> Result<Vec<u8>, ImageError> {
    if !(2..=11).contains(&minimum) {
        return Err(error("bad LZW code size"));
    }
    let clear = 1_usize << minimum;
    let end = clear + 1;
    let mut prefix = vec![0_u16; 4096];
    let mut suffix = vec![0_u8; 4096];
    let mut first = vec![0_u8; 4096];
    for code in 0..clear {
        suffix[code] = code as u8;
        first[code] = code as u8;
    }
    let mut output = Vec::with_capacity(limit);
    let mut stack = Vec::new();
    let mut size = u32::from(minimum) + 1;
    let mut next = end + 1;
    let mut previous: Option<usize> = None;
    let (mut buffer, mut count, mut position) = (0_u32, 0_u32, 0_usize);
    while output.len() < limit {
        while count < size {
            let Some(&byte) = data.get(position) else {
                return Ok(output);
            };
            buffer |= u32::from(byte) << count;
            count += 8;
            position += 1;
        }
        let code = (buffer & ((1 << size) - 1)) as usize;
        buffer >>= size;
        count -= size;
        if code == clear {
            size = u32::from(minimum) + 1;
            next = end + 1;
            previous = None;
            continue;
        }
        if code == end {
            break;
        }
        let Some(last) = previous else {
            if code >= clear {
                return Err(error("stream starts with a non-literal code"));
            }
            output.push(code as u8);
            previous = Some(code);
            continue;
        };
        // a code one past the table repeats the previous string plus its first byte
        let (mut walk, tail) = if code < next {
            (code, None)
        } else if code == next {
            (last, Some(first[last]))
        } else {
            return Err(error("code out of range"));
        };
        stack.clear();
        if let Some(tail) = tail {
            stack.push(tail);
        }
        while walk >= clear {
            stack.push(suffix[walk]);
            walk = usize::from(prefix[walk]);
        }
        stack.push(walk as u8);
        let room = limit - output.len();
        output.extend(stack.iter().rev().take(room));
        if next < 4096 {
            prefix[next] = last as u16;
            suffix[next] = *stack.last().expect("stack holds the string");
            first[next] = first[last];
            next += 1;
            if next == 1 << size && size < 12 {
                size += 1;
            }
        }
        previous = Some(code);
    }
    Ok(output)
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

    #[test]
    fn decodes_the_first_frame_with_transparency_and_interlacing() {
        for name in ["frame.gif", "frame-interlaced.gif", "animated.gif"] {
            let image =
                decode_gif(&fixture(name)).unwrap_or_else(|cause| panic!("{name}: {cause}"));
            let reference = fixture(&format!("{name}.rgba"));
            assert_eq!(image.pixels.len() * 4, reference.len(), "{name}");
            for (index, pixel) in image.pixels.iter().enumerate() {
                let want = &reference[index * 4..index * 4 + 4];
                if want[3] == 0 {
                    assert_eq!(pixel.alpha, 0, "{name} pixel {index}");
                } else {
                    assert_eq!(
                        [pixel.red, pixel.green, pixel.blue, pixel.alpha],
                        want,
                        "{name} pixel {index}"
                    );
                }
            }
        }
    }

    #[test]
    fn truncated_files_fail_or_decode_without_panicking() {
        let bytes = fixture("frame.gif");
        for end in 0..bytes.len() {
            let _ = decode_gif(&bytes[..end]);
        }
    }
}
