use crate::border::BorderStyle;
use crate::css::{
    BackgroundLayer, BackgroundSize, Color, Gradient, GradientKind, GradientStop, LayerImage,
    ObjectFit, Overflow, Radial, RadialSize,
};
use crate::dom::NodeId;
use crate::image::Image;
use crate::layout::{
    BoxBorder, ControlPaint, CornerRadii, ImageLayer, LayoutBox, LayoutDocument, ProseSpan, Rect,
    ResolvedEdges, SQUARE_CORNERS, StackingContextEntry, TextRun, overflow_clip_rect, paint_order,
    scrollbar_metrics, stacking_context_entries, translate_rect, visual_offset_for,
    visual_rect_for,
};
use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CanvasCommand {
    FillRect {
        node: NodeId,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        color: Color,
    },
    ClearRect {
        node: NodeId,
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
    // rounded overflow clips and clip-paths in device space; every write is scaled by their
    // coverage
    masks: Vec<Mask>,
}

#[derive(Clone, Debug)]
enum Mask {
    Rounded(Shape),
    Polygon {
        even_odd: bool,
        points: Vec<(f32, f32)>,
    },
    // a mask-image's alpha over device rows from its corner; outside it nothing shows
    Alpha {
        left: u32,
        top: u32,
        width: u32,
        alpha: Vec<u8>,
    },
}

fn mask_coverage(masks: &[Mask], point: (f32, f32)) -> f32 {
    masks.iter().map(|mask| mask.coverage(point)).product()
}

impl Mask {
    fn coverage(&self, point: (f32, f32)) -> f32 {
        match self {
            Self::Rounded(shape) => shape.coverage(point),
            Self::Polygon { even_odd, points } => {
                crate::clip_path::polygon_coverage(points, *even_odd, point)
            }
            Self::Alpha {
                left,
                top,
                width,
                alpha,
            } => {
                let (x, y) = (point.0 - *left as f32, point.1 - *top as f32);
                if x < 0.0 || y < 0.0 || x >= *width as f32 {
                    return 0.0;
                }
                alpha
                    .get(y as usize * *width as usize + x as usize)
                    .map_or(0.0, |&alpha| f32::from(alpha) / 255.0)
            }
        }
    }
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
            masks: Vec::new(),
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
        // edges snap to the nearest device pixel, as browsers snap boxes, so a 1px border at a
        // fractional offset stays one pixel and neighbouring boxes neither overlap nor gap
        let (left, right) = snap_span(x, width, self.width);
        let (top, bottom) = snap_span(y, height, self.height);
        if right <= left || bottom <= top {
            return;
        }
        // an opaque span is a row copy, which is the hot path for backgrounds and borders
        if color.alpha == 255 && self.masks.is_empty() {
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
        let gamma = text_gamma_table(color);
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
                let coverage = gamma[usize::from(glyph.coverage[row * glyph.width + column])];
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

    // a colour glyph's own pixels faded by the text colour's opacity, or its shape in the text
    // colour for a silhouette
    fn blend_color_glyph(
        &mut self,
        x: i32,
        y: i32,
        glyph: &crate::color_glyph::ColorGlyph,
        text: Color,
        silhouette: bool,
        clip: Option<crate::layout::Rect>,
    ) {
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
                let color = glyph.pixels[row * glyph.width + column];
                if color.alpha == 0 {
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
                let alpha = u32::from(color.alpha) * u32::from(text.alpha) / 255;
                let base = if silhouette { text } else { color };
                self.blend_pixel(
                    pixel_x as u32,
                    pixel_y as u32,
                    Color {
                        alpha: alpha as u8,
                        ..base
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
        if shape.is_square()
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
        let clip = clip.map(|clip| self.device_shape(Shape::square(clip)));
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
                    && clip.coverage(center) <= 0.0
                {
                    continue;
                }
                let coverage = device.coverage(center);
                if coverage <= 0.0 {
                    continue;
                }
                let color = paint.color_at(center);
                if color.alpha == 0 {
                    continue;
                }
                self.blend_pixel(pixel_x, pixel_y, color.scale_alpha(coverage.min(1.0)));
            }
        }
    }

    // paints the ring between two shapes; each pixel takes the side it is nearest relative to
    // that side's width, which splits each corner along the line between its two sides
    fn fill_border(&mut self, outer: Shape, inner: Shape, sides: &BorderSides, clip: Option<Rect>) {
        let outer = self.device_shape(outer);
        let inner = self.device_shape(inner);
        let clip = clip.map(|clip| self.device_shape(Shape::square(clip)));
        let widths = sides.widths.map(|width| width * self.scale);
        let hollow = inner.width > 0.0 && inner.height > 0.0;
        let left = outer.x.floor().clamp(0.0, self.width as f32) as u32;
        let top = outer.y.floor().clamp(0.0, self.height as f32) as u32;
        let right = (outer.x + outer.width).ceil().clamp(0.0, self.width as f32) as u32;
        let bottom = (outer.y + outer.height)
            .ceil()
            .clamp(0.0, self.height as f32) as u32;
        for pixel_y in top..bottom {
            for pixel_x in left..right {
                let center = (pixel_x as f32 + 0.5, pixel_y as f32 + 0.5);
                if let Some(clip) = clip
                    && clip.coverage(center) <= 0.0
                {
                    continue;
                }
                let mut coverage = outer.coverage(center);
                if hollow {
                    coverage *= 1.0 - inner.coverage(center);
                }
                if coverage <= 0.0 {
                    continue;
                }
                let distances = [
                    center.1 - outer.y,
                    outer.x + outer.width - center.0,
                    outer.y + outer.height - center.1,
                    center.0 - outer.x,
                ];
                let Some(side) =
                    (0..4)
                        .filter(|side| widths[*side] > 0.0)
                        .min_by(|first, second| {
                            (distances[*first] / widths[*first])
                                .total_cmp(&(distances[*second] / widths[*second]))
                        })
                else {
                    continue;
                };
                let (along, length) = if side % 2 == 0 {
                    (center.0 - outer.x, outer.width)
                } else {
                    (center.1 - outer.y, outer.height)
                };
                let (color, pattern) = border_pixel(
                    sides.styles[side],
                    sides.colors[side],
                    side,
                    BorderSpot {
                        along,
                        across: distances[side],
                        width: widths[side],
                        length,
                    },
                );
                let coverage = coverage * pattern;
                if coverage <= 0.0 || color.alpha == 0 {
                    continue;
                }
                self.blend_pixel(pixel_x, pixel_y, color.scale_alpha(coverage.min(1.0)));
            }
        }
    }

    // a straight stroke with round ends, antialiased by its distance from the center line
    fn fill_segment(
        &mut self,
        (from, to): ((f32, f32), (f32, f32)),
        width: f32,
        color: Color,
        clip: Option<Rect>,
    ) {
        let scale = self.scale;
        let device = |(x, y): (f32, f32)| ((x - self.shift.0) * scale, (y - self.shift.1) * scale);
        let (start, end) = (device(from), device(to));
        let half = width * scale / 2.0;
        let clip = clip.map(|clip| self.device_shape(Shape::square(clip)));
        let left = (start.0.min(end.0) - half - 1.0)
            .floor()
            .clamp(0.0, self.width as f32) as u32;
        let right = (start.0.max(end.0) + half + 1.0)
            .ceil()
            .clamp(0.0, self.width as f32) as u32;
        let top = (start.1.min(end.1) - half - 1.0)
            .floor()
            .clamp(0.0, self.height as f32) as u32;
        let bottom = (start.1.max(end.1) + half + 1.0)
            .ceil()
            .clamp(0.0, self.height as f32) as u32;
        let along = (end.0 - start.0, end.1 - start.1);
        let length = along.0 * along.0 + along.1 * along.1;
        for pixel_y in top..bottom {
            for pixel_x in left..right {
                let point = (pixel_x as f32 + 0.5, pixel_y as f32 + 0.5);
                if let Some(clip) = clip
                    && clip.coverage(point) <= 0.0
                {
                    continue;
                }
                let t = if length > 0.0 {
                    (((point.0 - start.0) * along.0 + (point.1 - start.1) * along.1) / length)
                        .clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let nearest = (start.0 + along.0 * t, start.1 + along.1 * t);
                let distance =
                    ((point.0 - nearest.0).powi(2) + (point.1 - nearest.1).powi(2)).sqrt();
                let coverage = (half - distance + 0.5).clamp(0.0, 1.0);
                if coverage > 0.0 {
                    self.blend_pixel(pixel_x, pixel_y, color.scale_alpha(coverage));
                }
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
            masks: Vec::new(),
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
                let factor = if self.masks.is_empty() {
                    factor
                } else {
                    factor * self.mask_at(pixel_x, pixel_y)
                };
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

    // one pass over the shadow's reach; an outer shadow shows only outside `frame` and an
    // inset one only inside it
    fn fill_shadow(&mut self, shadow: ShadowFill, clip: Option<Rect>) {
        let ShadowFill {
            shape,
            blur,
            frame,
            inset,
            color,
        } = shadow;
        if color.alpha == 0 {
            return;
        }
        let shape = self.device_shape(shape);
        let frame = self.device_shape(frame);
        let clip = clip.map(|clip| self.device_shape(Shape::square(clip)));
        // the css blur radius is twice the gaussian's standard deviation
        let sigma = blur * self.scale / 2.0;
        let reach = if inset {
            frame
        } else {
            shape.outset(blur * self.scale)
        };
        let left = reach.x.floor().clamp(0.0, self.width as f32) as u32;
        let top = reach.y.floor().clamp(0.0, self.height as f32) as u32;
        let right = (reach.x + reach.width).ceil().clamp(0.0, self.width as f32) as u32;
        let bottom = (reach.y + reach.height)
            .ceil()
            .clamp(0.0, self.height as f32) as u32;
        for pixel_y in top..bottom {
            for pixel_x in left..right {
                let center = (pixel_x as f32 + 0.5, pixel_y as f32 + 0.5);
                if let Some(clip) = clip
                    && clip.coverage(center) <= 0.0
                {
                    continue;
                }
                let inside = shadow_coverage(frame, 0.0, center);
                let shade = if inset { inside } else { 1.0 - inside };
                if shade <= 0.0 {
                    continue;
                }
                let covered = shadow_coverage(shape, sigma, center);
                let alpha = shade * if inset { 1.0 - covered } else { covered };
                if alpha > 0.0 {
                    self.blend_pixel(pixel_x, pixel_y, color.scale_alpha(alpha.min(1.0)));
                }
            }
        }
    }

    fn push_mask(&mut self, shape: Shape) {
        let device = self.device_shape(shape);
        self.masks.push(Mask::Rounded(device));
    }

    // a clip-path region relative to the border box corner at `origin`
    fn push_clip_path(&mut self, region: &crate::clip_path::ClipRegion, origin: (f32, f32)) {
        use crate::clip_path::ClipRegion;
        match region {
            ClipRegion::Rounded(rect, radii) => {
                self.push_mask(Shape::from_rect(translate_rect(*rect, origin), *radii));
            }
            ClipRegion::Polygon { even_odd, points } => {
                let device = |(x, y): (f32, f32)| {
                    (
                        (x + origin.0 - self.shift.0) * self.scale,
                        (y + origin.1 - self.shift.1) * self.scale,
                    )
                };
                let points = points.iter().copied().map(device).collect();
                self.masks.push(Mask::Polygon {
                    even_odd: *even_odd,
                    points,
                });
            }
        }
    }

    // the mask layers paint like backgrounds placed from the border box, and their alpha
    // becomes coverage for the whole subtree
    fn push_image_mask(&mut self, layout_box: &LayoutBox, images: &[ImageLayer], rect: Rect) {
        let mask = match DeviceBounds::of(self, rect, None) {
            Some(bounds) => {
                let outer = Shape::from_rect(rect, layout_box.radii);
                let mut layer = self.layer(bounds);
                paint_layers(
                    &mut layer,
                    &layout_box.masks,
                    layout_box,
                    images,
                    outer,
                    outer,
                    1.0,
                    None,
                );
                let alpha = layer.pixels.chunks_exact(4).map(|pixel| pixel[3]).collect();
                recycle_layer(layer.pixels);
                Mask::Alpha {
                    left: bounds.left,
                    top: bounds.top,
                    width: bounds.right - bounds.left,
                    alpha,
                }
            }
            None => Mask::Alpha {
                left: 0,
                top: 0,
                width: 0,
                alpha: Vec::new(),
            },
        };
        self.masks.push(mask);
    }

    fn pop_mask(&mut self) {
        self.masks.pop();
    }

    fn mask_at(&self, x: u32, y: u32) -> f32 {
        mask_coverage(&self.masks, (x as f32 + 0.5, y as f32 + 0.5))
    }

    fn blend_pixel(&mut self, x: u32, y: u32, color: Color) {
        let color = if self.masks.is_empty() {
            color
        } else {
            let coverage = self.mask_at(x, y);
            if coverage <= 0.0 {
                return;
            }
            color.scale_alpha(coverage)
        };
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
            masks: Vec::new(),
        }
    }

    // draws a premultiplied layer over this surface at the given group opacity
    fn composite(&mut self, layer: Canvas, bounds: DeviceBounds, opacity: f32) {
        let factor = (opacity.clamp(0.0, 1.0) * 256.0).round() as u32;
        let stride = self.width as usize * 4;
        let layer_stride = layer.width as usize * 4;
        let masked = !self.masks.is_empty();
        for (row, source_row) in
            (bounds.top..bounds.bottom).zip(layer.pixels.chunks_exact(layer_stride))
        {
            let start = row as usize * stride + bounds.left as usize * 4;
            let end = row as usize * stride + bounds.right as usize * 4;
            for (column, (target, source)) in self.pixels[start..end]
                .chunks_exact_mut(4)
                .zip(source_row.chunks_exact(4))
                .enumerate()
            {
                if source[3] == 0 {
                    continue;
                }
                let factor = if masked {
                    let center = (
                        (bounds.left as usize + column) as f32 + 0.5,
                        row as f32 + 0.5,
                    );
                    (factor as f32 * mask_coverage(&self.masks, center)).round() as u32
                } else {
                    factor
                };
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
    for shadow in layout_box.shadows.iter().filter(|shadow| !shadow.inset) {
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
        && layout_box.shadows.is_empty()
        && usize::from(!layout_box.border.is_none())
            + usize::from(layout_box.background.alpha > 0)
            + layout_box.backgrounds.len()
            <= 1
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
    radii: CornerRadii,
}

impl Shape {
    fn from_rect(rect: Rect, radii: CornerRadii) -> Self {
        Self {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
            radii,
        }
    }

    fn square(rect: Rect) -> Self {
        Self::from_rect(rect, SQUARE_CORNERS)
    }

    fn is_square(&self) -> bool {
        self.radii
            .iter()
            .all(|(horizontal, vertical)| *horizontal <= 0.0 || *vertical <= 0.0)
    }

    fn to_device(self, scale: f32) -> Self {
        Self {
            x: self.x * scale,
            y: self.y * scale,
            width: self.width * scale,
            height: self.height * scale,
            radii: self
                .radii
                .map(|(horizontal, vertical)| (horizontal * scale, vertical * scale)),
        }
    }

    // each side moves in by its own width, and each corner curve shrinks with its two sides
    fn inset(self, edges: ResolvedEdges) -> Self {
        let [top_left, top_right, bottom_right, bottom_left] = self.radii;
        let shrink = |(horizontal, vertical): (f32, f32), across: f32, down: f32| {
            ((horizontal - across).max(0.0), (vertical - down).max(0.0))
        };
        Self {
            x: self.x + edges.left,
            y: self.y + edges.top,
            width: (self.width - edges.left - edges.right).max(0.0),
            height: (self.height - edges.top - edges.bottom).max(0.0),
            radii: [
                shrink(top_left, edges.left, edges.top),
                shrink(top_right, edges.right, edges.top),
                shrink(bottom_right, edges.right, edges.bottom),
                shrink(bottom_left, edges.left, edges.bottom),
            ],
        }
    }

    // grows every side, and every rounded corner with it, as spread shadows and outlines do
    fn outset(self, amount: f32) -> Self {
        Self {
            x: self.x - amount,
            y: self.y - amount,
            width: (self.width + amount * 2.0).max(0.0),
            height: (self.height + amount * 2.0).max(0.0),
            radii: self.radii.map(|(horizontal, vertical)| {
                if horizontal > 0.0 && vertical > 0.0 {
                    ((horizontal + amount).max(0.0), (vertical + amount).max(0.0))
                } else {
                    (0.0, 0.0)
                }
            }),
        }
    }

    // one pixel of feathering along the edge, which reads as antialiasing
    fn coverage(self, point: (f32, f32)) -> f32 {
        (0.5 - self.signed_distance(point)).clamp(0.0, 1.0)
    }

    fn signed_distance(self, point: (f32, f32)) -> f32 {
        let edge = (self.x - point.0)
            .max(point.0 - self.x - self.width)
            .max(self.y - point.1)
            .max(point.1 - self.y - self.height);
        let right = point.0 > self.x + self.width / 2.0;
        let bottom = point.1 > self.y + self.height / 2.0;
        let corner = match (right, bottom) {
            (false, false) => 0,
            (true, false) => 1,
            (true, true) => 2,
            (false, true) => 3,
        };
        let (radius_x, radius_y) = self.radii[corner];
        let (radius_x, radius_y) = (radius_x.min(self.width), radius_y.min(self.height));
        if radius_x <= 0.0 || radius_y <= 0.0 {
            return edge;
        }
        let along_x = if right {
            point.0 - (self.x + self.width - radius_x)
        } else {
            self.x + radius_x - point.0
        };
        let along_y = if bottom {
            point.1 - (self.y + self.height - radius_y)
        } else {
            self.y + radius_y - point.1
        };
        if along_x <= 0.0 || along_y <= 0.0 {
            return edge;
        }
        // the ellipse's implicit distance over its gradient approximates the true distance
        let (normal_x, normal_y) = (along_x / radius_x, along_y / radius_y);
        let length = (normal_x * normal_x + normal_y * normal_y).sqrt();
        let gradient =
            ((normal_x / radius_x).powi(2) + (normal_y / radius_y).powi(2)).sqrt() / length;
        (length - 1.0) / gradient
    }
}

enum Paint {
    Solid(Color),
    Ramp(Ramp),
}

impl Paint {
    fn color_at(&self, point: (f32, f32)) -> Color {
        match self {
            Self::Solid(color) => *color,
            Self::Ramp(ramp) => ramp.color_at(point),
        }
    }
}

// a gradient laid out over its tile, in layout pixels from the tile's corner; stops are
// offsets along the gradient line or ray with premultiplied colors, as gradients blend
struct Ramp {
    corner: (f32, f32),
    scale: f32,
    size: (f32, f32),
    repeat: (bool, bool),
    shape: RampShape,
    repeating: bool,
    stops: Vec<(f32, [f32; 4])>,
}

enum RampShape {
    // the start of the line, and its direction over its length
    Linear((f32, f32), (f32, f32)),
    // the center and the radii of the ending shape
    Radial((f32, f32), (f32, f32)),
}

impl Ramp {
    fn new(
        canvas: &Canvas,
        gradient: &Gradient,
        tile: Rect,
        repeat: (bool, bool),
        font_size: f32,
        opacity: f32,
    ) -> Option<Self> {
        let (width, height) = (tile.width, tile.height);
        if width <= 0.0 || height <= 0.0 {
            return None;
        }
        let (shape, length) = match gradient.kind {
            GradientKind::Linear(degrees) => linear_ramp(degrees.to_radians(), width, height),
            GradientKind::Corner(x, y) => {
                linear_ramp((x * height).atan2(-y * width), width, height)
            }
            GradientKind::Radial(ref radial) => {
                let Radial {
                    circle,
                    size,
                    center,
                } = **radial;
                let center = (
                    center.0.resolve(width, font_size).unwrap_or(width / 2.0),
                    center.1.resolve(height, font_size).unwrap_or(height / 2.0),
                );
                let radii = radial_radii(circle, size, center, (width, height), font_size);
                let radii = (radii.0.max(0.001), radii.1.max(0.001));
                (RampShape::Radial(center, radii), radii.0)
            }
        };
        let mut stops = ramp_stops(&gradient.stops, length, font_size, opacity);
        let (first, last) = (stops[0].0, stops[stops.len() - 1].0);
        let mut repeating = gradient.repeating;
        // a repeat too short to draw paints the average of its stops spread evenly
        if repeating && last - first < 1.0 / (length * canvas.scale).max(1.0) {
            let segments = (stops.len() - 1) as f32;
            let mut average = [0.0; 4];
            for pair in stops.windows(2) {
                for (channel, sum) in average.iter_mut().enumerate() {
                    *sum += (pair[0].1[channel] + pair[1].1[channel]) / 2.0 / segments;
                }
            }
            stops = vec![(0.0, average)];
            repeating = false;
        }
        let device = canvas.device_shape(Shape::square(tile));
        Some(Self {
            corner: (device.x, device.y),
            scale: canvas.scale,
            size: (width, height),
            repeat,
            shape,
            repeating,
            stops,
        })
    }

    fn color_at(&self, point: (f32, f32)) -> Color {
        let mut x = (point.0 - self.corner.0) / self.scale;
        let mut y = (point.1 - self.corner.1) / self.scale;
        if self.repeat.0 {
            x = x.rem_euclid(self.size.0);
        } else if !(0.0..self.size.0).contains(&x) {
            return Color::transparent();
        }
        if self.repeat.1 {
            y = y.rem_euclid(self.size.1);
        } else if !(0.0..self.size.1).contains(&y) {
            return Color::transparent();
        }
        let offset = match self.shape {
            RampShape::Linear(start, axis) => (x - start.0) * axis.0 + (y - start.1) * axis.1,
            RampShape::Radial(center, radii) => {
                ((x - center.0) / radii.0).hypot((y - center.1) / radii.1)
            }
        };
        self.sample(offset)
    }

    fn sample(&self, offset: f32) -> Color {
        let stops = &self.stops;
        let (first, last) = (stops[0].0, stops[stops.len() - 1].0);
        let offset = if self.repeating {
            first + (offset - first).rem_euclid(last - first)
        } else {
            offset
        };
        let next = stops.partition_point(|stop| stop.0 <= offset);
        let color = match (
            next.checked_sub(1).map(|index| stops[index]),
            stops.get(next).copied(),
        ) {
            (Some(before), Some(after)) => {
                let ratio = (offset - before.0) / (after.0 - before.0);
                std::array::from_fn(|channel| {
                    before.1[channel] + (after.1[channel] - before.1[channel]) * ratio
                })
            }
            (Some(stop), None) | (None, Some(stop)) => stop.1,
            (None, None) => return Color::transparent(),
        };
        let alpha = color[3];
        if alpha <= 0.0 {
            return Color::transparent();
        }
        let channel = |value: f32| (value / alpha).round().clamp(0.0, 255.0) as u8;
        Color {
            red: channel(color[0]),
            green: channel(color[1]),
            blue: channel(color[2]),
            alpha: (alpha * 255.0).round().clamp(0.0, 255.0) as u8,
        }
    }
}

// the line through the tile's center whose ends meet the corners, and its length
fn linear_ramp(angle: f32, width: f32, height: f32) -> (RampShape, f32) {
    let direction = (angle.sin(), -angle.cos());
    let length = (width * direction.0).abs() + (height * direction.1).abs();
    let start = (
        width / 2.0 - direction.0 * length / 2.0,
        height / 2.0 - direction.1 * length / 2.0,
    );
    let axis = if length > 0.0 {
        (direction.0 / length, direction.1 / length)
    } else {
        (0.0, 0.0)
    };
    (RampShape::Linear(start, axis), length)
}

// an ellipse sized to corners keeps the shape it has at the matching sides, scaled by root two
fn radial_radii(
    circle: bool,
    size: RadialSize,
    center: (f32, f32),
    (width, height): (f32, f32),
    font_size: f32,
) -> (f32, f32) {
    let horizontal = (center.0.abs(), (width - center.0).abs());
    let vertical = (center.1.abs(), (height - center.1).abs());
    let closest = (horizontal.0.min(horizontal.1), vertical.0.min(vertical.1));
    let farthest = (horizontal.0.max(horizontal.1), vertical.0.max(vertical.1));
    let both = |radius: f32| (radius, radius);
    match (size, circle) {
        (RadialSize::Explicit(horizontal, vertical), _) => (
            horizontal.resolve(width, font_size).unwrap_or(0.0),
            vertical.resolve(height, font_size).unwrap_or(0.0),
        ),
        (RadialSize::ClosestSide, true) => both(closest.0.min(closest.1)),
        (RadialSize::FarthestSide, true) => both(farthest.0.max(farthest.1)),
        (RadialSize::ClosestCorner, true) => both(closest.0.hypot(closest.1)),
        (RadialSize::FarthestCorner, true) => both(farthest.0.hypot(farthest.1)),
        (RadialSize::ClosestSide, false) => closest,
        (RadialSize::FarthestSide, false) => farthest,
        (RadialSize::ClosestCorner, false) => (
            closest.0 * std::f32::consts::SQRT_2,
            closest.1 * std::f32::consts::SQRT_2,
        ),
        (RadialSize::FarthestCorner, false) => (
            farthest.0 * std::f32::consts::SQRT_2,
            farthest.1 * std::f32::consts::SQRT_2,
        ),
    }
}

// stop offsets as fractions of the line: the ends default to its ends, a stop never sits
// before an earlier one, and stops without a position spread evenly between their neighbors
fn ramp_stops(
    stops: &[GradientStop],
    length: f32,
    font_size: f32,
    opacity: f32,
) -> Vec<(f32, [f32; 4])> {
    let mut offsets: Vec<Option<f32>> = stops
        .iter()
        .map(|stop| {
            let position = stop.position?.resolve(length, font_size)?;
            Some(if length > 0.0 { position / length } else { 0.0 })
        })
        .collect();
    let last = offsets.len() - 1;
    offsets[0].get_or_insert(0.0);
    offsets[last].get_or_insert(1.0);
    let mut floor = f32::MIN;
    for offset in offsets.iter_mut().flatten() {
        *offset = offset.max(floor);
        floor = *offset;
    }
    let mut index = 1;
    while index < last {
        if offsets[index].is_none() {
            let end = (index..=last)
                .find(|&next| offsets[next].is_some())
                .unwrap_or(last);
            let from = offsets[index - 1].unwrap_or(0.0);
            let to = offsets[end].unwrap_or(1.0);
            let gaps = (end - index + 1) as f32;
            for (step, offset) in offsets[index..end].iter_mut().enumerate() {
                *offset = Some(from + (to - from) * (step + 1) as f32 / gaps);
            }
            index = end;
        }
        index += 1;
    }
    stops
        .iter()
        .zip(offsets)
        .map(|(stop, offset)| {
            let color = stop.color;
            let alpha = f32::from(color.alpha) / 255.0 * opacity.clamp(0.0, 1.0);
            (
                offset.unwrap_or(0.0),
                [
                    f32::from(color.red) * alpha,
                    f32::from(color.green) * alpha,
                    f32::from(color.blue) * alpha,
                    alpha,
                ],
            )
        })
        .collect()
}

struct ShadowFill {
    shape: Shape,
    blur: f32,
    // the border box an outer shadow stays outside of, or the padding box an inset one fills
    frame: Shape,
    inset: bool,
    color: Color,
}

// how much of a pixel a shape covers once blurred by a gaussian of deviation `sigma`, in
// device pixels; a blurred edge is the error function of the distance to it, exact axis by
// axis for a square shape; unblurred square edges snap to whole pixels as filled boxes do
fn shadow_coverage(shape: Shape, sigma: f32, point: (f32, f32)) -> f32 {
    let sharp = sigma < 0.5;
    let scale = sigma * std::f32::consts::SQRT_2;
    if shape.is_square() {
        let axis = |start: f32, length: f32, at: f32| {
            if length <= 0.0 {
                0.0
            } else if sharp {
                let first = start.round();
                let last = (start + length).round().max(first + 1.0);
                if (first..last).contains(&at) {
                    1.0
                } else {
                    0.0
                }
            } else {
                0.5 * (erf((at - start) / scale) - erf((at - start - length) / scale))
            }
        };
        return axis(shape.x, shape.width, point.0) * axis(shape.y, shape.height, point.1);
    }
    if sharp {
        return (0.5 - shape.signed_distance(point)).clamp(0.0, 1.0);
    }
    // a rounded shape blurs row by row, as gpui draws shadows: each row is a span whose ends
    // follow the corner curve and blurs exactly across, and four rows under the gaussian sum
    // the blur down
    let half = (shape.width / 2.0, shape.height / 2.0);
    let local = (point.0 - shape.x - half.0, point.1 - shape.y - half.1);
    let corner = match (local.0 > 0.0, local.1 > 0.0) {
        (false, false) => 0,
        (true, false) => 1,
        (true, true) => 2,
        (false, true) => 3,
    };
    let (radius_x, radius_y) = shape.radii[corner];
    let radius = ((radius_x + radius_y) / 2.0).min(half.0).min(half.1);
    let start = (-3.0 * sigma).clamp(local.1 - half.1, local.1 + half.1);
    let end = (3.0 * sigma).clamp(local.1 - half.1, local.1 + half.1);
    let step = (end - start) / 4.0;
    (0..4)
        .map(|sample| {
            let offset = start + step * (sample as f32 + 0.5);
            let row = (half.1 - radius - (local.1 - offset).abs()).min(0.0);
            let reach = half.0 - radius + (radius * radius - row * row).max(0.0).sqrt();
            let span = 0.5 * (erf((local.0 + reach) / scale) - erf((local.0 - reach) / scale));
            let weight = (-offset * offset / (2.0 * sigma * sigma)).exp()
                / (sigma * (2.0 * std::f32::consts::PI).sqrt());
            span * weight * step
        })
        .sum()
}

// abramowitz and stegun 7.1.26, within 1.5e-7
fn erf(x: f32) -> f32 {
    let t = 1.0 / (1.0 + 0.327_591_1 * x.abs());
    let poly = t
        * (0.254_829_6
            + t * (-0.284_496_74 + t * (1.421_413_7 + t * (-1.453_152 + t * 1.061_405_4))));
    (1.0 - poly * (-x * x).exp()).copysign(x)
}

// an outer shadow grows from the border box and an inset one shrinks from the padding box
fn paint_shadow(
    canvas: &mut Canvas,
    shadow: &crate::layout::ResolvedShadow,
    border_box: Shape,
    padding_box: Shape,
    opacity: f32,
    clip: Option<Rect>,
) {
    let (frame, mut base) = if shadow.inset {
        (padding_box, padding_box.outset(-shadow.spread))
    } else {
        (border_box, border_box.outset(shadow.spread))
    };
    base.x += shadow.offset_x;
    base.y += shadow.offset_y;
    canvas.fill_shadow(
        ShadowFill {
            shape: base,
            blur: shadow.blur,
            frame,
            inset: shadow.inset,
            color: shadow.color.scale_alpha(opacity),
        },
        clip,
    );
}

struct BorderSides {
    // top, right, bottom and left, in layout pixels
    widths: [f32; 4],
    styles: [BorderStyle; 4],
    colors: [Color; 4],
}

// where a pixel sits on its border side, in device pixels
struct BorderSpot {
    along: f32,
    // from the outer edge inward
    across: f32,
    width: f32,
    length: f32,
}

// the color and pattern coverage a border style gives one pixel of a side
fn border_pixel(style: BorderStyle, color: Color, side: usize, spot: BorderSpot) -> (Color, f32) {
    let BorderSpot {
        along,
        across,
        width,
        length,
    } = spot;
    let shade = Color {
        red: (u16::from(color.red) * 2 / 3) as u8,
        green: (u16::from(color.green) * 2 / 3) as u8,
        blue: (u16::from(color.blue) * 2 / 3) as u8,
        ..color
    };
    // light falls on the bottom and right of an outset box
    let lit = side == 1 || side == 2;
    let inner_half = across > width / 2.0;
    match style {
        BorderStyle::Dashed => {
            let dash = (width * 3.0).max(1.0);
            // whole dashes fit the side, with one at each end
            let pairs = ((length - dash) / (dash * 2.0)).round().max(0.0);
            let period = if pairs > 0.0 {
                (length - dash) / pairs
            } else {
                length.max(dash) * 2.0
            };
            let phase = along.rem_euclid(period);
            let coverage = ((dash - phase + 0.5).clamp(0.0, 1.0) * (phase + 0.5).clamp(0.0, 1.0))
                .max((phase - period + 0.5).clamp(0.0, 1.0));
            (color, coverage)
        }
        BorderStyle::Dotted if width >= 2.0 => {
            let dots = (length / (width * 2.0)).round().max(1.0);
            let period = length / dots;
            let dot = (along / period).floor() * period + period / 2.0;
            let distance = ((along - dot).powi(2) + (across - width / 2.0).powi(2)).sqrt();
            (color, (width / 2.0 - distance + 0.5).clamp(0.0, 1.0))
        }
        BorderStyle::Dotted => {
            let phase = along.rem_euclid(width.max(1.0) * 2.0);
            (color, if phase < width.max(1.0) { 1.0 } else { 0.0 })
        }
        BorderStyle::Double if width >= 3.0 => {
            let third = width / 3.0;
            let coverage = (third - across + 0.5)
                .clamp(0.0, 1.0)
                .max((across - third * 2.0 + 0.5).clamp(0.0, 1.0));
            (color, coverage)
        }
        BorderStyle::Inset => (if lit { color } else { shade }, 1.0),
        BorderStyle::Outset => (if lit { shade } else { color }, 1.0),
        BorderStyle::Groove => (if lit == inner_half { shade } else { color }, 1.0),
        BorderStyle::Ridge => (if lit == inner_half { color } else { shade }, 1.0),
        _ => (color, 1.0),
    }
}

// the device pixels a span covers once its edges snap, never thinner than one pixel
fn snap_span(start: f32, length: f32, limit: u32) -> (u32, u32) {
    let first = start.round();
    let last = (start + length).round().max(first + 1.0);
    (
        (first.max(0.0) as u32).min(limit),
        (last.max(0.0) as u32).min(limit),
    )
}

fn paint_border(
    canvas: &mut Canvas,
    outer: Shape,
    inner: Shape,
    border: &BoxBorder,
    opacity: f32,
    clip: Option<Rect>,
) {
    if border.is_none() {
        return;
    }
    let widths = border.widths;
    let sides = BorderSides {
        widths: [widths.top, widths.right, widths.bottom, widths.left],
        styles: border.styles,
        colors: border.colors.map(|color| color.scale_alpha(opacity)),
    };
    let mut drawn = (0..4).filter(|side| sides.widths[*side] > 0.0);
    let first = drawn.clone().next().unwrap_or(0);
    let color = sides.colors[first];
    let plain =
        drawn.all(|side| sides.styles[side] == BorderStyle::Solid && sides.colors[side] == color);
    if plain && outer.is_square() {
        // square solid borders paint as four edges, so the interior is never touched
        let paint = Paint::Solid(color);
        for edge in [
            Rect {
                x: outer.x,
                y: outer.y,
                width: outer.width,
                height: widths.top,
            },
            Rect {
                x: outer.x,
                y: outer.y + outer.height - widths.bottom,
                width: outer.width,
                height: widths.bottom,
            },
            Rect {
                x: outer.x,
                y: outer.y + widths.top,
                width: widths.left,
                height: (outer.height - widths.top - widths.bottom).max(0.0),
            },
            Rect {
                x: outer.x + outer.width - widths.right,
                y: outer.y + widths.top,
                width: widths.right,
                height: (outer.height - widths.top - widths.bottom).max(0.0),
            },
        ] {
            canvas.fill_shape(Shape::square(edge), &paint, clip);
        }
        return;
    }
    canvas.fill_border(outer, inner, &sides, clip);
}

// the marks a form control draws over its box: a check, a dot, an arrow, a slider, a bar
fn paint_control(
    canvas: &mut Canvas,
    control: ControlPaint,
    rect: Rect,
    opacity: f32,
    clip: Option<Rect>,
) {
    let point = |x: f32, y: f32| (rect.x + rect.width * x, rect.y + rect.height * y);
    let round = |radius: f32| [(radius, radius); 4];
    match control {
        ControlPaint::Checkbox { checked: true, .. } => {
            let white = Color::WHITE.scale_alpha(opacity);
            let stroke = (rect.width * 0.14).max(1.5);
            canvas.fill_segment((point(0.22, 0.52), point(0.42, 0.72)), stroke, white, clip);
            canvas.fill_segment((point(0.42, 0.72), point(0.8, 0.3)), stroke, white, clip);
        }
        ControlPaint::Radio {
            checked: true,
            accent,
        } => {
            let size = rect.width.min(rect.height) * 0.5;
            let dot = Rect {
                x: rect.x + (rect.width - size) / 2.0,
                y: rect.y + (rect.height - size) / 2.0,
                width: size,
                height: size,
            };
            canvas.fill_shape(
                Shape::from_rect(dot, round(size / 2.0)),
                &Paint::Solid(accent.scale_alpha(opacity)),
                clip,
            );
        }
        ControlPaint::Select { color } => {
            let center = (rect.x + rect.width - 11.0, rect.y + rect.height / 2.0);
            let color = color.scale_alpha(opacity);
            canvas.fill_segment(
                ((center.0 - 4.0, center.1 - 2.0), (center.0, center.1 + 2.0)),
                1.5,
                color,
                clip,
            );
            canvas.fill_segment(
                ((center.0, center.1 + 2.0), (center.0 + 4.0, center.1 - 2.0)),
                1.5,
                color,
                clip,
            );
        }
        ControlPaint::Range { fraction, accent } => {
            let thumb = rect.height.min(16.0);
            let track_height = 4.0_f32.min(rect.height);
            let track = Rect {
                x: rect.x + thumb / 2.0 - 2.0,
                y: rect.y + (rect.height - track_height) / 2.0,
                width: (rect.width - thumb + 4.0).max(0.0),
                height: track_height,
            };
            let thumb_x = rect.x + (rect.width - thumb) * fraction;
            canvas.fill_shape(
                Shape::from_rect(track, round(track_height / 2.0)),
                &Paint::Solid(Color::opaque(239, 239, 239).scale_alpha(opacity)),
                clip,
            );
            canvas.fill_shape(
                Shape::from_rect(
                    Rect {
                        width: (thumb_x + thumb / 2.0 - track.x).max(0.0),
                        ..track
                    },
                    round(track_height / 2.0),
                ),
                &Paint::Solid(accent.scale_alpha(opacity)),
                clip,
            );
            canvas.fill_shape(
                Shape::from_rect(
                    Rect {
                        x: thumb_x,
                        y: rect.y + (rect.height - thumb) / 2.0,
                        width: thumb,
                        height: thumb,
                    },
                    round(thumb / 2.0),
                ),
                &Paint::Solid(accent.scale_alpha(opacity)),
                clip,
            );
        }
        ControlPaint::Progress {
            fraction: Some(fraction),
            accent,
        } => fill_bar(canvas, rect, fraction, accent.scale_alpha(opacity), clip),
        ControlPaint::Meter { fraction, color } => {
            fill_bar(canvas, rect, fraction, color.scale_alpha(opacity), clip);
        }
        ControlPaint::FileButton { width } => {
            let button = Rect {
                width: width.min(rect.width),
                ..rect
            };
            let outer = Shape::from_rect(button, round(3.0));
            canvas.fill_shape(
                outer,
                &Paint::Solid(Color::opaque(239, 239, 239).scale_alpha(opacity)),
                clip,
            );
            let edge = Color::opaque(118, 118, 118).scale_alpha(opacity);
            let border = BoxBorder {
                widths: ResolvedEdges {
                    top: 1.0,
                    right: 1.0,
                    bottom: 1.0,
                    left: 1.0,
                },
                styles: [BorderStyle::Solid; 4],
                colors: [edge; 4],
            };
            paint_border(
                canvas,
                outer,
                outer.inset(border.widths),
                &border,
                1.0,
                clip,
            );
        }
        _ => {}
    }
}

// the filled part of a progress or meter bar, inside its rounded track
fn fill_bar(canvas: &mut Canvas, rect: Rect, fraction: f32, color: Color, clip: Option<Rect>) {
    let radius = rect.height / 2.0;
    let bar = Rect {
        width: rect.width * fraction.clamp(0.0, 1.0),
        ..rect
    };
    canvas.fill_shape(
        Shape::from_rect(bar, [(radius, radius); 4]),
        &Paint::Solid(color),
        clip,
    );
}

// an outline traces the border box, pushed out by its offset, and takes no space
fn paint_outline(
    canvas: &mut Canvas,
    layout_box: &LayoutBox,
    rect: Rect,
    opacity: f32,
    clip: Option<Rect>,
) {
    let Some(outline) = layout_box.outline.filter(|_| layout_box.visible) else {
        return;
    };
    let inner = Shape::from_rect(rect, layout_box.radii).outset(outline.offset);
    let outer = inner.outset(outline.width);
    let width = outline.width;
    let border = BoxBorder {
        widths: ResolvedEdges {
            top: width,
            right: width,
            bottom: width,
            left: width,
        },
        styles: [outline.style; 4],
        colors: [outline.color; 4],
    };
    paint_border(canvas, outer, inner, &border, opacity, clip);
}

// an overflow clip with rounded corners clips to the padding box's curves as well
fn overflow_mask(layout_box: &LayoutBox, rect: Rect) -> Option<Shape> {
    let clips = layout_box.overflow.clips();
    let padding_box = Shape::from_rect(rect, layout_box.radii).inset(layout_box.border.widths);
    (clips && !padding_box.is_square()).then_some(padding_box)
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
    // the root background paints the whole canvas, as CSS backgrounds require; its images
    // stay with the root box
    if let Some(root) = canvas_background(&document.root) {
        let viewport = Shape::square(Rect {
            x: 0.0,
            y: 0.0,
            width: document.viewport.width,
            height: document.viewport.height,
        });
        paint_backgrounds(
            &mut canvas,
            root,
            &[],
            viewport,
            viewport,
            root.opacity,
            None,
        );
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

fn canvas_background(root: &LayoutBox) -> Option<&LayoutBox> {
    let mut candidates = Vec::new();
    collect_root_boxes(root, 0, &mut candidates);
    candidates
        .into_iter()
        .find(|layout_box| layout_box.background.alpha > 0 || !layout_box.backgrounds.is_empty())
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
    let (node, x, y, width, height) = match command {
        CanvasCommand::FillRect {
            node,
            x,
            y,
            width,
            height,
            ..
        }
        | CanvasCommand::ClearRect {
            node,
            x,
            y,
            width,
            height,
        } => (node, *x, *y, *width, *height),
    };
    if *node != canvas_box.node {
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
    // a clip-path cuts everything the box paints, its shadow and outline included
    if let Some(region) = &layout_box.clip_path {
        if region.is_empty() {
            return;
        }
        canvas.push_clip_path(region, (rect.x, rect.y));
    }
    if !layout_box.masks.is_empty() {
        canvas.push_image_mask(layout_box, images, rect);
    }
    let content_clip = paint_box_surface(canvas, layout_box, images, clip, offset, rect, opacity);
    let mask = overflow_mask(layout_box, rect);
    if let Some(mask) = mask {
        canvas.push_mask(mask);
    }
    let mut contexts = stacking_context_entries(layout_box, clip, ancestor_offset);
    contexts.sort_by_key(|entry| (entry.z_index, entry.order));
    for entry in contexts.iter().filter(|entry| entry.z_index < 0) {
        paint_context_entry(canvas, entry, commands, images, prose_selections);
    }
    paint_inline_fragments(canvas, layout_box, images, content_clip, offset);
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
        paint_context_entry(canvas, entry, commands, images, prose_selections);
    }
    if mask.is_some() {
        canvas.pop_mask();
    }
    paint_scrollbars(canvas, layout_box, offset, clip);
    paint_outline(canvas, layout_box, rect, opacity, clip);
    if !layout_box.masks.is_empty() {
        canvas.pop_mask();
    }
    if layout_box.clip_path.is_some() {
        canvas.pop_mask();
    }
}

fn paint_context_entry(
    canvas: &mut Canvas,
    entry: &StackingContextEntry<'_>,
    commands: &[CanvasCommand],
    images: &[ImageLayer],
    prose_selections: &[ProseSpan],
) {
    let masks = entry
        .clippers
        .iter()
        .filter_map(|&(clipper, offset)| overflow_mask(clipper, visual_rect_for(clipper, offset)))
        .collect::<Vec<_>>();
    for &mask in &masks {
        canvas.push_mask(mask);
    }
    paint_context(
        canvas,
        entry.layout,
        commands,
        images,
        prose_selections,
        entry.clip,
        entry.ancestor_offset,
    );
    for _ in &masks {
        canvas.pop_mask();
    }
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
        if child.creates_stacking_context || child.inline {
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
    let mask = overflow_mask(layout_box, rect);
    if let Some(mask) = mask {
        canvas.push_mask(mask);
    }
    paint_inline_fragments(canvas, layout_box, images, content_clip, offset);
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
    if mask.is_some() {
        canvas.pop_mask();
    }
    paint_scrollbars(canvas, layout_box, offset, clip);
    paint_outline(canvas, layout_box, rect, layout_box.opacity, clip);
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
    let content_clip = if layout_box.overflow.clips() || layout_box.is_document() {
        intersect_clip(clip, translate_rect(overflow_clip_rect(layout_box), offset))
    } else {
        clip
    };
    // a hidden box paints nothing of its own but still clips what its children paint
    if !layout_box.visible {
        return content_clip;
    }
    let outer = Shape::from_rect(rect, layout_box.radii);
    let surface = outer.inset(layout_box.border.widths);
    for shadow in layout_box
        .shadows
        .iter()
        .rev()
        .filter(|shadow| !shadow.inset)
    {
        paint_shadow(canvas, shadow, outer, surface, opacity, clip);
    }
    paint_backgrounds(canvas, layout_box, images, outer, surface, opacity, clip);
    paint_border(canvas, outer, surface, &layout_box.border, opacity, clip);
    if let Some(control) = layout_box.control {
        let inner = Rect {
            x: surface.x,
            y: surface.y,
            width: surface.width,
            height: surface.height,
        };
        paint_control(canvas, control, inner, opacity, clip);
    }
    for shadow in layout_box
        .shadows
        .iter()
        .rev()
        .filter(|shadow| shadow.inset)
    {
        paint_shadow(canvas, shadow, outer, surface, opacity, clip);
    }
    if layout_box.tag == "img"
        && let Some(layer) = images
            .iter()
            .find(|layer| layer.background.is_none() && layer.node == layout_box.node)
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

// the color lies under every image layer, the last layer lowest and the first on top; images
// take their place from the padding box and paint across the border box
fn paint_backgrounds(
    canvas: &mut Canvas,
    layout_box: &LayoutBox,
    images: &[ImageLayer],
    outer: Shape,
    surface: Shape,
    opacity: f32,
    clip: Option<Rect>,
) {
    if layout_box.background.alpha > 0 {
        let color = layout_box.background.scale_alpha(opacity);
        canvas.fill_shape(outer, &Paint::Solid(color), clip);
    }
    paint_layers(
        canvas,
        &layout_box.backgrounds,
        layout_box,
        images,
        surface,
        outer,
        opacity,
        clip,
    );
}

// layers take their place from `origin` and paint across `painted`, the last lowest
#[allow(clippy::too_many_arguments)]
fn paint_layers(
    canvas: &mut Canvas,
    layers: &[BackgroundLayer],
    layout_box: &LayoutBox,
    images: &[ImageLayer],
    origin: Shape,
    painted: Shape,
    opacity: f32,
    clip: Option<Rect>,
) {
    let area = Rect {
        x: origin.x,
        y: origin.y,
        width: origin.width,
        height: origin.height,
    };
    let font_size = layout_box.font_size;
    for layer in layers.iter().rev() {
        let repeat = (layer.repeat.x, layer.repeat.y);
        match &layer.image {
            LayerImage::Gradient(gradient) => {
                let tile = background_placement(area, None, layer, font_size);
                if let Some(ramp) = Ramp::new(canvas, gradient, tile, repeat, font_size, opacity) {
                    canvas.fill_shape(painted, &Paint::Ramp(ramp), clip);
                }
            }
            LayerImage::Url(source) => {
                let Some(found) = images.iter().find(|image| {
                    image.background == Some(*source) && image.node == layout_box.node
                }) else {
                    continue;
                };
                let natural = (found.image.width as f32, found.image.height as f32);
                paint_image(
                    canvas,
                    ImageDraw {
                        image: &found.image,
                        placement: background_placement(area, Some(natural), layer, font_size),
                        area: origin,
                        clip,
                        opacity,
                        repeat,
                    },
                );
            }
        }
    }
}

// inline element backgrounds and borders sit under the text of their block
fn paint_inline_fragments(
    canvas: &mut Canvas,
    layout_box: &LayoutBox,
    images: &[ImageLayer],
    clip: Option<Rect>,
    offset: (f32, f32),
) {
    for fragment in layout_box.children.iter().filter(|child| child.inline) {
        let rect = visual_rect_for(fragment, offset);
        paint_box_surface(
            canvas,
            fragment,
            images,
            clip,
            offset,
            rect,
            fragment.opacity,
        );
        paint_outline(canvas, fragment, rect, fragment.opacity, clip);
    }
}

fn paint_box_text(
    canvas: &mut Canvas,
    layout_box: &LayoutBox,
    clip: Option<Rect>,
    offset: (f32, f32),
    prose_selections: &[ProseSpan],
) {
    if !layout_box.visible {
        return;
    }
    paint_control_selection(canvas, layout_box, clip, offset);
    let selected = paint_prose_selection(canvas, layout_box, prose_selections, clip, offset);
    for run in &layout_box.text {
        paint_text(canvas, run, clip, offset);
    }
    // selected text takes the ::selection color, drawn again inside the highlight
    if let Some(color) = layout_box.selection_color {
        for (index, area) in selected {
            let run = TextRun {
                color,
                ..layout_box.text[index].clone()
            };
            if let Some(area) = intersect_clip(clip, area) {
                paint_text(canvas, &run, Some(area), offset);
            }
        }
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
) -> Vec<(usize, Rect)> {
    let mut selected = Vec::new();
    for span in prose_selections {
        for (index, run) in layout_box.text.iter().enumerate() {
            if run.node != Some(span.node) || run.text.trim().is_empty() {
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
            let area = Rect {
                x: run.x + offset.0 + prefix_width,
                y: run.y + offset.1,
                width: selected_width,
                height,
            };
            fill_rect_optional(
                canvas,
                area.x,
                area.y,
                area.width,
                area.height,
                layout_box.selection.unwrap_or(SELECTION_BACKGROUND),
                clip,
            );
            selected.push((index, area));
        }
    }
    selected
}

const SELECTION_BACKGROUND: Color = Color::opaque(170, 205, 238);

fn width_within_run(run: &TextRun, units: usize, fonts: &crate::font::FontSet) -> f32 {
    let mut consumed = 0;
    let mut prefix = String::new();
    for character in run.text.chars() {
        if consumed >= units {
            break;
        }
        prefix.push(character);
        consumed += character.len_utf16();
    }
    // the prefix measured as the run draws it, shaped and spaced
    if !fonts.is_builtin() {
        let shaped = fonts.shape(
            &prefix,
            run.font_stack,
            run.font_size,
            run.font,
            run.letter_spacing == 0.0,
        );
        return shaped.width + run.letter_spacing * shaped.glyphs.len() as f32;
    }
    let mut width = 0.0;
    let mut consumed = 0;
    for character in run.text.chars() {
        if consumed >= units {
            break;
        }
        width +=
            fonts.advance(character, run.font_stack, run.font_size, run.font) + run.letter_spacing;
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
                .any(|run| run.node == Some(span.node))
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
        let from = width_of_characters(run, selected_start_char);
        let to = width_of_characters(run, selected_end_char);
        fill_rect_optional(
            canvas,
            run.x + offset.0 + from,
            run.y + offset.1,
            (to - from).max(0.0),
            run.font_size.max(1.0),
            layout_box.selection.unwrap_or(SELECTION_BACKGROUND),
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
    Some((run.x + width_of_characters(run, character_offset), run.y))
}

// the width of a run's first characters as the font draws them
fn width_of_characters(run: &TextRun, characters: usize) -> f32 {
    let fonts = crate::font::active_fonts();
    if fonts.is_builtin() {
        return characters as f32 * text_advance(run.font_size);
    }
    let units = run.text.chars().take(characters).map(char::len_utf16).sum();
    width_within_run(run, units, &fonts)
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
    if layout_box.tag != "canvas" || !layout_box.visible {
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
    if !layout_box.visible {
        return;
    }
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
    let rounded = !area_device.is_square();
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
            let coverage = if rounded {
                area_device.coverage(center)
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
// an image without a natural size, as a gradient, fills the area along any auto side
fn background_placement(
    area: Rect,
    natural: Option<(f32, f32)>,
    background: &BackgroundLayer,
    font_size: f32,
) -> Rect {
    let (natural_width, natural_height) = natural.unwrap_or((area.width, area.height));
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
            match (width, height, natural) {
                (Some(width), Some(height), _) => (width, height),
                (Some(width), None, Some(_)) => (width, width * natural_height / natural_width),
                (None, Some(height), Some(_)) => (height * natural_width / natural_height, height),
                (Some(width), None, None) => (width, area.height),
                (None, Some(height), None) => (area.width, height),
                (None, None, _) => (natural_width, natural_height),
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
    if let Some(shadow) = run.decoration.shadow
        && shadow.color.alpha > 0
    {
        paint_text_shadow(canvas, run, shadow, clip, offset);
    }
    paint_glyphs(canvas, run, clip, offset, false);
    let stroke = run.decoration.stroke;
    let fonts = crate::font::active_fonts();
    if stroke.width > 0.0 && !fonts.is_builtin() {
        let stroked = TextRun {
            color: stroke.color.unwrap_or(run.color),
            ..run.clone()
        };
        paint_outline_text(canvas, &stroked, clip, offset, &fonts, false, stroke.width);
    }
    if run.decoration.has_lines() {
        paint_decoration_lines(canvas, run, clip, offset);
    }
}

// a shadow is the run drawn again in the shadow's color, offset, and blurred in a layer of
// its own when it has a blur radius
fn paint_text_shadow(
    canvas: &mut Canvas,
    run: &TextRun,
    shadow: crate::css::TextShadow,
    clip: Option<Rect>,
    offset: (f32, f32),
) {
    let shadowed = TextRun {
        color: shadow.color,
        decoration: crate::css::TextDecoration {
            shadow: None,
            ..run.decoration
        },
        ..run.clone()
    };
    let at = (offset.0 + shadow.x, offset.1 + shadow.y);
    if shadow.blur <= 0.0 {
        paint_glyphs(canvas, &shadowed, clip, at, true);
        if shadowed.decoration.has_lines() {
            paint_decoration_lines(canvas, &shadowed, clip, at);
        }
        return;
    }
    let reach = shadow.blur * 2.0;
    let area = Rect {
        x: run.x + at.0 - reach,
        y: run.y + at.1 - reach,
        width: run.width + reach * 2.0,
        height: run.font_size * 1.5 + reach * 2.0,
    };
    let Some(bounds) = DeviceBounds::of(canvas, area, clip) else {
        return;
    };
    let mut layer = canvas.layer(bounds);
    paint_glyphs(&mut layer, &shadowed, None, at, true);
    if shadowed.decoration.has_lines() {
        paint_decoration_lines(&mut layer, &shadowed, None, at);
    }
    // three box blurs approach a gaussian with the blur radius as twice its deviation
    let radius = ((shadow.blur / 2.0) * canvas.scale).round().max(1.0) as usize;
    for _ in 0..3 {
        box_blur(&mut layer, radius);
    }
    canvas.composite(layer, bounds, 1.0);
}

// a box blur of a layer's premultiplied pixels, horizontally then vertically
fn box_blur(layer: &mut Canvas, radius: usize) {
    let (width, height) = (layer.width as usize, layer.height as usize);
    let mut buffer = layer.pixels.clone();
    let window = (radius * 2 + 1) as u32;
    for (pass, (outer, inner, stride_outer, stride_inner)) in
        [(height, width, width * 4, 4), (width, height, 4, width * 4)]
            .into_iter()
            .enumerate()
    {
        let (source, target) = if pass == 0 {
            (&layer.pixels, &mut buffer)
        } else {
            (&buffer.clone(), &mut layer.pixels)
        };
        for line in 0..outer {
            let base = line * stride_outer;
            for channel in 0..4 {
                let at =
                    |position: usize| u32::from(source[base + position * stride_inner + channel]);
                let mut sum = 0_u32;
                for position in 0..=radius.min(inner.saturating_sub(1)) {
                    sum += at(position);
                }
                for position in 0..inner {
                    target[base + position * stride_inner + channel] = (sum / window) as u8;
                    if position + radius + 1 < inner {
                        sum += at(position + radius + 1);
                    }
                    if position >= radius {
                        sum -= at(position - radius);
                    }
                }
            }
        }
    }
}

// underline, overline and line-through, placed from the font's ascent, in the decoration's
// style and color
fn paint_decoration_lines(
    canvas: &mut Canvas,
    run: &TextRun,
    clip: Option<Rect>,
    offset: (f32, f32),
) {
    let fonts = crate::font::active_fonts();
    let decoration = run.decoration;
    let size = run.font_size;
    let ascent = fonts.ascent(run.font_stack, size);
    let baseline = run.y + offset.1 + ascent;
    let thickness = decoration.thickness.unwrap_or((size / 14.0).max(1.0));
    let color = decoration.color.unwrap_or(run.color);
    // a space a run picked up after its last word belongs to the text that follows, so the
    // lines stop at the word
    let trailing = run.text.len() - run.text.trim_end_matches(' ').len();
    let tail = if trailing > 0 {
        fonts.measure(&" ".repeat(trailing), run.font_stack, size, run.font)
            + run.letter_spacing * trailing as f32
    } else {
        0.0
    };
    let left = run.x + offset.0 + if run.rtl { tail } else { 0.0 };
    let width = (run.width - tail).max(0.0);
    let mut lines = Vec::new();
    if decoration.underline {
        lines.push(baseline + (size * 0.1).max(1.0));
    }
    if decoration.overline {
        lines.push(baseline - ascent);
    }
    if decoration.line_through {
        lines.push(baseline - size * 0.3);
    }
    for top in lines {
        match decoration.style {
            crate::css::DecorationStyle::Solid => {
                fill_rect_optional(canvas, left, top, width, thickness, color, clip);
            }
            crate::css::DecorationStyle::Double => {
                fill_rect_optional(canvas, left, top, width, thickness, color, clip);
                fill_rect_optional(
                    canvas,
                    left,
                    top + thickness * 2.0,
                    width,
                    thickness,
                    color,
                    clip,
                );
            }
            crate::css::DecorationStyle::Dotted | crate::css::DecorationStyle::Dashed => {
                let dash = if decoration.style == crate::css::DecorationStyle::Dotted {
                    thickness
                } else {
                    thickness * 3.0
                };
                let mut x = left;
                while x < left + width {
                    fill_rect_optional(
                        canvas,
                        x,
                        top,
                        dash.min(left + width - x),
                        thickness,
                        color,
                        clip,
                    );
                    x += dash * 2.0;
                }
            }
            crate::css::DecorationStyle::Wavy => {
                let period = thickness * 4.0;
                let mut x = left;
                let mut up = true;
                while x < left + width {
                    let next = (x + period / 2.0).min(left + width);
                    let (from, to) = if up {
                        (top + thickness, top - thickness)
                    } else {
                        (top - thickness, top + thickness)
                    };
                    canvas.fill_segment(((x, from), (next, to)), thickness, color, clip);
                    x = next;
                    up = !up;
                }
            }
        }
    }
}

// a silhouette draws colour glyphs in the run's colour, as their shadows are
fn paint_glyphs(
    canvas: &mut Canvas,
    run: &TextRun,
    clip: Option<Rect>,
    offset: (f32, f32),
    silhouette: bool,
) {
    let fonts = crate::font::active_fonts();
    if !fonts.is_builtin() {
        paint_outline_text(canvas, run, clip, offset, &fonts, silhouette, 0.0);
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
                    let width = if run.font.is_bold() {
                        pixel + 1.0
                    } else {
                        pixel
                    };
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
    silhouette: bool,
    stroke: f32,
) {
    let scale = canvas.scale();
    let device_size = run.font_size * scale;
    let shift = canvas.shift;
    let baseline =
        (run.y + offset.1 - shift.1 + fonts.ascent(run.font_stack, run.font_size)) * scale;
    let pen = (run.x + offset.0 - shift.0) * scale;
    let device_clip = clip.map(|clip| Rect {
        x: (clip.x - shift.0) * scale,
        y: (clip.y - shift.1) * scale,
        width: clip.width * scale,
        height: clip.height * scale,
    });
    // css turns ligatures off when letters are spaced apart
    let text = if run.rtl {
        run.text
            .chars()
            .map(crate::bidi::mirrored)
            .collect::<String>()
    } else {
        run.text.clone()
    };
    let shaped = fonts.shape(
        &text,
        run.font_stack,
        device_size,
        run.font,
        run.letter_spacing == 0.0,
    );
    let spacing = run.letter_spacing * scale;
    let total = shaped.width + spacing * shaped.glyphs.len() as f32;
    for (index, glyph) in shaped.glyphs.iter().enumerate() {
        let Some(face) = &glyph.face else {
            continue;
        };
        let offset = glyph.x + spacing * index as f32;
        // right-to-left glyphs are laid from the run's right edge leftward
        let x = if run.rtl {
            pen + total - offset - glyph.advance - spacing
        } else {
            pen + offset
        };
        // glyphs rasterize at quarter pixels, so the whole pixel comes from the same rounding
        let x = (x * 4.0).round() / 4.0;
        let colored = if stroke > 0.0 {
            None
        } else {
            fonts.color_glyph(face, glyph.glyph, device_size, x, run.color)
        };
        if let Some(colored) = colored {
            canvas.blend_color_glyph(
                x.floor() as i32 + colored.left,
                baseline.round() as i32 + colored.top,
                &colored,
                run.color,
                silhouette,
                device_clip,
            );
        } else if let Some(bitmap) =
            fonts.glyph_bitmap(face, glyph.glyph, device_size, x, stroke * scale)
        {
            canvas.blend_coverage(
                x.floor() as i32 + bitmap.left,
                baseline.round() as i32 + bitmap.top,
                &bitmap,
                run.color,
                device_clip,
            );
        }
    }
}

// the coverage correction chromium's skia applies to windows text (srgb gamma, contrast 1),
// one table per eighth of the text colour's luminance, so light text on dark keeps its weight
fn text_gamma_table(color: Color) -> &'static [u8; 256] {
    static TABLES: std::sync::OnceLock<[[u8; 256]; 8]> = std::sync::OnceLock::new();
    let tables = TABLES.get_or_init(|| {
        std::array::from_fn(|level| {
            let level = level as u32;
            correcting_table(
                ((level << 5) | (level << 2) | (level >> 1)) as f32 / 255.0,
                1.0,
            )
        })
    });
    let channel = |value: u8| srgb_to_linear(f32::from(value) / 255.0);
    let luma =
        0.2126 * channel(color.red) + 0.7152 * channel(color.green) + 0.0722 * channel(color.blue);
    let luminance = (linear_to_srgb(luma) * 255.0).round() as usize;
    &tables[luminance.min(255) >> 5]
}

fn correcting_table(src: f32, contrast: f32) -> [u8; 256] {
    let dst = 1.0 - src;
    let (linear_src, linear_dst) = (srgb_to_linear(src), srgb_to_linear(dst));
    // contrast fades out as the text approaches white
    let contrast = contrast * linear_dst;
    std::array::from_fn(|index| {
        let raw = index as f32 / 255.0;
        let alpha = raw + (1.0 - raw) * contrast * raw;
        let result = if (src - dst).abs() < 1.0 / 256.0 {
            alpha
        } else {
            let out = linear_to_srgb(linear_src * alpha + (1.0 - alpha) * linear_dst);
            // undo the linear blend the blit does next
            (out - dst) / (src - dst)
        };
        (result * 255.0).round().clamp(0.0, 255.0) as u8
    })
}

fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(value: f32) -> f32 {
    if value <= 0.0031308 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
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

    #[test]
    fn text_coverage_thickens_light_text_more_than_dark_text() {
        let white = text_gamma_table(Color::WHITE);
        let black = text_gamma_table(Color::BLACK);
        for table in [white, black] {
            assert_eq!((table[0], table[255]), (0, 255));
            assert!(table.windows(2).all(|pair| pair[0] <= pair[1]));
        }
        // white on black follows the srgb curve, half coverage reads as about 74 percent,
        // while contrast keeps black on white near linear
        assert_eq!((white[128], black[128]), (188, 119));
    }

    #[test]
    fn rect_edges_snap_to_whole_device_pixels() {
        assert_eq!(snap_span(538.7, 1.0, 1000), (539, 540));
        assert_eq!(snap_span(10.2, 0.3, 1000), (10, 11));
        assert_eq!(snap_span(0.5, 10.0, 1000).1, snap_span(10.5, 5.0, 1000).0);
        assert_eq!(snap_span(-4.0, 10.0, 3), (0, 3));
    }

    // node ids for hand-built layout trees, taken from one fixed document shape
    fn node(path: &[usize]) -> NodeId {
        crate::dom::Document::parse_markup("<i></i><i><i></i></i>")
            .expect("fixture parses")
            .node_at(path)
    }
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
                node: node(&[]),
                tag: "#document".to_string(),
                rect: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 64.0,
                    height: 24.0,
                },
                background: Color::opaque(245, 248, 250),
                backgrounds: Vec::new(),
                masks: Vec::new(),
                object_fit: crate::css::ObjectFit::Fill,
                transform: None,
                backface_hidden: false,
                clip_path: None,
                radii: SQUARE_CORNERS,
                opacity: 1.0,
                shadows: Vec::new(),
                border: BoxBorder::NONE,
                outline: None,
                text: vec![TextRun {
                    text: "ABCDEF".to_string(),
                    x: 4.0,
                    y: 2.0,
                    width: 36.0,
                    font_size: 12.0,
                    letter_spacing: 0.0,
                    font_stack: crate::font::FontStack::default(),
                    color: Color::BLACK,
                    font: crate::font::FontStyle::NORMAL,
                    node: Some(node(&[1, 0])),
                    node_offset: 0,
                    source_len: 6,
                    rtl: false,
                    decoration: Default::default(),
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
                overflow_axes: [Overflow::Visible; 2],
                visible: true,
                pointer_events: true,
                selectable: true,
                cursor: crate::css::Cursor::Auto,
                floated: false,
                inline: false,
                control: None,
                selection: None,
                selection_color: None,
                scroll_content: Size {
                    width: 64.0,
                    height: 24.0,
                },
                scroll_offset: (0.0, 0.0),
                margin: Default::default(),
            },
            images: Vec::new(),
            prose_selections: vec![crate::layout::ProseSpan {
                node: node(&[1, 0]),
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
    fn gradient_layers_tile_repeat_and_blend_premultiplied() {
        let document = Document::parse_markup(
            "<html><body><div id='a'></div><div id='b'></div><div id='c'></div></body></html>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(concat!(
            "html, body { margin: 0; background: #ffffff } div { width: 40px; height: 40px }",
            "#a { background: linear-gradient(#ff0000, #ff0000) 10px 0 / 10px 100% no-repeat, linear-gradient(to right, #0000ff, #0000ff) }",
            "#b { background: repeating-linear-gradient(to right, #000000 0 2px, transparent 2px 4px) }",
            "#c { background: radial-gradient(circle closest-side, #ff0000, transparent) }",
        ))
        .expect("stylesheet should parse");
        let canvas = paint(&crate::layout::layout(
            &document,
            &stylesheet,
            Size {
                width: 40.0,
                height: 120.0,
            },
        ));
        let pixel = |x: usize, y: usize| {
            let index = (y * canvas.width() as usize + x) * 4;
            <[u8; 3]>::try_from(&canvas.rgba_pixels()[index..index + 3]).expect("a pixel")
        };
        assert_eq!(pixel(15, 20), [255, 0, 0], "the first layer lies on top");
        assert_eq!(
            pixel(5, 20),
            [0, 0, 255],
            "outside its tile the lower layer shows"
        );
        assert_eq!(pixel(25, 20), [0, 0, 255]);
        assert_eq!(pixel(1, 60), [0, 0, 0]);
        assert_eq!(pixel(3, 60), [255, 255, 255], "a hard stop starts the gap");
        assert_eq!(pixel(33, 60), [0, 0, 0], "the stripes repeat every 4px");
        let center = pixel(20, 100);
        assert!(
            center[0] == 255 && center[1] < 16,
            "the center holds the first stop, got {center:?}"
        );
        let halfway = pixel(30, 100);
        assert!(
            halfway[0] >= 250 && (110..150).contains(&halfway[1]),
            "a fade to transparent keeps its hue, got {halfway:?}"
        );
        assert_eq!(
            pixel(0, 81),
            [255, 255, 255],
            "past the closest side is clear"
        );
    }

    #[test]
    fn mask_images_scale_the_box_and_its_subtree_by_their_alpha() {
        let document = Document::parse_markup(
            "<html><body><div id='a'><p></p></div><div id='b'></div></body></html>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(concat!(
            "html, body { margin: 0; background: #ffffff } div { width: 40px; height: 40px; background: #ff0000 }",
            "#a { mask-image: linear-gradient(to right, transparent 50%, #000 50%) }",
            "p { margin: 0; width: 80px; height: 10px; background: #0000ff }",
            "#b { -webkit-mask: linear-gradient(#000, #000) 0 0 / 10px 10px no-repeat }",
        ))
        .expect("stylesheet should parse");
        let canvas = paint(&crate::layout::layout(
            &document,
            &stylesheet,
            Size {
                width: 80.0,
                height: 80.0,
            },
        ));
        let pixel = |x: usize, y: usize| {
            let index = (y * canvas.width() as usize + x) * 4;
            <[u8; 3]>::try_from(&canvas.rgba_pixels()[index..index + 3]).expect("a pixel")
        };
        assert_eq!(
            pixel(10, 20),
            [255, 255, 255],
            "the transparent half hides the box"
        );
        assert_eq!(pixel(30, 20), [255, 0, 0]);
        assert_eq!(pixel(10, 5), [255, 255, 255], "the mask covers descendants");
        assert_eq!(pixel(30, 5), [0, 0, 255]);
        assert_eq!(
            pixel(60, 5),
            [255, 255, 255],
            "nothing shows past the border box"
        );
        assert_eq!(pixel(5, 45), [255, 0, 0]);
        assert_eq!(
            pixel(15, 45),
            [255, 255, 255],
            "an unrepeated tile masks the rest"
        );
        assert_eq!(pixel(5, 55), [255, 255, 255]);
    }

    #[test]
    fn rounded_overflow_cuts_contexts_painted_by_a_stacking_ancestor() {
        let document = Document::parse_markup(
            "<html><body><div id='frame'><div id='faded'></div></div></body></html>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(concat!(
            "html, body { margin: 0; background: #ffffff }",
            "#frame { position: relative; width: 40px; height: 40px; border-radius: 20px; overflow: hidden }",
            "#faded { width: 40px; height: 40px; background: #000000; opacity: 0.5 }",
        ))
        .expect("stylesheet should parse");
        let canvas = paint(&crate::layout::layout(
            &document,
            &stylesheet,
            Size {
                width: 40.0,
                height: 40.0,
            },
        ));
        let pixel = |x: usize, y: usize| {
            let index = (y * canvas.width() as usize + x) * 4;
            <[u8; 3]>::try_from(&canvas.rgba_pixels()[index..index + 3]).expect("a pixel")
        };
        assert_eq!(
            pixel(1, 1),
            [255, 255, 255],
            "the frame's rounded corner cuts the fade"
        );
        assert!((120..136).contains(&pixel(20, 20)[0]));
    }

    #[test]
    fn rounded_gradient_and_faded_surfaces_reach_the_pixels() {
        let document = Document::parse_markup("<html><body><div id='card'></div></body></html>")
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
    fn borders_paint_each_side_by_style_and_rounded_overflow_clips_children() {
        let render = |html: &str, css: &str| {
            let document = Document::parse_markup(&format!("<html><body>{html}</body></html>"))
                .expect("document should parse");
            let stylesheet = Stylesheet::parse(css).expect("stylesheet should parse");
            paint(&crate::layout::layout(
                &document,
                &stylesheet,
                Size {
                    width: 80.0,
                    height: 80.0,
                },
            ))
        };
        let pixel = |canvas: &Canvas, x: usize, y: usize| {
            let index = (y * canvas.width() as usize + x) * 4;
            canvas.rgba_pixels()[index..index + 3].to_vec()
        };
        let sides = render(
            "<div id='box'></div>",
            "html, body { margin: 0; background: #ffffff } #box { width: 20px; height: 20px; border-style: solid; border-width: 4px 8px 12px 6px; border-color: #ff0000 #00ff00 #0000ff #000000 }",
        );
        assert_eq!(pixel(&sides, 15, 1), vec![255, 0, 0], "top side");
        assert_eq!(pixel(&sides, 31, 15), vec![0, 255, 0], "right side");
        assert_eq!(pixel(&sides, 15, 30), vec![0, 0, 255], "bottom side");
        assert_eq!(pixel(&sides, 2, 15), vec![0, 0, 0], "left side");
        assert_eq!(
            pixel(&sides, 15, 15),
            vec![255, 255, 255],
            "the inside stays clear"
        );

        let styled = render(
            "<div id='none'></div><div id='double'></div><div id='dashed'></div>",
            "html, body { margin: 0; background: #ffffff } div { width: 60px; height: 9px; margin-bottom: 4px } #none { border: 9px #ff0000 } #double { border-top: 9px double #000000 } #dashed { border-top: 2px dashed #000000 }",
        );
        assert_eq!(
            pixel(&styled, 30, 4),
            vec![255, 255, 255],
            "a border with no style takes no space and paints nothing"
        );
        assert_eq!(
            pixel(&styled, 30, 14),
            vec![0, 0, 0],
            "outer line of a double border"
        );
        assert_eq!(
            pixel(&styled, 30, 17),
            vec![255, 255, 255],
            "gap of a double border"
        );
        assert_eq!(
            pixel(&styled, 30, 21),
            vec![0, 0, 0],
            "inner line of a double border"
        );
        let dashes = (0..60)
            .filter(|x| pixel(&styled, *x, 36) == vec![0, 0, 0])
            .count();
        assert!(
            (20..45).contains(&dashes),
            "dashes leave gaps, got {dashes} dark pixels"
        );

        let clipped = render(
            "<div id='clip'><div id='child'></div></div><div id='outlined'></div>",
            "html, body { margin: 0; background: #ffffff } #clip { width: 40px; height: 40px; overflow: hidden; border-radius: 20px } #child { width: 40px; height: 40px; background: #ff0000 } #outlined { width: 10px; height: 10px; margin: 10px; outline: 2px solid #0000ff; outline-offset: 2px }",
        );
        assert_eq!(
            pixel(&clipped, 20, 20),
            vec![255, 0, 0],
            "the child shows inside"
        );
        assert_eq!(
            pixel(&clipped, 1, 1),
            vec![255, 255, 255],
            "the rounded clip hides the child's corner"
        );
        assert_eq!(
            pixel(&clipped, 7, 55),
            vec![0, 0, 255],
            "the outline sits outside the box"
        );
        assert_eq!(
            pixel(&clipped, 9, 55),
            vec![255, 255, 255],
            "the offset leaves a gap"
        );

        let hidden = render(
            "<div id='hidden'><div id='shown'></div></div>",
            "html, body { margin: 0; background: #ffffff } #hidden { visibility: hidden; width: 40px; height: 40px; background: #ff0000; border: 2px solid #000000 } #shown { visibility: visible; width: 10px; height: 10px; background: #00ff00 }",
        );
        assert_eq!(
            pixel(&hidden, 5, 5),
            vec![0, 255, 0],
            "a visible child still paints"
        );
        assert_eq!(
            pixel(&hidden, 30, 30),
            vec![255, 255, 255],
            "a hidden box paints nothing"
        );
        assert_eq!(pixel(&hidden, 0, 30), vec![255, 255, 255], "nor its border");
    }

    #[test]
    fn text_decorations_shadows_and_ellipses_paint() {
        let document = Document::parse_markup(
            "<html><body><p id='under'>ab</p><p id='shadow'>ab</p><p id='cut'>abcdefghijklmnop</p></body></html>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(
            "html, body, p { margin: 0; background: #ffffff; color: #000000; font-size: 20px; line-height: 30px } #under { text-decoration: underline #ff0000 } #shadow { text-shadow: 3px 3px 0 #00ff00 } #cut { width: 60px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis }",
        )
        .expect("stylesheet should parse");
        let layout = crate::layout::layout(
            &document,
            &stylesheet,
            Size {
                width: 200.0,
                height: 120.0,
            },
        );
        let canvas = paint(&layout);
        let row_has = |y: usize, color: [u8; 3]| {
            (0..40).any(|x| {
                let index = (y * canvas.width() as usize + x) * 4;
                canvas.rgba_pixels()[index..index + 3] == color
            })
        };
        // a 20px glyph box centred in a 30px line has its baseline 25px down; the underline sits
        // two pixels below it
        assert!(
            row_has(27, [255, 0, 0]),
            "the underline is drawn in its color"
        );
        assert!(
            (30..60).any(|y| row_has(y, [0, 255, 0])),
            "the shadow is drawn"
        );
        let cut = &layout.root.children[0].children[0].children[2];
        let text = cut
            .text
            .iter()
            .map(|run| run.text.as_str())
            .collect::<String>();
        assert!(
            text.ends_with('\u{2026}'),
            "an overflowing nowrap line ends in an ellipsis: {text}"
        );
        assert!(cut.text.iter().map(|run| run.width).sum::<f32>() <= 60.0);
    }

    #[test]
    fn painting_changes_pixels_and_writes_a_ppm_header() {
        let document = LayoutDocument {
            viewport: Size {
                width: 32.0,
                height: 24.0,
            },
            root: LayoutBox {
                node: node(&[]),
                tag: "#document".to_string(),
                rect: Rect {
                    x: 2.0,
                    y: 3.0,
                    width: 20.0,
                    height: 10.0,
                },
                background: Color::opaque(10, 20, 30),
                backgrounds: Vec::new(),
                masks: Vec::new(),
                object_fit: crate::css::ObjectFit::Fill,
                transform: None,
                backface_hidden: false,
                clip_path: None,
                radii: SQUARE_CORNERS,
                opacity: 1.0,
                shadows: Vec::new(),
                border: BoxBorder::NONE,
                outline: None,
                text: vec![TextRun {
                    text: "A".to_string(),
                    x: 4.0,
                    y: 5.0,
                    width: 8.0,
                    font_size: 12.0,
                    letter_spacing: 0.0,
                    font_stack: crate::font::FontStack::default(),
                    color: Color::WHITE,
                    font: crate::font::FontStyle::NORMAL,
                    node: None,
                    node_offset: 0,
                    source_len: 0,
                    rtl: false,
                    decoration: Default::default(),
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
                overflow_axes: [Overflow::Visible; 2],
                visible: true,
                pointer_events: true,
                selectable: true,
                cursor: crate::css::Cursor::Auto,
                floated: false,
                inline: false,
                control: None,
                selection: None,
                selection_color: None,
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
                node: node(&[]),
                tag: "textarea".to_string(),
                rect: Rect {
                    x: 2.0,
                    y: 3.0,
                    width: 32.0,
                    height: 28.0,
                },
                background: Color::opaque(10, 20, 30),
                backgrounds: Vec::new(),
                masks: Vec::new(),
                object_fit: crate::css::ObjectFit::Fill,
                transform: None,
                backface_hidden: false,
                clip_path: None,
                radii: SQUARE_CORNERS,
                opacity: 1.0,
                shadows: Vec::new(),
                border: BoxBorder::NONE,
                outline: None,
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
                        font: crate::font::FontStyle::NORMAL,
                        node: None,
                        node_offset: 0,
                        source_len: 0,
                        rtl: false,
                        decoration: Default::default(),
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
                        font: crate::font::FontStyle::NORMAL,
                        node: None,
                        node_offset: 0,
                        source_len: 0,
                        rtl: false,
                        decoration: Default::default(),
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
                overflow_axes: [Overflow::Visible; 2],
                visible: true,
                pointer_events: true,
                selectable: true,
                cursor: crate::css::Cursor::Auto,
                floated: false,
                inline: false,
                control: None,
                selection: None,
                selection_color: None,
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
                node: node(&[]),
                tag: "#document".to_string(),
                rect: Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 20.0,
                    height: 20.0,
                },
                background: Color::opaque(245, 248, 250),
                backgrounds: Vec::new(),
                masks: Vec::new(),
                object_fit: crate::css::ObjectFit::Fill,
                transform: None,
                backface_hidden: false,
                clip_path: None,
                radii: SQUARE_CORNERS,
                opacity: 1.0,
                shadows: Vec::new(),
                border: BoxBorder::NONE,
                outline: None,
                text: Vec::new(),
                control_value: None,
                control_selection: None,
                control_focused: false,
                control_text_origin: None,
                children: vec![
                    LayoutBox {
                        node: node(&[0]),
                        tag: "canvas".to_string(),
                        rect: Rect {
                            x: 0.0,
                            y: 0.0,
                            width: 10.0,
                            height: 10.0,
                        },
                        background: Color::transparent(),
                        backgrounds: Vec::new(),
                        masks: Vec::new(),
                        object_fit: crate::css::ObjectFit::Fill,
                        transform: None,
                        backface_hidden: false,
                        clip_path: None,
                        radii: SQUARE_CORNERS,
                        opacity: 1.0,
                        shadows: Vec::new(),
                        border: BoxBorder::NONE,
                        outline: None,
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
                        overflow_axes: [Overflow::Visible; 2],
                        visible: true,
                        pointer_events: true,
                        selectable: true,
                        cursor: crate::css::Cursor::Auto,
                        floated: false,
                        inline: false,
                        control: None,
                        selection: None,
                        selection_color: None,
                        scroll_content: Size {
                            width: 10.0,
                            height: 10.0,
                        },
                        scroll_offset: (0.0, 0.0),
                        margin: Default::default(),
                    },
                    LayoutBox {
                        node: node(&[1]),
                        tag: "div".to_string(),
                        rect: Rect {
                            x: 0.0,
                            y: 0.0,
                            width: 10.0,
                            height: 10.0,
                        },
                        background: Color::opaque(0, 0, 255),
                        backgrounds: Vec::new(),
                        masks: Vec::new(),
                        object_fit: crate::css::ObjectFit::Fill,
                        transform: None,
                        backface_hidden: false,
                        clip_path: None,
                        radii: SQUARE_CORNERS,
                        opacity: 1.0,
                        shadows: Vec::new(),
                        border: BoxBorder::NONE,
                        outline: None,
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
                        overflow_axes: [Overflow::Visible; 2],
                        visible: true,
                        pointer_events: true,
                        selectable: true,
                        cursor: crate::css::Cursor::Auto,
                        floated: false,
                        inline: false,
                        control: None,
                        selection: None,
                        selection_color: None,
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
                overflow_axes: [Overflow::Visible; 2],
                visible: true,
                pointer_events: true,
                selectable: true,
                cursor: crate::css::Cursor::Auto,
                floated: false,
                inline: false,
                control: None,
                selection: None,
                selection_color: None,
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
            node: node(&[0]),
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
