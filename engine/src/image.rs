use crate::css::Color;
use crate::dom::Document;
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

const MAX_IMAGE_DIMENSION: u32 = 2048;
const MAX_IMAGE_PIXELS: usize = 4_000_000;
// decoded raster images are larger than rasterized SVG, but still bounded before allocation
const MAX_RASTER_DIMENSION: u32 = 16_384;
const MAX_RASTER_PIXELS: usize = 40_000_000;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Image {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pixels: Vec<Color>,
    pub(crate) levels: Levels,
}

// the picture's mip chain, halved down to a pixel the first time a draw shrinks it past half;
// a picture is no longer written once it is shared, so the chain never goes stale
#[derive(Default)]
pub(crate) struct Levels(std::sync::OnceLock<Vec<Image>>);

impl Clone for Levels {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl PartialEq for Levels {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl fmt::Debug for Levels {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Levels")
    }
}

impl Image {
    pub(crate) fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![Color::transparent(); width as usize * height as usize],
            levels: Levels::default(),
        }
    }

    // level 0 is the picture itself, and each one after it half the one before, every pixel
    // the mean of a two by two block in premultiplied alpha, as skia builds its mipmaps
    pub(crate) fn level(&self, index: usize) -> &Image {
        if index == 0 {
            return self;
        }
        let chain = self.levels.0.get_or_init(|| {
            let mut chain: Vec<Image> = Vec::new();
            while let Some(next) = chain.last().unwrap_or(self).halved() {
                chain.push(next);
            }
            chain
        });
        chain.get(index - 1).or(chain.last()).unwrap_or(self)
    }

    fn halved(&self) -> Option<Image> {
        if self.width <= 1 && self.height <= 1 {
            return None;
        }
        let (width, height) = ((self.width / 2).max(1), (self.height / 2).max(1));
        let mut pixels = Vec::with_capacity(width as usize * height as usize);
        for row in 0..height {
            for column in 0..width {
                let mut sum = [0_u32; 4];
                for (x, y) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let texel = self.pixel(
                        (column * 2 + x).min(self.width - 1),
                        (row * 2 + y).min(self.height - 1),
                    );
                    let alpha = u32::from(texel.alpha);
                    sum[0] += u32::from(texel.red) * alpha;
                    sum[1] += u32::from(texel.green) * alpha;
                    sum[2] += u32::from(texel.blue) * alpha;
                    sum[3] += alpha;
                }
                pixels.push(if sum[3] == 0 {
                    Color::transparent()
                } else {
                    let channel = |total: u32| ((total + sum[3] / 2) / sum[3]) as u8;
                    Color {
                        red: channel(sum[0]),
                        green: channel(sum[1]),
                        blue: channel(sum[2]),
                        alpha: ((sum[3] + 2) / 4) as u8,
                    }
                });
            }
        }
        Some(Image {
            width,
            height,
            pixels,
            levels: Levels::default(),
        })
    }

    pub(crate) fn pixel(&self, x: u32, y: u32) -> Color {
        self.pixels[(y * self.width + x) as usize]
    }

    pub(crate) fn blend_pixel(&mut self, x: i32, y: i32, color: Color) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        if color.alpha == 0 {
            return;
        }
        let index = (y as u32 * self.width + x as u32) as usize;
        let destination = self.pixels[index];
        let source_alpha = color.alpha as u32;
        let destination_alpha = destination.alpha as u32;
        let output_alpha = source_alpha + destination_alpha * (255 - source_alpha) / 255;
        if output_alpha == 0 {
            self.pixels[index] = Color::transparent();
            return;
        }
        let source_weight = source_alpha * 255 / output_alpha;
        let destination_weight = 255 - source_weight;
        self.pixels[index] = Color {
            red: ((color.red as u32 * source_weight + destination.red as u32 * destination_weight)
                / 255) as u8,
            green: ((color.green as u32 * source_weight
                + destination.green as u32 * destination_weight)
                / 255) as u8,
            blue: ((color.blue as u32 * source_weight
                + destination.blue as u32 * destination_weight)
                / 255) as u8,
            alpha: output_alpha.min(255) as u8,
        };
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ImageError(String);

impl ImageError {
    pub(crate) fn new(detail: impl Into<String>) -> Self {
        Self(detail.into())
    }
}

// rejects raster dimensions before a decoder allocates for them
pub(crate) fn check_dimensions(width: u32, height: u32) -> Result<(), ImageError> {
    if width == 0 || height == 0 {
        return Err(ImageError::new("image has no pixels"));
    }
    if width > MAX_RASTER_DIMENSION
        || height > MAX_RASTER_DIMENSION
        || (width as usize) * (height as usize) > MAX_RASTER_PIXELS
    {
        return Err(ImageError::new(format!(
            "image of {width}x{height} exceeds the decode limit"
        )));
    }
    Ok(())
}

impl fmt::Display for ImageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ImageError {}

// decodes any supported image by its leading bytes, falling back to the declared type for SVG
pub(crate) fn decode_image(bytes: &[u8], mime_type: &str) -> Result<Image, ImageError> {
    if crate::png::is_png(bytes) {
        return crate::png::decode_png(bytes);
    }
    if crate::jpeg::is_jpeg(bytes) {
        return crate::jpeg::decode_jpeg(bytes);
    }
    if crate::gif::is_gif(bytes) {
        return crate::gif::decode_gif(bytes);
    }
    if crate::webp::is_webp(bytes) {
        return crate::webp::decode_webp(bytes);
    }
    let declared_svg = mime_type
        .split(';')
        .next()
        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("image/svg+xml"));
    let text = std::str::from_utf8(bytes).ok();
    if declared_svg
        || text.is_some_and(|text| text.trim_start().starts_with('<') && text.contains("<svg"))
    {
        return parse_svg(text.ok_or_else(|| ImageError::new("SVG is not UTF-8"))?);
    }
    Err(ImageError::new("unsupported image format"))
}

// the MIME type and bytes of a data: URL, with base64 or percent-encoded payloads
pub(crate) fn decode_data_url(url: &str) -> Option<(String, Vec<u8>)> {
    let rest = url
        .get(..5)?
        .eq_ignore_ascii_case("data:")
        .then(|| &url[5..])?;
    let (header, payload) = rest.split_once(',')?;
    let base64 = header.to_ascii_lowercase().ends_with(";base64");
    let mime_type = header
        .split(';')
        .next()
        .filter(|mime| !mime.is_empty())
        .unwrap_or("text/plain")
        .to_ascii_lowercase();
    let bytes = if base64 {
        decode_base64(payload)?
    } else {
        percent_decode(payload)
    };
    Some((mime_type, bytes))
}

pub(crate) fn decode_base64(payload: &str) -> Option<Vec<u8>> {
    let mut output = Vec::with_capacity(payload.len() * 3 / 4);
    let (mut buffer, mut bits) = (0_u32, 0_u32);
    for byte in payload.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b' ' | b'\t' | b'\n' | b'\r' => continue,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(output)
}

fn percent_decode(payload: &str) -> Vec<u8> {
    let bytes = payload.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let decoded = (bytes[index] == b'%')
            .then(|| payload.get(index + 1..index + 3))
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match decoded {
            Some(byte) => {
                output.push(byte);
                index += 3;
            }
            None => {
                output.push(bytes[index]);
                index += 1;
            }
        }
    }
    output
}

// a copyable handle for an image URL written in a stylesheet, so computed styles stay Copy
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ImageSource(u32);

const MAX_IMAGE_SOURCES: usize = 4096;

#[derive(Default)]
struct ImageSourceTable {
    urls: Vec<Rc<str>>,
    ids: HashMap<Rc<str>, ImageSource>,
}

thread_local! {
    static IMAGE_SOURCES: RefCell<ImageSourceTable> = RefCell::default();
}

pub(crate) fn intern_image_source(url: &str) -> Option<ImageSource> {
    IMAGE_SOURCES.with(|table| {
        let mut table = table.borrow_mut();
        if let Some(source) = table.ids.get(url) {
            return Some(*source);
        }
        if table.urls.len() >= MAX_IMAGE_SOURCES {
            return None;
        }
        let url: Rc<str> = url.into();
        let source = ImageSource(table.urls.len() as u32);
        table.urls.push(url.clone());
        table.ids.insert(url, source);
        Some(source)
    })
}

pub(crate) fn image_source_url(source: ImageSource) -> Option<Rc<str>> {
    IMAGE_SOURCES.with(|table| table.borrow().urls.get(source.0 as usize).cloned())
}

// the icon as a png file's bytes, for system surfaces that load icons from disk
// an encoded picture as its size and straight RGBA pixels, row after row
pub fn decode_rgba(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let image = decode_image(bytes, "").ok()?;
    let rgba = image
        .pixels
        .iter()
        .flat_map(|color| [color.red, color.green, color.blue, color.alpha])
        .collect();
    Some((image.width, image.height, rgba))
}

pub fn icon_png(bytes: &[u8], size: u32) -> Option<Vec<u8>> {
    let (rgba, size) = rasterize_icon(bytes, size)?;
    Some(crate::png::encode_png(size, size, 4, &rgba))
}

// a square RGBA buffer for window and taskbar icons, scaled from an app owned image;
// each icon pixel averages the source pixels it covers, so large images shrink cleanly
pub fn rasterize_icon(bytes: &[u8], size: u32) -> Option<(Vec<u8>, u32)> {
    let size = size.clamp(16, 512);
    let image = decode_image(bytes, "").ok()?;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for row in 0..size {
        let top = row * image.height / size;
        let bottom = ((row + 1) * image.height / size)
            .max(top + 1)
            .min(image.height);
        for column in 0..size {
            let left = column * image.width / size;
            let right = ((column + 1) * image.width / size)
                .max(left + 1)
                .min(image.width);
            let mut sum = [0_u64; 4];
            for y in top..bottom {
                for x in left..right {
                    let color = image.pixel(x, y);
                    let alpha = u64::from(color.alpha);
                    sum[0] += u64::from(color.red) * alpha;
                    sum[1] += u64::from(color.green) * alpha;
                    sum[2] += u64::from(color.blue) * alpha;
                    sum[3] += alpha;
                }
            }
            let count = u64::from((bottom - top) * (right - left));
            let channel = |value: u64| value.checked_div(sum[3]).unwrap_or(0) as u8;
            rgba.extend_from_slice(&[
                channel(sum[0]),
                channel(sum[1]),
                channel(sum[2]),
                (sum[3] / count) as u8,
            ]);
        }
    }
    Some((rgba, size))
}

pub(crate) fn parse_svg(source: &str) -> Result<Image, ImageError> {
    let document = Document::parse_markup(source).map_err(|error| ImageError(error.to_string()))?;
    let root_node = document
        .element_children(document.root())
        .find(|&node| {
            document
                .element(node)
                .is_some_and(|element| element.name == "svg")
        })
        .ok_or_else(|| ImageError("SVG has no root element".to_string()))?;
    let root = document
        .element(root_node)
        .expect("the svg root is an element");
    let (_, _, view_width, view_height) = crate::svg::view_box(root)
        .or_else(|| {
            let width = root.attr("width").and_then(parse_scalar)?;
            let height = root.attr("height").and_then(parse_scalar)?;
            Some((0.0, 0.0, width, height))
        })
        .ok_or_else(|| ImageError("SVG needs width, height, or viewBox".to_string()))?;
    if !view_width.is_finite()
        || !view_height.is_finite()
        || view_width <= 0.0
        || view_height <= 0.0
    {
        return Err(ImageError(
            "SVG view box must be positive and finite".to_string(),
        ));
    }
    let width = parse_dimension(
        root.attr("width")
            .and_then(parse_scalar)
            .unwrap_or(view_width),
        "width",
    )?;
    let height = parse_dimension(
        root.attr("height")
            .and_then(parse_scalar)
            .unwrap_or(view_height),
        "height",
    )?;
    if (width as usize).saturating_mul(height as usize) > MAX_IMAGE_PIXELS {
        return Err(ImageError("SVG image is too large".to_string()));
    }
    let mut image = Image::new(width, height);
    let stylesheet = crate::css::Stylesheet::parse("").map_err(ImageError)?;
    let state = crate::css::StyleState::default();
    let (style, font_size) = stylesheet
        .computed_style(&document, root_node, &state)
        .ok_or_else(|| ImageError("SVG has no root element".to_string()))?;
    let style_of = |node, parent: &crate::css::Style, parent_font_size| {
        stylesheet.child_style(&document, node, parent, parent_font_size, &state)
    };
    let scene = crate::svg::build(
        &document,
        root_node,
        (&style, font_size),
        (0.0, 0.0, width as f32, height as f32),
        &style_of,
    );
    crate::svg::rasterize(&scene, &mut image, crate::svg::IDENTITY);
    Ok(image)
}

fn parse_scalar(value: &str) -> Option<f32> {
    value
        .trim()
        .strip_suffix("px")
        .unwrap_or(value.trim())
        .parse::<f32>()
        .ok()
        .filter(|value| value.is_finite())
}

fn parse_dimension(value: f32, name: &str) -> Result<u32, ImageError> {
    if !value.is_finite() || value <= 0.0 || value > MAX_IMAGE_DIMENSION as f32 {
        return Err(ImageError(format!(
            "SVG {name} must be between 1 and {MAX_IMAGE_DIMENSION}"
        )));
    }
    Ok(value.round().max(1.0) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_urls_decode_base64_and_percent_encoded_payloads() {
        let (mime, bytes) = decode_data_url("data:text/plain;base64,aGVs\nbG8=").expect("base64");
        assert_eq!(
            (mime.as_str(), bytes.as_slice()),
            ("text/plain", b"hello".as_slice())
        );
        let (mime, bytes) =
            decode_data_url("DATA:image/svg+xml,%3Csvg%20width='2'%20height='2'/%3E")
                .expect("text");
        assert_eq!(mime, "image/svg+xml");
        assert_eq!(bytes, b"<svg width='2' height='2'/>");
        assert!(decode_data_url("data:;base64,!!").is_none());
        assert!(decode_data_url("https://app.ceri.invalid/").is_none());
    }

    #[test]
    fn decoding_sniffs_the_format_and_names_broken_ones() {
        // a WebP header with no image in it
        let webp = b"RIFF\x04\x00\x00\x00WEBP";
        assert!(
            decode_image(webp, "image/webp")
                .unwrap_err()
                .to_string()
                .contains("webp")
        );
        let svg = decode_image(
            b"<svg width='3' height='2'><rect width='3' height='2'/></svg>",
            "",
        )
        .expect("SVG is recognized without a declared type");
        assert_eq!((svg.width, svg.height), (3, 2));
    }

    #[test]
    fn icons_shrink_any_supported_image_by_area_averaging() {
        let png = std::fs::read(format!(
            "{}/tests/fixtures/rgb8.png",
            env!("CARGO_MANIFEST_DIR")
        ))
        .expect("fixture");
        let (rgba, size) = rasterize_icon(&png, 16).expect("PNG icon");
        assert_eq!((rgba.len(), size), (16 * 16 * 4, 16));
        assert!(rgba.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 255));
    }

    #[test]
    fn parses_svg_shapes_into_pixels() {
        let image = parse_svg(
            r##"<svg width="20" height="12" viewbox="0 0 20 12">
                <rect x="1" y="2" width="8" height="5" fill="#ff0000"/>
                <circle cx="15" cy="6" r="4" fill="#0000ff"/>
            </svg>"##,
        )
        .expect("svg should parse");
        assert_eq!((image.width, image.height), (20, 12));
        assert_eq!(image.pixel(2, 3), Color::opaque(255, 0, 0));
        assert_eq!(image.pixel(15, 6), Color::opaque(0, 0, 255));
        assert_eq!(image.pixel(0, 0).alpha, 0);
    }

    #[test]
    fn strokes_svg_paths_with_round_caps_in_the_current_color() {
        let image = parse_svg(
            r##"<svg width="20" height="10" color="#ff0000">
                <path d="M5 5H15" fill="none" stroke="currentColor" stroke-width="4" stroke-linecap="round"/>
            </svg>"##,
        )
        .expect("svg should parse");
        assert_eq!(image.pixel(10, 4), Color::opaque(255, 0, 0));
        assert!(
            image.pixel(3, 5).alpha > 128,
            "the round cap reaches past the end"
        );
        assert_eq!(image.pixel(1, 5).alpha, 0);
        assert_eq!(image.pixel(10, 1).alpha, 0);
    }

    #[test]
    fn rejects_oversized_svg_dimensions() {
        let error = parse_svg(r#"<svg width="99999" height="99999"></svg>"#)
            .expect_err("oversized svg should fail");
        assert!(error.to_string().contains("width must be between"));
    }
}
