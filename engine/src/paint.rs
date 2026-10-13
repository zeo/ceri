use crate::css::{BackgroundImage, BackgroundSize, Color, Gradient, ObjectFit, Overflow};
use crate::dom::NodePath;
use crate::image::Image;
use crate::layout::{
    ImageLayer, LayoutBox, LayoutDocument, ProseSpan, Rect, TextRun, paint_order,
    scrollbar_metrics, stacking_context_entries, translate_rect, visual_offset_for,
    visual_rect_for,
};
use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CanvasCommand {
    FillRect {
        path: NodePath,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        color: Color,
    },
    ClearRect {
        path: NodePath,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    },
}

#[derive(Clone, Debug)]
pub struct Canvas {
    width: u32,
    height: u32,
    scale: f32,
    background: Color,
    pixels: Vec<u8>,
    // the layout point drawn at the surface's top-left corner, nonzero for transform layers
    shift: (f32, f32),
}

impl Canvas {
    pub fn new(width: u32, height: u32, background: Color) -> Self {
        Self::device(width, height, 1.0, background)
    }

    // width and height are layout pixels, the surface itself is allocated in device pixels
    pub fn scaled(width: f32, height: f32, scale: f32, background: Color) -> Self {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let device_width = (width.max(1.0) * scale).round().max(1.0) as u32;
        let device_height = (height.max(1.0) * scale).round().max(1.0) as u32;
        Self::device(device_width, device_height, scale, background)
    }

    fn device(width: u32, height: u32, scale: f32, background: Color) -> Self {
        let mut pixels = vec![0; width as usize * height as usize * 4];
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[background.red, background.green, background.blue, 255]);
        }
        Self {
            width,
            height,
            scale,
            background,
            pixels,
            shift: (0.0, 0.0),
        }
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn rgba_pixels(&self) -> &[u8] {
        &self.pixels
    }

    pub fn bytes_per_row(&self) -> usize {
        self.width as usize * 4
    }

    pub fn non_background_pixels(&self) -> usize {
        let background = [
            self.background.red,
            self.background.green,
            self.background.blue,
        ];
        self.pixels
            .chunks_exact(4)
            .filter(|pixel| pixel[..3] != background)
            .count()
    }

    pub fn write_ppm(&self, path: &Path) -> io::Result<()> {
        let mut file = File::create(path)?;
        write!(file, "P6\n{} {}\n255\n", self.width, self.height)?;
        for pixel in self.pixels.chunks_exact(4) {
            file.write_all(&pixel[..3])?;
        }
        Ok(())
    }

    pub fn write_bmp(&self, path: &Path) -> io::Result<()> {
        let row_stride = (self.width as usize * 3 + 3) & !3;
        let pixel_bytes = row_stride * self.height as usize;
        let file_size = 14 + 40 + pixel_bytes;
        let mut file = File::create(path)?;
        file.write_all(b"BM")?;
        file.write_all(&(file_size as u32).to_le_bytes())?;
        file.write_all(&[0; 4])?;
        file.write_all(&(54_u32).to_le_bytes())?;
        file.write_all(&(40_u32).to_le_bytes())?;
        file.write_all(&(self.width as i32).to_le_bytes())?;
        file.write_all(&(-(self.height as i32)).to_le_bytes())?;
        file.write_all(&(1_u16).to_le_bytes())?;
        file.write_all(&(24_u16).to_le_bytes())?;
        file.write_all(&[0; 4])?;
        file.write_all(&(pixel_bytes as u32).to_le_bytes())?;
        file.write_all(&[0; 16])?;
        let padding = vec![0; row_stride - self.width as usize * 3];
        for row in 0..self.height {
            let start = row as usize * self.width as usize * 4;
            for pixel in self.pixels[start..start + self.width as usize * 4].chunks_exact(4) {
                file.write_all(&[pixel[2], pixel[1], pixel[0]])?;
            }
            file.write_all(&padding)?;
        }
        Ok(())
    }

    pub fn write_png(&self, path: &Path) -> io::Result<()> {
        let mut raw = Vec::with_capacity((self.width as usize * 3 + 1) * self.height as usize);
        for row in 0..self.height as usize {
            raw.push(0);
            let start = row * self.bytes_per_row();
            for pixel in self.pixels[start..start + self.width as usize * 4].chunks_exact(4) {
                raw.extend_from_slice(&pixel[..3]);
            }
        }
        let mut header = Vec::with_capacity(13);
        header.extend_from_slice(&self.width.to_be_bytes());
        header.extend_from_slice(&self.height.to_be_bytes());
        header.extend_from_slice(&[8, 2, 0, 0, 0]);

        let mut file = File::create(path)?;
        file.write_all(&[137, 80, 78, 71, 13, 10, 26, 10])?;
        write_png_chunk(&mut file, b"IHDR", &header)?;
        write_png_chunk(&mut file, b"IDAT", &zlib_stored(&raw))?;
        write_png_chunk(&mut file, b"IEND", &[])
    }

    fn fill_rect(&mut self, x: f32, y: f32, width: f32, height: f32, color: Color) {
        if width <= 0.0 || height <= 0.0 || color.alpha == 0 {
            return;
        }
        let (x, y, width, height) = (
            (x - self.shift.0) * self.scale,
            (y - self.shift.1) * self.scale,
            width * self.scale,
            height * self.scale,
        );
        let left = (x.floor().max(0.0) as u32).min(self.width);
        let top = (y.floor().max(0.0) as u32).min(self.height);
        let right = ((x + width).ceil().max(0.0) as u32).min(self.width);
        let bottom = ((y + height).ceil().max(0.0) as u32).min(self.height);
        if right <= left || bottom <= top {
            return;
        }
        // an opaque span is a row copy, which is the hot path for backgrounds and borders
        if color.alpha == 255 {
            let sample = [color.red, color.green, color.blue, 255];
            let stride = self.width as usize * 4;
            for pixel_y in top..bottom {
                let start = pixel_y as usize * stride + left as usize * 4;
                let end = pixel_y as usize * stride + right as usize * 4;
                for slot in self.pixels[start..end].chunks_exact_mut(4) {
                    slot.copy_from_slice(&sample);
                }
            }
            return;
        }
        for pixel_y in top..bottom {
            for pixel_x in left..right {
                self.blend_pixel(pixel_x, pixel_y, color);
            }
        }
    }

    fn fill_rect_clipped(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        color: Color,
        clip: crate::layout::Rect,
    ) {
        let left = x.max(clip.x);
        let top = y.max(clip.y);
        let right = (x + width).min(clip.x + clip.width);
        let bottom = (y + height).min(clip.y + clip.height);
        self.fill_rect(left, top, right - left, bottom - top, color);
    }

    // device-space glyph blit, the coverage bitmap is already rasterized at this scale
    fn blend_coverage(
        &mut self,
        x: i32,
        y: i32,
        glyph: &crate::font::GlyphBitmap,
        color: Color,
        clip: Option<crate::layout::Rect>,
    ) {
        if color.alpha == 0 {
            return;
        }
        for row in 0..glyph.height {
            let pixel_y = y + row as i32;
            if pixel_y < 0 || pixel_y as u32 >= self.height {
                continue;
            }
            for column in 0..glyph.width {
                let pixel_x = x + column as i32;
                if pixel_x < 0 || pixel_x as u32 >= self.width {
                    continue;
                }
                let coverage = glyph.coverage[row * glyph.width + column];
                if coverage == 0 {
                    continue;
                }
                if let Some(clip) = clip {
                    let inside = (pixel_x as f32) >= clip.x.floor()
                        && (pixel_x as f32) < (clip.x + clip.width).ceil()
                        && (pixel_y as f32) >= clip.y.floor()
                        && (pixel_y as f32) < (clip.y + clip.height).ceil();
                    if !inside {
                        continue;
                    }
                }
                let alpha = u32::from(color.alpha) * u32::from(coverage) / 255;
                self.blend_pixel(
                    pixel_x as u32,
                    pixel_y as u32,
                    Color {
                        alpha: alpha as u8,
                        ..color
                    },
                );
            }
        }
    }

    // signed distance coverage gives antialiased corners without a second pass
    fn fill_shape(&mut self, shape: Shape, paint: &Paint, clip: Option<Rect>) {
        if shape.width <= 0.0 || shape.height <= 0.0 {
            return;
        }
        // a square edged solid fill is the common case, and the rect path is much cheaper
        if shape.radius <= 0.0
            && let Paint::Solid(color) = paint
        {
            match clip {
                Some(clip) => self.fill_rect_clipped(
                    shape.x,
                    shape.y,
                    shape.width,
                    shape.height,
                    *color,
                    clip,
                ),
                None => self.fill_rect(shape.x, shape.y, shape.width, shape.height, *color),
            }
            return;
        }
        let device = self.device_shape(shape);
        let clip = clip.map(|clip| self.device_shape(Shape::from_rect(clip, 0.0)));
        let radius = device
            .radius
            .min(device.width / 2.0)
            .min(device.height / 2.0)
            .max(0.0);
        let left = device.x.floor().max(0.0) as u32;
        let top = device.y.floor().max(0.0) as u32;
        let right = (device.x + device.width)
            .ceil()
            .clamp(0.0, self.width as f32) as u32;
        let bottom = (device.y + device.height)
            .ceil()
            .clamp(0.0, self.height as f32) as u32;
        for pixel_y in top.min(self.height)..bottom.min(self.height) {
            for pixel_x in left.min(self.width)..right.min(self.width) {
                let center = (pixel_x as f32 + 0.5, pixel_y as f32 + 0.5);
                if let Some(clip) = clip
                    && clip.coverage(center, 0.0) <= 0.0
                {
                    continue;
                }
                let coverage = device.coverage(center, radius);
                if coverage <= 0.0 {
                    continue;
                }
                let color = paint.color_at(center, device);
                if color.alpha == 0 {
                    continue;
                }
                self.blend_pixel(pixel_x, pixel_y, color.scale_alpha(coverage.min(1.0)));
            }
        }
    }

    fn device_shape(&self, shape: Shape) -> Shape {
        Shape {
            x: shape.x - self.shift.0,
            y: shape.y - self.shift.1,
            ..shape
        }
        .to_device(self.scale)
    }

    // a transparent surface covering `bounds` in layout space at its own resolution
    fn transparent(bounds: Rect, scale: f32) -> Canvas {
        let width = (bounds.width * scale).ceil().max(1.0) as u32;
        let height = (bounds.height * scale).ceil().max(1.0) as u32;
        let mut pixels = LAYER_BUFFERS
            .with(|pool| pool.borrow_mut().pop())
            .unwrap_or_default();
        pixels.clear();
        pixels.resize(width as usize * height as usize * 4, 0);
        Canvas {
            width,
            height,
            scale,
            background: Color::transparent(),
            pixels,
            shift: (bounds.x, bounds.y),
        }
    }

    // draws a transformed layer: each covered pixel is pulled back through the inverse
    // projection and sampled bilinearly, with transparent texels outside the layer giving
    // antialiased edges
    fn composite_transformed(
        &mut self,
        layer: Canvas,
        transform: &crate::transform::Matrix,
        clip: Option<Rect>,
        opacity: f32,
    ) {
        let layer_rect = Rect {
            x: layer.shift.0,
            y: layer.shift.1,
            width: layer.width as f32 / layer.scale,
            height: layer.height as f32 / layer.scale,
        };
        let corners = [
            (layer_rect.x, layer_rect.y),
            (layer_rect.x + layer_rect.width, layer_rect.y),
            (layer_rect.x, layer_rect.y + layer_rect.height),
            (
                layer_rect.x + layer_rect.width,
                layer_rect.y + layer_rect.height,
            ),
        ]
        .map(|(x, y)| transform.map_point(x, y));
        // a corner behind the viewer leaves the visible part unbounded, so the clip limits it
        let target = if corners.iter().all(Option::is_some) {
            let points = corners.map(|corner| corner.expect("checked above"));
            let left = points
                .iter()
                .map(|point| point.0)
                .fold(f32::INFINITY, f32::min);
            let top = points
                .iter()
                .map(|point| point.1)
                .fold(f32::INFINITY, f32::min);
            let right = points
                .iter()
                .map(|point| point.0)
                .fold(f32::NEG_INFINITY, f32::max);
            let bottom = points
                .iter()
                .map(|point| point.1)
                .fold(f32::NEG_INFINITY, f32::max);
            Rect {
                x: left - 1.0,
                y: top - 1.0,
                width: right - left + 2.0,
                height: bottom - top + 2.0,
            }
        } else {
            Rect {
                x: self.shift.0,
                y: self.shift.1,
                width: self.width as f32 / self.scale,
                height: self.height as f32 / self.scale,
            }
        };
        let Some(bounds) = DeviceBounds::of(self, target, clip) else {
            recycle_layer(layer.pixels);
            return;
        };
        let factor = opacity.clamp(0.0, 1.0);
        for pixel_y in bounds.top..bounds.bottom {
            for pixel_x in bounds.left..bounds.right {
                let x = (pixel_x as f32 + 0.5) / self.scale + self.shift.0;
                let y = (pixel_y as f32 + 0.5) / self.scale + self.shift.1;
                let Some((local_x, local_y)) = transform.unmap_point(x, y) else {
                    continue;
                };
                let u = (local_x - layer.shift.0) * layer.scale - 0.5;
                let v = (local_y - layer.shift.1) * layer.scale - 0.5;
                let sample = layer.sample_premultiplied(u, v);
                if sample[3] <= 0.0 {
                    continue;
                }
                let index = ((pixel_y * self.width + pixel_x) * 4) as usize;
                let alpha = sample[3] * factor;
                for (target, source) in self.pixels[index..index + 4].iter_mut().zip(sample) {
                    *target = (source * factor + f32::from(*target) * (1.0 - alpha / 255.0))
                        .round()
                        .clamp(0.0, 255.0) as u8;
                }
            }
        }
        recycle_layer(layer.pixels);
    }

    // bilinear sample of premultiplied pixels, transparent beyond the edges
    fn sample_premultiplied(&self, u: f32, v: f32) -> [f32; 4] {
        let (x0, y0) = (u.floor(), v.floor());
        let (fraction_x, fraction_y) = (u - x0, v - y0);
        let mut sum = [0.0_f32; 4];
        for (dy, weight_y) in [(0.0, 1.0 - fraction_y), (1.0, fraction_y)] {
            for (dx, weight_x) in [(0.0, 1.0 - fraction_x), (1.0, fraction_x)] {
                let (x, y) = (x0 + dx, y0 + dy);
                let weight = weight_x * weight_y;
                if weight == 0.0
                    || x < 0.0
                    || y < 0.0
                    || x >= self.width as f32
                    || y >= self.height as f32
                {
                    continue;
                }
                let index = ((y as u32 * self.width + x as u32) * 4) as usize;
                for (total, value) in sum.iter_mut().zip(&self.pixels[index..index + 4]) {
                    *total += f32::from(*value) * weight;
                }
            }
        }
        sum
    }

    fn blend_pixel(&mut self, x: u32, y: u32, color: Color) {
        let index = ((y * self.width + x) * 4) as usize;
        if color.alpha == 255 {
            self.pixels[index] = color.red;
            self.pixels[index + 1] = color.green;
            self.pixels[index + 2] = color.blue;
            self.pixels[index + 3] = 255;
            return;
        }
        let alpha = color.alpha as u32;
        let inverse = 255 - alpha;
        self.pixels[index] =
            ((color.red as u32 * alpha + self.pixels[index] as u32 * inverse) / 255) as u8;
        self.pixels[index + 1] =
            ((color.green as u32 * alpha + self.pixels[index + 1] as u32 * inverse) / 255) as u8;
        self.pixels[index + 2] =
            ((color.blue as u32 * alpha + self.pixels[index + 2] as u32 * inverse) / 255) as u8;
        self.pixels[index + 3] =
            ((alpha * 255 + self.pixels[index + 3] as u32 * inverse) / 255) as u8;
    }

    // a transparent surface the size of this one, cleared only inside the bounds that will be
    // composited; buffers are pooled so an animating fade does not allocate every frame
    // a transparent surface covering only the device pixels in bounds, aligned to this one
    // so compositing copies pixel for pixel
    fn layer(&self, bounds: DeviceBounds) -> Canvas {
        let width = bounds.right - bounds.left;
        let height = bounds.bottom - bounds.top;
        let mut pixels = LAYER_BUFFERS
            .with(|pool| pool.borrow_mut().pop())
            .unwrap_or_default();
        pixels.clear();
        pixels.resize(width as usize * height as usize * 4, 0);
        Canvas {
            width,
            height,
            scale: self.scale,
            background: Color::transparent(),
            pixels,
            shift: (
                self.shift.0 + bounds.left as f32 / self.scale,
                self.shift.1 + bounds.top as f32 / self.scale,
            ),
        }
    }

    // draws a premultiplied layer over this surface at the given group opacity
    fn composite(&mut self, layer: Canvas, bounds: DeviceBounds, opacity: f32) {
        let factor = (opacity.clamp(0.0, 1.0) * 256.0).round() as u32;
        let stride = self.width as usize * 4;
        let layer_stride = layer.width as usize * 4;
        for (row, source_row) in
            (bounds.top..bounds.bottom).zip(layer.pixels.chunks_exact(layer_stride))
        {
            let start = row as usize * stride + bounds.left as usize * 4;
            let end = row as usize * stride + bounds.right as usize * 4;
            for (target, source) in self.pixels[start..end]
                .chunks_exact_mut(4)
                .zip(source_row.chunks_exact(4))
            {
                if source[3] == 0 {
                    continue;
                }
                let alpha = (u32::from(source[3]) * factor) >> 8;
                let inverse = 255 - alpha;
                for (target, source) in target.iter_mut().zip(source) {
                    let faded = (u32::from(*source) * factor) >> 8;
                    *target = (faded + u32::from(*target) * inverse / 255).min(255) as u8;
                }
            }
        }
        recycle_layer(layer.pixels);
    }
}

const MAX_POOLED_LAYERS: usize = 8;
// larger buffers go back to the allocator, so one big layer does not stay resident
const MAX_POOLED_LAYER_BYTES: usize = 4 * 1024 * 1024;

fn recycle_layer(pixels: Vec<u8>) {
    if pixels.capacity() > MAX_POOLED_LAYER_BYTES {
        return;
    }
    LAYER_BUFFERS.with(|pool| {
        let mut pool = pool.borrow_mut();
        if pool.len() < MAX_POOLED_LAYERS {
            pool.push(pixels);
        }
    });
}
// a transformed subtree larger than this renders at reduced resolution instead of failing
const MAX_TRANSFORM_LAYER_PIXELS: f32 = 16_000_000.0;

thread_local! {
    static LAYER_BUFFERS: std::cell::RefCell<Vec<Vec<u8>>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[derive(Clone, Copy, Debug)]
struct DeviceBounds {
    left: u32,
    top: u32,
    right: u32,
    bottom: u32,
}

impl DeviceBounds {
    fn of(canvas: &Canvas, rect: Rect, clip: Option<Rect>) -> Option<Self> {
        let rect = intersect_clip(clip, rect)?;
        let rect = Rect {
            x: rect.x - canvas.shift.0,
            y: rect.y - canvas.shift.1,
            ..rect
        };
        let left = (rect.x * canvas.scale)
            .floor()
            .clamp(0.0, canvas.width as f32) as u32;
        let top = (rect.y * canvas.scale)
            .floor()
            .clamp(0.0, canvas.height as f32) as u32;
        let right = ((rect.x + rect.width) * canvas.scale)
            .ceil()
            .clamp(0.0, canvas.width as f32) as u32;
        let bottom = ((rect.y + rect.height) * canvas.scale)
            .ceil()
            .clamp(0.0, canvas.height as f32) as u32;
        (right > left && bottom > top).then_some(Self {
            left,
            top,
            right,
            bottom,
        })
    }
}

// everything a subtree can paint: its boxes, shadows and text, in layout space
fn subtree_bounds(layout_box: &LayoutBox, ancestor_offset: (f32, f32)) -> Rect {
    let offset = visual_offset_for(layout_box, ancestor_offset);
    let mut bounds = visual_rect_for(layout_box, ancestor_offset);
    if let Some(shadow) = layout_box.shadow {
        let reach = shadow.blur + shadow.spread.max(0.0);
        bounds = union_rect(
            bounds,
            Rect {
                x: bounds.x + shadow.offset_x - reach,
                y: bounds.y + shadow.offset_y - reach,
                width: bounds.width + reach * 2.0,
                height: bounds.height + reach * 2.0,
            },
        );
    }
    for run in &layout_box.text {
        bounds = union_rect(
            bounds,
            Rect {
                x: run.x + offset.0,
                y: run.y + offset.1,
                width: run.width,
                height: run.font_size * 1.5,
            },
        );
    }
    for child in &layout_box.children {
        bounds = union_rect(bounds, subtree_bounds(child, offset));
    }
    bounds
}

fn union_rect(first: Rect, second: Rect) -> Rect {
    let x = first.x.min(second.x);
    let y = first.y.min(second.y);
    Rect {
        x,
        y,
        width: (first.x + first.width).max(second.x + second.width) - x,
        height: (first.y + first.height).max(second.y + second.height) - y,
    }
}

// a box whose own paint cannot overlap itself fades correctly without a layer
fn fades_without_layer(layout_box: &LayoutBox) -> bool {
    layout_box.children.is_empty()
        && layout_box.text.is_empty()
        && layout_box.shadow.is_none()
        && (layout_box.border_width <= 0.0 || layout_box.border_radius <= 0.0)
        && layout_box.background_image.is_none()
        && !matches!(layout_box.tag.as_str(), "img" | "canvas")
        && layout_box.overflow == Overflow::Visible
}

fn write_png_chunk(file: &mut File, kind: &[u8; 4], body: &[u8]) -> io::Result<()> {
    file.write_all(&(body.len() as u32).to_be_bytes())?;
    file.write_all(kind)?;
    file.write_all(body)?;
    let mut checked = Vec::with_capacity(4 + body.len());
    checked.extend_from_slice(kind);
    checked.extend_from_slice(body);
    file.write_all(&crc32(&checked).to_be_bytes())
}

// stored deflate blocks keep the writer dependency-free at the cost of file size
fn zlib_stored(raw: &[u8]) -> Vec<u8> {
    let mut stream = vec![0x78, 0x01];
    let mut blocks = raw.chunks(0xffff).peekable();
    if raw.is_empty() {
        stream.extend_from_slice(&[1, 0, 0, 0xff, 0xff]);
    }
    while let Some(block) = blocks.next() {
        stream.push(u8::from(blocks.peek().is_none()));
        let length = block.len() as u16;
        stream.extend_from_slice(&length.to_le_bytes());
        stream.extend_from_slice(&(!length).to_le_bytes());
        stream.extend_from_slice(block);
    }
    stream.extend_from_slice(&adler32(raw).to_be_bytes());
    stream
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let carry = crc & 1;
            crc >>= 1;
            if carry != 0 {
                crc ^= 0xedb8_8320;
            }
        }
    }
    !crc
}

fn adler32(bytes: &[u8]) -> u32 {
    let mut low = 1_u32;
    let mut high = 0_u32;
    for byte in bytes {
        low = (low + u32::from(*byte)) % 65521;
        high = (high + low) % 65521;
    }
    (high << 16) | low
}

#[derive(Clone, Copy, Debug)]
struct Shape {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    radius: f32,
}

impl Shape {
    fn from_rect(rect: Rect, radius: f32) -> Self {
        Self {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
            radius,
        }
    }

    fn to_device(self, scale: f32) -> Self {
        Self {
            x: self.x * scale,
            y: self.y * scale,
            width: self.width * scale,
            height: self.height * scale,
            radius: self.radius * scale,
        }
    }

    fn inset(self, amount: f32) -> Self {
        Self {
            x: self.x + amount,
            y: self.y + amount,
            width: (self.width - amount * 2.0).max(0.0),
            height: (self.height - amount * 2.0).max(0.0),
            radius: (self.radius - amount).max(0.0),
        }
    }

    // one pixel of feathering along the edge, which reads as antialiasing
    fn coverage(self, point: (f32, f32), radius: f32) -> f32 {
        let distance = self.signed_distance(point, radius);
        (0.5 - distance).clamp(0.0, 1.0)
    }

    fn signed_distance(self, point: (f32, f32), radius: f32) -> f32 {
        let half = (self.width / 2.0, self.height / 2.0);
        let center = (self.x + half.0, self.y + half.1);
        let offset = (
            (point.0 - center.0).abs() - (half.0 - radius),
            (point.1 - center.1).abs() - (half.1 - radius),
        );
        let outside = (offset.0.max(0.0), offset.1.max(0.0));
        let outer = (outside.0 * outside.0 + outside.1 * outside.1).sqrt();
        outer + offset.0.max(offset.1).min(0.0) - radius
    }
}

enum Paint {
    Solid(Color),
    Gradient(Gradient),
}

impl Paint {
    fn color_at(&self, point: (f32, f32), shape: Shape) -> Color {
        match self {
            Self::Solid(color) => *color,
            Self::Gradient(gradient) => {
                let angle = gradient.angle.to_radians();
                let axis = (angle.sin(), -angle.cos());
                let center = (shape.x + shape.width / 2.0, shape.y + shape.height / 2.0);
                let extent = (shape.width * axis.0).abs() + (shape.height * axis.1).abs();
                let along = (point.0 - center.0) * axis.0 + (point.1 - center.1) * axis.1;
                let progress = if extent > 0.0 {
                    along / extent + 0.5
                } else {
                    0.0
                };
                gradient.color_at(progress)
            }
        }
    }
}

fn paint_shadow(
    canvas: &mut Canvas,
    shadow: &crate::layout::ResolvedShadow,
    rect: Rect,
    radius: f32,
    opacity: f32,
    clip: Option<Rect>,
) {
    let spread = shadow.spread;
    let base = Shape {
        x: rect.x + shadow.offset_x - spread,
        y: rect.y + shadow.offset_y - spread,
        width: rect.width + spread * 2.0,
        height: rect.height + spread * 2.0,
        radius: (radius + spread).max(0.0),
    };
    let color = shadow.color.scale_alpha(opacity);
    if shadow.blur <= 0.0 {
        canvas.fill_shape(base, &Paint::Solid(color), clip);
        return;
    }
    // concentric rings approximate a gaussian falloff without an offscreen buffer
    let steps = shadow.blur.ceil().clamp(1.0, 24.0) as usize;
    let step = shadow.blur / steps as f32;
    for ring in (0..=steps).rev() {
        let distance = step * ring as f32;
        let falloff = 1.0 - (ring as f32 / (steps as f32 + 1.0));
        let layer = Shape {
            x: base.x - distance,
            y: base.y - distance,
            width: base.width + distance * 2.0,
            height: base.height + distance * 2.0,
            radius: base.radius + distance,
        };
        canvas.fill_shape(
            layer,
            &Paint::Solid(color.scale_alpha(falloff * falloff / steps as f32 * 2.0)),
            clip,
        );
    }
}

pub(crate) fn paint(document: &LayoutDocument) -> Canvas {
    paint_with_commands(document, &[], 1.0)
}

pub(crate) fn paint_with_commands(
    document: &LayoutDocument,
    commands: &[CanvasCommand],
    scale: f32,
) -> Canvas {
    let mut canvas = Canvas::scaled(
        document.viewport.width,
        document.viewport.height,
        scale,
        Color::opaque(245, 248, 250),
    );
    // the root background paints the whole canvas, as CSS backgrounds require
    if let Some(root) = canvas_background(&document.root) {
        let viewport = Shape {
            x: 0.0,
            y: 0.0,
            width: document.viewport.width,
            height: document.viewport.height,
            radius: 0.0,
        };
        canvas.fill_shape(viewport, &root, None);
    }
    paint_box(
        &mut canvas,
        &document.root,
        commands,
        &document.images,
        &document.prose_selections,
        None,
        (0.0, 0.0),
    );
    canvas
}

fn canvas_background(root: &LayoutBox) -> Option<Paint> {
    let mut candidates = Vec::new();
    collect_root_boxes(root, 0, &mut candidates);
    candidates.into_iter().find_map(|layout_box| {
        if let Some(gradient) = layout_box.gradient {
            return Some(Paint::Gradient(gradient.scale_alpha(layout_box.opacity)));
        }
        (layout_box.background.alpha > 0)
            .then(|| Paint::Solid(layout_box.background.scale_alpha(layout_box.opacity)))
    })
}

fn collect_root_boxes<'a>(
    layout_box: &'a LayoutBox,
    depth: usize,
    output: &mut Vec<&'a LayoutBox>,
) {
    if depth > 2 {
        return;
    }
    if matches!(layout_box.tag.as_str(), "html" | "body") {
        output.push(layout_box);
    }
    for child in &layout_box.children {
        collect_root_boxes(child, depth + 1, output);
    }
}

fn intersect_clip(parent: Option<Rect>, rect: Rect) -> Option<Rect> {
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return None;
    }
    let Some(parent) = parent else {
        return Some(rect);
    };
    let left = parent.x.max(rect.x);
    let top = parent.y.max(rect.y);
    let right = (parent.x + parent.width).min(rect.x + rect.width);
    let bottom = (parent.y + parent.height).min(rect.y + rect.height);
    (right > left && bottom > top).then_some(Rect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

fn fill_rect_optional(
    canvas: &mut Canvas,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    color: Color,
    clip: Option<Rect>,
) {
    if let Some(clip) = clip {
        canvas.fill_rect_clipped(x, y, width, height, color, clip);
    } else {
        canvas.fill_rect(x, y, width, height, color);
    }
}

fn paint_canvas_command(
    canvas: &mut Canvas,
    canvas_box: &LayoutBox,
    command: &CanvasCommand,
    clip: Option<Rect>,
    rect: Rect,
) {
    let (path, x, y, width, height) = match command {
        CanvasCommand::FillRect {
            path,
            x,
            y,
            width,
            height,
            ..
        }
        | CanvasCommand::ClearRect {
            path,
            x,
            y,
            width,
            height,
        } => (path, *x, *y, *width, *height),
    };
    if path != &canvas_box.path {
        return;
    }
    let color = match command {
        CanvasCommand::FillRect { color, .. } => *color,
        CanvasCommand::ClearRect { .. } => {
            if canvas_box.background.alpha == 0 {
                canvas.background
            } else {
                canvas_box.background
            }
        }
    };
    let Some(command_clip) = intersect_clip(clip, rect) else {
        return;
    };
    canvas.fill_rect_clipped(rect.x + x, rect.y + y, width, height, color, command_clip);
}

fn paint_box(
    canvas: &mut Canvas,
    layout_box: &LayoutBox,
    commands: &[CanvasCommand],
    images: &[ImageLayer],
    prose_selections: &[ProseSpan],
    clip: Option<Rect>,
    ancestor_offset: (f32, f32),
) {
    paint_context(
        canvas,
        layout_box,
        commands,
        images,
        prose_selections,
        clip,
        ancestor_offset,
    );
}

fn paint_context(
    canvas: &mut Canvas,
    layout_box: &LayoutBox,
    commands: &[CanvasCommand],
    images: &[ImageLayer],
    prose_selections: &[ProseSpan],
    clip: Option<Rect>,
    ancestor_offset: (f32, f32),
) {
    let opacity = layout_box.opacity;
    if opacity <= 0.0 {
        return;
    }
    if let Some(transform) = layout_box.transform {
        let rect = visual_rect_for(layout_box, ancestor_offset);
        // the stored transform is relative to the border box corner
        let full = transform.about(rect.x, rect.y);
        if layout_box.backface_hidden && full.shows_back() {
            return;
        }
        let bounds = subtree_bounds(layout_box, ancestor_offset);
        // scaled content renders at the scale it is shown at, up to four times, so it stays sharp
        let mut layer_scale = canvas.scale * full.max_scale().clamp(1.0, 4.0);
        let area = bounds.width * bounds.height * layer_scale * layer_scale;
        if area > MAX_TRANSFORM_LAYER_PIXELS {
            layer_scale *= (MAX_TRANSFORM_LAYER_PIXELS / area).sqrt();
        }
        let mut layer = Canvas::transparent(bounds, layer_scale);
        paint_context_contents(
            &mut layer,
            layout_box,
            ContextPaint {
                commands,
                images,
                prose_selections,
                clip: None,
                ancestor_offset,
                opacity: 1.0,
            },
        );
        canvas.composite_transformed(layer, &full, clip, opacity);
        return;
    }
    // opacity fades the painted subtree as one image, so overlapping descendants do not
    // show through each other
    if opacity < 1.0 && !fades_without_layer(layout_box) {
        let Some(bounds) =
            DeviceBounds::of(canvas, subtree_bounds(layout_box, ancestor_offset), clip)
        else {
            return;
        };
        let mut layer = canvas.layer(bounds);
        paint_context_contents(
            &mut layer,
            layout_box,
            ContextPaint {
                commands,
                images,
                prose_selections,
                clip,
                ancestor_offset,
                opacity: 1.0,
            },
        );
        canvas.composite(layer, bounds, opacity);
        return;
    }
    paint_context_contents(
        canvas,
        layout_box,
        ContextPaint {
            commands,
            images,
            prose_selections,
            clip,
            ancestor_offset,
            opacity,
        },
    );
}

struct ContextPaint<'a> {
    commands: &'a [CanvasCommand],
    images: &'a [ImageLayer],
    prose_selections: &'a [ProseSpan],
    clip: Option<Rect>,
    ancestor_offset: (f32, f32),
    // the opacity this box's own surface paints with; a group layer applies it later
    opacity: f32,
}

fn paint_context_contents(canvas: &mut Canvas, layout_box: &LayoutBox, paint: ContextPaint<'_>) {
    let ContextPaint {
        commands,
        images,
        prose_selections,
        clip,
        ancestor_offset,
        opacity,
    } = paint;
    let offset = visual_offset_for(layout_box, ancestor_offset);
    let rect = visual_rect_for(layout_box, ancestor_offset);
    let content_clip = paint_box_surface(canvas, layout_box, images, clip, offset, rect, opacity);
    let mut contexts = stacking_context_entries(layout_box, clip, ancestor_offset);
    contexts.sort_by_key(|entry| (entry.z_index, entry.order));
    for entry in contexts.iter().filter(|entry| entry.z_index < 0) {
        paint_context(
            canvas,
            entry.layout,
            commands,
            images,
            prose_selections,
            entry.clip,
            entry.ancestor_offset,
        );
    }
    paint_box_text(
        canvas,
        layout_box,
        content_clip,
        offset,
        &prose_selections_for_box(layout_box, prose_selections),
    );
    paint_non_context_children(
        canvas,
        layout_box,
        commands,
        images,
        prose_selections,
        content_clip,
        offset,
    );
    paint_canvas_commands(canvas, layout_box, commands, content_clip, rect);
    for entry in contexts.iter().filter(|entry| entry.z_index >= 0) {
        paint_context(
            canvas,
            entry.layout,
            commands,
            images,
            prose_selections,
            entry.clip,
            entry.ancestor_offset,
        );
    }
    paint_scrollbars(canvas, layout_box, offset, clip);
}

fn paint_non_context_children(
    canvas: &mut Canvas,
    layout_box: &LayoutBox,
    commands: &[CanvasCommand],
    images: &[ImageLayer],
    prose_selections: &[ProseSpan],
    clip: Option<Rect>,
    ancestor_offset: (f32, f32),
) {
    for index in paint_order(&layout_box.children) {
        let child = &layout_box.children[index];
        if child.creates_stacking_context {
            continue;
        }
        paint_non_context_subtree(
            canvas,
            child,
            commands,
            images,
            prose_selections,
            clip,
            ancestor_offset,
        );
    }
}

fn paint_non_context_subtree(
    canvas: &mut Canvas,
    layout_box: &LayoutBox,
    commands: &[CanvasCommand],
    images: &[ImageLayer],
    prose_selections: &[ProseSpan],
    clip: Option<Rect>,
    ancestor_offset: (f32, f32),
) {
    let offset = visual_offset_for(layout_box, ancestor_offset);
    let rect = visual_rect_for(layout_box, ancestor_offset);
    let content_clip = paint_box_surface(
        canvas,
        layout_box,
        images,
        clip,
        offset,
        rect,
        layout_box.opacity,
    );
    paint_box_text(
        canvas,
        layout_box,
        content_clip,
        offset,
        &prose_selections_for_box(layout_box, prose_selections),
    );
    paint_non_context_children(
        canvas,
        layout_box,
        commands,
        images,
        prose_selections,
        content_clip,
        offset,
    );
    paint_canvas_commands(canvas, layout_box, commands, content_clip, rect);
    paint_scrollbars(canvas, layout_box, offset, clip);
}

fn paint_box_surface(
    canvas: &mut Canvas,
    layout_box: &LayoutBox,
    images: &[ImageLayer],
    clip: Option<Rect>,
    offset: (f32, f32),
    rect: Rect,
    opacity: f32,
) -> Option<Rect> {
    let radius = layout_box.border_radius;
    if let Some(shadow) = layout_box.shadow.as_ref() {
        paint_shadow(canvas, shadow, rect, radius, opacity, clip);
    }
    let outer = Shape::from_rect(rect, radius);
    let border = layout_box.border_width.max(0.0);
    if border > 0.0 && layout_box.border_color.alpha > 0 {
        let color = layout_box.border_color.scale_alpha(opacity);
        if radius > 0.0 {
            canvas.fill_shape(outer, &Paint::Solid(color), clip);
        } else {
            // square borders paint as four edges, so the box interior is filled once
            for edge in [
                Rect {
                    x: rect.x,
                    y: rect.y,
                    width: rect.width,
                    height: border,
                },
                Rect {
                    x: rect.x,
                    y: rect.y + rect.height - border,
                    width: rect.width,
                    height: border,
                },
                Rect {
                    x: rect.x,
                    y: rect.y + border,
                    width: border,
                    height: (rect.height - border * 2.0).max(0.0),
                },
                Rect {
                    x: rect.x + rect.width - border,
                    y: rect.y + border,
                    width: border,
                    height: (rect.height - border * 2.0).max(0.0),
                },
            ] {
                canvas.fill_shape(Shape::from_rect(edge, 0.0), &Paint::Solid(color), clip);
            }
        }
    }
    let surface = outer.inset(border);
    if let Some(gradient) = layout_box.gradient {
        canvas.fill_shape(
            surface,
            &Paint::Gradient(gradient.scale_alpha(opacity)),
            clip,
        );
    } else if layout_box.background.alpha > 0 {
        canvas.fill_shape(
            surface,
            &Paint::Solid(layout_box.background.scale_alpha(opacity)),
            clip,
        );
    }
    if let Some(background) = layout_box.background_image.as_ref()
        && let Some(layer) = images
            .iter()
            .find(|layer| layer.background && layer.path == layout_box.path)
    {
        let area = Rect {
            x: surface.x,
            y: surface.y,
            width: surface.width,
            height: surface.height,
        };
        paint_image(
            canvas,
            ImageDraw {
                image: &layer.image,
                placement: background_placement(
                    area,
                    &layer.image,
                    background,
                    layout_box.font_size,
                ),
                area: surface,
                clip,
                opacity,
                repeat: (background.repeat.x, background.repeat.y),
            },
        );
    }
    let content_clip = if matches!(layout_box.overflow, Overflow::Hidden | Overflow::Auto)
        || layout_box.path.is_empty()
    {
        intersect_clip(
            clip,
            translate_rect(scrollbar_metrics(layout_box).scrollport, offset),
        )
    } else {
        clip
    };
    if layout_box.tag == "img"
        && let Some(layer) = images
            .iter()
            .find(|layer| !layer.background && layer.path == layout_box.path)
    {
        let content = Rect {
            x: surface.x,
            y: surface.y,
            width: surface.width,
            height: surface.height,
        };
        paint_image(
            canvas,
            ImageDraw {
                image: &layer.image,
                placement: fitted_placement(content, &layer.image, layout_box.object_fit),
                area: surface,
                clip: content_clip,
                opacity,
                repeat: (false, false),
            },
        );
    }
    content_clip
}

fn paint_box_text(
    canvas: &mut Canvas,
    layout_box: &LayoutBox,
    clip: Option<Rect>,
    offset: (f32, f32),
    prose_selections: &[ProseSpan],
) {
    paint_control_selection(canvas, layout_box, clip, offset);
    paint_prose_selection(canvas, layout_box, prose_selections, clip, offset);
    for run in &layout_box.text {
        paint_text(canvas, run, clip, offset);
    }
    paint_control_caret(canvas, layout_box, clip, offset);
}

// highlights the selected UTF-16 span of runs that carry a DOM text node
fn paint_prose_selection(
    canvas: &mut Canvas,
    layout_box: &LayoutBox,
    prose_selections: &[ProseSpan],
    clip: Option<Rect>,
    offset: (f32, f32),
) {
    for span in prose_selections {
        for run in &layout_box.text {
            if run.node_path.as_ref() != Some(&span.path) || run.text.trim().is_empty() {
                continue;
            }
            let Some((selected_start, selected_end)) =
                run.selected_units(span.start, span.end, span.continued)
            else {
                continue;
            };
            let fonts = crate::font::active_fonts();
            let height = if fonts.is_builtin() {
                text_advance(run.font_size) / 6.0 * 7.0
            } else {
                run.font_size.max(1.0)
            };
            // the highlight spans from the first selected glyph to the last
            let prefix_width = width_within_run(run, selected_start, &fonts);
            let end_width = width_within_run(run, selected_end, &fonts);
            let selected_width = (end_width - prefix_width).max(0.0);
            fill_rect_optional(
                canvas,
                run.x + offset.0 + prefix_width,
                run.y + offset.1,
                selected_width,
                height,
                Color::opaque(170, 205, 238),
                clip,
            );
        }
    }
}

fn width_within_run(run: &TextRun, units: usize, fonts: &crate::font::FontSet) -> f32 {
    let mut width = 0.0;
    let mut consumed = 0;
    for character in run.text.chars() {
        if consumed >= units {
            break;
        }
        width +=
            fonts.advance(character, run.font_stack, run.font_size, run.bold) + run.letter_spacing;
        consumed += character.len_utf16();
    }
    width
}

fn prose_selections_for_box(
    layout_box: &LayoutBox,
    prose_selections: &[ProseSpan],
) -> Vec<ProseSpan> {
    prose_selections
        .iter()
        .filter(|span| {
            layout_box
                .text
                .iter()
                .any(|run| run.node_path.as_ref() == Some(&span.path))
        })
        .cloned()
        .collect()
}

fn paint_control_selection(
    canvas: &mut Canvas,
    layout_box: &LayoutBox,
    clip: Option<Rect>,
    offset: (f32, f32),
) {
    let Some(value) = &layout_box.control_value else {
        return;
    };
    let Some((start, end, _)) = layout_box.control_selection else {
        return;
    };
    if layout_box.control_text_origin.is_none() || start == end {
        return;
    }
    for (line_index, (line_start, line_end)) in control_line_ranges(value, &layout_box.text)
        .iter()
        .enumerate()
    {
        let Some(run) = layout_box.text.get(line_index) else {
            break;
        };
        let selected_start = start.max(*line_start);
        let selected_end = end.min(*line_end);
        if selected_start >= selected_end {
            continue;
        }
        let selected_start_char = utf16_char_offset(&run.text, selected_start - line_start);
        let selected_end_char = utf16_char_offset(&run.text, selected_end - line_start);
        let advance = text_advance(run.font_size);
        fill_rect_optional(
            canvas,
            run.x + offset.0 + selected_start_char as f32 * advance,
            run.y + offset.1,
            (selected_end_char - selected_start_char) as f32 * advance,
            run.font_size.max(1.0),
            Color::opaque(170, 205, 238),
            clip,
        );
    }
}

fn paint_control_caret(
    canvas: &mut Canvas,
    layout_box: &LayoutBox,
    clip: Option<Rect>,
    offset: (f32, f32),
) {
    if !layout_box.control_focused {
        return;
    }
    let Some(value) = &layout_box.control_value else {
        return;
    };
    if layout_box.control_text_origin.is_none() {
        return;
    }
    let caret = layout_box
        .control_selection
        .map(|(start, end, backward)| if backward { start } else { end })
        .unwrap_or_else(|| value.encode_utf16().count());
    let Some((x, y)) = control_text_position(layout_box, value, caret) else {
        return;
    };
    fill_rect_optional(
        canvas,
        x + offset.0,
        y + offset.1,
        1.0,
        layout_box.font_size.max(1.0),
        Color::BLACK,
        clip,
    );
}

fn control_line_ranges(value: &str, lines: &[TextRun]) -> Vec<(usize, usize)> {
    let units = value.encode_utf16().collect::<Vec<_>>();
    let mut start = 0;
    let mut ranges = Vec::new();
    for line in lines {
        let end = (start + line.text.encode_utf16().count()).min(units.len());
        ranges.push((start, end));
        start = end;
        if units.get(start) == Some(&(b'\n' as u16)) {
            start += 1;
        }
    }
    ranges
}

fn control_text_position(layout_box: &LayoutBox, value: &str, offset: usize) -> Option<(f32, f32)> {
    let ranges = control_line_ranges(value, &layout_box.text);
    let mut line_index = ranges.len().saturating_sub(1);
    for (index, (_, line_end)) in ranges.iter().enumerate() {
        if offset <= *line_end || index + 1 == ranges.len() {
            line_index = index;
            break;
        }
    }
    let (line_start, line_end) = ranges.get(line_index).copied()?;
    let run = layout_box.text.get(line_index)?;
    let line_offset = offset.saturating_sub(line_start).min(line_end - line_start);
    let character_offset = utf16_char_offset(&run.text, line_offset);
    Some((
        run.x + character_offset as f32 * text_advance(run.font_size),
        run.y,
    ))
}

fn text_advance(font_size: f32) -> f32 {
    (font_size / 9.0).round().max(1.0) * 6.0
}

fn utf16_char_offset(value: &str, offset: usize) -> usize {
    let mut units = 0;
    let mut characters = 0;
    for character in value.chars() {
        let length = character.len_utf16();
        if units + length > offset {
            break;
        }
        units += length;
        characters += 1;
    }
    characters
}

fn paint_canvas_commands(
    canvas: &mut Canvas,
    layout_box: &LayoutBox,
    commands: &[CanvasCommand],
    clip: Option<Rect>,
    rect: Rect,
) {
    if layout_box.tag != "canvas" {
        return;
    }
    for command in commands {
        paint_canvas_command(canvas, layout_box, command, clip, rect);
    }
}

fn paint_scrollbars(
    canvas: &mut Canvas,
    layout_box: &LayoutBox,
    offset: (f32, f32),
    clip: Option<Rect>,
) {
    let metrics = scrollbar_metrics(layout_box);
    let track_color = Color::opaque(232, 236, 240);
    let thumb_color = Color::opaque(126, 138, 150);
    if let Some(track) = metrics.vertical_track {
        let track = translate_rect(track, offset);
        fill_rect_optional(
            canvas,
            track.x,
            track.y,
            track.width,
            track.height,
            track_color,
            clip,
        );
        let thumb = scrollbar_thumb(
            track,
            metrics.scrollport.height,
            layout_box.scroll_content.height,
            layout_box.scroll_offset.1,
            (layout_box.scroll_content.height - metrics.scrollport.height).max(0.0),
            true,
        );
        fill_rect_optional(
            canvas,
            thumb.x,
            thumb.y,
            thumb.width,
            thumb.height,
            thumb_color,
            clip,
        );
    }
    if let Some(track) = metrics.horizontal_track {
        let track = translate_rect(track, offset);
        fill_rect_optional(
            canvas,
            track.x,
            track.y,
            track.width,
            track.height,
            track_color,
            clip,
        );
        let thumb = scrollbar_thumb(
            track,
            metrics.scrollport.width,
            layout_box.scroll_content.width,
            layout_box.scroll_offset.0,
            (layout_box.scroll_content.width - metrics.scrollport.width).max(0.0),
            false,
        );
        fill_rect_optional(
            canvas,
            thumb.x,
            thumb.y,
            thumb.width,
            thumb.height,
            thumb_color,
            clip,
        );
    }
    if let (Some(vertical), Some(horizontal)) = (metrics.vertical_track, metrics.horizontal_track) {
        let corner = translate_rect(
            Rect {
                x: vertical.x,
                y: horizontal.y,
                width: vertical.width,
                height: horizontal.height,
            },
            offset,
        );
        fill_rect_optional(
            canvas,
            corner.x,
            corner.y,
            corner.width,
            corner.height,
            track_color,
            clip,
        );
    }
}

fn scrollbar_thumb(
    track: Rect,
    viewport_extent: f32,
    content_extent: f32,
    offset: f32,
    max_offset: f32,
    vertical: bool,
) -> Rect {
    let track_extent = if vertical { track.height } else { track.width };
    let minimum = 8.0_f32.min(track_extent);
    let thumb_extent = if content_extent > 0.0 {
        (track_extent * (viewport_extent / content_extent).clamp(0.0, 1.0))
            .max(minimum)
            .min(track_extent)
    } else {
        track_extent
    };
    let travel = (track_extent - thumb_extent).max(0.0);
    let progress = if max_offset > 0.0 {
        (offset / max_offset).clamp(0.0, 1.0)
    } else {
        0.0
    };
    if vertical {
        Rect {
            x: track.x,
            y: track.y + travel * progress,
            width: track.width,
            height: thumb_extent,
        }
    } else {
        Rect {
            x: track.x + travel * progress,
            y: track.y,
            width: thumb_extent,
            height: track.height,
        }
    }
}

// an image mapped onto `placement` in layout space and painted only inside `area`,
// tiled across the area along the axes that repeat
struct ImageDraw<'a> {
    image: &'a Image,
    placement: Rect,
    area: Shape,
    clip: Option<Rect>,
    opacity: f32,
    repeat: (bool, bool),
}

fn paint_image(canvas: &mut Canvas, draw: ImageDraw<'_>) {
    let ImageDraw {
        image,
        placement,
        area,
        clip,
        opacity,
        repeat,
    } = draw;
    if placement.width <= 0.0
        || placement.height <= 0.0
        || image.width == 0
        || image.height == 0
        || opacity <= 0.0
    {
        return;
    }
    let scale = canvas.scale;
    let shift = canvas.shift;
    let area_device = canvas.device_shape(area);
    let radius = area_device
        .radius
        .min(area_device.width / 2.0)
        .min(area_device.height / 2.0)
        .max(0.0);
    let mut bounds = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: area.height,
    };
    if !repeat.0 {
        bounds = intersect_rect(
            bounds,
            Rect {
                y: bounds.y,
                height: bounds.height,
                ..placement
            },
        );
    }
    if !repeat.1 {
        bounds = intersect_rect(
            bounds,
            Rect {
                x: bounds.x,
                width: bounds.width,
                ..placement
            },
        );
    }
    let Some(bounds) = intersect_clip(clip, bounds) else {
        return;
    };
    let left = ((bounds.x - shift.0) * scale).floor().max(0.0) as u32;
    let top = ((bounds.y - shift.1) * scale).floor().max(0.0) as u32;
    let right = ((bounds.x - shift.0 + bounds.width) * scale)
        .ceil()
        .clamp(0.0, canvas.width as f32) as u32;
    let bottom = ((bounds.y - shift.1 + bounds.height) * scale)
        .ceil()
        .clamp(0.0, canvas.height as f32) as u32;
    let origin = (
        (placement.x - shift.0) * scale,
        (placement.y - shift.1) * scale,
    );
    let step = (
        image.width as f32 / (placement.width * scale),
        image.height as f32 / (placement.height * scale),
    );
    // whole-pixel placement at the image's own resolution copies pixels directly
    let aligned = (step.0 - 1.0).abs() < 1e-4
        && (step.1 - 1.0).abs() < 1e-4
        && origin.0.fract() == 0.0
        && origin.1.fract() == 0.0;
    for pixel_y in top..bottom {
        for pixel_x in left..right {
            let center = (pixel_x as f32 + 0.5, pixel_y as f32 + 0.5);
            let coverage = if radius > 0.0 {
                area_device.coverage(center, radius)
            } else {
                1.0
            };
            if coverage <= 0.0 {
                continue;
            }
            let color = if aligned {
                let column = wrap_or_none(pixel_x as f32 - origin.0, image.width, repeat.0);
                let row = wrap_or_none(pixel_y as f32 - origin.1, image.height, repeat.1);
                let (Some(column), Some(row)) = (column, row) else {
                    continue;
                };
                image.pixel(column, row)
            } else {
                let u = (center.0 - origin.0) * step.0 - 0.5;
                let v = (center.1 - origin.1) * step.1 - 0.5;
                sample_bilinear(image, u, v, repeat)
            };
            if color.alpha == 0 {
                continue;
            }
            canvas.blend_pixel(pixel_x, pixel_y, color.scale_alpha(opacity * coverage));
        }
    }
}

fn wrap_or_none(position: f32, size: u32, repeat: bool) -> Option<u32> {
    let position = position as i64;
    if repeat {
        Some(position.rem_euclid(i64::from(size)) as u32)
    } else {
        (0..i64::from(size))
            .contains(&position)
            .then_some(position as u32)
    }
}

// interpolates in premultiplied alpha so transparent neighbours do not darken edges
fn sample_bilinear(image: &Image, u: f32, v: f32, repeat: (bool, bool)) -> Color {
    let (x0, y0) = (u.floor(), v.floor());
    let (fraction_x, fraction_y) = (u - x0, v - y0);
    let index = |position: f32, size: u32, repeats: bool| -> u32 {
        let position = position as i64;
        if repeats {
            position.rem_euclid(i64::from(size)) as u32
        } else {
            position.clamp(0, i64::from(size) - 1) as u32
        }
    };
    let mut sum = [0.0_f32; 4];
    for (dy, weight_y) in [(0.0, 1.0 - fraction_y), (1.0, fraction_y)] {
        for (dx, weight_x) in [(0.0, 1.0 - fraction_x), (1.0, fraction_x)] {
            let weight = weight_x * weight_y;
            if weight == 0.0 {
                continue;
            }
            let color = image.pixel(
                index(x0 + dx, image.width, repeat.0),
                index(y0 + dy, image.height, repeat.1),
            );
            let alpha = f32::from(color.alpha) * weight;
            sum[0] += f32::from(color.red) * alpha;
            sum[1] += f32::from(color.green) * alpha;
            sum[2] += f32::from(color.blue) * alpha;
            sum[3] += alpha;
        }
    }
    if sum[3] <= 0.0 {
        return Color::transparent();
    }
    let channel = |value: f32| (value / sum[3]).round().clamp(0.0, 255.0) as u8;
    Color::rgba(
        channel(sum[0]),
        channel(sum[1]),
        channel(sum[2]),
        sum[3].round().clamp(0.0, 255.0) as u8,
    )
}

fn intersect_rect(first: Rect, second: Rect) -> Rect {
    let x = first.x.max(second.x);
    let y = first.y.max(second.y);
    Rect {
        x,
        y,
        width: ((first.x + first.width).min(second.x + second.width) - x).max(0.0),
        height: ((first.y + first.height).min(second.y + second.height) - y).max(0.0),
    }
}

// where object-fit puts the image inside the content box, centered as object-position defaults
fn fitted_placement(content: Rect, image: &Image, fit: ObjectFit) -> Rect {
    let (width, height) = (image.width as f32, image.height as f32);
    let contain = (content.width / width).min(content.height / height);
    let scale = match fit {
        ObjectFit::Fill => return content,
        ObjectFit::Contain => contain,
        ObjectFit::Cover => (content.width / width).max(content.height / height),
        ObjectFit::None => 1.0,
        ObjectFit::ScaleDown => contain.min(1.0),
    };
    let (placed_width, placed_height) = (width * scale, height * scale);
    Rect {
        x: content.x + (content.width - placed_width) / 2.0,
        y: content.y + (content.height - placed_height) / 2.0,
        width: placed_width,
        height: placed_height,
    }
}

// the first tile of a background image inside its positioning area
fn background_placement(
    area: Rect,
    image: &Image,
    background: &BackgroundImage,
    font_size: f32,
) -> Rect {
    let (natural_width, natural_height) = (image.width as f32, image.height as f32);
    let (width, height) = match background.size {
        BackgroundSize::Auto => (natural_width, natural_height),
        BackgroundSize::Cover | BackgroundSize::Contain => {
            let horizontal = area.width / natural_width;
            let vertical = area.height / natural_height;
            let scale = if background.size == BackgroundSize::Cover {
                horizontal.max(vertical)
            } else {
                horizontal.min(vertical)
            };
            (natural_width * scale, natural_height * scale)
        }
        BackgroundSize::Explicit(width, height) => {
            let width = width.resolve(area.width, font_size);
            let height = height.resolve(area.height, font_size);
            match (width, height) {
                (Some(width), Some(height)) => (width, height),
                (Some(width), None) => (width, width * natural_height / natural_width),
                (None, Some(height)) => (height * natural_width / natural_height, height),
                (None, None) => (natural_width, natural_height),
            }
        }
    };
    // percentages place the same point of the image and the area on each other
    let x = background
        .position
        .0
        .resolve(area.width - width, font_size)
        .unwrap_or(0.0);
    let y = background
        .position
        .1
        .resolve(area.height - height, font_size)
        .unwrap_or(0.0);
    Rect {
        x: area.x + x,
        y: area.y + y,
        width,
        height,
    }
}

fn paint_text(canvas: &mut Canvas, run: &TextRun, clip: Option<Rect>, offset: (f32, f32)) {
    let fonts = crate::font::active_fonts();
    if !fonts.is_builtin() {
        paint_outline_text(canvas, run, clip, offset, &fonts);
        return;
    }
    let cell = text_advance(run.font_size) + run.letter_spacing;
    let pixel = text_advance(run.font_size) / 6.0;
    let mut x = run.x;
    for character in run.text.chars() {
        let glyph = glyph_rows(character);
        for (row, bits) in glyph.iter().enumerate() {
            for column in 0..5 {
                if bits & (1 << (4 - column)) != 0 {
                    let width = if run.bold { pixel + 1.0 } else { pixel };
                    fill_rect_optional(
                        canvas,
                        x + column as f32 * pixel + offset.0,
                        run.y + row as f32 * pixel + offset.1,
                        width,
                        pixel,
                        run.color,
                        clip,
                    );
                }
            }
        }
        x += cell;
    }
}

fn paint_outline_text(
    canvas: &mut Canvas,
    run: &TextRun,
    clip: Option<Rect>,
    offset: (f32, f32),
    fonts: &crate::font::FontSet,
) {
    let scale = canvas.scale();
    let device_size = run.font_size * scale;
    let shift = canvas.shift;
    let baseline =
        (run.y + offset.1 - shift.1 + fonts.ascent(run.font_stack, run.font_size)) * scale;
    let mut pen = (run.x + offset.0 - shift.0) * scale;
    let device_clip = clip.map(|clip| Rect {
        x: (clip.x - shift.0) * scale,
        y: (clip.y - shift.1) * scale,
        width: clip.width * scale,
        height: clip.height * scale,
    });
    for character in run.text.chars() {
        if let Some(glyph) = fonts.glyph(character, run.font_stack, device_size, run.bold) {
            canvas.blend_coverage(
                pen.round() as i32 + glyph.left,
                baseline.round() as i32 + glyph.top,
                &glyph,
                run.color,
                device_clip,
            );
        }
        pen += fonts.advance(character, run.font_stack, device_size, run.bold)
            + run.letter_spacing * scale;
    }
}

fn glyph_rows(character: char) -> [u8; 7] {
    match character.to_ascii_uppercase() {
        'A' => [
            0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'B' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110,
        ],
        'C' => [
            0b01111, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b01111,
        ],
        'D' => [
            0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110,
        ],
        'E' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111,
        ],
        'F' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'G' => [
            0b01111, 0b10000, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111,
        ],
        'H' => [
            0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'I' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b11111,
        ],
        'J' => [
            0b00111, 0b00010, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100,
        ],
        'K' => [
            0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001,
        ],
        'L' => [
            0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111,
        ],
        'M' => [
            0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001,
        ],
        'N' => [
            0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001,
        ],
        'O' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'P' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'Q' => [
            0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101,
        ],
        'R' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001,
        ],
        'S' => [
            0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        'T' => [
            0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'U' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        'V' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100,
        ],
        'W' => [
            0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001,
        ],
        'X' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001,
        ],
        'Y' => [
            0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100,
        ],
        'Z' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111,
        ],
        '0' => [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        '1' => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        '2' => [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ],
        '3' => [
            0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110,
        ],
        '4' => [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        '5' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b00001, 0b00001, 0b11110,
        ],
        '6' => [
            0b01110, 0b10000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        '7' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        '8' => [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        '9' => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00001, 0b01110,
        ],
        '-' => [0, 0, 0, 0b11111, 0, 0, 0],
        '/' => [0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0, 0],
        '.' => [0, 0, 0, 0, 0, 0b00110, 0b00110],
        ':' => [0, 0b00110, 0b00110, 0, 0b00110, 0b00110, 0],
        '!' => [0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0, 0b00100],
        '?' => [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0, 0b00100],
        '(' => [
            0b00010, 0b00100, 0b01000, 0b01000, 0b01000, 0b00100, 0b00010,
        ],
        ')' => [
            0b01000, 0b00100, 0b00010, 0b00010, 0b00010, 0b00100, 0b01000,
        ],
        _ => [0, 0, 0, 0, 0, 0, 0],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::Stylesheet;
    use crate::dom::Document;
    use crate::layout::{LayoutBox, LayoutDocument, Rect, Size};

    #[test]
    fn prose_selection_highlight_covers_the_selected_glyph_span() {
        let document = LayoutDocument {
            viewport: Size {
                width: 64.0,
                height: 24.0,
            },
            root: LayoutBox {
                path: Vec::new(),
                tag: "root".to_string(),
                rect: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 64.0,
                    height: 24.0,
                },
                background: Color::opaque(245, 248, 250),
                gradient: None,
                background_image: None,
                object_fit: crate::css::ObjectFit::Fill,
                transform: None,
                backface_hidden: false,
                border_radius: 0.0,
                opacity: 1.0,
                shadow: None,
                border_width: 0.0,
                border_color: Color::transparent(),
                text: vec![TextRun {
                    text: "ABCDEF".to_string(),
                    x: 4.0,
                    y: 2.0,
                    width: 36.0,
                    font_size: 12.0,
                    letter_spacing: 0.0,
                    font_stack: crate::font::FontStack::default(),
                    color: Color::BLACK,
                    bold: false,
                    node_path: Some(vec![1, 0]),
                    node_offset: 0,
                    source_len: 6,
                }],
                control_value: None,
                control_selection: None,
                control_focused: false,
                control_text_origin: None,
                children: Vec::new(),
                position: crate::css::Position::Static,
                visual_offset: (0.0, 0.0),
                z_index: 0,
                creates_stacking_context: true,
                sticky_inset: crate::css::Edges::auto(),
                font_size: 16.0,
                overflow: Overflow::Visible,
                scroll_content: Size {
                    width: 64.0,
                    height: 24.0,
                },
                scroll_offset: (0.0, 0.0),
                margin: Default::default(),
            },
            images: Vec::new(),
            prose_selections: vec![crate::layout::ProseSpan {
                path: vec![1, 0],
                start: 1,
                end: 2,
                continued: false,
            }],
        };

        let canvas = paint(&document);
        let pixels = canvas.rgba_pixels();
        let stride = canvas.width() as usize;
        let page_background = [245, 248, 250];
        let cell = text_advance(12.0);
        let glyph_x = (4.0 + cell) as usize;
        for row in 0..7 {
            for column in 0..5 {
                let x = glyph_x + column as usize;
                let y = 2 + row as usize;
                let index = (y * stride + x) * 4;
                assert_ne!(
                    pixels[index..index + 3],
                    page_background,
                    "pixel ({x},{y}) inside the selected glyph should sit on the highlight"
                );
            }
        }
    }

    #[test]
    fn rounded_gradient_and_faded_surfaces_reach_the_pixels() {
        let document = Document::parse("<html><body><div id='card'></div></body></html>")
            .expect("document should parse");
        let stylesheet = Stylesheet::parse(
            "html, body { margin: 0 } #card { width: 40px; height: 40px; border-radius: 12px; background: #ff0000 }",
        )
        .expect("stylesheet should parse");
        let layout = crate::layout::layout(
            &document,
            &stylesheet,
            Size {
                width: 40.0,
                height: 40.0,
            },
        );
        let canvas = paint(&layout);
        let pixel = |x: usize, y: usize| {
            let index = (y * canvas.width() as usize + x) * 4;
            canvas.rgba_pixels()[index..index + 3].to_vec()
        };
        assert_eq!(
            pixel(20, 20),
            vec![255, 0, 0],
            "the centre should be filled"
        );
        assert_ne!(pixel(0, 0), vec![255, 0, 0], "the corner should be rounded");

        let faded = Stylesheet::parse(
            "html, body { margin: 0 } #card { width: 40px; height: 40px; background: #ff0000; opacity: 0.5 }",
        )
        .expect("stylesheet should parse");
        let canvas = paint(&crate::layout::layout(
            &document,
            &faded,
            Size {
                width: 40.0,
                height: 40.0,
            },
        ));
        let index = (20 * canvas.width() as usize + 20) * 4;
        let blended = canvas.rgba_pixels()[index];
        assert!(
            (240..=255).contains(&blended),
            "half opacity should blend towards the page background, got {blended}"
        );

        let gradient = Stylesheet::parse(
            "html, body { margin: 0 } #card { width: 40px; height: 40px; background: linear-gradient(90deg, #000000, #ffffff) }",
        )
        .expect("stylesheet should parse");
        let canvas = paint(&crate::layout::layout(
            &document,
            &gradient,
            Size {
                width: 40.0,
                height: 40.0,
            },
        ));
        let left = canvas.rgba_pixels()[(20 * canvas.width() as usize + 2) * 4];
        let right = canvas.rgba_pixels()[(20 * canvas.width() as usize + 37) * 4];
        assert!(
            right > left + 100,
            "the gradient should brighten across the box, got {left} then {right}"
        );
    }

    #[test]
    fn painting_changes_pixels_and_writes_a_ppm_header() {
        let document = LayoutDocument {
            viewport: Size {
                width: 32.0,
                height: 24.0,
            },
            root: LayoutBox {
                path: Vec::new(),
                tag: "root".to_string(),
                rect: Rect {
                    x: 2.0,
                    y: 3.0,
                    width: 20.0,
                    height: 10.0,
                },
                background: Color::opaque(10, 20, 30),
                gradient: None,
                background_image: None,
                object_fit: crate::css::ObjectFit::Fill,
                transform: None,
                backface_hidden: false,
                border_radius: 0.0,
                opacity: 1.0,
                shadow: None,
                border_width: 0.0,
                border_color: Color::transparent(),
                text: vec![TextRun {
                    text: "A".to_string(),
                    x: 4.0,
                    y: 5.0,
                    width: 8.0,
                    font_size: 12.0,
                    letter_spacing: 0.0,
                    font_stack: crate::font::FontStack::default(),
                    color: Color::WHITE,
                    bold: false,
                    node_path: None,
                    node_offset: 0,
                    source_len: 0,
                }],
                control_value: None,
                control_selection: None,
                control_focused: false,
                control_text_origin: None,
                children: Vec::new(),
                position: crate::css::Position::Static,
                visual_offset: (0.0, 0.0),
                z_index: 0,
                creates_stacking_context: true,
                sticky_inset: crate::css::Edges::auto(),
                font_size: 16.0,
                overflow: Overflow::Visible,
                scroll_content: Size {
                    width: 20.0,
                    height: 10.0,
                },
                scroll_offset: (0.0, 0.0),
                margin: Default::default(),
            },
            images: Vec::new(),
            prose_selections: Vec::new(),
        };
        let canvas = paint(&document);
        assert!(canvas.non_background_pixels() > 0);

        let mut focused = document.clone();
        focused.root.control_value = Some("A".to_string());
        focused.root.control_selection = Some((0, 1, false));
        focused.root.control_focused = true;
        focused.root.control_text_origin = Some((4.0, 5.0));
        let focused_canvas = paint(&focused);
        assert_ne!(
            focused_canvas.rgba_pixels(),
            canvas.rgba_pixels(),
            "focused controls should add a software caret"
        );
        let caret_x = (4.0 + text_advance(12.0)) as u32;
        let caret_index = ((5 * focused_canvas.width() + caret_x) * 4) as usize;
        assert_eq!(
            &focused_canvas.rgba_pixels()[caret_index..caret_index + 3],
            &[0, 0, 0]
        );
        let selection_index = ((5 * focused_canvas.width() + 4) * 4) as usize;
        assert_eq!(
            &focused_canvas.rgba_pixels()[selection_index..selection_index + 3],
            &[170, 205, 238]
        );
        focused.root.control_selection = Some((0, 1, true));
        let backward_canvas = paint(&focused);
        let backward_caret_index = ((5 * backward_canvas.width() + 4) * 4) as usize;
        assert_eq!(
            &backward_canvas.rgba_pixels()[backward_caret_index..backward_caret_index + 3],
            &[0, 0, 0]
        );

        let path = std::env::temp_dir().join(format!("ceri-engine-{}.bmp", std::process::id()));
        canvas.write_bmp(&path).expect("bmp should write");
        let bytes = std::fs::read(&path).expect("bmp should be readable");
        assert_eq!(&bytes[..2], b"BM");
        std::fs::remove_file(path).expect("test bmp should be removable");
    }

    #[test]
    fn painting_textarea_selection_and_caret_use_line_offsets() {
        let mut document = LayoutDocument {
            viewport: Size {
                width: 40.0,
                height: 40.0,
            },
            root: LayoutBox {
                path: Vec::new(),
                tag: "textarea".to_string(),
                rect: Rect {
                    x: 2.0,
                    y: 3.0,
                    width: 32.0,
                    height: 28.0,
                },
                background: Color::opaque(10, 20, 30),
                gradient: None,
                background_image: None,
                object_fit: crate::css::ObjectFit::Fill,
                transform: None,
                backface_hidden: false,
                border_radius: 0.0,
                opacity: 1.0,
                shadow: None,
                border_width: 0.0,
                border_color: Color::transparent(),
                text: vec![
                    TextRun {
                        text: "AB".to_string(),
                        x: 4.0,
                        y: 5.0,
                        width: 6.0,
                        font_size: 12.0,
                        letter_spacing: 0.0,
                        font_stack: crate::font::FontStack::default(),
                        color: Color::WHITE,
                        bold: false,
                        node_path: None,
                        node_offset: 0,
                        source_len: 0,
                    },
                    TextRun {
                        text: "CD".to_string(),
                        x: 4.0,
                        y: 20.0,
                        width: 6.0,
                        font_size: 12.0,
                        letter_spacing: 0.0,
                        font_stack: crate::font::FontStack::default(),
                        color: Color::WHITE,
                        bold: false,
                        node_path: None,
                        node_offset: 0,
                        source_len: 0,
                    },
                ],
                control_value: Some("ABCD".to_string()),
                control_selection: Some((2, 3, false)),
                control_focused: true,
                control_text_origin: Some((4.0, 5.0)),
                children: Vec::new(),
                position: crate::css::Position::Static,
                visual_offset: (0.0, 0.0),
                z_index: 0,
                creates_stacking_context: true,
                sticky_inset: crate::css::Edges::auto(),
                font_size: 12.0,
                overflow: Overflow::Visible,
                scroll_content: Size {
                    width: 32.0,
                    height: 28.0,
                },
                scroll_offset: (0.0, 0.0),
                margin: Default::default(),
            },
            images: Vec::new(),
            prose_selections: Vec::new(),
        };
        let canvas = paint(&document);
        let selection_index = ((20 * canvas.width() + 9) * 4) as usize;
        assert_eq!(
            &canvas.rgba_pixels()[selection_index..selection_index + 3],
            &[170, 205, 238]
        );
        let caret_index = ((20 * canvas.width() + 10) * 4) as usize;
        assert_eq!(
            &canvas.rgba_pixels()[caret_index..caret_index + 3],
            &[0, 0, 0]
        );
        document.root.control_selection = Some((0, 0, false));
        assert_ne!(paint(&document).rgba_pixels(), canvas.rgba_pixels());
    }

    #[test]
    fn canvas_commands_stay_in_document_paint_order() {
        let document = LayoutDocument {
            viewport: Size {
                width: 20.0,
                height: 20.0,
            },
            root: LayoutBox {
                path: Vec::new(),
                tag: "root".to_string(),
                rect: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 20.0,
                    height: 20.0,
                },
                background: Color::opaque(245, 248, 250),
                gradient: None,
                background_image: None,
                object_fit: crate::css::ObjectFit::Fill,
                transform: None,
                backface_hidden: false,
                border_radius: 0.0,
                opacity: 1.0,
                shadow: None,
                border_width: 0.0,
                border_color: Color::transparent(),
                text: Vec::new(),
                control_value: None,
                control_selection: None,
                control_focused: false,
                control_text_origin: None,
                children: vec![
                    LayoutBox {
                        path: vec![0],
                        tag: "canvas".to_string(),
                        rect: Rect {
                            x: 0.0,
                            y: 0.0,
                            width: 10.0,
                            height: 10.0,
                        },
                        background: Color::transparent(),
                        gradient: None,
                        background_image: None,
                        object_fit: crate::css::ObjectFit::Fill,
                        transform: None,
                        backface_hidden: false,
                        border_radius: 0.0,
                        opacity: 1.0,
                        shadow: None,
                        border_width: 0.0,
                        border_color: Color::transparent(),
                        text: Vec::new(),
                        control_value: None,
                        control_selection: None,
                        control_focused: false,
                        control_text_origin: None,
                        children: Vec::new(),
                        position: crate::css::Position::Static,
                        visual_offset: (0.0, 0.0),
                        z_index: 0,
                        creates_stacking_context: false,
                        sticky_inset: crate::css::Edges::auto(),
                        font_size: 16.0,
                        overflow: Overflow::Visible,
                        scroll_content: Size {
                            width: 10.0,
                            height: 10.0,
                        },
                        scroll_offset: (0.0, 0.0),
                        margin: Default::default(),
                    },
                    LayoutBox {
                        path: vec![1],
                        tag: "div".to_string(),
                        rect: Rect {
                            x: 0.0,
                            y: 0.0,
                            width: 10.0,
                            height: 10.0,
                        },
                        background: Color::opaque(0, 0, 255),
                        gradient: None,
                        background_image: None,
                        object_fit: crate::css::ObjectFit::Fill,
                        transform: None,
                        backface_hidden: false,
                        border_radius: 0.0,
                        opacity: 1.0,
                        shadow: None,
                        border_width: 0.0,
                        border_color: Color::transparent(),
                        text: Vec::new(),
                        control_value: None,
                        control_selection: None,
                        control_focused: false,
                        control_text_origin: None,
                        children: Vec::new(),
                        position: crate::css::Position::Static,
                        visual_offset: (0.0, 0.0),
                        z_index: 0,
                        creates_stacking_context: false,
                        sticky_inset: crate::css::Edges::auto(),
                        font_size: 16.0,
                        overflow: Overflow::Visible,
                        scroll_content: Size {
                            width: 10.0,
                            height: 10.0,
                        },
                        scroll_offset: (0.0, 0.0),
                        margin: Default::default(),
                    },
                ],
                position: crate::css::Position::Static,
                visual_offset: (0.0, 0.0),
                z_index: 0,
                creates_stacking_context: true,
                sticky_inset: crate::css::Edges::auto(),
                font_size: 16.0,
                overflow: Overflow::Visible,
                scroll_content: Size {
                    width: 20.0,
                    height: 20.0,
                },
                scroll_offset: (0.0, 0.0),
                margin: Default::default(),
            },
            images: Vec::new(),
            prose_selections: Vec::new(),
        };
        let commands = [CanvasCommand::FillRect {
            path: vec![0],
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
            color: Color::opaque(255, 0, 0),
        }];

        let canvas = paint_with_commands(&document, &commands, 1.0);
        assert_eq!(
            &canvas.rgba_pixels()[((5 * 20 + 5) * 4) as usize..][..4],
            [0, 0, 255, 255]
        );
    }
}
