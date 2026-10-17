use crate::css::Color;
use crate::dom::{Document, Element, NodeId};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

const MAX_IMAGE_DIMENSION: u32 = 2048;
const MAX_IMAGE_PIXELS: usize = 4_000_000;
// decoded raster images are larger than rasterized SVG, but still bounded before allocation
const MAX_RASTER_DIMENSION: u32 = 16_384;
const MAX_RASTER_PIXELS: usize = 40_000_000;
const MAX_SVG_DEPTH: usize = 64;
const MAX_LINE_STEPS: usize = 16_384;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Image {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) pixels: Vec<Color>,
}

impl Image {
    pub(crate) fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![Color::transparent(); width as usize * height as usize],
        }
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

#[derive(Clone, Copy)]
struct PaintStyle {
    fill: Option<Color>,
    stroke: Option<Color>,
    stroke_width: f32,
}

impl Default for PaintStyle {
    fn default() -> Self {
        Self {
            fill: Some(Color::BLACK),
            stroke: None,
            stroke_width: 1.0,
        }
    }
}

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
    if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        return Err(ImageError::new("WebP images are not supported yet"));
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

fn decode_base64(payload: &str) -> Option<Vec<u8>> {
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
    let (view_x, view_y, view_width, view_height) = root
        .attr("viewbox")
        .and_then(parse_view_box)
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
    let scale_x = width as f32 / view_width;
    let scale_y = height as f32 / view_height;
    render_element(
        &mut image,
        &document,
        root_node,
        PaintStyle::default(),
        (view_x, view_y, scale_x, scale_y),
        0,
    );
    Ok(image)
}

fn render_element(
    image: &mut Image,
    document: &Document,
    node: NodeId,
    parent_style: PaintStyle,
    viewport: (f32, f32, f32, f32),
    depth: usize,
) {
    if depth > MAX_SVG_DEPTH {
        return;
    }
    let Some(element) = document.element(node) else {
        return;
    };
    let style = parse_style(element, parent_style);
    let (view_x, view_y, scale_x, scale_y) = viewport;
    match element.name.as_str() {
        "rect" => {
            let x = scalar_attr(element, "x").unwrap_or(0.0);
            let y = scalar_attr(element, "y").unwrap_or(0.0);
            let width = scalar_attr(element, "width").unwrap_or(0.0);
            let height = scalar_attr(element, "height").unwrap_or(0.0);
            draw_rect(
                image,
                map_x(x, view_x, scale_x),
                map_y(y, view_y, scale_y),
                width * scale_x,
                height * scale_y,
                style,
            );
        }
        "circle" => {
            let cx = scalar_attr(element, "cx").unwrap_or(0.0);
            let cy = scalar_attr(element, "cy").unwrap_or(0.0);
            let radius = scalar_attr(element, "r").unwrap_or(0.0);
            draw_circle(
                image,
                map_x(cx, view_x, scale_x),
                map_y(cy, view_y, scale_y),
                radius * scale_x.min(scale_y),
                style,
            );
        }
        "line" => {
            let x1 = scalar_attr(element, "x1").unwrap_or(0.0);
            let y1 = scalar_attr(element, "y1").unwrap_or(0.0);
            let x2 = scalar_attr(element, "x2").unwrap_or(0.0);
            let y2 = scalar_attr(element, "y2").unwrap_or(0.0);
            draw_line(
                image,
                map_x(x1, view_x, scale_x),
                map_y(y1, view_y, scale_y),
                map_x(x2, view_x, scale_x),
                map_y(y2, view_y, scale_y),
                style,
            );
        }
        _ => {}
    }
    for child in document.element_children(node) {
        render_element(image, document, child, style, viewport, depth + 1);
    }
}

fn draw_rect(image: &mut Image, x: f32, y: f32, width: f32, height: f32, style: PaintStyle) {
    if let Some(fill) = style.fill {
        let left = x.floor().clamp(0.0, image.width as f32) as i32;
        let top = y.floor().clamp(0.0, image.height as f32) as i32;
        let right = (x + width).ceil().clamp(0.0, image.width as f32) as i32;
        let bottom = (y + height).ceil().clamp(0.0, image.height as f32) as i32;
        for pixel_y in top..bottom {
            for pixel_x in left..right {
                image.blend_pixel(pixel_x, pixel_y, fill);
            }
        }
    }
    if let Some(stroke) = style.stroke {
        let thickness = style.stroke_width.max(1.0);
        draw_rect_outline(image, x, y, width, height, thickness, stroke);
    }
}

fn draw_rect_outline(
    image: &mut Image,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    thickness: f32,
    color: Color,
) {
    draw_rect(
        image,
        x,
        y,
        width,
        thickness,
        PaintStyle {
            fill: Some(color),
            stroke: None,
            stroke_width: 1.0,
        },
    );
    draw_rect(
        image,
        x,
        y + height - thickness,
        width,
        thickness,
        PaintStyle {
            fill: Some(color),
            stroke: None,
            stroke_width: 1.0,
        },
    );
    draw_rect(
        image,
        x,
        y,
        thickness,
        height,
        PaintStyle {
            fill: Some(color),
            stroke: None,
            stroke_width: 1.0,
        },
    );
    draw_rect(
        image,
        x + width - thickness,
        y,
        thickness,
        height,
        PaintStyle {
            fill: Some(color),
            stroke: None,
            stroke_width: 1.0,
        },
    );
}

fn draw_circle(image: &mut Image, cx: f32, cy: f32, radius: f32, style: PaintStyle) {
    let left = (cx - radius).floor().clamp(0.0, image.width as f32) as i32;
    let top = (cy - radius).floor().clamp(0.0, image.height as f32) as i32;
    let right = (cx + radius).ceil().clamp(0.0, image.width as f32) as i32;
    let bottom = (cy + radius).ceil().clamp(0.0, image.height as f32) as i32;
    let stroke_width = style.stroke_width.max(1.0).min(MAX_IMAGE_DIMENSION as f32);
    for pixel_y in top..bottom {
        for pixel_x in left..right {
            let dx = pixel_x as f32 + 0.5 - cx;
            let dy = pixel_y as f32 + 0.5 - cy;
            let distance = (dx * dx + dy * dy).sqrt();
            if let Some(fill) = style.fill
                && distance <= radius
            {
                image.blend_pixel(pixel_x, pixel_y, fill);
            }
            if let Some(stroke) = style.stroke
                && (distance - radius).abs() <= stroke_width / 2.0
            {
                image.blend_pixel(pixel_x, pixel_y, stroke);
            }
        }
    }
}

fn draw_line(image: &mut Image, x1: f32, y1: f32, x2: f32, y2: f32, style: PaintStyle) {
    let Some(color) = style.stroke.or(style.fill) else {
        return;
    };
    let distance = (x2 - x1).hypot(y2 - y1).ceil().max(1.0);
    if !distance.is_finite() || distance as usize > MAX_LINE_STEPS {
        return;
    }
    let distance = distance as usize;
    let radius = style.stroke_width.max(1.0).min(MAX_IMAGE_DIMENSION as f32) / 2.0;
    for step in 0..=distance {
        let progress = step as f32 / distance as f32;
        let x = x1 + (x2 - x1) * progress;
        let y = y1 + (y2 - y1) * progress;
        let left = (x - radius).floor() as i32;
        let top = (y - radius).floor() as i32;
        let right = (x + radius).ceil() as i32;
        let bottom = (y + radius).ceil() as i32;
        for pixel_y in top..bottom {
            for pixel_x in left..right {
                image.blend_pixel(pixel_x, pixel_y, color);
            }
        }
    }
}

fn parse_style(element: &Element, mut style: PaintStyle) -> PaintStyle {
    for (name, value) in &element.attrs {
        match name.as_str() {
            "fill" => style.fill = parse_paint(value),
            "stroke" => style.stroke = parse_paint(value),
            "stroke-width" => {
                style.stroke_width = parse_scalar(value).unwrap_or(style.stroke_width)
            }
            "style" => {
                for declaration in value.split(';') {
                    let Some((property, value)) = declaration.split_once(':') else {
                        continue;
                    };
                    match property.trim().to_ascii_lowercase().as_str() {
                        "fill" => style.fill = parse_paint(value.trim()),
                        "stroke" => style.stroke = parse_paint(value.trim()),
                        "stroke-width" => {
                            style.stroke_width =
                                parse_scalar(value.trim()).unwrap_or(style.stroke_width)
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    style
}

fn parse_paint(value: &str) -> Option<Color> {
    if value.trim().eq_ignore_ascii_case("none") {
        None
    } else {
        Color::parse(value)
    }
}

fn scalar_attr(element: &Element, name: &str) -> Option<f32> {
    element.attr(name).and_then(parse_scalar)
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

fn parse_view_box(value: &str) -> Option<(f32, f32, f32, f32)> {
    let values = value
        .split(|character: char| character.is_ascii_whitespace() || character == ',')
        .filter(|value| !value.is_empty())
        .map(str::parse::<f32>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if values.len() != 4 || values.iter().any(|value| !value.is_finite()) {
        return None;
    }
    Some((values[0], values[1], values[2], values[3]))
}

fn map_x(value: f32, view_x: f32, scale: f32) -> f32 {
    (value - view_x) * scale
}

fn map_y(value: f32, view_y: f32, scale: f32) -> f32 {
    (value - view_y) * scale
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
    fn decoding_sniffs_the_format_and_names_unsupported_ones() {
        let webp = b"RIFF\x00\x00\x00\x00WEBPVP8 ";
        assert!(
            decode_image(webp, "image/webp")
                .unwrap_err()
                .to_string()
                .contains("WebP")
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
        assert!(rgba.chunks_exact(4).all(|pixel| pixel[3] == 255));
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
    fn rejects_oversized_svg_dimensions() {
        let error = parse_svg(r#"<svg width="99999" height="99999"></svg>"#)
            .expect_err("oversized svg should fail");
        assert!(error.to_string().contains("width must be between"));
    }
}
