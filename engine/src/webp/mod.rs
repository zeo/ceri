// WebP: the RIFF container with its simple lossy, simple lossless and extended forms, the
// alpha chunk, and the first frame of an animation; lossy planes become RGB through libwebp's
// fancy upsampling so colours match what browsers show

mod lossless;
mod lossy;
mod tables;

use crate::css::Color;
use crate::image::{Image, ImageError, check_dimensions};

pub(crate) fn is_webp(bytes: &[u8]) -> bool {
    bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP"
}

fn error(detail: &str) -> ImageError {
    ImageError::new(format!("webp: {detail}"))
}

fn u24(bytes: &[u8]) -> u32 {
    u32::from(bytes[0]) | (u32::from(bytes[1]) << 8) | (u32::from(bytes[2]) << 16)
}

// the chunks of a RIFF payload, each its four-character code and data
fn chunks(mut bytes: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut found = Vec::new();
    while bytes.len() >= 8 {
        let code = [bytes[0], bytes[1], bytes[2], bytes[3]];
        let size = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
        let end = 8 + size.min(bytes.len() - 8);
        found.push((code, &bytes[8..end]));
        let padded = 8 + size + (size & 1);
        if padded > bytes.len() {
            break;
        }
        bytes = &bytes[padded..];
    }
    found
}

pub(crate) fn decode_webp(bytes: &[u8]) -> Result<Image, ImageError> {
    if !is_webp(bytes) {
        return Err(error("not a RIFF WEBP file"));
    }
    let size = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    let body = &bytes[12..(8 + size).min(bytes.len()).max(12)];
    let parts = chunks(body);
    let find = |code: &[u8; 4]| {
        parts
            .iter()
            .find(|(found, _)| found == code)
            .map(|(_, data)| *data)
    };
    if let Some(extended) = find(b"VP8X") {
        if extended.len() < 10 {
            return Err(error("short VP8X chunk"));
        }
        let (width, height) = (u24(&extended[4..7]) + 1, u24(&extended[7..10]) + 1);
        check_dimensions(width, height).map_err(|_| error("too large"))?;
        // an animation shows its first frame on a transparent canvas
        if let Some(frame) = find(b"ANMF") {
            if frame.len() < 16 {
                return Err(error("short ANMF chunk"));
            }
            let (x, y) = (u24(&frame[0..3]) * 2, u24(&frame[3..6]) * 2);
            let inner = chunks(&frame[16..]);
            let image = decode_frame(&inner)?;
            let mut canvas = Image::new(width, height);
            for row in 0..image.height {
                for column in 0..image.width {
                    let (cx, cy) = (x + column, y + row);
                    if cx < width && cy < height {
                        canvas.pixels[(cy * width + cx) as usize] =
                            image.pixels[(row * image.width + column) as usize];
                    }
                }
            }
            return Ok(canvas);
        }
        return decode_frame(&parts);
    }
    decode_frame(&parts)
}

// one frame of an animation: its place, how long it shows, whether its area clears after it
// and whether it blends over what is there, with the chunks that hold its picture
pub(crate) struct WebpFrame {
    pub(crate) left: u32,
    pub(crate) top: u32,
    pub(crate) duration: u32,
    pub(crate) dispose: bool,
    pub(crate) blend: bool,
    data: Vec<u8>,
}

impl WebpFrame {
    // the frame's picture: its size and straight RGBA
    pub(crate) fn rgba(&self) -> Option<(u32, u32, Vec<u8>)> {
        let image = decode_frame(&chunks(&self.data)).ok()?;
        let rgba = image
            .pixels
            .iter()
            .flat_map(|color| [color.red, color.green, color.blue, color.alpha])
            .collect();
        Some((image.width, image.height, rgba))
    }
}

// the frames of an animated WebP with its canvas size and how many times it plays, None for
// ever; a still image has none
pub(crate) fn webp_frames(bytes: &[u8]) -> Option<(u32, u32, Vec<WebpFrame>, Option<u32>)> {
    if !is_webp(bytes) {
        return None;
    }
    let size = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    let parts = chunks(&bytes[12..(8 + size).min(bytes.len()).max(12)]);
    let extended = parts.iter().find(|(code, _)| code == b"VP8X")?.1;
    if extended.len() < 10 {
        return None;
    }
    let (width, height) = (u24(&extended[4..7]) + 1, u24(&extended[7..10]) + 1);
    let plays = parts
        .iter()
        .find(|(code, _)| code == b"ANIM")
        .map(|(_, data)| data)
        .filter(|data| data.len() >= 6)
        .map_or(Some(1), |data| {
            let count = u32::from(u16::from_le_bytes([data[4], data[5]]));
            (count != 0).then_some(count)
        });
    let frames: Vec<WebpFrame> = parts
        .iter()
        .filter(|(code, data)| code == b"ANMF" && data.len() >= 16)
        .map(|(_, data)| {
            let duration = u24(&data[12..15]);
            WebpFrame {
                left: u24(&data[0..3]) * 2,
                top: u24(&data[3..6]) * 2,
                duration: if duration <= 10 { 100 } else { duration },
                dispose: data[15] & 1 == 1,
                blend: data[15] & 2 == 0,
                data: data[16..].to_vec(),
            }
        })
        .collect();
    (!frames.is_empty()).then_some((width, height, frames, plays))
}

// one still image: a VP8L chunk, or a VP8 chunk with an optional ALPH chunk before it
fn decode_frame(parts: &[([u8; 4], &[u8])]) -> Result<Image, ImageError> {
    let find = |code: &[u8; 4]| {
        parts
            .iter()
            .find(|(found, _)| found == code)
            .map(|(_, data)| *data)
    };
    if let Some(data) = find(b"VP8L") {
        let (width, height, rgba) = lossless::decode(data).ok_or_else(|| error("bad VP8L data"))?;
        return Ok(image_from_rgba(width, height, &rgba));
    }
    let data = find(b"VP8 ").ok_or_else(|| error("no image chunk"))?;
    let planes = lossy::decode(data).ok_or_else(|| error("bad VP8 data"))?;
    let mut rgba = upsample(&planes);
    if let Some(alpha) = find(b"ALPH") {
        let values = decode_alpha(alpha, planes.width, planes.height)
            .ok_or_else(|| error("bad ALPH data"))?;
        for (pixel, value) in rgba.as_chunks_mut::<4>().0.iter_mut().zip(values) {
            pixel[3] = value;
        }
    }
    Ok(image_from_rgba(
        planes.width as u32,
        planes.height as u32,
        &rgba,
    ))
}

// browsers take libwebp's premultiplied output, which truncates; a colour here is the one
// that premultiplies to that same value again when drawn
fn straight(value: u8, alpha: u8) -> u8 {
    let (value, alpha) = (u32::from(value), u32::from(alpha));
    let premultiplied = (value * alpha * 32897) >> 23;
    ((premultiplied * 255 + alpha / 2) / alpha).min(255) as u8
}

fn image_from_rgba(width: u32, height: u32, rgba: &[u8]) -> Image {
    Image {
        width,
        height,
        pixels: rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| match pixel[3] {
                0 => Color {
                    red: 0,
                    green: 0,
                    blue: 0,
                    alpha: 0,
                },
                255 => Color {
                    red: pixel[0],
                    green: pixel[1],
                    blue: pixel[2],
                    alpha: 255,
                },
                alpha => Color {
                    red: straight(pixel[0], alpha),
                    green: straight(pixel[1], alpha),
                    blue: straight(pixel[2], alpha),
                    alpha,
                },
            })
            .collect(),
    }
}

// the alpha plane: raw or lossless-coded, then unfiltered
fn decode_alpha(data: &[u8], width: usize, height: usize) -> Option<Vec<u8>> {
    let header = *data.first()?;
    let (compression, filtering) = (header & 3, (header >> 2) & 3);
    let mut alpha = match compression {
        0 => data.get(1..1 + width * height)?.to_vec(),
        1 => {
            let mut reader = lossless::BitReader::new(&data[1..]);
            let pixels = lossless::decode_stream(&mut reader, width as u32, height as u32, true)?;
            pixels.iter().map(|&pixel| (pixel >> 8) as u8).collect()
        }
        _ => return None,
    };
    if alpha.len() != width * height {
        return None;
    }
    for y in 0..height {
        for x in 0..width {
            let at = y * width + x;
            let left = || alpha[at - 1];
            let above = || alpha[at - width];
            let predicted = match (filtering, x, y) {
                (0, ..) | (_, 0, 0) => 0,
                (_, _, 0) => left(),
                (_, 0, _) => above(),
                (1, ..) => left(),
                (2, ..) => above(),
                _ => (i32::from(left()) + i32::from(above()) - i32::from(alpha[at - width - 1]))
                    .clamp(0, 255) as u8,
            };
            alpha[at] = alpha[at].wrapping_add(predicted);
        }
    }
    Some(alpha)
}

fn mult_hi(value: i32, coefficient: i32) -> i32 {
    (value * coefficient) >> 8
}

fn clip8(value: i32) -> u8 {
    if value & !16383 == 0 {
        (value >> 6) as u8
    } else if value < 0 {
        0
    } else {
        255
    }
}

// libwebp's fixed-point BT.601 conversion
fn yuv_to_rgb(y: u8, u: u8, v: u8) -> [u8; 3] {
    let (y, u, v) = (i32::from(y), i32::from(u), i32::from(v));
    [
        clip8(mult_hi(y, 19077) + mult_hi(v, 26149) - 14234),
        clip8(mult_hi(y, 19077) - mult_hi(u, 6419) - mult_hi(v, 13320) + 8708),
        clip8(mult_hi(y, 19077) + mult_hi(u, 33050) - 17685),
    ]
}

// one or two output rows from their luma rows and the chroma rows above and below them, the
// chroma weighted 9-3-3-1 as libwebp's fancy upsampler does
#[allow(clippy::too_many_arguments)]
fn upsample_pair(
    planes: &Planes,
    top_y: usize,
    bottom_y: Option<usize>,
    top_uv: usize,
    current_uv: usize,
    out: &mut [u8],
    top_row: usize,
    bottom_row: Option<usize>,
) {
    let width = planes.width;
    let load = |row: usize, x: usize| -> u32 {
        let at = row * planes.uv_stride + x;
        u32::from(planes.u[at]) | (u32::from(planes.v[at]) << 16)
    };
    let emit = |out: &mut [u8], luma_row: usize, out_row: usize, x: usize, uv: u32| {
        let y = planes.y[luma_row * planes.y_stride + x];
        let [r, g, b] = yuv_to_rgb(y, (uv & 0xff) as u8, ((uv >> 16) & 0xff) as u8);
        let at = (out_row * width + x) * 4;
        out[at..at + 4].copy_from_slice(&[r, g, b, 255]);
    };
    let mut top_left = load(top_uv, 0);
    let mut left = load(current_uv, 0);
    emit(
        out,
        top_y,
        top_row,
        0,
        (3 * top_left + left + 0x0002_0002) >> 2,
    );
    if let (Some(bottom_y), Some(bottom_row)) = (bottom_y, bottom_row) {
        emit(
            out,
            bottom_y,
            bottom_row,
            0,
            (3 * left + top_left + 0x0002_0002) >> 2,
        );
    }
    let last_pair = (width - 1) >> 1;
    for x in 1..=last_pair {
        let top = load(top_uv, x);
        let current = load(current_uv, x);
        let average = top_left + top + left + current + 0x0008_0008;
        let diagonal_12 = (average + 2 * (top + left)) >> 3;
        let diagonal_03 = (average + 2 * (top_left + current)) >> 3;
        emit(
            out,
            top_y,
            top_row,
            2 * x - 1,
            (diagonal_12 + top_left) >> 1,
        );
        emit(out, top_y, top_row, 2 * x, (diagonal_03 + top) >> 1);
        if let (Some(bottom_y), Some(bottom_row)) = (bottom_y, bottom_row) {
            emit(
                out,
                bottom_y,
                bottom_row,
                2 * x - 1,
                (diagonal_03 + left) >> 1,
            );
            emit(
                out,
                bottom_y,
                bottom_row,
                2 * x,
                (diagonal_12 + current) >> 1,
            );
        }
        top_left = top;
        left = current;
    }
    if width & 1 == 0 {
        emit(
            out,
            top_y,
            top_row,
            width - 1,
            (3 * top_left + left + 0x0002_0002) >> 2,
        );
        if let (Some(bottom_y), Some(bottom_row)) = (bottom_y, bottom_row) {
            emit(
                out,
                bottom_y,
                bottom_row,
                width - 1,
                (3 * left + top_left + 0x0002_0002) >> 2,
            );
        }
    }
}

use lossy::Planes;

// RGBA from the planes: the first row alone, then rows in pairs between two chroma rows, and
// the last row alone when the height is even
fn upsample(planes: &Planes) -> Vec<u8> {
    let (width, height) = (planes.width, planes.height);
    let mut out = vec![0_u8; width * height * 4];
    upsample_pair(planes, 0, None, 0, 0, &mut out, 0, None);
    let mut y = 0;
    while y + 2 < height {
        let chroma = y / 2;
        upsample_pair(
            planes,
            y + 1,
            Some(y + 2),
            chroma,
            chroma + 1,
            &mut out,
            y + 1,
            Some(y + 2),
        );
        y += 2;
    }
    if height & 1 == 0 && height > 1 {
        let chroma = (height - 1) / 2;
        upsample_pair(
            planes,
            height - 1,
            None,
            chroma,
            chroma,
            &mut out,
            height - 1,
            None,
        );
    }
    out
}

// a lossless WebP file of RGBA pixels, for canvas exports
pub(crate) fn encode_webp(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    if width == 0 || height == 0 || width > 16384 || height > 16384 {
        return None;
    }
    let data = lossless::encode(width, height, rgba);
    let padded = data.len() + (data.len() & 1);
    let mut file = Vec::with_capacity(20 + padded);
    file.extend_from_slice(b"RIFF");
    file.extend_from_slice(&((4 + 8 + padded) as u32).to_le_bytes());
    file.extend_from_slice(b"WEBPVP8L");
    file.extend_from_slice(&(data.len() as u32).to_le_bytes());
    file.extend_from_slice(&data);
    if data.len() & 1 == 1 {
        file.push(0);
    }
    Some(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lossless_files_round_trip() {
        let (width, height) = (37_u32, 23_u32);
        let mut rgba = Vec::new();
        for y in 0..height {
            for x in 0..width {
                // flat areas, a gradient and some translucency exercise copies and literals
                let flat = x < 12;
                rgba.extend_from_slice(&[
                    if flat { 200 } else { (x * 7) as u8 },
                    (y * 11) as u8,
                    if flat { 30 } else { (x * y) as u8 },
                    if y > 18 { 128 } else { 255 },
                ]);
            }
        }
        let file = encode_webp(width, height, &rgba).expect("a file");
        // translucent colours come back as browsers premultiply them
        let expected: Vec<u8> = rgba
            .chunks(4)
            .flat_map(|pixel| {
                if pixel[3] == 255 {
                    [pixel[0], pixel[1], pixel[2], 255]
                } else {
                    [
                        straight(pixel[0], pixel[3]),
                        straight(pixel[1], pixel[3]),
                        straight(pixel[2], pixel[3]),
                        pixel[3],
                    ]
                }
            })
            .collect();
        assert!(is_webp(&file));
        let image = decode_webp(&file).expect("the file decodes");
        assert_eq!((image.width, image.height), (width, height));
        let decoded: Vec<u8> = image
            .pixels
            .iter()
            .flat_map(|color| [color.red, color.green, color.blue, color.alpha])
            .collect();
        let first = decoded
            .chunks(4)
            .zip(expected.chunks(4))
            .position(|(a, b)| a != b);
        assert_eq!(
            first,
            None,
            "pixel {:?} is {:?}, not {:?}",
            first,
            first.map(|at| &decoded[at * 4..at * 4 + 4]),
            first.map(|at| &expected[at * 4..at * 4 + 4])
        );
    }

    #[test]
    fn colours_convert_as_libwebp_does() {
        assert_eq!(yuv_to_rgb(16, 128, 128), [0, 0, 0]);
        assert_eq!(yuv_to_rgb(235, 128, 128), [255, 255, 255]);
        assert_eq!(yuv_to_rgb(81, 90, 240), [254, 0, 0]);
    }
}
