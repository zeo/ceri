// a canvas element's 2d context over a premultiplied RGBA bitmap, drawn the way Skia draws
// it on the cpu: rectangles cover pixels by area, and scaled pictures sample as chromium's
// image smoothing settings choose, measured against its output

use super::typed::{ElementKind, new_typed};
use super::*;
use crate::dom::Namespace;
use crate::image::Image;
use crate::layout::ImageLayer;

type Rect4 = (f32, f32, f32, f32);
// a drawImage source's pixels and the width and height script measures it in
type Measured = (Bitmap, (f32, f32));

const METHODS: &[&str] = &[
    "fillRect",
    "clearRect",
    "drawImage",
    "getImageData",
    "putImageData",
    "createImageData",
];

#[derive(Clone, Default)]
pub(super) struct Bitmap {
    width: u32,
    height: u32,
    pixels: Vec<[u8; 4]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Smoothing {
    Low,
    Medium,
    High,
}

impl Smoothing {
    fn name(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

// how drawImage samples its source, as chromium picks it from the smoothing settings and
// whether the draw is a strict upscale
#[derive(Clone, Copy, Debug, PartialEq)]
enum Sampling {
    Nearest,
    // skia's legacy bilinear: 16.16 fixed point positions with four bits of subpixel
    FixedBilinear,
    Bilinear,
    // linear between bilinear samples of the two mip levels around this level
    Mipmapped(f32),
    Mitchell,
}

pub(super) struct CanvasContextData {
    node: NodeId,
    fill_style: Color,
    smoothing_enabled: bool,
    smoothing_quality: Smoothing,
    bitmap: Bitmap,
    // the bitmap as paint reads it, rebuilt after the next change
    snapshot: Option<std::sync::Arc<Image>>,
}

fn mul255(a: u32, b: u32) -> u32 {
    let product = a * b + 128;
    (product + (product >> 8)) >> 8
}

fn premultiply([red, green, blue, alpha]: [u8; 4]) -> [u8; 4] {
    let scale = |channel: u8| mul255(channel.into(), alpha.into()) as u8;
    [scale(red), scale(green), scale(blue), alpha]
}

fn unpremultiply([red, green, blue, alpha]: [u8; 4]) -> [u8; 4] {
    if alpha == 0 {
        return [0; 4];
    }
    let alpha32 = u32::from(alpha);
    let scale = |channel: u8| ((u32::from(channel) * 255 + alpha32 / 2) / alpha32).min(255) as u8;
    [scale(red), scale(green), scale(blue), alpha]
}

fn scale(pixel: [u8; 4], amount: u32) -> [u8; 4] {
    pixel.map(|channel| mul255(channel.into(), amount) as u8)
}

fn source_over(pixel: &mut [u8; 4], source: [u8; 4]) {
    let inverse = 255 - u32::from(source[3]);
    for (channel, source) in pixel.iter_mut().zip(source) {
        *channel = (u32::from(source) + mul255((*channel).into(), inverse)).min(255) as u8;
    }
}

// the share of the pixel starting at `pixel` that the span from start to end covers
fn overlap(start: f32, end: f32, pixel: i64) -> f32 {
    (end.min(pixel as f32 + 1.0) - start.max(pixel as f32)).max(0.0)
}

// a rectangle with its width and height made positive, as the canvas methods take them
fn normalized((x, y, width, height): Rect4) -> Rect4 {
    (
        x.min(x + width),
        y.min(y + height),
        width.abs(),
        height.abs(),
    )
}

impl CanvasContextData {
    pub(super) fn set_smoothing_enabled(&mut self, enabled: bool) {
        self.smoothing_enabled = enabled;
    }

    // chromium's choice: no smoothing is nearest, low the legacy bilinear, and medium or high a
    // linear mipmap blend unless the draw grows both ways, where high turns cubic
    fn sampling(&self, from: Rect4, to: Rect4) -> Sampling {
        if !self.smoothing_enabled {
            return Sampling::Nearest;
        }
        let upscale = to.2 > from.2 && to.3 > from.3;
        match (self.smoothing_quality, upscale) {
            (Smoothing::Low, _) => Sampling::FixedBilinear,
            (Smoothing::Medium, true) => Sampling::Bilinear,
            (Smoothing::High, true) => Sampling::Mitchell,
            (_, false) => {
                // the geometric mean of the two shrinks picks the level, half a level finer
                let shrink = (from.2 / to.2) * (from.3 / to.3);
                let level = 0.5 * shrink.log2() - 0.5;
                if level > 0.0 {
                    Sampling::Mipmapped(level)
                } else {
                    Sampling::Bilinear
                }
            }
        }
    }
}

impl Bitmap {
    pub(super) fn new(width: u32, height: u32) -> Self {
        // a canvas too large to allocate draws nothing, as when a browser cannot back one
        if width > 0 && height > 0 && crate::image::check_dimensions(width, height).is_err() {
            return Self::default();
        }
        Self {
            width,
            height,
            pixels: vec![[0; 4]; width as usize * height as usize],
        }
    }

    fn from_image(image: &Image) -> Self {
        Self {
            width: image.width,
            height: image.height,
            pixels: image
                .pixels
                .iter()
                .map(|color| premultiply([color.red, color.green, color.blue, color.alpha]))
                .collect(),
        }
    }

    fn to_image(&self) -> Image {
        Image {
            width: self.width,
            height: self.height,
            pixels: self
                .pixels
                .iter()
                .map(|&pixel| {
                    let [red, green, blue, alpha] = unpremultiply(pixel);
                    Color {
                        red,
                        green,
                        blue,
                        alpha,
                    }
                })
                .collect(),
        }
    }

    // calls paint with every pixel the rectangle reaches and how much of it, out of 255
    fn cover(&mut self, rect: Rect4, mut paint: impl FnMut(&mut [u8; 4], u32, i64, i64)) {
        let (left, top, width, height) = normalized(rect);
        let (right, bottom) = (left + width, top + height);
        let columns = left.floor().max(0.0) as i64..right.ceil().min(self.width as f32) as i64;
        let rows = top.floor().max(0.0) as i64..bottom.ceil().min(self.height as f32) as i64;
        for row in rows {
            let vertical = overlap(top, bottom, row);
            for column in columns.clone() {
                let coverage = (overlap(left, right, column) * vertical * 255.0).round() as u32;
                if coverage > 0 {
                    let index = row as usize * self.width as usize + column as usize;
                    paint(&mut self.pixels[index], coverage, column, row);
                }
            }
        }
    }

    fn fill_rect(&mut self, rect: Rect4, color: Color) {
        let source = premultiply([color.red, color.green, color.blue, color.alpha]);
        self.cover(rect, |pixel, coverage, _, _| {
            source_over(pixel, scale(source, coverage))
        });
    }

    fn clear_rect(&mut self, rect: Rect4) {
        self.cover(rect, |pixel, coverage, _, _| {
            *pixel = scale(*pixel, 255 - coverage)
        });
    }

    // the premultiplied colour at a point, mixed from the four nearest pixels with the
    // picture's edges extended outward
    fn sample(&self, x: f32, y: f32) -> [u8; 4] {
        let (x, y) = (x - 0.5, y - 0.5);
        let (fx, fy) = (x - x.floor(), y - y.floor());
        let clamp = |value: f32, limit: u32| (value.max(0.0) as u32).min(limit - 1) as usize;
        let (left, right) = (
            clamp(x.floor(), self.width),
            clamp(x.floor() + 1.0, self.width),
        );
        let (top, bottom) = (
            clamp(y.floor(), self.height),
            clamp(y.floor() + 1.0, self.height),
        );
        let at = |column: usize, row: usize| self.pixels[row * self.width as usize + column];
        let corners = [
            at(left, top),
            at(right, top),
            at(left, bottom),
            at(right, bottom),
        ];
        std::array::from_fn(|channel| {
            let [a, b, c, d] = corners.map(|pixel| f32::from(pixel[channel]));
            let upper = a + (b - a) * fx;
            let lower = c + (d - c) * fx;
            (upper + (lower - upper) * fy).round() as u8
        })
    }

    // the source position in 16.16 fixed point, from coordinates mapped in f32 as skia maps them
    fn fixed(position: f32) -> i64 {
        (position * 65536.0).floor() as i64
    }

    fn nearest(&self, u: f32, v: f32) -> [u8; 4] {
        // a position on a pixel's left edge belongs to the pixel before it
        let column = ((Self::fixed(u) - 1) >> 16).clamp(0, i64::from(self.width) - 1);
        let row = ((Self::fixed(v) - 1) >> 16).clamp(0, i64::from(self.height) - 1);
        self.pixels[row as usize * self.width as usize + column as usize]
    }

    fn fixed_bilinear(&self, u: f32, v: f32) -> [u8; 4] {
        let x = Self::fixed(u) - (1 << 15);
        let y = Self::fixed(v) - (1 << 15);
        let (left, top) = (x >> 16, y >> 16);
        let (sub_x, sub_y) = (((x >> 12) & 15) as u32, ((y >> 12) & 15) as u32);
        let at = |column: i64, row: i64| {
            let column = column.clamp(0, i64::from(self.width) - 1) as usize;
            let row = row.clamp(0, i64::from(self.height) - 1) as usize;
            self.pixels[row * self.width as usize + column]
        };
        let corners = [
            (at(left, top), (16 - sub_x) * (16 - sub_y)),
            (at(left + 1, top), sub_x * (16 - sub_y)),
            (at(left, top + 1), (16 - sub_x) * sub_y),
            (at(left + 1, top + 1), sub_x * sub_y),
        ];
        std::array::from_fn(|channel| {
            let total: u32 = corners
                .iter()
                .map(|(pixel, weight)| u32::from(pixel[channel]) * weight)
                .sum();
            (total >> 8) as u8
        })
    }

    // mitchell-netravali with b = c = 1/3 over the 4x4 pixels around a point
    fn mitchell(&self, u: f32, v: f32) -> [u8; 4] {
        fn weight(distance: f32) -> f32 {
            let t = distance.abs();
            let (b, c) = (1.0 / 3.0, 1.0 / 3.0);
            if t < 1.0 {
                ((12.0 - 9.0 * b - 6.0 * c) * t * t * t
                    + (-18.0 + 12.0 * b + 6.0 * c) * t * t
                    + (6.0 - 2.0 * b))
                    / 6.0
            } else if t < 2.0 {
                ((-b - 6.0 * c) * t * t * t
                    + (6.0 * b + 30.0 * c) * t * t
                    + (-12.0 * b - 48.0 * c) * t
                    + (8.0 * b + 24.0 * c))
                    / 6.0
            } else {
                0.0
            }
        }
        let (x, y) = (u - 0.5, v - 0.5);
        let (left, top) = (x.floor(), y.floor());
        let mut sum = [0.0f32; 4];
        for row in -1..3 {
            let wy = weight(y - (top + row as f32));
            let source_row = (top as i64 + row).clamp(0, i64::from(self.height) - 1) as usize;
            for column in -1..3 {
                let w = weight(x - (left + column as f32)) * wy;
                let source_column =
                    (left as i64 + column).clamp(0, i64::from(self.width) - 1) as usize;
                let pixel = self.pixels[source_row * self.width as usize + source_column];
                for (total, channel) in sum.iter_mut().zip(pixel) {
                    *total += w * f32::from(channel);
                }
            }
        }
        // a cubic overshoots, and premultiplied colour may never pass its alpha
        let alpha = sum[3].round().clamp(0.0, 255.0);
        std::array::from_fn(|channel| match channel {
            3 => alpha as u8,
            _ => sum[channel].round().clamp(0.0, alpha) as u8,
        })
    }

    // the next mip level: each pixel the rounded mean of the 2x2 block above it, the odd last
    // row or column repeating its neighbour
    fn halved(&self) -> Self {
        let (width, height) = ((self.width / 2).max(1), (self.height / 2).max(1));
        let at = |column: u32, row: u32| {
            let (column, row) = (column.min(self.width - 1), row.min(self.height - 1));
            self.pixels[(row * self.width + column) as usize]
        };
        let pixels = (0..height)
            .flat_map(|row| (0..width).map(move |column| (column, row)))
            .map(|(column, row)| {
                let block = [
                    at(column * 2, row * 2),
                    at(column * 2 + 1, row * 2),
                    at(column * 2, row * 2 + 1),
                    at(column * 2 + 1, row * 2 + 1),
                ];
                std::array::from_fn(|channel| {
                    let total: u32 = block.iter().map(|pixel| u32::from(pixel[channel])).sum();
                    ((total + 2) / 4) as u8
                })
            })
            .collect();
        Self {
            width,
            height,
            pixels,
        }
    }

    fn draw_image(&mut self, source: &Bitmap, from: Rect4, to: Rect4, sampling: Sampling) {
        let (mut sx, mut sy, mut sw, mut sh) = normalized(from);
        let (mut dx, mut dy, mut dw, mut dh) = normalized(to);
        if sw == 0.0 || sh == 0.0 || source.pixels.is_empty() {
            return;
        }
        // a source rectangle reaching past the picture shrinks, and the destination with it
        let (ratio_x, ratio_y) = (dw / sw, dh / sh);
        let (left, right) = (sx.max(0.0), (sx + sw).min(source.width as f32));
        let (top, bottom) = (sy.max(0.0), (sy + sh).min(source.height as f32));
        if right <= left || bottom <= top {
            return;
        }
        dx += (left - sx) * ratio_x;
        dy += (top - sy) * ratio_y;
        (dw, dh) = ((right - left) * ratio_x, (bottom - top) * ratio_y);
        (sx, sy, sw, sh) = (left, top, right - left, bottom - top);
        // the levels a mipmapped draw blends, each with its size relative to the source
        let levels = match sampling {
            Sampling::Mipmapped(level) => {
                let lower = level.floor() as usize;
                let mut chain = vec![source.clone()];
                while chain.len() <= lower + 1
                    && chain
                        .last()
                        .is_some_and(|last| last.width > 1 || last.height > 1)
                {
                    let next = chain
                        .last()
                        .expect("the chain starts with the source")
                        .halved();
                    chain.push(next);
                }
                let upper = chain.len().min(lower + 2) - 1;
                Some((chain, lower.min(upper), upper, level - level.floor()))
            }
            _ => None,
        };
        let (scale_x, scale_y) = (sw / dw, sh / dh);
        self.cover((dx, dy, dw, dh), |pixel, coverage, column, row| {
            let u = sx + (column as f32 + 0.5 - dx) * scale_x;
            let v = sy + (row as f32 + 0.5 - dy) * scale_y;
            let sample = match (&levels, sampling) {
                (_, Sampling::Nearest) => source.nearest(u, v),
                (_, Sampling::FixedBilinear) => source.fixed_bilinear(u, v),
                (_, Sampling::Mitchell) => source.mitchell(u, v),
                (Some((chain, lower, upper, blend)), _) => {
                    let at = |level: &Bitmap| {
                        let (ratio_x, ratio_y) = (
                            level.width as f32 / source.width as f32,
                            level.height as f32 / source.height as f32,
                        );
                        level.sample(u * ratio_x, v * ratio_y)
                    };
                    let (near, far) = (at(&chain[*lower]), at(&chain[*upper]));
                    std::array::from_fn(|channel| {
                        let (near, far) = (f32::from(near[channel]), f32::from(far[channel]));
                        (near + (far - near) * blend).round() as u8
                    })
                }
                _ => source.sample(u, v),
            };
            source_over(pixel, scale(sample, coverage));
        });
    }

    // a PNG, or for "image/jpeg" a JPEG blended onto black, which premultiplied colour already is
    fn encode(&self, mime: &str, quality: u8) -> Option<Vec<u8>> {
        if self.pixels.is_empty() {
            return None;
        }
        if mime == "image/jpeg" {
            let rgb = self
                .pixels
                .iter()
                .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]]);
            return crate::jpeg::encode_jpeg(
                self.width,
                self.height,
                &rgb.collect::<Vec<_>>(),
                quality,
            );
        }
        let rgba = self.image_data(0, 0, self.width, self.height);
        Some(crate::png::encode_png(self.width, self.height, 4, &rgba))
    }

    // straight RGBA for a region, transparent black wherever it leaves the bitmap
    fn image_data(&self, x: i64, y: i64, width: u32, height: u32) -> Vec<u8> {
        let mut data = vec![0; width as usize * height as usize * 4];
        for row in 0..i64::from(height) {
            let source_row = y + row;
            if !(0..i64::from(self.height)).contains(&source_row) {
                continue;
            }
            for column in 0..i64::from(width) {
                let source_column = x + column;
                if !(0..i64::from(self.width)).contains(&source_column) {
                    continue;
                }
                let pixel =
                    self.pixels[source_row as usize * self.width as usize + source_column as usize];
                let at = (row as usize * width as usize + column as usize) * 4;
                data[at..at + 4].copy_from_slice(&unpremultiply(pixel));
            }
        }
        data
    }

    // writes straight RGBA from an image data's dirty rectangle, replacing what was there
    fn put_image_data(&mut self, data: &[u8], size: (u32, u32), at: (i64, i64), dirty: Rect4) {
        let (x, y, width, height) = normalized(dirty);
        let (left, top) = (x.max(0.0) as i64, y.max(0.0) as i64);
        let right = ((x + width) as i64).min(i64::from(size.0));
        let bottom = ((y + height) as i64).min(i64::from(size.1));
        for row in top..bottom {
            let target_row = at.1 + row;
            if !(0..i64::from(self.height)).contains(&target_row) {
                continue;
            }
            for column in left..right {
                let target_column = at.0 + column;
                if !(0..i64::from(self.width)).contains(&target_column) {
                    continue;
                }
                let from = (row as usize * size.0 as usize + column as usize) * 4;
                let pixel = [data[from], data[from + 1], data[from + 2], data[from + 3]];
                self.pixels[target_row as usize * self.width as usize + target_column as usize] =
                    premultiply(pixel);
            }
        }
    }
}

// the bitmap size a canvas element's width and height attributes ask for
pub(crate) fn canvas_size(document: &Document, node: NodeId) -> (u32, u32) {
    let side = |name: &str, default: u32| {
        document
            .attribute(node, name)
            .and_then(non_negative_integer)
            .unwrap_or(default)
    };
    (side("width", 300), side("height", 150))
}

// the HTML rules for parsing a non-negative integer, within the range a reflected
// unsigned long keeps
fn non_negative_integer(value: &str) -> Option<u32> {
    let digits = value.trim_start_matches(|character: char| character.is_ascii_whitespace());
    let digits = digits.strip_prefix('+').unwrap_or(digits);
    let end = digits
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(digits.len());
    digits[..end]
        .parse::<u32>()
        .ok()
        .filter(|&value| value <= i32::MAX as u32)
}

pub(crate) fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let word = chunk
            .iter()
            .zip([16, 8, 0])
            .fold(0_u32, |word, (&byte, shift)| {
                word | u32::from(byte) << shift
            });
        for index in 0..4 {
            text.push(if index <= chunk.len() {
                char::from(ALPHABET[(word >> (18 - 6 * index) & 63) as usize])
            } else {
                '='
            });
        }
    }
    text
}

fn is_html(document: &Document, node: NodeId, name: &str) -> bool {
    document
        .element(node)
        .is_some_and(|element| element.name == name && element.namespace == Namespace::Html)
}

impl ScriptRuntime {
    pub(super) fn canvas_context(&mut self, node: NodeId) -> CanvasContextHandle {
        let (width, height) = canvas_size(&self.document.borrow(), node);
        self.canvas_contexts
            .entry(node)
            .or_insert_with(|| {
                Rc::new(RefCell::new(CanvasContextData {
                    node,
                    fill_style: Color::BLACK,
                    smoothing_enabled: true,
                    smoothing_quality: Smoothing::Low,
                    bitmap: Bitmap::new(width, height),
                    snapshot: None,
                }))
            })
            .clone()
    }

    // setting width or height, even to the value it had, clears the bitmap and the context
    pub(super) fn reset_canvas(&mut self, node: NodeId) {
        let Some(context) = self.canvas_contexts.get(&node) else {
            return;
        };
        let (width, height) = canvas_size(&self.document.borrow(), node);
        let mut context = context.borrow_mut();
        context.bitmap = Bitmap::new(width, height);
        context.fill_style = Color::BLACK;
        context.snapshot = None;
    }

    // a layer for each connected canvas with a bitmap, for paint to place like an img
    pub(crate) fn canvas_layers(&self) -> Vec<ImageLayer> {
        let document = self.document.borrow();
        self.canvas_contexts
            .values()
            .filter_map(|context| {
                let mut context = context.borrow_mut();
                let CanvasContextData {
                    node,
                    bitmap,
                    snapshot,
                    ..
                } = &mut *context;
                if bitmap.pixels.is_empty() || !document.is_connected(*node) {
                    return None;
                }
                let image = snapshot.get_or_insert_with(|| std::sync::Arc::new(bitmap.to_image()));
                Some(ImageLayer {
                    node: *node,
                    image: image.clone(),
                    background: None,
                })
            })
            .collect()
    }

    pub(super) fn canvas_element_member(
        &mut self,
        handle: &ElementHandle,
        name: &str,
        value: Option<Value>,
    ) -> Result<Value, ScriptError> {
        let node = handle.id;
        let (width, height) = canvas_size(&self.document.borrow(), node);
        Ok(match (name, value) {
            ("width", None) => Value::Number(f64::from(width)),
            ("height", None) => Value::Number(f64::from(height)),
            (_, Some(value)) => {
                let size = self.coerce_uint32(value)?;
                let default = if name == "width" { 300 } else { 150 };
                let size = if size > i32::MAX as u32 {
                    default
                } else {
                    size
                };
                self.set_element_attribute(handle, name, size.to_string());
                Value::Undefined
            }
            _ => Value::Undefined,
        })
    }

    // toDataURL(type, quality) and toBlob(callback, type, quality); types other than PNG and
    // JPEG fall back to PNG, and quality outside 0 to 1 means Chromium's default of 92
    pub(super) fn export_canvas(
        &mut self,
        node: NodeId,
        name: &str,
        arguments: &[Value],
    ) -> Result<Value, ScriptError> {
        let callback = arguments.first().cloned().unwrap_or(Value::Undefined);
        if name == "toBlob" && !is_callable(&callback) {
            return Err(self.type_error(
                "Failed to execute 'toBlob' on 'HTMLCanvasElement': parameter 1 is not of type 'BlobCallback'.",
            ));
        }
        let options = arguments
            .get(usize::from(name == "toBlob")..)
            .unwrap_or_default();
        let mime = match options.first() {
            None | Some(Value::Undefined) => "image/png",
            Some(value)
                if self
                    .coerce_string(value.clone())?
                    .eq_ignore_ascii_case("image/jpeg") =>
            {
                "image/jpeg"
            }
            Some(_) => "image/png",
        };
        let quality = match options.get(1) {
            Some(&Value::Number(quality)) if (0.0..=1.0).contains(&quality) => {
                (quality * 100.0 + 0.5) as u8
            }
            _ => 92,
        };
        let encoded = match self.canvas_contexts.get(&node) {
            Some(context) => context.borrow().bitmap.encode(mime, quality),
            None => {
                let (width, height) = canvas_size(&self.document.borrow(), node);
                Bitmap::new(width, height).encode(mime, quality)
            }
        };
        if name == "toDataURL" {
            return Ok(Value::String(
                encoded
                    .map_or_else(
                        || "data:,".to_string(),
                        |bytes| format!("data:{mime};base64,{}", base64(&bytes)),
                    )
                    .into(),
            ));
        }
        let blob = encoded.map_or(Value::Null, |bytes| {
            Value::Blob(Rc::new(BlobData {
                bytes,
                mime_type: mime.to_string(),
            }))
        });
        self.schedule_callback(callback, vec![blob]);
        Ok(Value::Undefined)
    }

    pub(super) fn canvas_context_property(
        &self,
        context: &CanvasContextHandle,
        property: &str,
    ) -> Result<Value, ScriptError> {
        if let Some(&name) = METHODS.iter().find(|&&name| name == property) {
            return Ok(Value::Builtin(Builtin::CanvasMethod(context.clone(), name)));
        }
        Ok(match property {
            "canvas" => self.node_value(context.borrow().node),
            "fillStyle" => Value::String(color_css(context.borrow().fill_style).into()),
            "imageSmoothingEnabled" => Value::Bool(context.borrow().smoothing_enabled),
            "imageSmoothingQuality" => {
                Value::String(context.borrow().smoothing_quality.name().into())
            }
            _ => {
                return Err(runtime_error(format!(
                    "CanvasRenderingContext2D has no property {property}"
                )));
            }
        })
    }

    pub(super) fn set_canvas_fill_style(
        &mut self,
        context: &CanvasContextHandle,
        value: &str,
    ) -> bool {
        let Some(color) = Color::parse(value) else {
            return false;
        };
        context.borrow_mut().fill_style = color;
        true
    }

    // an enum attribute ignores a value outside its set, as webidl has it
    pub(super) fn set_canvas_smoothing_quality(context: &CanvasContextHandle, value: &str) {
        let quality = match value {
            "low" => Smoothing::Low,
            "medium" => Smoothing::Medium,
            "high" => Smoothing::High,
            _ => return,
        };
        context.borrow_mut().smoothing_quality = quality;
    }

    pub(super) fn call_canvas_method(
        &mut self,
        context: &CanvasContextHandle,
        name: &str,
        arguments: &[Value],
    ) -> Result<Value, ScriptError> {
        match name {
            "fillRect" | "clearRect" => {
                if let Some(&[x, y, width, height]) =
                    self.canvas_numbers(name, arguments, 4)?.as_deref()
                {
                    let mut context = context.borrow_mut();
                    let fill = context.fill_style;
                    match name {
                        "fillRect" => context.bitmap.fill_rect((x, y, width, height), fill),
                        _ => context.bitmap.clear_rect((x, y, width, height)),
                    }
                    context.snapshot = None;
                }
                Ok(Value::Undefined)
            }
            "drawImage" => {
                self.draw_canvas_image(context, arguments)?;
                Ok(Value::Undefined)
            }
            "getImageData" => self.get_image_data(context, arguments),
            "createImageData" => self.create_image_data(arguments),
            _ => {
                self.put_image_data(context, arguments)?;
                Ok(Value::Undefined)
            }
        }
    }

    // the arguments as numbers, None when one is not finite, which makes the call do nothing
    fn canvas_numbers(
        &mut self,
        method: &str,
        arguments: &[Value],
        count: usize,
    ) -> Result<Option<Vec<f32>>, ScriptError> {
        if arguments.len() < count {
            return Err(self.type_error(format!(
                "Failed to execute '{method}' on 'CanvasRenderingContext2D': {count} arguments required, but only {} present.",
                arguments.len()
            )));
        }
        let mut numbers = Vec::with_capacity(count);
        for value in &arguments[..count] {
            numbers.push(self.coerce_number(value.clone())?);
        }
        Ok(numbers
            .iter()
            .all(|number| number.is_finite())
            .then(|| numbers.iter().map(|&number| number as f32).collect()))
    }

    // the picture a drawImage source shows now and the size script measures it in, which for a
    // video with anamorphic pixels differs from the picture's; None while it has nothing
    fn canvas_source(&mut self, source: &Value) -> Result<Option<Measured>, ScriptError> {
        let node = match source {
            Value::Element(handle) => Some(handle.id),
            _ => None,
        };
        let document = self.document.borrow();
        let kind = node.and_then(|node| {
            ["img", "canvas", "video"]
                .into_iter()
                .find(|&name| is_html(&document, node, name))
        });
        drop(document);
        let (Some(node), Some(kind)) = (node, kind) else {
            return Err(self.type_error(
                "Failed to execute 'drawImage' on 'CanvasRenderingContext2D': The provided value is not of type '(CSSImageValue or HTMLCanvasElement or HTMLImageElement or HTMLVideoElement or ImageBitmap or OffscreenCanvas or SVGImageElement or VideoFrame)'.",
            ));
        };
        let bitmap = match kind {
            "img" => match self.image_content(node) {
                Ok(image) => Ok(image.map(|image| Bitmap::from_image(&image))),
                Err(()) => Err(self.throw_value(dom_exception(
                    "InvalidStateError",
                    "Failed to execute 'drawImage' on 'CanvasRenderingContext2D': The HTMLImageElement provided is in the 'broken' state.",
                ))),
            },
            "canvas" => {
                let (width, height) = canvas_size(&self.document.borrow(), node);
                if width == 0 || height == 0 {
                    return Err(self.throw_value(dom_exception(
                        "InvalidStateError",
                        "Failed to execute 'drawImage' on 'CanvasRenderingContext2D': The image argument is a canvas element with a width or height of 0.",
                    )));
                }
                Ok(self
                    .canvas_contexts
                    .get(&node)
                    .map(|context| context.borrow().bitmap.clone()))
            }
            // a video with no frame decoded yet has nothing to draw
            _ => {
                let natural = self.video_size(node);
                return Ok(self.video_content(node).map(|image| {
                    let natural = (natural.0 as f32, natural.1 as f32);
                    (Bitmap::from_image(&image), natural)
                }));
            }
        }?;
        Ok(bitmap.map(|bitmap| {
            let size = (bitmap.width as f32, bitmap.height as f32);
            (bitmap, size)
        }))
    }

    fn draw_canvas_image(
        &mut self,
        context: &CanvasContextHandle,
        arguments: &[Value],
    ) -> Result<(), ScriptError> {
        let count = arguments.len().min(9);
        if !matches!(count, 3 | 5 | 9) {
            return Err(self.type_error(format!(
                "Failed to execute 'drawImage' on 'CanvasRenderingContext2D': Valid arities are: [3, 5, 9], but {} arguments provided.",
                arguments.len()
            )));
        }
        let source = self.canvas_source(&arguments[0])?;
        let Some(numbers) = self.canvas_numbers("drawImage", &arguments[1..], count - 1)? else {
            return Ok(());
        };
        let Some((source, natural)) = source else {
            return Ok(());
        };
        let whole = (0.0, 0.0, natural.0, natural.1);
        let (from, to) = match numbers[..] {
            [x, y] => (whole, (x, y, whole.2, whole.3)),
            [x, y, width, height] => (whole, (x, y, width, height)),
            [sx, sy, sw, sh, dx, dy, dw, dh] => ((sx, sy, sw, sh), (dx, dy, dw, dh)),
            _ => return Ok(()),
        };
        let (x, y) = (
            source.width as f32 / natural.0,
            source.height as f32 / natural.1,
        );
        let from = (from.0 * x, from.1 * y, from.2 * x, from.3 * y);
        let mut context = context.borrow_mut();
        let sampling = context.sampling(normalized(from), normalized(to));
        context.bitmap.draw_image(&source, from, to, sampling);
        context.snapshot = None;
        Ok(())
    }

    // the WebIDL long conversion getImageData and putImageData take their coordinates through
    fn canvas_longs(
        &mut self,
        method: &str,
        arguments: &[Value],
        count: usize,
    ) -> Result<Vec<i64>, ScriptError> {
        if arguments.len() < count {
            return Err(self.type_error(format!(
                "Failed to execute '{method}' on 'CanvasRenderingContext2D': {count} arguments required, but only {} present.",
                arguments.len()
            )));
        }
        let mut numbers = Vec::with_capacity(count);
        for value in &arguments[..count] {
            numbers.push(i64::from(self.coerce_int32(value.clone())?));
        }
        Ok(numbers)
    }

    fn get_image_data(
        &mut self,
        context: &CanvasContextHandle,
        arguments: &[Value],
    ) -> Result<Value, ScriptError> {
        let values = self.canvas_longs("getImageData", arguments, 4)?;
        let (mut x, mut y, mut width, mut height) = (values[0], values[1], values[2], values[3]);
        for (extent, name) in [(width, "width"), (height, "height")] {
            if extent == 0 {
                return Err(self.throw_value(dom_exception(
                    "IndexSizeError",
                    &format!("Failed to execute 'getImageData' on 'CanvasRenderingContext2D': The source {name} is 0."),
                )));
            }
        }
        if width < 0 {
            (x, width) = (x + width, -width);
        }
        if height < 0 {
            (y, height) = (y + height, -height);
        }
        let (width, height) = (width as u32, height as u32);
        if crate::image::check_dimensions(width, height).is_err() {
            return Err(self.range_error("Out of memory at ImageData creation"));
        }
        let data = context.borrow().bitmap.image_data(x, y, width, height);
        Ok(image_data_object(width, height, data))
    }

    // transparent black pixels of the size given, or of the size of another ImageData
    fn create_image_data(&mut self, arguments: &[Value]) -> Result<Value, ScriptError> {
        let (width, height) = match arguments {
            [other] => {
                let width = self.get_property(other, "width")?;
                let height = self.get_property(other, "height")?;
                (self.coerce_uint32(width)?, self.coerce_uint32(height)?)
            }
            _ => {
                let values = self.canvas_longs("createImageData", arguments, 2)?;
                (
                    values[0].unsigned_abs().min(u64::from(u32::MAX)) as u32,
                    values[1].unsigned_abs().min(u64::from(u32::MAX)) as u32,
                )
            }
        };
        for (extent, name) in [(width, "width"), (height, "height")] {
            if extent == 0 {
                return Err(self.throw_value(dom_exception(
                    "IndexSizeError",
                    &format!("Failed to execute 'createImageData' on 'CanvasRenderingContext2D': The source {name} is 0."),
                )));
            }
        }
        if crate::image::check_dimensions(width, height).is_err() {
            return Err(self.range_error("Out of memory at ImageData creation"));
        }
        let data = vec![0; width as usize * height as usize * 4];
        Ok(image_data_object(width, height, data))
    }

    fn put_image_data(
        &mut self,
        context: &CanvasContextHandle,
        arguments: &[Value],
    ) -> Result<(), ScriptError> {
        let count = if arguments.len() >= 7 { 7 } else { 3 };
        let image_data = arguments.first().cloned().unwrap_or(Value::Undefined);
        let coordinates =
            self.canvas_longs("putImageData", arguments.get(1..).unwrap_or(&[]), count - 1)?;
        let width = self.get_property(&image_data, "width")?;
        let width = self.coerce_uint32(width)?;
        let height = self.get_property(&image_data, "height")?;
        let height = self.coerce_uint32(height)?;
        let bytes = match self.get_property(&image_data, "data")? {
            Value::TypedArray(array) => {
                let array = array.borrow();
                array
                    .buffer
                    .borrow()
                    .get(array.start..array.end)
                    .map(<[u8]>::to_vec)
            }
            _ => None,
        };
        let Some(bytes) = bytes.filter(|bytes| bytes.len() == width as usize * height as usize * 4)
        else {
            return Err(self.type_error(
                "Failed to execute 'putImageData' on 'CanvasRenderingContext2D': parameter 1 is not of type 'ImageData'.",
            ));
        };
        let dirty = match coordinates[..] {
            [_, _, x, y, dirty_width, dirty_height] => {
                (x as f32, y as f32, dirty_width as f32, dirty_height as f32)
            }
            _ => (0.0, 0.0, width as f32, height as f32),
        };
        let mut context = context.borrow_mut();
        context.bitmap.put_image_data(
            &bytes,
            (width, height),
            (coordinates[0], coordinates[1]),
            dirty,
        );
        context.snapshot = None;
        Ok(())
    }
}

fn image_data_object(width: u32, height: u32, data: Vec<u8>) -> Value {
    let length = data.len();
    plain_object([
        ("width".to_string(), Value::Number(f64::from(width))),
        ("height".to_string(), Value::Number(f64::from(height))),
        (
            "data".to_string(),
            new_typed(
                ElementKind::Uint8Clamped,
                Rc::new(RefCell::new(data)),
                0,
                length,
            ),
        ),
        ("colorSpace".to_string(), Value::String("srgb".into())),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rectangles_cover_pixels_by_area_and_compose_source_over() {
        let mut bitmap = Bitmap::new(4, 2);
        bitmap.fill_rect((0.5, 0.0, 2.0, 1.0), Color::opaque(255, 0, 0));
        assert_eq!(bitmap.pixels[0], [128, 0, 0, 128]);
        assert_eq!(bitmap.pixels[1], [255, 0, 0, 255]);
        assert_eq!(bitmap.pixels[2], [128, 0, 0, 128]);
        assert_eq!(bitmap.pixels[3], [0; 4]);
        bitmap.fill_rect(
            (0.0, 0.0, -4.0, 1.0),
            Color {
                red: 0,
                green: 0,
                blue: 255,
                alpha: 128,
            },
        );
        assert_eq!(
            bitmap.pixels[0],
            [128, 0, 0, 128],
            "a flipped rect starts at its far edge"
        );
        bitmap.fill_rect(
            (0.0, 0.0, 4.0, 1.0),
            Color {
                red: 0,
                green: 0,
                blue: 255,
                alpha: 128,
            },
        );
        assert_eq!(bitmap.pixels[1], [127, 0, 128, 255]);
        bitmap.clear_rect((1.0, 0.0, 1.0, 0.5));
        assert_eq!(bitmap.pixels[1], [63, 0, 64, 127]);
        assert_eq!(unpremultiply(bitmap.pixels[1]), [126, 0, 129, 127]);
        assert_eq!(bitmap.image_data(-1, 1, 2, 1), vec![0; 8]);
    }

    #[test]
    fn images_scale_with_low_quality_bilinear_and_clipped_source_rectangles() {
        let mut source = Bitmap::new(2, 1);
        source.pixels = vec![[0, 0, 0, 255], [255, 255, 255, 255]];
        let mut bitmap = Bitmap::new(4, 1);
        bitmap.draw_image(
            &source,
            (0.0, 0.0, 2.0, 1.0),
            (0.0, 0.0, 4.0, 1.0),
            Sampling::FixedBilinear,
        );
        let reds = bitmap
            .pixels
            .iter()
            .map(|pixel| pixel[0])
            .collect::<Vec<_>>();
        // chromium's low quality bilinear keeps four bits of subpixel position
        assert_eq!(reds, [0, 63, 191, 255]);
        let mut copy = Bitmap::new(4, 1);
        copy.draw_image(
            &source,
            (-2.0, 0.0, 4.0, 1.0),
            (0.0, 0.0, 4.0, 1.0),
            Sampling::FixedBilinear,
        );
        let copied = copy.pixels.iter().map(|pixel| pixel[3]).collect::<Vec<_>>();
        assert_eq!(
            copied,
            [0, 0, 255, 255],
            "the part of the source past the picture draws nothing"
        );
        assert_eq!(copy.pixels[2], [0, 0, 0, 255]);
        let mut exact = Bitmap::new(2, 1);
        exact.draw_image(
            &source,
            (0.0, 0.0, 2.0, 1.0),
            (0.0, 0.0, 2.0, 1.0),
            Sampling::FixedBilinear,
        );
        assert_eq!(exact.pixels, source.pixels);
    }

    #[test]
    fn base64_pads_partial_groups() {
        assert_eq!(
            ["", "f", "fo", "foo", "foob"].map(|text| base64(text.as_bytes())),
            ["", "Zg==", "Zm8=", "Zm9v", "Zm9vYg=="]
        );
    }

    #[test]
    fn image_data_writes_straight_colours_through_a_dirty_rectangle() {
        let mut bitmap = Bitmap::new(3, 1);
        let data = [10, 20, 30, 255, 200, 100, 50, 128, 1, 2, 3, 0];
        bitmap.put_image_data(&data, (3, 1), (0, 0), (1.0, 0.0, 5.0, 1.0));
        assert_eq!(bitmap.pixels[0], [0; 4]);
        assert_eq!(
            bitmap.image_data(0, 0, 3, 1)[4..],
            [199, 100, 50, 128, 0, 0, 0, 0]
        );
        assert_eq!(non_negative_integer("  +42px"), Some(42));
        assert_eq!(non_negative_integer("-3"), None);
        assert_eq!(non_negative_integer("3000000000"), None);
    }
}
