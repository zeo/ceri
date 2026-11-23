use crate::border::BorderStyle;
use crate::css::{
    BackgroundLayer, BackgroundSize, Color, Gradient, GradientKind, GradientStop, LayerImage,
    ObjectFit, Overflow, Position, PseudoElement, Radial, RadialSize, Style,
};
use crate::image::Image;
use crate::layout::{
    BoxBorder, ControlPaint, CornerRadii, ImageLayer, LayoutBox, LayoutDocument, ProseSpan, Rect,
    ResolvedEdges, SQUARE_CORNERS, Scrollbar, StackingContextEntry, TextRun, ThumbPlacement,
    overflow_clip_rect, paint_order, part_box, scrollbar_metrics, stacking_context_entries,
    thumb_placement, translate_rect, user_scroll_range, visual_offset_for, visual_rect_for,
};
use crate::png::encode_png;
use std::cell::RefCell;
use std::fs::File;
use std::io::{self, Write};
use std::path::Path;
use std::rc::Rc;

#[derive(Clone, Debug)]
pub struct Canvas {
    width: u32,
    height: u32,
    scale: f32,
    background: Color,
    // every pixel is opaque, so text can blend per subpixel; skia drops to grayscale in layers
    opaque: bool,
    pixels: Vec<u8>,
    // the layout point drawn at the surface's top-left corner, nonzero for transform layers
    shift: (f32, f32),
    // device pixels vector content moves by after it snaps, the fraction of a translate that
    // paints without a layer
    nudge: (f32, f32),
    // rounded overflow clips and clip-paths in device space; every write is scaled by their
    // coverage
    masks: Vec<Mask>,
    revision: Revision,
    // video frames a presenter shows under this surface, which leaves their boxes transparent;
    // none where every frame is painted in
    planes: Option<Vec<Plane>>,
}

// a video frame shown under the canvas: the node whose frame it is, the frame size it was
// fitted at, the device box it shows in, where the frame's corner lands and how many device
// pixels one frame pixel spans
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Plane {
    pub(crate) node: crate::dom::NodeId,
    pub(crate) size: (u32, u32),
    pub(crate) bounds: [u32; 4],
    pub(crate) origin: (f32, f32),
    pub(crate) scale: (f32, f32),
}

// a plane with the frame it shows, and the gpu surface holding that frame when it has one
pub(crate) type PlaneFrame = (
    Plane,
    std::sync::Arc<Image>,
    Option<std::sync::Arc<dyn crate::video::VideoSurface>>,
);

fn overlaps(one: [u32; 4], other: [u32; 4]) -> bool {
    one[0] < other[2] && other[0] < one[2] && one[1] < other[3] && other[1] < one[3]
}

// numbers what a canvas shows, so a presenter that holds the pixels numbered before can upload
// only the device boxes that changed since
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Revision {
    pub(crate) id: u64,
    // the revision this one was painted over, and the boxes it repainted
    pub(crate) changed: Option<(u64, Boxes)>,
}

// the device boxes a frame repainted, each its left, top, right and bottom; a frame damages
// only a few areas, and past that many the last box grows to take the rest in
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Boxes {
    boxes: [[u32; 4]; Region::LIMIT],
    count: usize,
}

impl Boxes {
    fn push(&mut self, added: [u32; 4]) {
        if self.count < self.boxes.len() {
            self.boxes[self.count] = added;
            self.count += 1;
        } else {
            let last = &mut self.boxes[self.count - 1];
            *last = [
                last[0].min(added[0]),
                last[1].min(added[1]),
                last[2].max(added[2]),
                last[3].max(added[3]),
            ];
        }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = [u32; 4]> + '_ {
        self.boxes[..self.count].iter().copied()
    }

    // the one box around them all
    #[cfg_attr(not(feature = "windowed"), expect(dead_code))]
    pub(crate) fn bounds(&self) -> [u32; 4] {
        self.iter()
            .reduce(|all, next| {
                [
                    all[0].min(next[0]),
                    all[1].min(next[1]),
                    all[2].max(next[2]),
                    all[3].max(next[3]),
                ]
            })
            .unwrap_or_default()
    }
}

impl From<[u32; 4]> for Boxes {
    fn from(only: [u32; 4]) -> Self {
        let mut boxes = Self::default();
        boxes.push(only);
        boxes
    }
}

fn next_revision() -> u64 {
    static REVISIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    REVISIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
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
        // a clear background leaves the surface see-through, as a transparent window shows it
        let opaque = background.alpha > 0;
        let mut pixels = vec![0; width as usize * height as usize * 4];
        if opaque {
            for pixel in pixels.as_chunks_mut::<4>().0.iter_mut() {
                pixel.copy_from_slice(&[background.red, background.green, background.blue, 255]);
            }
        }
        Self {
            width,
            height,
            scale,
            background,
            opaque,
            pixels,
            shift: (0.0, 0.0),
            nudge: (0.0, 0.0),
            masks: Vec::new(),
            revision: Revision {
                id: next_revision(),
                changed: None,
            },
            planes: None,
        }
    }

    pub(crate) fn revision(&self) -> Revision {
        self.revision
    }

    pub(crate) fn planes(&self) -> &[Plane] {
        self.planes.as_deref().unwrap_or_default()
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
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| pixel[..3] != background)
            .count()
    }

    pub fn write_ppm(&self, path: &Path) -> io::Result<()> {
        let mut file = File::create(path)?;
        write!(file, "P6\n{} {}\n255\n", self.width, self.height)?;
        for pixel in self.pixels.as_chunks::<4>().0.iter() {
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
            for pixel in self.pixels[start..start + self.width as usize * 4]
                .as_chunks::<4>()
                .0
                .iter()
            {
                file.write_all(&[pixel[2], pixel[1], pixel[0]])?;
            }
            file.write_all(&padding)?;
        }
        Ok(())
    }

    // a see-through surface keeps its alpha, its premultiplied pixels made straight as png
    // stores them
    pub fn write_png(&self, path: &Path) -> io::Result<()> {
        let channels = if self.opaque { 3 } else { 4 };
        let mut bytes = Vec::with_capacity(self.width as usize * channels * self.height as usize);
        for row in 0..self.height as usize {
            let start = row * self.bytes_per_row();
            for pixel in self.pixels[start..start + self.width as usize * 4]
                .as_chunks::<4>()
                .0
                .iter()
            {
                if self.opaque {
                    bytes.extend_from_slice(&pixel[..3]);
                    continue;
                }
                let alpha = u32::from(pixel[3]);
                let straight = |value: u8| match alpha {
                    0 => 0,
                    _ => ((u32::from(value) * 255 + alpha / 2) / alpha).min(255) as u8,
                };
                bytes.extend_from_slice(&[
                    straight(pixel[0]),
                    straight(pixel[1]),
                    straight(pixel[2]),
                    pixel[3],
                ]);
            }
        }
        std::fs::write(path, encode_png(self.width, self.height, channels, &bytes))
    }

    // a copy with each viewport rectangle filled over it, numbered anew so it uploads whole
    pub(crate) fn overlaid(&self, fills: &[([f32; 4], Color)]) -> Self {
        let mut canvas = self.clone();
        for ([left, top, right, bottom], color) in fills {
            canvas.fill_rect(*left, *top, right - left, bottom - top, *color);
        }
        canvas.revision = Revision {
            id: next_revision(),
            changed: None,
        };
        canvas
    }

    fn fill_rect(&mut self, x: f32, y: f32, width: f32, height: f32, color: Color) {
        self.fill_snapped(x, y, width, height, color, [0, self.width, 0, self.height]);
    }

    // the device pixels a rect covers once its edges snap to the nearest device pixel, as
    // browsers snap boxes, so a 1px border at a fractional offset stays one pixel and
    // neighbouring boxes neither overlap nor gap; then only those inside `within`
    fn fill_snapped(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        color: Color,
        within: [u32; 4],
    ) {
        if width <= 0.0 || height <= 0.0 || color.alpha == 0 {
            return;
        }
        let (x, y, width, height) = (
            (x - self.shift.0) * self.scale,
            (y - self.shift.1) * self.scale,
            width * self.scale,
            height * self.scale,
        );
        let (left, right) = snap_span(x, width, self.width);
        let (top, bottom) = snap_span(y, height, self.height);
        let (left, right) = (left.max(within[0]), right.min(within[1]));
        let (top, bottom) = (top.max(within[2]), bottom.min(within[3]));
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
                self.pixels[start..end].as_chunks_mut::<4>().0.fill(sample);
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
        // the rect snaps whole before the clip cuts it, so a sliver the clip leaves of a box
        // never grows into a row the box does not cover
        let edge = |at: f32, shift: f32, limit: u32| {
            ((at - shift) * self.scale).round().clamp(0.0, limit as f32) as u32
        };
        let within = [
            edge(clip.x, self.shift.0, self.width),
            edge(clip.x + clip.width, self.shift.0, self.width),
            edge(clip.y, self.shift.1, self.height),
            edge(clip.y + clip.height, self.shift.1, self.height),
        ];
        self.fill_snapped(x, y, width, height, color, within);
    }

    // device-space glyph blit, the coverage bitmap is already rasterized at this scale;
    // subpixel coverage blends per channel on an opaque surface when `lcd` allows it, as skia's
    // lcd16 blit does, and is averaged to one coverage elsewhere
    fn blend_coverage(
        &mut self,
        x: i32,
        y: i32,
        glyph: &crate::font::GlyphBitmap,
        color: Color,
        clip: Option<crate::layout::Rect>,
        lcd: bool,
    ) {
        if color.alpha == 0 {
            return;
        }
        // a glyph over a plane blends over transparent pixels, which subpixel blending cannot
        let over_plane = self.planes().iter().any(|plane| {
            overlaps(
                plane.bounds,
                [
                    x.max(0) as u32,
                    y.max(0) as u32,
                    (x + glyph.width as i32).max(0) as u32,
                    (y + glyph.height as i32).max(0) as u32,
                ],
            )
        });
        if glyph.subpixels && lcd && self.opaque && !over_plane {
            self.blend_subpixels(x, y, glyph, color, clip);
            return;
        }
        let gamma = text_gamma_table(color);
        let coverage_at = |index: usize| {
            if glyph.subpixels {
                let [red, green, blue] =
                    [0, 1, 2].map(|at| u16::from(glyph.coverage[index * 3 + at]));
                ((red + green + blue) / 3) as u8
            } else {
                glyph.coverage[index]
            }
        };
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
                let coverage = gamma[usize::from(coverage_at(row * glyph.width + column))];
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

    // skia's blend of an lcd16 mask: each channel's corrected coverage kept to five bits and
    // scaled to 32, then the source colour mixed into the destination channel by channel
    fn blend_subpixels(
        &mut self,
        x: i32,
        y: i32,
        glyph: &crate::font::GlyphBitmap,
        color: Color,
        clip: Option<crate::layout::Rect>,
    ) {
        let tables = [color.red, color.green, color.blue].map(text_gamma);
        let source = [color.red, color.green, color.blue].map(i32::from);
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
                let at = (row * glyph.width + column) * 3;
                let mut masks = [0, 1, 2].map(|channel| {
                    let five =
                        i32::from(tables[channel][usize::from(glyph.coverage[at + channel])] >> 3);
                    five + (five >> 4)
                });
                if masks == [0; 3] {
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
                let alpha = if self.masks.is_empty() {
                    color.alpha
                } else {
                    color
                        .scale_alpha(self.mask_at(pixel_x as u32, pixel_y as u32))
                        .alpha
                };
                if alpha == 0 {
                    continue;
                }
                if alpha < 255 {
                    let alpha = i32::from(alpha) + 1;
                    masks = masks.map(|mask| (mask * alpha) >> 8);
                }
                let index = ((pixel_y as u32 * self.width + pixel_x as u32) * 4) as usize;
                for channel in 0..3 {
                    let destination = i32::from(self.pixels[index + channel]);
                    self.pixels[index + channel] = (destination
                        + (((source[channel] - destination) * masks[channel]) >> 5))
                        as u8;
                }
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
        let _profile = crate::profile::scope("paint: shape");
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
        let [left, top, right, bottom] = narrow([left, top, right, bottom], clip);
        let (left, right) = (left.min(self.width), right.min(self.width));
        let steady = paint.steady();
        let mut columns = None;
        for pixel_y in top.min(self.height)..bottom.min(self.height) {
            let row = pixel_y as f32 + 0.5;
            // most of a row lies whole inside the shape, where only the paint is asked for
            let whole = device.whole_run(row, left..right);
            let whole = match clip {
                Some(clip) => run_where(whole, |pixel| {
                    clip.coverage((pixel as f32 + 0.5, row)) > 0.0
                }),
                None => whole,
            };
            for pixel_x in (left..whole.start).chain(whole.end..right) {
                let center = (pixel_x as f32 + 0.5, row);
                if let Some(clip) = clip
                    && clip.coverage(center) <= 0.0
                {
                    continue;
                }
                let coverage = device.coverage(center);
                if coverage <= 0.0 {
                    continue;
                }
                let color = paint.pixel_color(steady, center);
                if color.alpha == 0 {
                    continue;
                }
                self.blend_pixel(pixel_x, pixel_y, color.scale_alpha(coverage.min(1.0)));
            }
            let color_at = |pixel_x: u32| paint.color_at((pixel_x as f32 + 0.5, row));
            match steady {
                // the row is one color between the first and the last pixel its tile covers
                Steady::AlongRows => {
                    let drawn = run_where(whole, |pixel_x| color_at(pixel_x).alpha != 0);
                    let color = color_at(drawn.start);
                    if color.alpha == 255 && self.masks.is_empty() {
                        let at = pixel_y as usize * self.width as usize * 4;
                        let sample = [color.red, color.green, color.blue, 255];
                        let run = at + drawn.start as usize * 4..at + drawn.end as usize * 4;
                        self.pixels[run].as_chunks_mut::<4>().0.fill(sample);
                    } else {
                        for pixel_x in drawn {
                            self.blend_pixel(pixel_x, pixel_y, color);
                        }
                    }
                }
                // every row its tile covers repeats the first such row
                Steady::DownColumns => {
                    if !paint.covers_row(row) {
                        continue;
                    }
                    let colors = columns
                        .get_or_insert_with(|| (left..right).map(color_at).collect::<Vec<_>>());
                    for pixel_x in whole {
                        let color = colors[(pixel_x - left) as usize];
                        if color.alpha != 0 {
                            self.blend_pixel(pixel_x, pixel_y, color);
                        }
                    }
                }
                Steady::Nowhere => {
                    for pixel_x in whole {
                        let color = paint.pixel_color(steady, (pixel_x as f32 + 0.5, row));
                        if color.alpha != 0 {
                            self.blend_pixel(pixel_x, pixel_y, color);
                        }
                    }
                }
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
        let [left, top, right, bottom] = narrow([left, top, right, bottom], clip);
        let _profile = crate::profile::scope("paint: border");
        for pixel_y in top..bottom {
            let row = pixel_y as f32 + 0.5;
            // the ring leaves out what lies whole inside the inner shape
            let inside = if hollow {
                inner.whole_run(row, left..right)
            } else {
                left..left
            };
            for pixel_x in (left..inside.start).chain(inside.end..right) {
                let center = (pixel_x as f32 + 0.5, row);
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
        let [left, top, right, bottom] = narrow([left, top, right, bottom], clip);
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

    // chromium snaps a box's border box to device pixels before rounding its corners, so a
    // rounded box at a fractional offset lands on the same pixels a square one would
    fn snap_rect(&self, rect: Rect) -> Rect {
        let edge = |at: f32, shift: f32| {
            layout_units((at - shift) * self.scale).round() / self.scale + shift
        };
        let (left, top) = (edge(rect.x, self.shift.0), edge(rect.y, self.shift.1));
        Rect {
            x: left,
            y: top,
            width: edge(rect.x + rect.width, self.shift.0) - left,
            height: edge(rect.y + rect.height, self.shift.1) - top,
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
            opaque: false,
            pixels,
            shift: (bounds.x, bounds.y),
            nudge: (0.0, 0.0),
            masks: Vec::new(),
            revision: Revision::default(),
            planes: None,
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
        let [left, top, right, bottom] = narrow([left, top, right, bottom], clip);
        // the middle shows no shadow: an inset one fades out a few deviations inside its
        // shape, and an outer one hides under its frame; corners curve in by under a third
        // of their radius
        let (still, margin) = if inset {
            (shape, 4.0 * sigma + 1.0)
        } else {
            (frame, 1.0)
        };
        let corner = still
            .radii
            .iter()
            .fold(0.0_f32, |most, radius| most.max(radius.0).max(radius.1));
        let margin = margin + corner * 0.3;
        let hollow = [
            (still.x + margin).ceil(),
            (still.y + margin).ceil(),
            (still.x + still.width - margin).floor(),
            (still.y + still.height - margin).floor(),
        ];
        let mut within = Blurred::new(frame, 0.0, left..right);
        let mut blurred = Blurred::new(shape, sigma, left..right);
        for pixel_y in top..bottom {
            let row = pixel_y as f32;
            let (within_row, blurred_row) = (within.row(row + 0.5), blurred.row(row + 0.5));
            let skipped = if row >= hollow[1] && row + 1.0 <= hollow[3] && hollow[0] < hollow[2] {
                hollow[0].max(0.0) as u32..hollow[2].max(0.0) as u32
            } else {
                0..0
            };
            let mut pixel_x = left;
            while pixel_x < right {
                if skipped.contains(&pixel_x) {
                    pixel_x = skipped.end;
                    continue;
                }
                let center = (pixel_x as f32 + 0.5, row + 0.5);
                pixel_x += 1;
                if let Some(clip) = clip
                    && clip.coverage(center) <= 0.0
                {
                    continue;
                }
                let inside = within.at(&within_row, pixel_x - 1);
                let shade = if inset { inside } else { 1.0 - inside };
                if shade <= 0.0 {
                    continue;
                }
                let covered = blurred.at(&blurred_row, pixel_x - 1);
                let alpha = shade * if inset { 1.0 - covered } else { covered };
                if alpha > 0.0 {
                    self.blend_pixel(pixel_x - 1, pixel_y, color.scale_alpha(alpha.min(1.0)));
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
                let alpha = layer
                    .pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|pixel| pixel[3])
                    .collect();
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

    // background-clip: text, the coverage of every glyph the box and its subtree draw, whatever
    // their fill
    fn push_glyph_mask(
        &mut self,
        layout_box: &LayoutBox,
        bounds: Option<DeviceBounds>,
        offset: (f32, f32),
    ) {
        fn silhouettes(layer: &mut Canvas, layout_box: &LayoutBox, offset: (f32, f32)) {
            for run in &layout_box.text {
                let solid = TextRun {
                    color: Color::opaque(0, 0, 0),
                    ..run.clone()
                };
                paint_glyphs(layer, &solid, None, offset, true);
            }
            for child in &layout_box.children {
                silhouettes(layer, child, offset);
            }
        }
        let mask = match bounds {
            Some(bounds) => {
                let mut layer = self.layer(bounds);
                silhouettes(&mut layer, layout_box, offset);
                let alpha = layer
                    .pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|pixel| pixel[3])
                    .collect();
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
            opaque: false,
            pixels,
            shift: (
                self.shift.0 + bounds.left as f32 / self.scale,
                self.shift.1 + bounds.top as f32 / self.scale,
            ),
            nudge: self.nudge,
            masks: Vec::new(),
            revision: Revision::default(),
            planes: None,
        }
    }

    // a transparent surface over the device pixels `rect` touches, aligned to this one but
    // free to reach past its edges, as a filter that spreads needs
    fn aligned_layer(&self, rect: Rect) -> Canvas {
        let left = ((rect.x - self.shift.0) * self.scale).floor();
        let top = ((rect.y - self.shift.1) * self.scale).floor();
        let right = ((rect.x + rect.width - self.shift.0) * self.scale).ceil();
        let bottom = ((rect.y + rect.height - self.shift.1) * self.scale).ceil();
        let width = (right - left).max(1.0) as u32;
        let height = (bottom - top).max(1.0) as u32;
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
            opaque: false,
            pixels,
            shift: (
                self.shift.0 + left / self.scale,
                self.shift.1 + top / self.scale,
            ),
            nudge: self.nudge,
            masks: Vec::new(),
            revision: Revision::default(),
            planes: None,
        }
    }

    fn extent(&self) -> Rect {
        Rect {
            x: self.shift.0,
            y: self.shift.1,
            width: self.width as f32 / self.scale,
            height: self.height as f32 / self.scale,
        }
    }

    // draws a premultiplied layer aligned to this surface over it, inside the bounds, at the
    // given group opacity
    fn composite(&mut self, layer: Canvas, bounds: DeviceBounds, opacity: f32) {
        let factor = (opacity.clamp(0.0, 1.0) * 256.0).round() as u32;
        let stride = self.width as usize * 4;
        let layer_stride = layer.width as usize * 4;
        let masked = !self.masks.is_empty();
        let origin = |from: f32, to: f32| ((from - to) * self.scale).round() as i64;
        let origin = (
            origin(layer.shift.0, self.shift.0),
            origin(layer.shift.1, self.shift.1),
        );
        let left = i64::from(bounds.left).max(origin.0);
        let right = i64::from(bounds.right).min(origin.0 + i64::from(layer.width));
        let top = i64::from(bounds.top).max(origin.1);
        let bottom = i64::from(bounds.bottom).min(origin.1 + i64::from(layer.height));
        if right <= left || bottom <= top {
            recycle_layer(layer.pixels);
            return;
        }
        let bounds = DeviceBounds {
            left: left as u32,
            top: top as u32,
            right: right as u32,
            bottom: bottom as u32,
        };
        let first = (top - origin.1) as usize * layer_stride + (left - origin.0) as usize * 4;
        for (row, source_row) in
            (bounds.top..bounds.bottom).zip(layer.pixels[first..].chunks(layer_stride))
        {
            let start = row as usize * stride + bounds.left as usize * 4;
            let end = row as usize * stride + bounds.right as usize * 4;
            for (column, (target, source)) in self.pixels[start..end]
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(source_row.as_chunks::<4>().0.iter())
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

// keeps a loop to the pixels whose centers can land in a device clip, which feathers half a
// pixel past its edges
fn narrow(bounds: [u32; 4], clip: Option<Shape>) -> [u32; 4] {
    let Some(clip) = clip else { return bounds };
    let near = |edge: f32| (edge - 1.0).floor().max(0.0) as u32;
    let far = |edge: f32| (edge + 1.0).ceil().max(0.0) as u32;
    let [left, top, right, bottom] = bounds;
    [
        left.max(near(clip.x)),
        top.max(near(clip.y)),
        right.min(far(clip.x + clip.width)),
        bottom.min(far(clip.y + clip.height)),
    ]
}

// the pixels from the first to the last that pass a test which holds over one unbroken run
fn run_where(pixels: std::ops::Range<u32>, holds: impl Fn(u32) -> bool) -> std::ops::Range<u32> {
    let Some(start) = pixels.clone().find(|&pixel| holds(pixel)) else {
        return pixels.start..pixels.start;
    };
    let end = pixels
        .rev()
        .find(|&pixel| holds(pixel))
        .map_or(start, |last| last + 1);
    start..end
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

// a subtree's bounds, and how far glyphs, outlines and focus rings can reach past them, which
// is none under a transform
type Extent = (Rect, Option<f32>);
type Extents = std::collections::HashMap<(usize, [u32; 2]), Extent>;

thread_local! {
    // the extent of each box asked about while one layout paints, by the box's address and
    // the offset it was asked at: the paint asks at every box on its way down, and each
    // answer walks the whole subtree
    static EXTENTS: RefCell<Option<Extents>> = const { RefCell::new(None) };
}

struct MemoizedExtents {
    began: bool,
}

impl Drop for MemoizedExtents {
    fn drop(&mut self) {
        if self.began {
            EXTENTS.with(|extents| *extents.borrow_mut() = None);
        }
    }
}

// the boxes asked about must stay where and as they are while the guard lives
fn memoize_extents() -> MemoizedExtents {
    EXTENTS.with(|extents| {
        let mut extents = extents.borrow_mut();
        let began = extents.is_none();
        if began {
            *extents = Some(std::collections::HashMap::new());
        }
        MemoizedExtents { began }
    })
}

// everything a subtree can paint: its boxes, shadows and text, in layout space
fn subtree_bounds(layout_box: &LayoutBox, ancestor_offset: (f32, f32)) -> Rect {
    subtree_extent(layout_box, ancestor_offset).0
}

fn subtree_extent(layout_box: &LayoutBox, ancestor_offset: (f32, f32)) -> Extent {
    let key = (
        std::ptr::from_ref(layout_box) as usize,
        [ancestor_offset.0.to_bits(), ancestor_offset.1.to_bits()],
    );
    let known = EXTENTS.with(|extents| extents.borrow().as_ref()?.get(&key).copied());
    if let Some(extent) = known {
        return extent;
    }
    let extent = measure_subtree_extent(layout_box, ancestor_offset);
    EXTENTS.with(|extents| {
        if let Some(extents) = extents.borrow_mut().as_mut() {
            extents.insert(key, extent);
        }
    });
    extent
}

fn measure_subtree_extent(layout_box: &LayoutBox, ancestor_offset: (f32, f32)) -> Extent {
    let outline = layout_box
        .outline
        .as_ref()
        .map_or(0.0, |outline| outline.width + outline.offset.max(0.0));
    // glyphs overhang the runs measured below by less than their font size; a box with none
    // paints nothing past its bounds but its outline
    let glyphs = if layout_box.text.is_empty()
        && layout_box.control.is_none()
        && layout_box.control_value.is_none()
    {
        0.0
    } else {
        layout_box.font_size
    };
    let mut reach = layout_box.transform.is_none().then(|| outline.max(glyphs));
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
        let glyphs = Rect {
            x: run.x + offset.0,
            y: run.y + offset.1,
            width: run.width,
            height: run.font_size * 1.5,
        };
        bounds = union_rect(bounds, glyphs);
        // a blurred text shadow spreads twice its blur, as paint_text_shadow draws it
        if let Some(shadow) = run.decoration.shadow {
            let reach = shadow.blur.max(0.0) * 2.0;
            let cast = translate_rect(glyphs, (shadow.x, shadow.y));
            bounds = union_rect(bounds, outset_rect(cast, reach));
        }
    }
    for child in &layout_box.children {
        let (child_bounds, child_reach) = subtree_extent(child, offset);
        bounds = union_rect(bounds, child_bounds);
        reach = reach.zip(child_reach).map(|(most, next)| most.max(next));
    }
    (
        outset_rect(bounds, crate::filter::reach(&layout_box.filters.own)),
        reach,
    )
}

fn outset_rect(rect: Rect, amount: f32) -> Rect {
    Rect {
        x: rect.x - amount,
        y: rect.y - amount,
        width: rect.width + amount * 2.0,
        height: rect.height + amount * 2.0,
    }
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
        && !matches!(layout_box.tag.as_str(), "img" | "canvas" | "video" | "svg")
        && layout_box.overflow == Overflow::Visible
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

    // the pixels of a row, by the height of their centers, that the shape covers whole: half
    // a pixel inside every edge and clear of the corner curves, by the arithmetic of the
    // signed distance, so coverage there is exactly one
    fn whole_run(self, row: f32, pixels: std::ops::Range<u32>) -> std::ops::Range<u32> {
        if self.y - row > -0.5 || row - self.y - self.height > -0.5 {
            return pixels.start..pixels.start;
        }
        let bottom = row > self.y + self.height / 2.0;
        // how far in from its side a corner's curve ends, where the row crosses the corner
        let curve = |corner: usize| {
            let (radius_x, radius_y) = self.radii[corner];
            let (radius_x, radius_y) = (radius_x.min(self.width), radius_y.min(self.height));
            let along_y = if bottom {
                row - (self.y + self.height - radius_y)
            } else {
                self.y + radius_y - row
            };
            (radius_x > 0.0 && radius_y > 0.0 && along_y > 0.0).then_some(radius_x)
        };
        let (near, far) = if bottom {
            (curve(3), curve(2))
        } else {
            (curve(0), curve(1))
        };
        let middle = self.x + self.width / 2.0;
        run_where(pixels, |pixel| {
            let x = pixel as f32 + 0.5;
            self.x - x <= -0.5
                && x - self.x - self.width <= -0.5
                && if x > middle {
                    far.is_none_or(|radius| x - (self.x + self.width - radius) <= 0.0)
                } else {
                    near.is_none_or(|radius| self.x + radius - x <= 0.0)
                }
        })
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

// where a paint repeats itself, which spares working every pixel out: the same color along
// each row of its tile, or the same down each column
#[derive(Clone, Copy)]
enum Steady {
    AlongRows,
    DownColumns,
    Nowhere,
}

impl Paint {
    fn color_at(&self, point: (f32, f32)) -> Color {
        match self {
            Self::Solid(color) => *color,
            Self::Ramp(ramp) => ramp.color_at(point),
        }
    }

    // a paint asked at every pixel takes its colors from a table, one asked once a row or a
    // column works each out
    fn pixel_color(&self, steady: Steady, point: (f32, f32)) -> Color {
        match (self, steady) {
            (Self::Ramp(ramp), Steady::Nowhere) => ramp.tabled_color_at(point),
            _ => self.color_at(point),
        }
    }

    fn steady(&self) -> Steady {
        match self {
            Self::Solid(_) => Steady::AlongRows,
            Self::Ramp(ramp) => match ramp.shape {
                RampShape::Linear(_, (0.0, _)) => Steady::AlongRows,
                RampShape::Linear(_, (_, 0.0)) => Steady::DownColumns,
                _ => Steady::Nowhere,
            },
        }
    }

    // whether the paint's tile, or a repeat of it, covers the row at this height
    fn covers_row(&self, row: f32) -> bool {
        match self {
            Self::Solid(_) => true,
            Self::Ramp(ramp) => {
                ramp.repeat.1 || (0.0..ramp.size.1).contains(&((row - ramp.corner.1) / ramp.scale))
            }
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
    // the length of the gradient line or ray in device pixels
    reach: f32,
    // the colors a few times a device pixel apart along it, made when a fill first asks for
    // one at every pixel
    table: std::cell::OnceCell<Option<RampTable>>,
}

// a ramp's colors at even steps between its first and last stop, each as at its step's middle,
// or none where a stop falls inside the step and the color is worked out exactly
struct RampTable {
    steps_per_offset: f32,
    colors: Box<[Option<Color>]>,
}

impl RampTable {
    // a quarter of a device pixel moves no smooth ramp a whole level of any channel
    const STEPS_PER_PIXEL: f32 = 4.0;
    const MOST_STEPS: f32 = 65_536.0;

    fn new(ramp: &Ramp) -> Option<Self> {
        let (first, last) = (ramp.stops[0].0, ramp.stops[ramp.stops.len() - 1].0);
        let span = last - first;
        let count = (span * ramp.reach * Self::STEPS_PER_PIXEL).ceil();
        if !(1.0..=Self::MOST_STEPS).contains(&count) {
            return None;
        }
        let steps_per_offset = count / span;
        let colors = (0..count as usize)
            .map(|step| {
                let (low, high) = (
                    first + step as f32 / steps_per_offset,
                    first + (step + 1) as f32 / steps_per_offset,
                );
                let inside = ramp.stops.iter().any(|stop| low < stop.0 && stop.0 < high);
                (!inside).then(|| ramp.sample(first + (step as f32 + 0.5) / steps_per_offset))
            })
            .collect();
        Some(Self {
            steps_per_offset,
            colors,
        })
    }
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
        // an ellipse's ramp runs fastest along its longer radius
        let reach = match shape {
            RampShape::Radial(_, radii) => radii.0.max(radii.1),
            RampShape::Linear(..) => length,
        } * canvas.scale;
        Some(Self {
            corner: (device.x, device.y),
            scale: canvas.scale,
            size: (width, height),
            repeat,
            shape,
            repeating,
            stops,
            reach,
            table: std::cell::OnceCell::new(),
        })
    }

    fn color_at(&self, point: (f32, f32)) -> Color {
        self.offset_at(point)
            .map_or(Color::transparent(), |offset| self.sample(offset))
    }

    // as color_at, from the table of the ramp's colors where it has one
    fn tabled_color_at(&self, point: (f32, f32)) -> Color {
        let Some(offset) = self.offset_at(point) else {
            return Color::transparent();
        };
        let (first, last) = (self.stops[0].0, self.stops[self.stops.len() - 1].0);
        let offset = if self.repeating {
            first + (offset - first).rem_euclid(last - first)
        } else {
            offset
        };
        let table = self.table.get_or_init(|| RampTable::new(self));
        match table {
            Some(table) if (first..last).contains(&offset) => {
                let step = ((offset - first) * table.steps_per_offset) as usize;
                table
                    .colors
                    .get(step)
                    .copied()
                    .flatten()
                    .unwrap_or_else(|| self.sample(offset))
            }
            _ => self.sample(offset),
        }
    }

    // where along the ramp a point lies, or none outside a tile that does not repeat
    fn offset_at(&self, point: (f32, f32)) -> Option<f32> {
        let mut x = (point.0 - self.corner.0) / self.scale;
        let mut y = (point.1 - self.corner.1) / self.scale;
        if self.repeat.0 {
            x = x.rem_euclid(self.size.0);
        } else if !(0.0..self.size.0).contains(&x) {
            return None;
        }
        if self.repeat.1 {
            y = y.rem_euclid(self.size.1);
        } else if !(0.0..self.size.1).contains(&y) {
            return None;
        }
        Some(match self.shape {
            RampShape::Linear(start, axis) => (x - start.0) * axis.0 + (y - start.1) * axis.1,
            // hypot's guard against overflow costs more than the rest of the pixel, and a
            // tile is far too small to need it
            RampShape::Radial(center, radii) => {
                let (dx, dy) = ((x - center.0) / radii.0, (y - center.1) / radii.1);
                (dx * dx + dy * dy).sqrt()
            }
        })
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
    // the sine and cosine of a straight direction come out a hair off zero, which would
    // tilt a gradient that runs straight down or across
    let straight = |part: f32| if part.abs() < 1e-6 { 0.0 } else { part };
    let direction = (straight(angle.sin()), straight(-angle.cos()));
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

// how much of each pixel a shape covers once blurred by a gaussian of deviation `sigma`, in
// device pixels, asked a row of pixel centers at a time. a blurred edge is the error function
// of the distance to it, exact axis by axis for a square shape, and an unblurred square
// covers each pixel by its overlap. a rounded shape blurs row by row, as gpui draws shadows:
// each row is a span whose ends follow the corner curve and blurs exactly across, and four
// rows under the gaussian sum the blur down. what the pixels of a row share is worked out
// once a row, and what stays the same from row to row once a column
struct Blurred {
    shape: Shape,
    sigma: f32,
    left: u32,
    // by column: a square shape's coverage across, or a rounded one's span on the rows clear
    // of its corners, with the reach the span was worked out for
    across: Vec<Option<(u32, f32)>>,
}

enum BlurredRow {
    // a square shape's coverage down, at the row's height
    Square(f32),
    Sharp(f32),
    // for the left half and the right, then the step between the four rows summed
    Rounded([BlurredHalf; 2], f32),
}

struct BlurredHalf {
    // the reach and the weight of each of the four rows summed
    samples: [(f32, f32); 4],
    // whether all four reach the same
    level: bool,
    // the pixels so far inside every reach that each row's span is exactly one, and the
    // coverage they all then have
    settled: (std::ops::Range<u32>, f32),
}

fn blurred_axis(start: f32, length: f32, at: f32, sigma: f32) -> f32 {
    if length <= 0.0 {
        0.0
    } else if sigma < 0.5 {
        ((start + length).min(at + 0.5) - start.max(at - 0.5)).clamp(0.0, 1.0)
    } else {
        let scale = sigma * std::f32::consts::SQRT_2;
        0.5 * (erf((at - start) / scale) - erf((at - start - length) / scale))
    }
}

impl Blurred {
    fn new(shape: Shape, sigma: f32, columns: std::ops::Range<u32>) -> Self {
        Self {
            shape,
            sigma,
            left: columns.start,
            across: vec![None; columns.len()],
        }
    }

    fn row(&self, y: f32) -> BlurredRow {
        let (shape, sigma) = (self.shape, self.sigma);
        let columns = self.left..self.left + self.across.len() as u32;
        if shape.is_square() {
            return BlurredRow::Square(blurred_axis(shape.y, shape.height, y, sigma));
        }
        if sigma < 0.5 {
            return BlurredRow::Sharp(y);
        }
        let half = (shape.width / 2.0, shape.height / 2.0);
        let local = y - shape.y - half.1;
        let start = (-3.0 * sigma).clamp(local - half.1, local + half.1);
        let end = (3.0 * sigma).clamp(local - half.1, local + half.1);
        let step = (end - start) / 4.0;
        let scale = sigma * std::f32::consts::SQRT_2;
        let middle = shape.x + half.0;
        let corners = if local > 0.0 {
            [(3, false), (2, true)]
        } else {
            [(0, false), (1, true)]
        };
        let halves = corners.map(|(corner, right)| {
            let (radius_x, radius_y) = shape.radii[corner];
            let radius = ((radius_x + radius_y) / 2.0).min(half.0).min(half.1);
            let samples: [(f32, f32); 4] = std::array::from_fn(|sample| {
                let offset = start + step * (sample as f32 + 0.5);
                let row = (half.1 - radius - (local - offset).abs()).min(0.0);
                let reach = half.0 - radius + (radius * radius - row * row).max(0.0).sqrt();
                let weight = (-offset * offset / (2.0 * sigma * sigma)).exp()
                    / (sigma * (2.0 * std::f32::consts::PI).sqrt());
                (reach, weight)
            });
            let level = samples
                .iter()
                .all(|sample| sample.0.to_bits() == samples[0].0.to_bits());
            // the error function comes out as exactly one a little past four deviations, so
            // well inside the shortest reach every span is one; the ends are checked by the
            // arithmetic `at` uses, and what lies between them is further inside still
            let inside = samples
                .iter()
                .fold(f32::INFINITY, |least, sample| least.min(sample.0))
                - 4.5 * scale;
            let whole = |pixel: u32| {
                let local = pixel as f32 + 0.5 - shape.x - shape.width / 2.0;
                (local > 0.0) == right
                    && samples.iter().all(|&(reach, _)| {
                        0.5 * (erf((local + reach) / scale) - erf((local - reach) / scale)) == 1.0
                    })
            };
            let (from, to) = if right {
                (middle, middle + inside)
            } else {
                (middle - inside, middle)
            };
            let from = (from.ceil().max(columns.start as f32) as u32).min(columns.end);
            let to = (to.floor().max(0.0) as u32).min(columns.end);
            let run = if from < to && whole(from) && whole(to - 1) {
                from..to
            } else {
                0..0
            };
            BlurredHalf {
                samples,
                level,
                settled: (run, samples.iter().map(|&(_, weight)| weight * step).sum()),
            }
        });
        BlurredRow::Rounded(halves, step)
    }

    fn at(&mut self, row: &BlurredRow, pixel_x: u32) -> f32 {
        let (shape, sigma) = (self.shape, self.sigma);
        let x = pixel_x as f32 + 0.5;
        let kept = &mut self.across[(pixel_x - self.left) as usize];
        match row {
            BlurredRow::Square(down) => {
                let across = match *kept {
                    Some((_, across)) => across,
                    None => {
                        let across = blurred_axis(shape.x, shape.width, x, sigma);
                        *kept = Some((0, across));
                        across
                    }
                };
                across * down
            }
            BlurredRow::Sharp(y) => (0.5 - shape.signed_distance((x, *y))).clamp(0.0, 1.0),
            BlurredRow::Rounded(halves, step) => {
                let scale = sigma * std::f32::consts::SQRT_2;
                let local = x - shape.x - shape.width / 2.0;
                let BlurredHalf {
                    samples,
                    level,
                    settled,
                } = &halves[usize::from(local > 0.0)];
                if settled.0.contains(&pixel_x) {
                    return settled.1;
                }
                let span = |reach: f32| {
                    0.5 * (erf((local + reach) / scale) - erf((local - reach) / scale))
                };
                if !level {
                    return samples
                        .iter()
                        .map(|&(reach, weight)| span(reach) * weight * step)
                        .sum();
                }
                let reach = samples[0].0;
                let across = match *kept {
                    Some((known, across)) if known == reach.to_bits() => across,
                    _ => {
                        let across = span(reach);
                        *kept = Some((reach.to_bits(), across));
                        across
                    }
                };
                samples
                    .iter()
                    .map(|&(_, weight)| across * weight * step)
                    .sum()
            }
        }
    }
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
    let _profile = crate::profile::scope("paint: shadow");
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
        ControlPaint::Checkbox {
            indeterminate: true,
            ..
        } => {
            let white = Color::WHITE.scale_alpha(opacity);
            let stroke = (rect.width * 0.14).max(1.5);
            canvas.fill_segment((point(0.25, 0.5), point(0.75, 0.5)), stroke, white, clip);
        }
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
        ControlPaint::Range {
            fraction,
            accent,
            disabled,
            styled: false,
            travel: (start, length),
            thumb,
        } => {
            // chromium's slider: an 8px bordered track inset a pixel from each end, filled with
            // the accent up to a 16px thumb whose position snaps to whole pixels
            let thumb_x = rect.x + start + (length * fraction).round();
            let track = Rect {
                x: rect.x + start + 1.0,
                y: rect.y + (rect.height - 8.0) / 2.0,
                width: (length + thumb - 2.0).max(0.0),
                height: 8.0,
            };
            let (filled, filled_border, empty, empty_border, knob) = if disabled {
                let gray = Color::opaque(203, 203, 203);
                let border = Color::opaque(177, 177, 177);
                let empty = Color::opaque(250, 250, 250);
                (gray, border, empty, Color::opaque(210, 210, 210), gray)
            } else {
                let border = accent.mix(Color::opaque(117, 117, 117), 0.5);
                let empty = Color::opaque(239, 239, 239);
                (accent, border, empty, Color::opaque(178, 178, 178), accent)
            };
            let mut bar = |width: f32, (fill, border): (Color, Color)| {
                let outer = Rect { width, ..track };
                let inner = Rect {
                    x: outer.x + 1.0,
                    y: outer.y + 1.0,
                    width: (width - 2.0).max(0.0),
                    height: 6.0,
                };
                canvas.fill_shape(
                    Shape::from_rect(outer, round(4.0)),
                    &Paint::Solid(border.scale_alpha(opacity)),
                    clip,
                );
                canvas.fill_shape(
                    Shape::from_rect(inner, round(3.0)),
                    &Paint::Solid(fill.scale_alpha(opacity)),
                    clip,
                );
            };
            bar(track.width, (empty, empty_border));
            bar(
                (thumb_x + thumb / 2.0 - track.x).max(0.0),
                (filled, filled_border),
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
                &Paint::Solid(knob.scale_alpha(opacity)),
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
// the rounded padding box a clipping box masks its content to, snapped as its background is
fn overflow_mask(canvas: &Canvas, layout_box: &LayoutBox, rect: Rect) -> Option<Shape> {
    let clips = layout_box.overflow.clips();
    let padding_box =
        Shape::from_rect(canvas.snap_rect(rect), layout_box.radii).inset(layout_box.border.widths);
    (clips && !padding_box.is_square()).then_some(padding_box)
}

pub(crate) fn paint(document: &LayoutDocument) -> Canvas {
    paint_with_canvases(document, &[], 1.0, false)
}

// canvas elements reach paint as image layers of their own, so drawing on one repaints it
// without a new layout; with planes a video a presenter can show under the canvas leaves a
// transparent box instead, unless a backdrop filter would read through it
pub(crate) fn paint_with_canvases(
    document: &LayoutDocument,
    canvases: &[ImageLayer],
    scale: f32,
    planes: bool,
) -> Canvas {
    paint_frame(document, canvases, scale, planes, BASE)
}

// the whole frame over a base of its own: the white a page shows through where it paints
// nothing, or nothing at all for a window the desktop shows through
pub(crate) fn paint_frame(
    document: &LayoutDocument,
    canvases: &[ImageLayer],
    scale: f32,
    planes: bool,
    base: Color,
) -> Canvas {
    let _profile = crate::profile::scope("paint: whole frame");
    let layers;
    let images = if canvases.is_empty() {
        &document.images
    } else {
        // a frame taken at paint, as of an animated image, comes before the one layout placed
        layers = canvases
            .iter()
            .chain(&document.images)
            .cloned()
            .collect::<Vec<_>>();
        &layers
    };
    let mut canvas = Canvas::scaled(
        document.viewport.width,
        document.viewport.height,
        scale,
        base,
    );
    if planes && !has_backdrop(&document.root) {
        canvas.planes = Some(Vec::new());
    }
    paint_document(&mut canvas, document, images, None);
    canvas
}

pub(crate) const BASE: Color = Color::WHITE;

// what a frame has to paint again over the one before it
#[derive(Debug, PartialEq)]
pub(crate) enum Damage {
    Nothing,
    // separate rectangles, so changes far apart do not repaint what lies between them
    Areas(Vec<Rect>),
    Everything,
}

// the rectangles a frame changed; ones that touch or sit close merge, and past a few the
// pair that wastes the least area merges, so a repaint never walks the tree many times
#[derive(Default)]
struct Region {
    rects: Vec<Rect>,
}

impl Region {
    const LIMIT: usize = 6;
    // a repaint reaches a few device pixels past its rectangle, so near neighbours join
    const NEAR: f32 = 8.0;

    fn add(&mut self, mut rect: Rect) {
        let near = |a: &Rect, b: &Rect| {
            a.x <= b.x + b.width + Self::NEAR
                && b.x <= a.x + a.width + Self::NEAR
                && a.y <= b.y + b.height + Self::NEAR
                && b.y <= a.y + a.height + Self::NEAR
        };
        while let Some(index) = self.rects.iter().position(|other| near(other, &rect)) {
            rect = union_rect(rect, self.rects.swap_remove(index));
        }
        self.rects.push(rect);
        while self.rects.len() > Self::LIMIT {
            let area = |rect: Rect| rect.width * rect.height;
            let mut best = (0, 1, f32::INFINITY);
            for first in 0..self.rects.len() {
                for second in first + 1..self.rects.len() {
                    let (a, b) = (self.rects[first], self.rects[second]);
                    let waste = area(union_rect(a, b)) - area(a) - area(b);
                    if waste < best.2 {
                        best = (first, second, waste);
                    }
                }
            }
            let joined = union_rect(self.rects[best.0], self.rects.swap_remove(best.1));
            self.rects.swap_remove(best.0);
            self.add(joined);
        }
    }
}

// compares the layout and layers a frame was painted from with the next frame's; a new frame
// of a video shown as one of planes changes nothing painted while its size holds
pub(crate) fn damage(
    before: (&LayoutDocument, &[ImageLayer]),
    after: (&LayoutDocument, &[ImageLayer]),
    planes: &[Plane],
) -> Damage {
    let _profile = crate::profile::scope("paint: damage");
    let ((old, old_canvases), (new, new_canvases)) = (before, after);
    if old.viewport != new.viewport || old.prose_selections != new.prose_selections {
        return Damage::Everything;
    }
    let mut area = Region::default();
    if !changed_area(&old.root, &new.root, ((0.0, 0.0), (0.0, 0.0)), 0, &mut area) {
        return Damage::Everything;
    }
    // a layer draws over its node's box wherever it sits in the list, and a box that came,
    // went or moved is in the changed area already, so what is left is a node whose picture
    // is new, changed or gone
    let old_layers = old.images.iter().chain(old_canvases).collect::<Vec<_>>();
    let new_layers = new.images.iter().chain(new_canvases).collect::<Vec<_>>();
    let same = |a: &ImageLayer, b: &ImageLayer| a.node == b.node && a.background == b.background;
    let mut redrawn = Vec::new();
    for after in &new_layers {
        let shown = planes.iter().any(|plane| {
            plane.node == after.node && plane.size == (after.image.width, after.image.height)
        });
        let kept = old_layers.iter().any(|before| {
            same(before, after) && std::sync::Arc::ptr_eq(&before.image, &after.image)
        });
        if !kept && !shown {
            redrawn.push(after.node);
        }
    }
    redrawn.extend(
        old_layers
            .iter()
            .filter(|before| !new_layers.iter().any(|after| same(before, after)))
            .map(|before| before.node),
    );
    if !redrawn.is_empty() && !node_area(&new.root, (0.0, 0.0), &redrawn, &mut area) {
        return Damage::Everything;
    }
    match area.rects {
        rects if rects.is_empty() => Damage::Nothing,
        // a backdrop filter reads what lies around it, so a change anywhere may reach it
        _ if has_backdrop(&new.root) => Damage::Everything,
        rects => Damage::Areas(rects),
    }
}

// false when a change cannot be bounded: under a transform, or on the boxes that give the
// canvas its background
fn changed_area(
    old: &LayoutBox,
    new: &LayoutBox,
    offsets: ((f32, f32), (f32, f32)),
    depth: usize,
    area: &mut Region,
) -> bool {
    if old.same_own(new) && old.children.len() == new.children.len() {
        if old.transform.is_some() || !old.filters.own.is_empty() {
            if old.children == new.children {
                return true;
            }
        } else {
            let inner = (
                visual_offset_for(old, offsets.0),
                visual_offset_for(new, offsets.1),
            );
            return old
                .children
                .iter()
                .zip(&new.children)
                .all(|(old, new)| changed_area(old, new, inner, depth + 1, area));
        }
    } else if depth <= 2 && matches!(new.tag.as_str(), "html" | "body") {
        return false;
    }
    let (Some(old), Some(new)) = (ink(old, offsets.0), ink(new, offsets.1)) else {
        return false;
    };
    area.add(union_rect(old, new));
    true
}

fn node_area(
    layout_box: &LayoutBox,
    ancestor_offset: (f32, f32),
    nodes: &[crate::dom::NodeId],
    area: &mut Region,
) -> bool {
    if nodes.contains(&layout_box.node) {
        let Some(ink) = ink(layout_box, ancestor_offset) else {
            return false;
        };
        area.add(ink);
    }
    if layout_box.transform.is_some() {
        // a transformed layer's ink is out of reach, so a redrawn image under it repaints all
        return !has_node(layout_box, nodes);
    }
    let offset = visual_offset_for(layout_box, ancestor_offset);
    layout_box
        .children
        .iter()
        .all(|child| node_area(child, offset, nodes, area))
}

fn has_node(layout_box: &LayoutBox, nodes: &[crate::dom::NodeId]) -> bool {
    nodes.contains(&layout_box.node)
        || layout_box
            .children
            .iter()
            .any(|child| has_node(child, nodes))
}

fn has_backdrop(layout_box: &LayoutBox) -> bool {
    !layout_box.filters.backdrop.is_empty() || layout_box.children.iter().any(has_backdrop)
}

// everything a box and its subtree can paint, none under a transform; glyphs, outlines and
// focus rings can reach past the boxes and runs subtree_bounds measures
fn ink(layout_box: &LayoutBox, ancestor_offset: (f32, f32)) -> Option<Rect> {
    let (bounds, reach) = subtree_extent(layout_box, ancestor_offset);
    Some(outset_rect(bounds, reach?))
}

// paints area again over the frame before, which already shows the rest; a clip rounds its
// edges and keeps slivers a pixel wide, so the paint reaches a margin past the area and the
// margin's earlier pixels go back afterwards, one pixel wider since the margin's own edges
// can round outward
pub(crate) fn repaint(
    canvas: &mut Canvas,
    document: &LayoutDocument,
    canvases: &[ImageLayer],
    area: Rect,
) {
    let _profile = crate::profile::scope("paint: repaint");
    const MARGIN: u32 = 2;
    let Some(kept) = DeviceBounds::of(canvas, area, None) else {
        return;
    };
    let scale = canvas.scale;
    let painted = Rect {
        x: (kept.left as f32 - MARGIN as f32) / scale,
        y: (kept.top as f32 - MARGIN as f32) / scale,
        width: (kept.right - kept.left + 2 * MARGIN) as f32 / scale,
        height: (kept.bottom - kept.top + 2 * MARGIN) as f32 / scale,
    };
    let reach = DeviceBounds {
        left: kept.left.saturating_sub(MARGIN + 1),
        top: kept.top.saturating_sub(MARGIN + 1),
        right: (kept.right + MARGIN + 1).min(canvas.width),
        bottom: (kept.bottom + MARGIN + 1).min(canvas.height),
    };
    let stride = canvas.bytes_per_row();
    let line = |row: usize| {
        row * stride + reach.left as usize * 4..row * stride + reach.right as usize * 4
    };
    let mut saved = Vec::with_capacity(line(0).len() * (reach.bottom - reach.top) as usize);
    for row in reach.top as usize..reach.bottom as usize {
        saved.extend_from_slice(&canvas.pixels[line(row)]);
    }
    let kept_box = [kept.left, kept.top, kept.right, kept.bottom];
    let before = canvas.planes.clone();
    if let Some(planes) = &mut canvas.planes {
        planes.retain(|plane| !overlaps(plane.bounds, kept_box));
    }
    let base = canvas.background;
    if base.alpha > 0 {
        canvas.fill_rect(painted.x, painted.y, painted.width, painted.height, base);
    } else {
        let columns = kept.left.saturating_sub(MARGIN) as usize * 4
            ..(kept.right + MARGIN).min(canvas.width) as usize * 4;
        for row in kept.top.saturating_sub(MARGIN)..(kept.bottom + MARGIN).min(canvas.height) {
            let start = row as usize * stride;
            canvas.pixels[start + columns.start..start + columns.end].fill(0);
        }
    }
    let layers = canvases
        .iter()
        .chain(&document.images)
        .cloned()
        .collect::<Vec<_>>();
    paint_document(canvas, document, &layers, Some(painted));
    let inner = (kept.left - reach.left) as usize * 4..(kept.right - reach.left) as usize * 4;
    for (row, before) in
        (reach.top as usize..reach.bottom as usize).zip(saved.chunks(line(0).len()))
    {
        let after = &mut canvas.pixels[line(row)];
        if (kept.top as usize..kept.bottom as usize).contains(&row) {
            after[..inner.start].copy_from_slice(&before[..inner.start]);
            after[inner.end..].copy_from_slice(&before[inner.end..]);
        } else {
            after.copy_from_slice(before);
        }
    }
    if let Some(before) = before {
        match settled_planes(&before, canvas.planes.take().unwrap_or_default(), kept_box) {
            Some(planes) => canvas.planes = Some(planes),
            None => {
                *canvas = paint_frame(document, canvases, scale, true, base);
                return;
            }
        }
    }
    canvas.revision = Revision {
        id: next_revision(),
        changed: Some((canvas.revision.id, kept_box.into())),
    };
}

// brings a canvas that shows the frame before the one numbered `front` up to the frame after
// it: the areas that changed from it to `front` paint again along with the areas changing now,
// and the result is numbered as a change to `front`
pub(crate) fn repaint_behind(
    canvas: &mut Canvas,
    document: &LayoutDocument,
    canvases: &[ImageLayer],
    (behind, areas): (&[Rect], &[Rect]),
    front: u64,
) {
    let mut region = Region::default();
    for &area in behind.iter().chain(areas) {
        region.add(area);
    }
    let shown = canvas.revision.id;
    repaint_areas(canvas, document, canvases, &region.rects);
    if let Some((base, _)) = &mut canvas.revision.changed
        && *base == shown
    {
        *base = front;
    }
}

// repaints each area and numbers the result as one change to what the canvas showed, so a
// presenter holding that uploads the repainted boxes and nothing between them
pub(crate) fn repaint_areas(
    canvas: &mut Canvas,
    document: &LayoutDocument,
    canvases: &[ImageLayer],
    areas: &[Rect],
) {
    let shown = canvas.revision.id;
    let mut boxes = Boxes::default();
    let _extents = memoize_extents();
    for &area in areas {
        let last = canvas.revision.id;
        repaint(canvas, document, canvases, area);
        match canvas.revision.changed {
            _ if canvas.revision.id == last => {}
            Some((_, repainted)) => repainted.iter().for_each(|next| boxes.push(next)),
            // the repaint fell back to painting everything
            None => return,
        }
    }
    if boxes.count > 0 {
        canvas.revision = Revision {
            id: next_revision(),
            changed: Some((shown, boxes)),
        };
    }
}

// a frame laid out from the last one by moving one scroller's content a whole number of device
// pixels: the pixels already there move with it, and only what scrolls into view, the bars and
// whatever stays put over the scrollport paint again
pub(crate) struct ScrollRepaint {
    moved: DeviceBounds,
    shift: (i32, i32),
    repairs: Vec<Rect>,
}

// none when moving pixels could show anything a full paint would not: a backdrop that is not
// one flat colour, or transforms, filters, masks and planes on the way
pub(crate) fn scroll_repaint(
    canvas: &Canvas,
    before: &LayoutDocument,
    after: &LayoutDocument,
) -> Option<ScrollRepaint> {
    fn scrolled(
        before: &LayoutBox,
        after: &LayoutBox,
        path: &mut Vec<usize>,
        found: &mut Vec<Vec<usize>>,
    ) {
        if before.scroll_offset != after.scroll_offset {
            found.push(path.clone());
        }
        for (index, (before, after)) in before.children.iter().zip(&after.children).enumerate() {
            path.push(index);
            scrolled(before, after, path, found);
            path.pop();
        }
    }
    // a positioned box may stay put or escape the scroller's clip; unless it moved with the
    // content and stays inside the clip, it paints again where it was, where it is, and where
    // the moving pixels carried its old copy
    fn settle(
        (old, new): (&LayoutBox, &LayoutBox),
        offsets: ((f32, f32), (f32, f32)),
        (clip, delta): (Rect, (f32, f32)),
        repairs: &mut Vec<Rect>,
    ) -> bool {
        if matches!(
            new.position,
            Position::Sticky | Position::Absolute | Position::Fixed
        ) {
            let (Some(was), Some(now)) = (ink(old, offsets.0), ink(new, offsets.1)) else {
                return false;
            };
            let carried = translate_rect(was, (-delta.0, -delta.1));
            let near = |a: f32, b: f32| (a - b).abs() < 1.0e-3;
            let kept = near(now.x, carried.x)
                && near(now.y, carried.y)
                && near(now.width, carried.width)
                && near(now.height, carried.height)
                && contains_rect(clip, was)
                && contains_rect(clip, now);
            if !kept {
                repairs.extend([was, now]);
                repairs.extend(intersect_clip(Some(clip), carried));
                return true;
            }
        }
        let inner = (
            visual_offset_for(old, offsets.0),
            visual_offset_for(new, offsets.1),
        );
        old.children
            .iter()
            .zip(&new.children)
            .all(|pair| settle(pair, inner, (clip, delta), repairs))
    }
    if !canvas.planes().is_empty() || has_backdrop(&after.root) {
        return None;
    }
    let mut found = Vec::new();
    scrolled(&before.root, &after.root, &mut Vec::new(), &mut found);
    let [path] = found.as_slice() else {
        return None;
    };
    let mut chain = vec![(&after.root, (0.0, 0.0))];
    let mut old = &before.root;
    for &index in path {
        let (parent, ancestor_offset) = *chain.last()?;
        chain.push((
            &parent.children[index],
            visual_offset_for(parent, ancestor_offset),
        ));
        old = &old.children[index];
    }
    let (scroller, scroller_offset) = *chain.last()?;
    let delta = (
        scroller.scroll_offset.0 - old.scroll_offset.0,
        scroller.scroll_offset.1 - old.scroll_offset.1,
    );
    let scale = canvas.scale;
    let whole = |value: f32| {
        let device = value * scale;
        ((device - device.round()).abs() < 1.0e-3).then_some(device.round() as i32)
    };
    let shift = (whole(delta.0)?, whole(delta.1)?);
    let mut clip = Rect {
        x: 0.0,
        y: 0.0,
        width: after.viewport.width,
        height: after.viewport.height,
    };
    for &(layout_box, ancestor_offset) in &chain {
        if layout_box.transform.is_some()
            || !layout_box.filters.own.is_empty()
            || !layout_box.masks.is_empty()
            || layout_box.clip_path.is_some()
        {
            return None;
        }
        if layout_box.overflow.clips() || layout_box.is_document() {
            let offset = visual_offset_for(layout_box, ancestor_offset);
            clip = intersect_clip(
                Some(clip),
                translate_rect(overflow_clip_rect(layout_box), offset),
            )?;
        }
    }
    // the backdrop the content scrolls over must be one colour wherever the clip reaches
    let mut flat = false;
    for &(layout_box, ancestor_offset) in chain.iter().rev() {
        if !layout_box.backgrounds.is_empty()
            || layout_box.shadows.iter().any(|shadow| shadow.inset)
            || !layout_box.text.is_empty()
            || layout_box.control.is_some()
            || layout_box.control_value.is_some()
            || layout_box.svg.is_some()
            || layout_box
                .outline
                .is_some_and(|outline| outline.offset < 0.0)
        {
            return None;
        }
        if !layout_box.is_document() {
            let rect = visual_rect_for(layout_box, ancestor_offset);
            let border = layout_box.border.widths;
            let padding = Rect {
                x: rect.x + border.left,
                y: rect.y + border.top,
                width: rect.width - border.left - border.right,
                height: rect.height - border.top - border.bottom,
            };
            if !contains_rect(padding, clip) {
                return None;
            }
        }
        if layout_box.background.alpha == 255 {
            flat = true;
            break;
        }
    }
    if !flat && canvas_background(&after.root).is_some_and(|root| !root.backgrounds.is_empty()) {
        return None;
    }
    let width = canvas.width as f32;
    let height = canvas.height as f32;
    // a pixel the clip's edge crosses blends what lies on both sides, so it paints again; an
    // edge on a pixel boundary blends nothing
    let inward = |edge: f32, toward: f32| {
        let device = edge * scale;
        match (device - device.round()).abs() < 1e-3 {
            true => device.round(),
            false => device.round() + toward,
        }
    };
    let moved = DeviceBounds {
        left: inward(clip.x, 1.0).clamp(0.0, width) as u32,
        top: inward(clip.y, 1.0).clamp(0.0, height) as u32,
        right: inward(clip.right(), -1.0).clamp(0.0, width) as u32,
        bottom: inward(clip.bottom(), -1.0).clamp(0.0, height) as u32,
    };
    if moved.right <= moved.left + shift.0.unsigned_abs()
        || moved.bottom <= moved.top + shift.1.unsigned_abs()
    {
        return None;
    }
    let mut repairs = Vec::new();
    // what stays put over or around the content: the pixels moving carry its old copy along
    let mut stationary = Vec::new();
    for pair in chain.windows(2) {
        let &[(parent, ancestor_offset), (next, _)] = pair else {
            continue;
        };
        let offset = visual_offset_for(parent, ancestor_offset);
        for child in parent
            .children
            .iter()
            .filter(|child| !std::ptr::eq(*child, next))
        {
            stationary.push(ink(child, offset)?);
        }
    }
    let inner = (
        visual_offset_for(old, scroller_offset),
        visual_offset_for(scroller, scroller_offset),
    );
    if !old
        .children
        .iter()
        .zip(&scroller.children)
        .all(|pair| settle(pair, inner, (clip, delta), &mut repairs))
    {
        return None;
    }
    let metrics = scrollbar_metrics(scroller);
    for track in [metrics.vertical_track, metrics.horizontal_track]
        .into_iter()
        .flatten()
    {
        let track = translate_rect(track, inner.1);
        repairs.push(track);
        stationary.push(track);
    }
    for &(layout_box, ancestor_offset) in &chain {
        let rect = visual_rect_for(layout_box, ancestor_offset);
        for (corner, (horizontal, vertical)) in layout_box.radii.into_iter().enumerate() {
            if horizontal <= 0.0 && vertical <= 0.0 {
                continue;
            }
            let corner = Rect {
                x: if matches!(corner, 0 | 3) {
                    rect.x
                } else {
                    rect.right() - horizontal
                },
                y: if corner < 2 {
                    rect.y
                } else {
                    rect.bottom() - vertical
                },
                width: horizontal,
                height: vertical,
            };
            stationary.push(corner);
        }
    }
    for rect in stationary {
        repairs.extend(intersect_clip(Some(clip), rect));
        repairs.extend(intersect_clip(
            Some(clip),
            translate_rect(rect, (-delta.0, -delta.1)),
        ));
    }
    let device = |left: u32, top: u32, right: u32, bottom: u32| Rect {
        x: left as f32 / scale,
        y: top as f32 / scale,
        width: right.saturating_sub(left) as f32 / scale,
        height: bottom.saturating_sub(top) as f32 / scale,
    };
    let edge = device(moved.left, moved.top, moved.right, moved.bottom);
    repairs.extend(
        [
            Rect {
                height: edge.y - clip.y,
                ..clip
            },
            Rect {
                y: edge.bottom(),
                height: clip.bottom() - edge.bottom(),
                ..clip
            },
            Rect {
                width: edge.x - clip.x,
                ..clip
            },
            Rect {
                x: edge.right(),
                width: clip.right() - edge.right(),
                ..clip
            },
        ]
        .into_iter()
        .filter(|band| band.width > 0.0 && band.height > 0.0),
    );
    let (dx, dy) = (shift.0.unsigned_abs(), shift.1.unsigned_abs());
    if shift.1 > 0 {
        repairs.push(device(
            moved.left,
            moved.bottom - dy,
            moved.right,
            moved.bottom,
        ));
    } else if shift.1 < 0 {
        repairs.push(device(moved.left, moved.top, moved.right, moved.top + dy));
    }
    if shift.0 > 0 {
        repairs.push(device(
            moved.right - dx,
            moved.top,
            moved.right,
            moved.bottom,
        ));
    } else if shift.0 < 0 {
        repairs.push(device(moved.left, moved.top, moved.left + dx, moved.bottom));
    }
    Some(ScrollRepaint {
        moved,
        shift,
        repairs: merge_rects(repairs),
    })
}

pub(crate) fn scroll(
    canvas: &mut Canvas,
    plan: ScrollRepaint,
    document: &LayoutDocument,
    canvases: &[ImageLayer],
) {
    let ScrollRepaint {
        moved,
        shift,
        repairs,
    } = plan;
    let shown = canvas.revision.id;
    let stride = canvas.bytes_per_row();
    let (dx, dy) = (
        shift.0.unsigned_abs() as usize,
        shift.1.unsigned_abs() as usize,
    );
    let width = (moved.right - moved.left) as usize - dx;
    let (from_x, to_x) = match shift.0 > 0 {
        true => (moved.left as usize + dx, moved.left as usize),
        false => (moved.left as usize, moved.left as usize + dx),
    };
    let rows = moved.top as usize..moved.bottom as usize - dy;
    let mut copy = |row: usize| {
        let (from, to) = match shift.1 > 0 {
            true => (row + dy, row),
            false => (row, row + dy),
        };
        canvas.pixels.copy_within(
            from * stride + from_x * 4..from * stride + (from_x + width) * 4,
            to * stride + to_x * 4,
        );
    };
    // rows move up from the top down and down from the bottom up, so none is read once written
    if shift.1 > 0 {
        rows.for_each(&mut copy);
    } else {
        rows.rev().for_each(&mut copy);
    }
    let mut changed = [moved.left, moved.top, moved.right, moved.bottom];
    for rect in repairs {
        if let Some(bounds) = DeviceBounds::of(canvas, rect, None) {
            changed = [
                changed[0].min(bounds.left.saturating_sub(3)),
                changed[1].min(bounds.top.saturating_sub(3)),
                changed[2].max((bounds.right + 3).min(canvas.width)),
                changed[3].max((bounds.bottom + 3).min(canvas.height)),
            ];
        }
        let last = canvas.revision.id;
        repaint(canvas, document, canvases, rect);
        if canvas.revision.id != last && canvas.revision.changed.is_none() {
            // the repaint fell back to painting everything
            return;
        }
    }
    canvas.revision = Revision {
        id: next_revision(),
        changed: Some((shown, changed.into())),
    };
}

fn contains_rect(outer: Rect, inner: Rect) -> bool {
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner.right() <= outer.right()
        && inner.bottom() <= outer.bottom()
}

// two rects become their union where it covers little more than both, so an area paints once
// and two thin bands at right angles stay apart
fn merge_rects(mut rects: Vec<Rect>) -> Vec<Rect> {
    let area = |rect: Rect| rect.width * rect.height;
    let touch = |a: Rect, b: Rect| area(union_rect(a, b)) <= (area(a) + area(b)) * 1.25;
    let mut merged = Vec::<Rect>::new();
    while let Some(mut rect) = rects.pop() {
        while let Some(at) = merged.iter().position(|other| touch(rect, *other)) {
            rect = union_rect(rect, merged.swap_remove(at));
        }
        merged.push(rect);
    }
    merged
}

// the planes after a repaint of the kept box: one found again reaching past the box keeps the
// box it had, since only its part inside was repainted; none when a plane appeared, changed
// or went away where pixels outside the box still show the frame before, or when two overlap
fn settled_planes(before: &[Plane], found: Vec<Plane>, kept: [u32; 4]) -> Option<Vec<Plane>> {
    let inside = |bounds: [u32; 4]| {
        bounds[0] >= kept[0] && bounds[1] >= kept[1] && bounds[2] <= kept[2] && bounds[3] <= kept[3]
    };
    let lost = before.iter().any(|plane| {
        overlaps(plane.bounds, kept) && !found.iter().any(|again| again.node == plane.node)
    });
    if lost {
        return None;
    }
    let planes = found
        .into_iter()
        .map(|plane| {
            if inside(plane.bounds) {
                return Some(plane);
            }
            before
                .iter()
                .find(|old| {
                    **old
                        == Plane {
                            bounds: old.bounds,
                            ..plane.clone()
                        }
                })
                .cloned()
        })
        .collect::<Option<Vec<_>>>()?;
    let overlapping = planes.iter().enumerate().any(|(at, plane)| {
        planes[at + 1..]
            .iter()
            .any(|other| overlaps(plane.bounds, other.bounds))
    });
    (!overlapping).then_some(planes)
}

fn paint_document(
    canvas: &mut Canvas,
    document: &LayoutDocument,
    images: &[ImageLayer],
    clip: Option<Rect>,
) {
    let _profile = crate::profile::scope("paint: document");
    let _extents = memoize_extents();
    // the root background paints the whole canvas, as CSS backgrounds require; its images
    // stay with the root box
    if let Some(root) = canvas_background(&document.root) {
        let viewport = Shape::square(Rect {
            x: 0.0,
            y: 0.0,
            width: document.viewport.width,
            height: document.viewport.height,
        });
        paint_backgrounds(canvas, root, &[], viewport, viewport, root.opacity, clip);
    }
    paint_box(
        canvas,
        &document.root,
        images,
        &document.prose_selections,
        clip,
        (0.0, 0.0),
    );
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

fn paint_box(
    canvas: &mut Canvas,
    layout_box: &LayoutBox,
    images: &[ImageLayer],
    prose_selections: &[ProseSpan],
    clip: Option<Rect>,
    ancestor_offset: (f32, f32),
) {
    paint_context(
        canvas,
        layout_box,
        images,
        prose_selections,
        clip,
        ancestor_offset,
    );
}

fn paint_context(
    canvas: &mut Canvas,
    layout_box: &LayoutBox,
    images: &[ImageLayer],
    prose_selections: &[ProseSpan],
    clip: Option<Rect>,
    ancestor_offset: (f32, f32),
) {
    let opacity = layout_box.opacity;
    if opacity <= 0.0 || misses(layout_box, ancestor_offset, clip) {
        return;
    }
    if let Some(transform) = layout_box.transform.as_deref() {
        let rect = visual_rect_for(layout_box, ancestor_offset);
        // the stored transform is relative to the border box corner
        let full = transform.about(rect.x, rect.y);
        if layout_box.backface_hidden && full.shows_back() {
            return;
        }
        // an svg leaf that only moves draws its vectors at the fractional spot, as skia's matrix
        // places them, instead of resampling a layer
        if let Some(translation) = full.translation().filter(|_| {
            layout_box.svg.is_some()
                && layout_box.children.is_empty()
                && layout_box.filters.own.is_empty()
        }) {
            let device = (translation.0 * canvas.scale, translation.1 * canvas.scale);
            let whole = (device.0.round(), device.1.round());
            let moved = (whole.0 / canvas.scale, whole.1 / canvas.scale);
            let saved = (canvas.shift, canvas.nudge);
            canvas.shift = (saved.0.0 - moved.0, saved.0.1 - moved.1);
            canvas.nudge = (
                saved.1.0 + device.0 - whole.0,
                saved.1.1 + device.1 - whole.1,
            );
            paint_context_contents(
                canvas,
                layout_box,
                ContextPaint {
                    images,
                    prose_selections,
                    clip: clip.map(|clip| Rect {
                        x: clip.x - moved.0,
                        y: clip.y - moved.1,
                        ..clip
                    }),
                    ancestor_offset,
                    opacity,
                },
            );
            (canvas.shift, canvas.nudge) = saved;
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
                images,
                prose_selections,
                clip: None,
                ancestor_offset,
                opacity: 1.0,
            },
        );
        // the filter acts in the box's own space, before the transform
        filter_layer(&mut layer, &layout_box.filters.own);
        canvas.composite_transformed(layer, &full, clip, opacity);
        return;
    }
    paint_backdrop(
        canvas,
        layout_box,
        visual_rect_for(layout_box, ancestor_offset),
        clip,
        opacity,
    );
    if !layout_box.filters.own.is_empty() {
        paint_filtered(
            canvas,
            &layout_box.filters.own,
            (subtree_bounds(layout_box, ancestor_offset), clip),
            opacity,
            |layer| {
                paint_context_contents(
                    layer,
                    layout_box,
                    ContextPaint {
                        images,
                        prose_selections,
                        clip: None,
                        ancestor_offset,
                        opacity: 1.0,
                    },
                );
            },
        );
        return;
    }
    // opacity fades the painted subtree as one image, so overlapping descendants do not
    // show through each other
    if opacity < 1.0 && !fades_without_layer(layout_box) {
        // the layer spans the whole subtree whatever the clip, so where its pixels land never
        // depends on how much of it a frame repaints
        let subtree = subtree_bounds(layout_box, ancestor_offset);
        let (Some(whole), Some(bounds)) = (
            DeviceBounds::of(canvas, subtree, None),
            DeviceBounds::of(canvas, subtree, clip),
        ) else {
            return;
        };
        let mut layer = canvas.layer(whole);
        paint_context_contents(
            &mut layer,
            layout_box,
            ContextPaint {
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
            images,
            prose_selections,
            clip,
            ancestor_offset,
            opacity,
        },
    );
}

// a subtree whose ink lies outside the clip paints nothing a repaint keeps
fn misses(layout_box: &LayoutBox, ancestor_offset: (f32, f32), clip: Option<Rect>) -> bool {
    clip.zip(ink(layout_box, ancestor_offset))
        .is_some_and(|(clip, ink)| intersect_clip(Some(clip), ink).is_none())
}

fn filter_layer(layer: &mut Canvas, filters: &[crate::filter::Filter]) {
    let size = (layer.width as usize, layer.height as usize);
    crate::filter::apply(filters, &mut layer.pixels, size, layer.scale, false);
}

// paints into a layer that leaves the filter room to spread, filters it, and draws it inside
// the clip; `bounds` is everything the paint and its filter can cover
fn paint_filtered(
    canvas: &mut Canvas,
    filters: &[crate::filter::Filter],
    (bounds, clip): (Rect, Option<Rect>),
    opacity: f32,
    paint: impl FnOnce(&mut Canvas),
) {
    // what lies past the surface only matters as far as the filter reaches into it; the clip
    // leaves the layer whole, so its pixels land the same however much a frame repaints
    let Some(area) = intersect_clip(
        Some(outset_rect(canvas.extent(), crate::filter::reach(filters))),
        bounds,
    ) else {
        return;
    };
    let mut layer = canvas.aligned_layer(area);
    paint(&mut layer);
    filter_layer(&mut layer, filters);
    match DeviceBounds::of(canvas, layer.extent(), clip) {
        Some(shown) => canvas.composite(layer, shown, opacity),
        None => recycle_layer(layer.pixels),
    }
}

// backdrop-filter redraws what the surface already holds behind the border box, filtered and
// cut to the rounded box, before the box paints; chromium reads the backdrop past the edges
// of the box as if mirrored there
fn paint_backdrop(
    canvas: &mut Canvas,
    layout_box: &LayoutBox,
    rect: Rect,
    clip: Option<Rect>,
    opacity: f32,
) {
    let filters = &layout_box.filters.backdrop;
    if filters.is_empty() {
        return;
    }
    let (Some(input), Some(shown)) = (
        DeviceBounds::of(canvas, rect, None),
        DeviceBounds::of(canvas, rect, clip),
    ) else {
        return;
    };
    let mut layer = canvas.layer(input);
    let row_bytes = layer.width as usize * 4;
    let stride = canvas.width as usize * 4;
    for (row, target) in layer.pixels.chunks_exact_mut(row_bytes).enumerate() {
        let start = (input.top as usize + row) * stride + input.left as usize * 4;
        target.copy_from_slice(&canvas.pixels[start..start + row_bytes]);
    }
    let size = (layer.width as usize, layer.height as usize);
    crate::filter::apply(filters, &mut layer.pixels, size, canvas.scale, true);
    let shape = canvas.device_shape(Shape::from_rect(canvas.snap_rect(rect), layout_box.radii));
    for (row, line) in layer.pixels.chunks_exact_mut(row_bytes).enumerate() {
        for (column, pixel) in line.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let center = (
                (input.left as usize + column) as f32 + 0.5,
                (input.top as usize + row) as f32 + 0.5,
            );
            let coverage = shape.coverage(center);
            if coverage < 1.0 {
                for value in pixel {
                    *value = (f32::from(*value) * coverage).round() as u8;
                }
            }
        }
    }
    canvas.composite(layer, shown, opacity);
}

struct ContextPaint<'a> {
    images: &'a [ImageLayer],
    prose_selections: &'a [ProseSpan],
    clip: Option<Rect>,
    ancestor_offset: (f32, f32),
    // the opacity this box's own surface paints with; a group layer applies it later
    opacity: f32,
}

fn paint_context_contents(canvas: &mut Canvas, layout_box: &LayoutBox, paint: ContextPaint<'_>) {
    let ContextPaint {
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
    let mask = overflow_mask(canvas, layout_box, rect);
    if let Some(mask) = mask {
        canvas.push_mask(mask);
    }
    let opaque = canvas.opaque;
    canvas.opaque &=
        !scrolls_in_own_layer(canvas, layout_box) && !floats_in_own_layer(canvas, layout_box);
    let mut contexts = stacking_context_entries(layout_box, clip, ancestor_offset);
    contexts.sort_by_key(|entry| (entry.z_index, entry.order));
    for entry in contexts.iter().filter(|entry| entry.z_index < 0) {
        paint_context_entry(canvas, entry, images, prose_selections);
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
        images,
        prose_selections,
        content_clip,
        offset,
    );
    for entry in contexts.iter().filter(|entry| entry.z_index >= 0) {
        paint_context_entry(canvas, entry, images, prose_selections);
    }
    canvas.opaque = opaque;
    if mask.is_some() {
        canvas.pop_mask();
    }
    paint_scrollbars(canvas, layout_box, images, (offset, opacity), clip);
    paint_outline(canvas, layout_box, rect, opacity, clip);
    if !layout_box.masks.is_empty() {
        canvas.pop_mask();
    }
    if layout_box.clip_path.is_some() {
        canvas.pop_mask();
    }
}

// from a device scale of 1.5 chromium composites any scroller that can scroll, opaque
// background or not, and text in a layer that is not opaque loses subpixel blending
fn scrolls_in_own_layer(canvas: &Canvas, layout_box: &LayoutBox) -> bool {
    let (x, y) = user_scroll_range(layout_box);
    canvas.scale >= 1.5
        && !layout_box.is_document()
        && layout_box.background.alpha < 255
        && (x > 0.0 || y > 0.0)
}

// chromium gives a box in the top layer a layer of its own, and a fixed box too, and text in a
// layer that is not opaque all over is drawn in grayscale: a see-through background or a shadow
// painted past the box always counts, and rounded corners do on a dense display or in the top
// layer (fixed boxes measured against webview2 157 at 1x, 1.5x and 2x)
fn floats_in_own_layer(canvas: &Canvas, layout_box: &LayoutBox) -> bool {
    let dense = canvas.scale >= 1.5;
    let shadowed = layout_box.shadows.iter().any(|shadow| {
        !shadow.inset
            && (shadow.blur > 0.0
                || shadow.spread > 0.0
                || shadow.offset_x != 0.0
                || shadow.offset_y != 0.0)
    });
    let rounded = layout_box
        .radii
        .iter()
        .any(|(across, down)| *across > 0.0 && *down > 0.0);
    (layout_box.top_layer || layout_box.position == Position::Fixed)
        && (layout_box.background.alpha < 255
            || shadowed
            || ((dense || layout_box.top_layer) && rounded))
}

fn paint_context_entry(
    canvas: &mut Canvas,
    entry: &StackingContextEntry<'_>,
    images: &[ImageLayer],
    prose_selections: &[ProseSpan],
) {
    let masks = entry
        .clippers
        .iter()
        .filter_map(|&(clipper, offset)| {
            overflow_mask(canvas, clipper, visual_rect_for(clipper, offset))
        })
        .collect::<Vec<_>>();
    for &mask in &masks {
        canvas.push_mask(mask);
    }
    let opaque = canvas.opaque;
    canvas.opaque &= !entry
        .clippers
        .iter()
        .any(|&(clipper, _)| scrolls_in_own_layer(canvas, clipper));
    if entry.layout.creates_stacking_context {
        paint_context(
            canvas,
            entry.layout,
            images,
            prose_selections,
            entry.clip,
            entry.ancestor_offset,
        );
    } else {
        paint_non_context_subtree(
            canvas,
            entry.layout,
            images,
            prose_selections,
            entry.clip,
            entry.ancestor_offset,
        );
    }
    canvas.opaque = opaque;
    for _ in &masks {
        canvas.pop_mask();
    }
}

fn paint_non_context_children(
    canvas: &mut Canvas,
    layout_box: &LayoutBox,
    images: &[ImageLayer],
    prose_selections: &[ProseSpan],
    clip: Option<Rect>,
    ancestor_offset: (f32, f32),
) {
    for index in paint_order(&layout_box.children) {
        let child = &layout_box.children[index];
        if child.creates_stacking_context || child.inline || child.paints_positioned() {
            continue;
        }
        paint_non_context_subtree(
            canvas,
            child,
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
    images: &[ImageLayer],
    prose_selections: &[ProseSpan],
    clip: Option<Rect>,
    ancestor_offset: (f32, f32),
) {
    if misses(layout_box, ancestor_offset, clip) {
        return;
    }
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
    let mask = overflow_mask(canvas, layout_box, rect);
    if let Some(mask) = mask {
        canvas.push_mask(mask);
    }
    let opaque = canvas.opaque;
    canvas.opaque &= !scrolls_in_own_layer(canvas, layout_box);
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
        images,
        prose_selections,
        content_clip,
        offset,
    );
    canvas.opaque = opaque;
    if mask.is_some() {
        canvas.pop_mask();
    }
    paint_scrollbars(
        canvas,
        layout_box,
        images,
        (offset, layout_box.opacity),
        clip,
    );
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
        // a clip that leaves nothing still clips, where none would let the children paint anywhere
        Some(
            intersect_clip(clip, translate_rect(overflow_clip_rect(layout_box), offset))
                .unwrap_or_default(),
        )
    } else {
        clip
    };
    // a hidden box paints nothing of its own but still clips what its children paint
    if !layout_box.visible {
        return content_clip;
    }
    let outer = Shape::from_rect(canvas.snap_rect(rect), layout_box.radii);
    let surface = outer.inset(layout_box.border.widths);
    for shadow in layout_box
        .shadows
        .iter()
        .rev()
        .filter(|shadow| !shadow.inset)
    {
        paint_shadow(canvas, shadow, outer, surface, opacity, clip);
    }
    if layout_box.background_clip_text {
        let area = Rect {
            x: outer.x,
            y: outer.y,
            width: outer.width,
            height: outer.height,
        };
        let bounds = DeviceBounds::of(canvas, area, clip);
        canvas.push_glyph_mask(layout_box, bounds, offset);
        paint_backgrounds(canvas, layout_box, images, outer, surface, opacity, clip);
        canvas.pop_mask();
    } else {
        paint_backgrounds(canvas, layout_box, images, outer, surface, opacity, clip);
    }
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
    if matches!(layout_box.tag.as_str(), "img" | "canvas" | "video")
        && let Some(layer) = images
            .iter()
            .find(|layer| layer.background.is_none() && layer.node == layout_box.node)
    {
        // the object fits the unsnapped box, as layout places it, before its rect snaps
        let padding_box = Shape::from_rect(rect, layout_box.radii).inset(layout_box.border.widths);
        let content = Rect {
            x: padding_box.x,
            y: padding_box.y,
            width: padding_box.width,
            height: padding_box.height,
        };
        let mut fitted = fitted_placement(
            content,
            &layer.image,
            layout_box.object_fit,
            layout_box.object_position.as_deref().copied(),
            layout_box.font_size,
        );
        if layout_box.tag == "video" {
            fitted = whole_sized(fitted, canvas.scale);
        }
        paint_image(
            canvas,
            ImageDraw {
                image: &layer.image,
                // chromium draws replaced images into their pixel-snapped rect
                placement: canvas.snap_rect(fitted),
                area: surface,
                clip: content_clip,
                opacity,
                repeat: (false, false),
                video: (layout_box.tag == "video").then_some(layout_box.node),
            },
        );
    }
    if let Some(scene) = &layout_box.svg {
        // blink translates an svg root to its pixel-snapped border box before it paints
        paint_svg(
            canvas,
            scene,
            canvas.snap_rect(rect),
            surface,
            content_clip,
            opacity,
        );
    }
    content_clip
}

// a scene rasterized over its whole viewport at one scale and subpixel offset
struct SvgRaster {
    scene: Rc<crate::svg::Scene>,
    key: [u32; 5],
    image: Rc<Image>,
    used: u64,
}

// rasters of the scenes that are costly to draw, so a repaint of a sliver of one, or the
// next frame's whole of it, copies pixels instead of filling its paths again
#[derive(Default)]
struct SvgRasters {
    entries: Vec<SvgRaster>,
    clock: u64,
}

thread_local! {
    static SVG_RASTERS: RefCell<SvgRasters> = RefCell::new(SvgRasters::default());
}

impl SvgRasters {
    // the most pixels one raster may hold, and all of them together, at four bytes each
    const LARGEST: usize = 3_000_000;
    const BUDGET: usize = 6_000_000;
    // a scene quicker to draw than this is drawn again each time
    const WORTH: std::time::Duration = std::time::Duration::from_micros(400);

    fn find(&mut self, scene: &Rc<crate::svg::Scene>, key: [u32; 5]) -> Option<Rc<Image>> {
        self.clock += 1;
        let clock = self.clock;
        let entry = self.entries.iter_mut().find(|entry| {
            entry.key == key && (Rc::ptr_eq(&entry.scene, scene) || entry.scene == *scene)
        })?;
        entry.used = clock;
        Some(entry.image.clone())
    }

    fn keep(&mut self, scene: &Rc<crate::svg::Scene>, key: [u32; 5], image: Rc<Image>) {
        let pixels = |image: &Image| image.width as usize * image.height as usize;
        let mut held: usize = self.entries.iter().map(|entry| pixels(&entry.image)).sum();
        while held + pixels(&image) > Self::BUDGET {
            let Some(oldest) =
                (0..self.entries.len()).min_by_key(|&index| self.entries[index].used)
            else {
                break;
            };
            held -= pixels(&self.entries.swap_remove(oldest).image);
        }
        self.entries.push(SvgRaster {
            scene: scene.clone(),
            key,
            image,
            used: self.clock,
        });
    }
}

// rasterizes the scene at device resolution, then draws that pixel for pixel: a scene of a
// size worth keeping is drawn whole and kept when it was slow, and a larger one is drawn over
// the part of its viewport that shows
fn paint_svg(
    canvas: &mut Canvas,
    scene: &Rc<crate::svg::Scene>,
    rect: Rect,
    area: Shape,
    clip: Option<Rect>,
    opacity: f32,
) {
    let _profile = crate::profile::scope("paint: svg");
    let (x, y, width, height) = scene.viewport;
    let viewport = Rect {
        x: rect.x + x,
        y: rect.y + y,
        width,
        height,
    };
    let Some(bounds) = intersect_clip(clip, viewport) else {
        return;
    };
    let (scale, shift, nudge) = (canvas.scale, canvas.shift, canvas.nudge);
    let device = |bounds: Rect| {
        (
            ((bounds.x - shift.0) * scale + nudge.0).floor(),
            ((bounds.y - shift.1) * scale + nudge.1).floor(),
            ((bounds.x + bounds.width - shift.0) * scale + nudge.0).ceil(),
            ((bounds.y + bounds.height - shift.1) * scale + nudge.1).ceil(),
        )
    };
    let origin = (
        (rect.x - shift.0) * scale + nudge.0,
        (rect.y - shift.1) * scale + nudge.1,
    );
    let draw =
        |canvas: &mut Canvas, image: &Image, (left, top, right, bottom): (f32, f32, f32, f32)| {
            paint_image(
                canvas,
                ImageDraw {
                    image,
                    placement: Rect {
                        x: left / scale + shift.0,
                        y: top / scale + shift.1,
                        width: (right - left) / scale,
                        height: (bottom - top) / scale,
                    },
                    area,
                    clip,
                    opacity,
                    repeat: (false, false),
                    video: None,
                },
            );
        };
    let rasterize = |(left, top, right, bottom): (f32, f32, f32, f32)| {
        let mut image = Image::new((right - left) as u32, (bottom - top) as u32);
        crate::svg::rasterize(
            scene,
            &mut image,
            [scale, 0.0, 0.0, scale, origin.0 - left, origin.1 - top],
        );
        image
    };
    let whole = device(viewport);
    let (whole_width, whole_height) = (whole.2 - whole.0, whole.3 - whole.1);
    if whole_width >= 1.0
        && whole_height >= 1.0
        && whole_width * whole_height <= SvgRasters::LARGEST as f32
    {
        // the same scene at the same scale and fraction of a pixel rasterizes the same
        let key = [
            scale.to_bits(),
            (origin.0 - whole.0).to_bits(),
            (origin.1 - whole.1).to_bits(),
            whole_width as u32,
            whole_height as u32,
        ];
        let kept = SVG_RASTERS.with(|rasters| rasters.borrow_mut().find(scene, key));
        let image = kept.unwrap_or_else(|| {
            let started = std::time::Instant::now();
            let image = Rc::new(rasterize(whole));
            if started.elapsed() >= SvgRasters::WORTH {
                SVG_RASTERS.with(|rasters| rasters.borrow_mut().keep(scene, key, image.clone()));
            }
            image
        });
        draw(canvas, &image, whole);
        return;
    }
    let shown = device(bounds);
    let shown = (
        shown.0.max(0.0),
        shown.1.max(0.0),
        shown.2.min(canvas.width as f32),
        shown.3.min(canvas.height as f32),
    );
    if shown.2 <= shown.0 || shown.3 <= shown.1 {
        return;
    }
    draw(canvas, &rasterize(shown), shown);
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
                        video: None,
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
pub(crate) fn width_of_characters(run: &TextRun, characters: usize) -> f32 {
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

pub(crate) fn utf16_char_offset(value: &str, offset: usize) -> usize {
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

fn paint_scrollbars(
    canvas: &mut Canvas,
    layout_box: &LayoutBox,
    images: &[ImageLayer],
    (offset, opacity): ((f32, f32), f32),
    clip: Option<Rect>,
) {
    if !layout_box.visible {
        return;
    }
    let metrics = scrollbar_metrics(layout_box);
    let bars = [
        (metrics.vertical_track, true),
        (metrics.horizontal_track, false),
    ];
    let corner = match bars {
        [(Some(vertical), _), (Some(horizontal), _)] => Some(translate_rect(
            Rect {
                x: vertical.x,
                y: horizontal.y,
                width: vertical.width,
                height: horizontal.height,
            },
            offset,
        )),
        _ => None,
    };
    let placed = |vertical: bool| {
        thumb_placement(layout_box, &metrics, vertical).map(|placement| ThumbPlacement {
            region: translate_rect(placement.region, offset),
            thumb: translate_rect(placement.thumb, offset),
            ..placement
        })
    };
    let (thumb_color, track_color) = match &layout_box.scrollbar {
        Scrollbar::Hidden => return,
        Scrollbar::Styled(parts) => {
            let mut surface = |pseudo: PseudoElement, style: &Style, rect: Rect| {
                let part = part_box(
                    layout_box.node,
                    pseudo,
                    style,
                    rect,
                    (layout_box.font_size, layout_box.cursor),
                );
                // chromium draws scrollbar parts without their filter
                paint_box_surface(
                    canvas,
                    &part,
                    images,
                    clip,
                    (0.0, 0.0),
                    rect,
                    opacity * part.opacity,
                );
            };
            for (track, vertical) in bars {
                let Some(track) = track.map(|track| translate_rect(track, offset)) else {
                    continue;
                };
                surface(PseudoElement::Scrollbar, &parts.bar, track);
                if let Some(style) = &parts.track {
                    surface(PseudoElement::ScrollbarTrack, style, track);
                }
                if let (Some(style), Some(placement)) = (&parts.thumb, placed(vertical)) {
                    surface(PseudoElement::ScrollbarThumb, style, placement.thumb);
                }
            }
            match (corner, &parts.corner) {
                (Some(corner), Some(style)) => {
                    surface(PseudoElement::ScrollbarCorner, style, corner);
                }
                (Some(corner), None) => {
                    canvas.fill_shape(
                        Shape::square(corner),
                        &Paint::Solid(Color::WHITE.scale_alpha(opacity)),
                        clip,
                    );
                }
                (None, _) => {}
            }
            return;
        }
        Scrollbar::Standard {
            colors: Some(colors),
            ..
        } => *colors,
        Scrollbar::Native | Scrollbar::Standard { .. } => {
            (Color::opaque(139, 139, 139), Color::opaque(252, 252, 252))
        }
    };
    let solid = |color: Color| Paint::Solid(color.scale_alpha(opacity));
    for (track, vertical) in bars {
        let (Some(track), Some(placement)) = (track, placed(vertical)) else {
            continue;
        };
        let track = translate_rect(track, offset);
        canvas.fill_shape(Shape::square(track), &solid(track_color), clip);
        // fluent arrows: a triangle in each end button pointing out of the bar
        let (thickness, start, end) = if vertical {
            (track.width, track.y, track.y + track.height)
        } else {
            (track.height, track.x, track.x + track.width)
        };
        let middle = if vertical {
            track.x + thickness / 2.0
        } else {
            track.y + thickness / 2.0
        };
        let button = placement.button;
        for (apex, base) in [
            (start + 0.4 * button, start + 0.8 * button),
            (end - 0.4 * button, end - 0.8 * button),
        ] {
            fill_arrow(
                canvas,
                (apex, base),
                (middle, 0.3 * thickness),
                vertical,
                &solid(thumb_color),
                clip,
            );
        }
        let thumb = placement.thumb;
        let radius = thumb.width.min(thumb.height) / 2.0;
        canvas.fill_shape(
            Shape::from_rect(thumb, [(radius, radius); 4]),
            &solid(thumb_color),
            clip,
        );
    }
    if let Some(corner) = corner {
        canvas.fill_shape(Shape::square(corner), &solid(track_color), clip);
    }
}

// a triangle from an apex to a base centered across the bar, built from one-pixel slabs whose
// fractional ends antialias its slopes
fn fill_arrow(
    canvas: &mut Canvas,
    (apex, base): (f32, f32),
    (middle, half_width): (f32, f32),
    vertical: bool,
    paint: &Paint,
    clip: Option<Rect>,
) {
    let height = (base - apex).abs();
    if height <= 0.0 {
        return;
    }
    let (low, high) = (apex.min(base), apex.max(base));
    let mut along = low.floor();
    while along < high {
        let (from, to) = (along.max(low), (along + 1.0).min(high));
        let spread = half_width * ((from + to) / 2.0 - apex).abs() / height;
        let slab = if vertical {
            Rect {
                x: middle - spread,
                y: from,
                width: 2.0 * spread,
                height: to - from,
            }
        } else {
            Rect {
                x: from,
                y: middle - spread,
                width: to - from,
                height: 2.0 * spread,
            }
        };
        canvas.fill_shape(Shape::square(slab), paint, clip);
        along += 1.0;
    }
}

// an image mapped onto `placement` in layout space and painted only inside `area`,
// tiled across the area along the axes that repeat; a video's frame may become a plane
struct ImageDraw<'a> {
    image: &'a Image,
    placement: Rect,
    area: Shape,
    clip: Option<Rect>,
    opacity: f32,
    repeat: (bool, bool),
    video: Option<crate::dom::NodeId>,
}

fn paint_image(canvas: &mut Canvas, draw: ImageDraw<'_>) {
    let _profile = crate::profile::scope("paint: image");
    let ImageDraw {
        image,
        placement,
        area,
        clip,
        opacity,
        repeat,
        video,
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
    let device = [left, top, right, bottom];
    let plain = !rounded && opacity >= 1.0 && canvas.masks.is_empty();
    // a plane goes under its box, which the opaque canvas gives up as far as the frame would
    // cover it, so what is drawn over blends as over a painted frame; planes never overlap,
    // which keeps their order out of the presenter
    if let Some(node) = video
        && let Some(planes) = &mut canvas.planes
        && !planes
            .iter()
            .any(|plane| plane.node != node && overlaps(plane.bounds, device))
    {
        planes.retain(|plane| plane.node != node);
        if left >= right || top >= bottom {
            return;
        }
        planes.push(Plane {
            node,
            size: (image.width, image.height),
            bounds: device,
            origin,
            scale: (1.0 / step.0, 1.0 / step.1),
        });
        let stride = canvas.width as usize * 4;
        for row in top..bottom {
            let start = row as usize * stride;
            let pixels = &mut canvas.pixels[start + left as usize * 4..start + right as usize * 4];
            if plain {
                pixels.fill(0);
                continue;
            }
            for (column, pixel) in (left..right).zip(pixels.as_chunks_mut::<4>().0) {
                let point = (column as f32 + 0.5, row as f32 + 0.5);
                let shape = if rounded {
                    area_device.coverage(point)
                } else {
                    1.0
                };
                let cover = opacity * shape * mask_coverage(&canvas.masks, point);
                let keep = 255 - (cover.clamp(0.0, 1.0) * 255.0).round() as u32;
                for channel in pixel {
                    *channel = (u32::from(*channel) * keep / 255) as u8;
                }
            }
        }
        return;
    }
    // a frame left on the gpu is read back and painted once paint finds it is no plane
    if image.pixels.len() < image.width as usize * image.height as usize {
        return;
    }
    // whole-pixel placement at the image's own resolution copies pixels directly
    let aligned = (step.0 - 1.0).abs() < 1e-4
        && (step.1 - 1.0).abs() < 1e-4
        && origin.0.fract() == 0.0
        && origin.1.fract() == 0.0;
    // the taps along each axis are worked out once, so a frame costs its blends alone
    let taps = |pixel: u32, origin: f32, step: f32, size: u32, repeat: bool| {
        if aligned {
            wrap_or_none(pixel as f32 - origin, size, repeat).map(|at| Tap {
                near: at,
                far: at,
                fraction: 0.0,
            })
        } else {
            Some(Tap::at(
                (pixel as f32 + 0.5 - origin) * step - 0.5,
                size,
                repeat,
            ))
        }
    };
    let shape = rounded.then_some(&area_device);
    // a picture shrunk to half or less is drawn from the smallest mip level still as large as
    // the draw, bilinear from there, as chromium's image decode cache uploads it
    let level = step.0.min(step.1).log2().floor();
    if !aligned && video.is_none() && level >= 1.0 {
        let mip = image.level(level as usize);
        let ratio = (
            mip.width as f32 / image.width as f32,
            mip.height as f32 / image.height as f32,
        );
        let columns = (left..right)
            .map(|pixel_x| {
                Tap::at(
                    (pixel_x as f32 + 0.5 - origin.0) * step.0 * ratio.0 - 0.5,
                    mip.width,
                    repeat.0,
                )
            })
            .collect::<Vec<_>>();
        for pixel_y in top..bottom {
            let row = Tap::at(
                (pixel_y as f32 + 0.5 - origin.1) * step.1 * ratio.1 - 0.5,
                mip.height,
                repeat.1,
            );
            for (pixel_x, column) in (left..right).zip(&columns) {
                let color = blend_taps(mip, *column, row);
                put_image_pixel(canvas, (pixel_x, pixel_y), color, plain, opacity, shape);
            }
        }
        return;
    }
    let columns = (left..right)
        .map(|pixel_x| taps(pixel_x, origin.0, step.0, image.width, repeat.0))
        .collect::<Vec<_>>();
    for pixel_y in top..bottom {
        let Some(row) = taps(pixel_y, origin.1, step.1, image.height, repeat.1) else {
            continue;
        };
        for (pixel_x, column) in (left..right).zip(&columns) {
            let Some(column) = *column else {
                continue;
            };
            let color = blend_taps(image, column, row);
            put_image_pixel(canvas, (pixel_x, pixel_y), color, plain, opacity, shape);
        }
    }
}

// with nothing to scale a pixel by, an opaque one is stored as it is
fn put_image_pixel(
    canvas: &mut Canvas,
    (pixel_x, pixel_y): (u32, u32),
    color: Color,
    plain: bool,
    opacity: f32,
    shape: Option<&Shape>,
) {
    if plain && color.alpha == 255 {
        let at = pixel_y as usize * canvas.bytes_per_row() + pixel_x as usize * 4;
        canvas.pixels[at..at + 4].copy_from_slice(&[color.red, color.green, color.blue, 255]);
        return;
    }
    let coverage = shape.map_or(1.0, |shape| {
        shape.coverage((pixel_x as f32 + 0.5, pixel_y as f32 + 0.5))
    });
    if coverage <= 0.0 || color.alpha == 0 {
        return;
    }
    let factor = opacity * coverage;
    let color = if factor >= 1.0 {
        color
    } else {
        color.scale_alpha(factor)
    };
    canvas.blend_pixel(pixel_x, pixel_y, color);
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

// the two texels either side of a sample position along one axis, and how far past the near
// one it lies
#[derive(Clone, Copy)]
struct Tap {
    near: u32,
    far: u32,
    fraction: f32,
}

impl Tap {
    fn at(position: f32, size: u32, repeat: bool) -> Self {
        let floor = position.floor();
        let index = |position: f32| -> u32 {
            let position = position as i64;
            if repeat {
                position.rem_euclid(i64::from(size)) as u32
            } else {
                position.clamp(0, i64::from(size) - 1) as u32
            }
        };
        Self {
            near: index(floor),
            far: index(floor + 1.0),
            fraction: position - floor,
        }
    }

    // the far texel's weight in 256ths, the subtexel precision of GPU texture filtering
    fn weight(self) -> u32 {
        (self.fraction * 256.0 + 0.5) as u32
    }
}

// interpolates in premultiplied alpha so transparent neighbours do not darken edges
fn blend_taps(image: &Image, x: Tap, y: Tap) -> Color {
    let texels = [
        (x.near, y.near),
        (x.far, y.near),
        (x.near, y.far),
        (x.far, y.far),
    ]
    .map(|(column, row)| image.pixel(column, row));
    // opaque texels need no premultiplying, which is every texel of a video frame or a photo
    if texels.iter().all(|texel| texel.alpha == 255) {
        let (along, down) = (x.weight(), y.weight());
        let weights = [
            (256 - along) * (256 - down),
            along * (256 - down),
            (256 - along) * down,
            along * down,
        ];
        let mut sum = [1_u32 << 15; 3];
        for (texel, weight) in texels.iter().zip(weights) {
            sum[0] += u32::from(texel.red) * weight;
            sum[1] += u32::from(texel.green) * weight;
            sum[2] += u32::from(texel.blue) * weight;
        }
        let [red, green, blue] = sum.map(|sum| (sum >> 16) as u8);
        return Color::opaque(red, green, blue);
    }
    let weights = [
        (1.0 - x.fraction) * (1.0 - y.fraction),
        x.fraction * (1.0 - y.fraction),
        (1.0 - x.fraction) * y.fraction,
        x.fraction * y.fraction,
    ];
    let mut sum = [0.0_f32; 4];
    for (color, weight) in texels.into_iter().zip(weights) {
        if weight == 0.0 {
            continue;
        }
        let alpha = f32::from(color.alpha) * weight;
        sum[0] += f32::from(color.red) * alpha;
        sum[1] += f32::from(color.green) * alpha;
        sum[2] += f32::from(color.blue) * alpha;
        sum[3] += alpha;
    }
    if sum[3] <= 0.0 {
        return Color::transparent();
    }
    let channel = |value: f32| round_byte(value / sum[3]);
    Color::rgba(
        channel(sum[0]),
        channel(sum[1]),
        channel(sum[2]),
        round_byte(sum[3]),
    )
}

// f32::round is a library call on baseline x86-64, and this runs per pixel per channel
fn round_byte(value: f32) -> u8 {
    let value = value.clamp(0.0, 255.0);
    let whole = value as u8;
    whole + u8::from(value - f32::from(whole) >= 0.5)
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

// chromium rounds the size of a video's fitted rect to whole device pixels and keeps its
// offset, before the rect snaps, so a frame is as wide wherever a fraction puts the box
fn whole_sized(rect: Rect, scale: f32) -> Rect {
    Rect {
        width: (rect.width * scale).round() / scale,
        height: (rect.height * scale).round() / scale,
        ..rect
    }
}

// where object-fit puts the image inside the content box and object-position moves it
// blink's object-fit in 1/64 px layout units: the constrained side by integer muldiv and the
// centring offset truncated, so the rect snaps to the pixels chromium paints
fn fitted_placement(
    content: Rect,
    image: &Image,
    fit: ObjectFit,
    position: Option<(crate::css::Length, crate::css::Length)>,
    font_size: f32,
) -> Rect {
    let scale = crate::font::device_scale();
    let unit = |value: f32| (value * scale * 64.0) as i64;
    let natural = (unit(image.width as f32), unit(image.height as f32));
    let size = (unit(content.width), unit(content.height));
    let fitted = |grow: bool| {
        let height = size.0 * natural.1 / natural.0;
        if (grow && height < size.1) || (!grow && height > size.1) {
            (size.1 * natural.0 / natural.1, size.1)
        } else {
            (size.0, height)
        }
    };
    let (width, height) = match fit {
        _ if natural.0 == 0 || natural.1 == 0 => return content,
        ObjectFit::Fill => return content,
        ObjectFit::Contain => fitted(false),
        ObjectFit::Cover => fitted(true),
        ObjectFit::None => natural,
        ObjectFit::ScaleDown if fitted(false).0 > natural.0 => natural,
        ObjectFit::ScaleDown => fitted(false),
    };
    let centre = |space: i64, object: i64| ((space - object) as f32 / 64.0 * 0.5 * 64.0) as i64;
    let pixels = |raw: i64| raw as f32 / 64.0 / scale;
    // a percentage places the same point of the picture and the box on each other
    let (x, y) = match position {
        Some((x, y)) => (
            x.resolve(pixels(size.0 - width), font_size).unwrap_or(0.0),
            y.resolve(pixels(size.1 - height), font_size).unwrap_or(0.0),
        ),
        None => (
            pixels(centre(size.0, width)),
            pixels(centre(size.1, height)),
        ),
    };
    Rect {
        x: content.x + x,
        y: content.y + y,
        width: pixels(width),
        height: pixels(height),
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
    let _profile = crate::profile::scope("paint: text");
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
    // the blur reads past the clip, so the layer holds the whole shadow
    let (Some(whole), Some(bounds)) = (
        DeviceBounds::of(canvas, area, None),
        DeviceBounds::of(canvas, area, clip),
    ) else {
        return;
    };
    let mut layer = canvas.layer(whole);
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

// blink holds positions in 1/64 device pixel layout units, so a half pixel is exact and rounds
// up where float error would land just under it
fn layout_units(device: f32) -> f32 {
    (device * 64.0).round() / 64.0
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
    let baseline = layout_units(
        (run.y + offset.1 - shift.1 + fonts.ascent(run.font_stack, run.font_size)) * scale,
    );
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
            fonts.text_glyph(face, glyph.glyph, device_size, x, stroke * scale)
        {
            canvas.blend_coverage(
                x.floor() as i32 + bitmap.left,
                baseline.round() as i32 + bitmap.top,
                &bitmap,
                run.color,
                device_clip,
                device_size <= 48.0,
            );
        }
    }
}

// skia/BUILD.gn blends text in srgb with contrast 1 on windows and with gamma 1.2 and contrast
// 0.2 on linux
const TEXT_GAMMA: (f32, f32) = if cfg!(windows) {
    (0.0, 1.0)
} else {
    (1.2, 0.2)
};

// the coverage correction chromium's skia applies to text, one table per eighth of the text colour's luminance, so light text on dark keeps its weight
fn text_gamma_table(color: Color) -> &'static [u8; 256] {
    // skia's integer luminance of the gamma-encoded channels
    let luminance =
        (u32::from(color.red) * 54 + u32::from(color.green) * 183 + u32::from(color.blue) * 19)
            >> 8;
    text_gamma(luminance as u8)
}

// the table for one luminance, or for one channel of lcd text
fn text_gamma(luminance: u8) -> &'static [u8; 256] {
    static TABLES: std::sync::OnceLock<[[u8; 256]; 8]> = std::sync::OnceLock::new();
    let tables = TABLES.get_or_init(|| {
        std::array::from_fn(|level| {
            let level = level as u32;
            correcting_table(
                ((level << 5) | (level << 2) | (level >> 1)) as f32 / 255.0,
                TEXT_GAMMA,
            )
        })
    });
    &tables[usize::from(luminance >> 5)]
}

fn correcting_table(src: f32, (gamma, contrast): (f32, f32)) -> [u8; 256] {
    // a gamma of zero stands for the srgb curve, as in skia
    let to_luma = |value: f32| {
        if gamma == 0.0 {
            srgb_to_linear(value)
        } else {
            value.powf(gamma)
        }
    };
    let from_luma = |value: f32| {
        if gamma == 0.0 {
            linear_to_srgb(value)
        } else {
            value.powf(1.0 / gamma)
        }
    };
    let dst = 1.0 - src;
    let (linear_src, linear_dst) = (to_luma(src), to_luma(dst));
    // contrast fades out as the text approaches white
    let contrast = contrast * linear_dst;
    std::array::from_fn(|index| {
        let raw = index as f32 / 255.0;
        let alpha = raw + (1.0 - raw) * contrast * raw;
        let result = if (src - dst).abs() < 1.0 / 256.0 {
            alpha
        } else {
            let out = from_luma(linear_src * alpha + (1.0 - alpha) * linear_dst);
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
    #[test]
    fn a_video_frame_takes_a_whole_number_of_device_pixels_from_its_fitted_offset() {
        // a 16:9 frame contained in a 3464 by 1948 box at 56, 78
        let fitted = super::Rect {
            x: 56.444,
            y: 78.0,
            width: 3463.111,
            height: 1948.0,
        };
        let sized = super::whole_sized(fitted, 1.0);
        assert_eq!(
            (sized.x, sized.width, sized.height),
            (56.444, 3463.0, 1948.0)
        );
        // the right edge then snaps to 3519, where snapping the fitted rect gave 3520
        assert_eq!((sized.x + sized.width).round(), 3519.0);
        assert_eq!((fitted.x + fitted.width).round(), 3520.0);
        let dense = super::whole_sized(fitted, 1.5);
        assert_eq!(dense.width * 1.5, 5195.0);
    }

    use super::*;

    // how much of a pixel a shape covers once blurred by a gaussian of deviation `sigma`, in
    // device pixels; a blurred edge is the error function of the distance to it, exact axis by
    // axis for a square shape, and an unblurred square covers each pixel by its overlap
    fn shadow_coverage(shape: Shape, sigma: f32, point: (f32, f32)) -> f32 {
        let sharp = sigma < 0.5;
        let scale = sigma * std::f32::consts::SQRT_2;
        if shape.is_square() {
            let axis = |start: f32, length: f32, at: f32| {
                if length <= 0.0 {
                    0.0
                } else if sharp {
                    ((start + length).min(at + 0.5) - start.max(at - 0.5)).clamp(0.0, 1.0)
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

    // a shadow filled a row at a time must come out as it does asking every pixel for the
    // blurred coverage on its own
    #[test]
    fn shadows_fill_by_rows_as_they_do_pixel_by_pixel() {
        fn fill_each_pixel(canvas: &mut Canvas, shadow: &ShadowFill, clip: Option<Rect>) {
            let shape = canvas.device_shape(shadow.shape);
            let frame = canvas.device_shape(shadow.frame);
            let clip = clip.map(|clip| canvas.device_shape(Shape::square(clip)));
            let sigma = shadow.blur * canvas.scale / 2.0;
            // a shadow is drawn as far as its blur radius and no further
            let reach = if shadow.inset {
                frame
            } else {
                shape.outset(shadow.blur * canvas.scale)
            };
            for pixel_y in 0..canvas.height {
                for pixel_x in 0..canvas.width {
                    let center = (pixel_x as f32 + 0.5, pixel_y as f32 + 0.5);
                    let reached = pixel_x as f32 >= reach.x.floor()
                        && (pixel_x as f32) < (reach.x + reach.width).ceil()
                        && pixel_y as f32 >= reach.y.floor()
                        && (pixel_y as f32) < (reach.y + reach.height).ceil();
                    if !reached || clip.is_some_and(|clip| clip.coverage(center) <= 0.0) {
                        continue;
                    }
                    let inside = shadow_coverage(frame, 0.0, center);
                    let covered = shadow_coverage(shape, sigma, center);
                    let alpha = if shadow.inset {
                        inside * (1.0 - covered)
                    } else {
                        (1.0 - inside) * covered
                    };
                    if alpha > 0.0 {
                        let color = shadow.color.scale_alpha(alpha.min(1.0));
                        canvas.blend_pixel(pixel_x, pixel_y, color);
                    }
                }
            }
        }
        let rounded = [(9.0, 9.0), (2.0, 14.0), (0.0, 0.0), (20.0, 6.0)];
        let boxes = [
            (SQUARE_CORNERS, (12.3, 9.7, 40.4, 30.2)),
            (rounded, (12.3, 9.7, 40.4, 30.2)),
            ([(40.0, 40.0); 4], (20.0, 12.0, 24.0, 24.0)),
            (rounded, (-5.0, 30.0, 30.0, 40.0)),
        ];
        let clips = [
            None,
            Some(Rect {
                x: 9.3,
                y: 4.1,
                width: 33.4,
                height: 27.9,
            }),
        ];
        for (radii, (x, y, width, height)) in boxes {
            for blur in [0.0, 0.4, 3.0, 14.0] {
                for inset in [false, true] {
                    for clip in clips {
                        let frame = Shape {
                            x,
                            y,
                            width,
                            height,
                            radii,
                        };
                        let shadow = |spread: f32| ShadowFill {
                            shape: Shape {
                                x: x + 2.0,
                                y: y + 3.0,
                                ..frame.outset(spread)
                            },
                            blur,
                            frame,
                            inset,
                            color: Color::rgba(10, 20, 30, 200),
                        };
                        let spread = if inset { -2.5 } else { 1.5 };
                        let mut by_rows = Canvas::scaled(66.0, 60.0, 1.5, Color::WHITE);
                        let mut by_pixels = Canvas::scaled(66.0, 60.0, 1.5, Color::WHITE);
                        by_rows.fill_shadow(shadow(spread), clip);
                        fill_each_pixel(&mut by_pixels, &shadow(spread), clip);
                        assert!(
                            by_rows.pixels == by_pixels.pixels,
                            "{frame:?} blurred {blur} inset {inset} in {clip:?}"
                        );
                    }
                }
            }
        }
    }

    // a ramp's table keeps every pixel within a level of its exact color, and a hard stop
    // exactly where it falls
    #[test]
    fn a_tabled_ramp_matches_the_exact_one_to_a_level() {
        let ramp = |repeating: bool| Ramp {
            corner: (4.0, 2.0),
            scale: 1.5,
            size: (300.0, 200.0),
            repeat: (false, false),
            shape: RampShape::Radial((120.0, 60.0), (150.0, 90.0)),
            repeating,
            stops: vec![
                (0.0, [250.0, 240.0, 230.0, 1.0]),
                (0.5317, [10.0, 120.0, 40.0, 0.7]),
                (0.5317, [200.0, 0.0, 90.0, 0.9]),
                (0.8, [0.0, 0.0, 0.0, 0.0]),
            ],
            reach: 225.0,
            table: std::cell::OnceCell::new(),
        };
        for repeating in [false, true] {
            let ramp = ramp(repeating);
            for y in [2.5, 60.25, 93.0, 150.5] {
                // finer than the table, so some points fall in the step a stop splits
                for x in (0..45_000).map(|x| x as f32 / 100.0) {
                    let (exact, tabled) = (ramp.color_at((x, y)), ramp.tabled_color_at((x, y)));
                    let apart = [
                        exact.red.abs_diff(tabled.red),
                        exact.green.abs_diff(tabled.green),
                        exact.blue.abs_diff(tabled.blue),
                        exact.alpha.abs_diff(tabled.alpha),
                    ];
                    assert!(
                        apart.iter().all(|&apart| apart <= 1),
                        "at {x}, {y}: {exact:?} {tabled:?}"
                    );
                }
            }
            assert!(ramp.table.get().is_some_and(Option::is_some));
        }
    }

    // a shape filled by whole runs and by steady rows or columns of its paint must come out
    // as it does asking every pixel for its coverage and color
    #[test]
    fn shapes_fill_by_runs_as_they_do_pixel_by_pixel() {
        fn fill_each_pixel(canvas: &mut Canvas, shape: Shape, paint: &Paint, clip: Option<Rect>) {
            let device = canvas.device_shape(shape);
            let clip = clip.map(|clip| canvas.device_shape(Shape::square(clip)));
            for pixel_y in 0..canvas.height {
                for pixel_x in 0..canvas.width {
                    let center = (pixel_x as f32 + 0.5, pixel_y as f32 + 0.5);
                    let coverage = device.coverage(center);
                    let color = paint.pixel_color(paint.steady(), center);
                    if clip.is_some_and(|clip| clip.coverage(center) <= 0.0)
                        || coverage <= 0.0
                        || color.alpha == 0
                    {
                        continue;
                    }
                    canvas.blend_pixel(pixel_x, pixel_y, color.scale_alpha(coverage.min(1.0)));
                }
            }
        }
        let scale = 1.5;
        let ramp = |axis: (f32, f32), repeat: (bool, bool)| {
            Paint::Ramp(Ramp {
                corner: (9.0, 6.0),
                scale,
                size: (40.0, 30.0),
                repeat,
                shape: RampShape::Linear((0.0, 0.0), axis),
                repeating: false,
                stops: vec![
                    (0.0, [200.0, 30.0, 10.0, 1.0]),
                    (0.6, [5.0, 60.0, 110.0, 0.5]),
                    (1.0, [0.0, 0.0, 0.0, 0.0]),
                ],
                reach: 60.0,
                table: std::cell::OnceCell::new(),
            })
        };
        let paints = [
            Paint::Solid(Color::rgba(20, 90, 200, 255)),
            Paint::Solid(Color::rgba(20, 90, 200, 120)),
            ramp((0.0, 1.0 / 30.0), (false, false)),
            ramp((1.0 / 40.0, 0.0), (false, false)),
            ramp((1.0 / 40.0, 0.0), (true, true)),
            ramp((0.02, 0.02), (false, true)),
        ];
        let shapes = [
            ([(8.0, 8.0); 4], (3.3, 2.7, 50.5, 37.2)),
            (
                [(30.0, 12.0), (0.0, 0.0), (4.0, 20.0), (60.0, 60.0)],
                (0.0, 0.0, 44.0, 41.0),
            ),
            ([(3.0, 3.0); 4], (10.2, 10.6, 0.8, 20.0)),
            ([(2.0, 2.0); 4], (-6.0, -4.0, 30.0, 70.0)),
            // every edge 0.45 of a device pixel past a pixel center
            ([(4.0, 4.0); 4], (4.7, 4.7, 30.6, 22.6)),
        ];
        let clips = [
            None,
            Some(Rect {
                x: 7.3,
                y: 5.1,
                width: 21.4,
                height: 17.9,
            }),
        ];
        for paint in &paints {
            for (radii, (x, y, width, height)) in shapes {
                for clip in clips {
                    let shape = Shape {
                        x,
                        y,
                        width,
                        height,
                        radii,
                    };
                    let mut by_runs = Canvas::scaled(60.0, 44.0, scale, Color::WHITE);
                    let mut by_pixels = Canvas::scaled(60.0, 44.0, scale, Color::WHITE);
                    by_runs.fill_shape(shape, paint, clip);
                    fill_each_pixel(&mut by_pixels, shape, paint, clip);
                    assert!(by_runs.pixels == by_pixels.pixels, "{shape:?} in {clip:?}");
                }
            }
        }
    }

    #[test]
    fn text_coverage_thickens_light_text_more_than_dark_text() {
        let white = text_gamma_table(Color::WHITE);
        let black = text_gamma_table(Color::BLACK);
        for table in [white, black] {
            assert_eq!((table[0], table[255]), (0, 255));
            assert!(table.windows(2).all(|pair| pair[0] <= pair[1]));
        }
        // white on black follows the gamma curve while contrast keeps black on white near linear
        let expected = if cfg!(windows) {
            (188, 119)
        } else {
            (144, 124)
        };
        assert_eq!((white[128], black[128]), expected);
    }

    const DAMAGE_PAGE: &str = concat!(
        "<html><body><main><div id='dot'></div><p id='words'>Recording now, 02:42</p>",
        "<section id='list'><div class='row'>alpha</div><div class='row'>beta</div>",
        "<div class='row' id='far'>gamma</div></section><div id='turned'>turned</div>",
        "<div id='soft'></div><input id='field' value='clip name'>",
        "<div id='fade'>fading</div></main></body></html>",
    );

    const DAMAGE_STYLE: &str = concat!(
        "html, body { margin: 0; background: #121315; color: #e8e8e8; font: 13px sans-serif }",
        "main { padding: 13px }",
        "#dot { width: 9px; height: 9px; border-radius: 50%; background: #ff4d4d; margin: 7px }",
        "#list { height: 41px; overflow: auto; border: 1px solid #333; border-radius: 6px }",
        ".row { height: 18px; padding: 0 5px; background: linear-gradient(#202124, #2a2b2e) }",
        ".row { text-shadow: 0 1px 3px #000 } .row:first-child { opacity: 0.8 }",
        "#turned { width: 60px; transform: rotate(8deg); background: #335; margin: 9px 21px }",
        "#soft { width: 30px; height: 12px; background: #4a8; filter: blur(2px); margin: 3px }",
        "#field { outline: 2px solid #8af; outline-offset: 1px; margin: 5px }",
        "#fade { opacity: 0.6; box-shadow: 0 1px 3px #000 }",
    );

    fn damage_layout(extra: &str, scrolled: f32) -> LayoutDocument {
        let document = Document::parse_markup(DAMAGE_PAGE).expect("document should parse");
        let mut state = crate::css::StyleState::default();
        let list = document
            .find_element_by_id("list")
            .expect("the list exists");
        state.scroll_offsets.insert(list, (0.0, scrolled));
        let stylesheet =
            Stylesheet::parse(&format!("{DAMAGE_STYLE}{extra}")).expect("stylesheet should parse");
        crate::layout::layout_with_state(
            &document,
            &stylesheet,
            Size {
                width: 157.0,
                height: 233.0,
            },
            &state,
        )
    }

    #[test]
    fn repainting_the_damage_matches_a_whole_frame() {
        let frames = [
            (
                "#dot { box-shadow: 0 0 0 2px rgba(255, 77, 77, 0.2) }",
                "#dot { box-shadow: 0 0 0 6px rgba(255, 77, 77, 0.05) }",
            ),
            ("", "#fade { opacity: 0.35 }"),
            ("", "#far { color: #ff0 }"),
            ("", "#words { text-shadow: 1px 2px 3px #f00 }"),
            (
                "#words { text-decoration: underline }",
                "#words { text-decoration: underline wavy #8af }",
            ),
            ("", "#field { outline-color: #f80 }"),
            ("", "#soft { background: #a48 }"),
            // two changes far apart, which repaint as two rectangles
            ("", "#dot { background: #4dff4d } #fade { opacity: 0.35 }"),
        ];
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/fonts/ceri-test.ttf"
        );
        let face = crate::font::FontFace::parse(std::fs::read(path).expect("the font should read"))
            .expect("the font should parse");
        let outlines = std::rc::Rc::new(crate::font::FontSet::new(std::rc::Rc::new(face), None));
        for (outlined, scale, scrolled) in [false, true]
            .into_iter()
            .flat_map(|outlined| [1.0, 1.5, 2.0].map(|scale| (outlined, scale)))
            .flat_map(|(outlined, scale)| [0.0, 7.5].map(|scrolled| (outlined, scale, scrolled)))
        {
            let _fonts = outlined.then(|| crate::font::use_fonts(outlines.clone(), 1.0));
            for (before, after) in frames {
                let old = damage_layout(before, scrolled);
                let new = damage_layout(after, scrolled);
                let Damage::Areas(areas) = damage((&old, &[]), (&new, &[]), &[]) else {
                    panic!("{after} should damage an area");
                };
                let mut canvas = paint_with_canvases(&old, &[], scale, false);
                for &area in &areas {
                    repaint(&mut canvas, &new, &[], area);
                }
                let whole = paint_with_canvases(&new, &[], scale, false);
                let width = whole.width() as usize;
                let differing = canvas
                    .rgba_pixels()
                    .chunks(4)
                    .zip(whole.rgba_pixels().chunks(4))
                    .enumerate()
                    .filter(|(_, (a, b))| a != b)
                    .map(|(index, _)| (index % width, index / width))
                    .collect::<Vec<_>>();
                assert!(
                    differing.is_empty(),
                    "{after} at {scale}x scrolled {scrolled} outlined {outlined}: {} pixels from {:?} to {:?} differ, damage {areas:?}",
                    differing.len(),
                    differing.first(),
                    differing.last(),
                );
            }
        }
    }

    const SCROLL_PAGE: &str = concat!(
        "<html><body><header id='bar'>pinned bar</header><main><p>Recordings, 12 today</p>",
        "<section id='list'><div class='row'>alpha</div><div class='row'>beta</div>",
        "<div class='row' id='held'>gamma <i id='tag'>new</i></div><div class='row'>delta</div>",
        "<div class='row'>epsilon</div><div class='row'>zeta</div><div class='row'>eta</div>",
        "<div class='row'>theta</div><div class='row'>iota</div><b id='loose'>loose</b>",
        "</section><div id='over'>over</div><div id='filler'></div></main></body></html>",
    );

    const SCROLL_STYLE: &str = concat!(
        "html, body { margin: 0; background: #121315; color: #e8e8e8; font: 13px sans-serif }",
        "main { padding: 13px } p { margin: 0 0 7px }",
        "#bar { position: fixed; bottom: 3px; left: 40px; width: 70px; height: 14px; background: #345 }",
        "#list { height: 90px; overflow: auto; border: 1px solid #333; border-radius: 6px }",
        ".row { height: 18px; padding: 0 5px; background: linear-gradient(#202124, #2a2b2e) }",
        ".row { text-shadow: 0 1px 3px #000 } #held { position: relative }",
        "#tag { position: absolute; right: 4px; top: 2px; color: #8af }",
        "#loose { position: absolute; left: 90px; top: 50px; background: #533 }",
        "#over { position: absolute; left: 20px; top: 60px; width: 30px; height: 9px }",
        "#over { background: rgba(255, 128, 0, 0.5) } #filler { height: 400px }",
    );

    fn scroll_layout(list: f32, root: f32) -> LayoutDocument {
        let document = Document::parse_markup(SCROLL_PAGE).expect("document should parse");
        let mut state = crate::css::StyleState::default();
        let scroller = document
            .find_element_by_id("list")
            .expect("the list exists");
        state.scroll_offsets.insert(scroller, (0.0, list));
        state.scroll_offsets.insert(document.root(), (0.0, root));
        let stylesheet = Stylesheet::parse(SCROLL_STYLE).expect("stylesheet should parse");
        crate::layout::layout_with_state(
            &document,
            &stylesheet,
            Size {
                width: 157.0,
                height: 233.0,
            },
            &state,
        )
    }

    #[test]
    fn scrolling_the_pixels_matches_a_whole_frame_to_a_level() {
        let moves = [
            ((0.0, 0.0), (6.0, 0.0)),
            ((6.0, 0.0), (36.0, 0.0)),
            ((36.0, 0.0), (12.0, 0.0)),
            ((12.0, 30.0), (24.0, 30.0)),
            ((6.0, 0.0), (6.0, 12.0)),
            ((6.0, 12.0), (6.0, 72.0)),
            ((6.0, 72.0), (6.0, 12.0)),
        ];
        for scale in [1.0, 1.5, 2.0] {
            for ((list, root), (next_list, next_root)) in moves {
                let old = scroll_layout(list, root);
                let new = scroll_layout(next_list, next_root);
                let mut canvas = paint_with_canvases(&old, &[], scale, false);
                let plan = scroll_repaint(&canvas, &old, &new).unwrap_or_else(|| {
                    panic!("{list},{root} to {next_list},{next_root} should move pixels")
                });
                scroll(&mut canvas, plan, &new, &[]);
                // positions moved in f32 can round a gradient channel the other way, a level at most
                let whole = paint_with_canvases(&new, &[], scale, false);
                let width = whole.width() as usize;
                let differing = canvas
                    .rgba_pixels()
                    .chunks(4)
                    .zip(whole.rgba_pixels().chunks(4))
                    .enumerate()
                    .filter(|(_, (a, b))| a.iter().zip(*b).any(|(a, b)| a.abs_diff(*b) > 1))
                    .map(|(index, _)| (index % width, index / width))
                    .collect::<Vec<_>>();
                assert!(
                    differing.is_empty(),
                    "{list},{root} to {next_list},{next_root} at {scale}x: {} pixels from {:?} to {:?} differ",
                    differing.len(),
                    differing.first(),
                    differing.last(),
                );
            }
        }
    }

    #[test]
    fn scrolling_falls_back_where_pixels_cannot_move() {
        let old = scroll_layout(0.0, 0.0);
        let canvas = paint_with_canvases(&old, &[], 1.5, false);
        // a css pixel is a pixel and a half at 1.5x
        assert!(scroll_repaint(&canvas, &old, &scroll_layout(1.0, 0.0)).is_none());
        assert!(scroll_repaint(&canvas, &old, &scroll_layout(6.0, 12.0)).is_none());
    }

    #[test]
    fn svg_rasters_are_found_by_scene_and_key_and_evicted_oldest_first() {
        let document = Document::parse_markup(
            "<svg id='a' viewBox='0 0 10 10'><path d='M0 0h10v10z'/></svg><svg id='b' viewBox='0 0 10 10'><path d='M0 0h5v5z'/></svg>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse("svg { display: block; width: 10px; height: 10px }")
            .expect("stylesheet should parse");
        let layout = crate::layout::layout(
            &document,
            &stylesheet,
            Size {
                width: 20.0,
                height: 20.0,
            },
        );
        let mut scenes = Vec::new();
        let mut stack = vec![&layout.root];
        while let Some(layout_box) = stack.pop() {
            scenes.extend(layout_box.svg.clone());
            stack.extend(&layout_box.children);
        }
        assert_eq!(scenes.len(), 2);
        let mut rasters = SvgRasters::default();
        let big = || Rc::new(Image::new(2000, 1400));
        rasters.keep(&scenes[0], [1, 0, 0, 2000, 1400], big());
        assert!(rasters.find(&scenes[0], [1, 0, 0, 2000, 1400]).is_some());
        assert!(rasters.find(&scenes[1], [1, 0, 0, 2000, 1400]).is_none());
        assert!(rasters.find(&scenes[0], [2, 0, 0, 2000, 1400]).is_none());
        // an equal scene built again by a later layout finds the raster too
        let again = Rc::new((*scenes[0]).clone());
        assert!(rasters.find(&again, [1, 0, 0, 2000, 1400]).is_some());
        rasters.keep(&scenes[1], [1, 0, 0, 2000, 1400], big());
        rasters.find(&scenes[0], [1, 0, 0, 2000, 1400]);
        // a third raster passes the budget, and the one used longest ago goes
        rasters.keep(&scenes[1], [3, 0, 0, 2000, 1400], big());
        assert!(rasters.find(&scenes[0], [1, 0, 0, 2000, 1400]).is_some());
        assert!(rasters.find(&scenes[1], [1, 0, 0, 2000, 1400]).is_none());
        assert!(rasters.find(&scenes[1], [3, 0, 0, 2000, 1400]).is_some());
    }
    #[test]
    fn changes_far_apart_damage_separate_rectangles() {
        let old = damage_layout("", 0.0);
        let new = damage_layout("#dot { background: #4dff4d } #fade { opacity: 0.35 }", 0.0);
        let Damage::Areas(areas) = damage((&old, &[]), (&new, &[]), &[]) else {
            panic!("the changes should damage areas");
        };
        assert_eq!(areas.len(), 2, "{areas:?}");
        let covered: f32 = areas.iter().map(|area| area.width * area.height).sum();
        assert!(covered < 157.0 * 233.0 / 3.0, "{areas:?}");
        // a crowd of changes stays within the region's limit
        let mut region = Region::default();
        for step in 0..40 {
            region.add(Rect {
                x: step as f32 * 50.0,
                y: (step % 7) as f32 * 90.0,
                width: 4.0,
                height: 4.0,
            });
        }
        assert!(region.rects.len() <= Region::LIMIT);
    }
    // canvases that trade places in the layer list, one drawn anew and one gone, repaint
    // their own boxes only
    #[test]
    fn layers_that_reorder_come_or_go_repaint_only_their_boxes() {
        let document = Document::parse_markup(
            "<html><body><canvas id='a' width='10' height='10'></canvas><p>between</p><canvas id='b' width='10' height='10'></canvas><canvas id='c' width='10' height='10'></canvas></body></html>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(
            "html, body { margin: 0; background: #123 } canvas { display: block; margin: 4px }",
        )
        .expect("stylesheet should parse");
        let layout = crate::layout::layout(
            &document,
            &stylesheet,
            Size {
                width: 80.0,
                height: 120.0,
            },
        );
        let filled = |red: u8| {
            let mut image = Image::new(10, 10);
            image.pixels.fill(Color {
                red,
                green: 40,
                blue: 90,
                alpha: 255,
            });
            std::sync::Arc::new(image)
        };
        let layer = |id: &str, image: &std::sync::Arc<Image>| ImageLayer {
            node: document.find_element_by_id(id).expect("canvas"),
            image: image.clone(),
            background: None,
        };
        let (red, blue, green) = (filled(220), filled(30), filled(120));
        let before = [layer("a", &red), layer("b", &blue), layer("c", &green)];
        let after = [layer("b", &blue), layer("a", &green)];
        let Damage::Areas(areas) = damage((&layout, &before), (&layout, &after), &[]) else {
            panic!("a reordered layer list should damage areas");
        };
        let mut canvas = paint_with_canvases(&layout, &before, 1.0, false);
        for &area in &areas {
            repaint(&mut canvas, &layout, &after, area);
        }
        let whole = paint_with_canvases(&layout, &after, 1.0, false);
        assert!(canvas.rgba_pixels() == whole.rgba_pixels());
        let b = document.find_element_by_id("b").expect("canvas");
        let mut unmoved = Region::default();
        assert!(node_area(&layout.root, (0.0, 0.0), &[b], &mut unmoved));
        let apart = |a: &Rect, b: &Rect| {
            a.x + a.width <= b.x
                || b.x + b.width <= a.x
                || a.y + a.height <= b.y
                || b.y + b.height <= a.y
        };
        assert!(
            areas
                .iter()
                .all(|area| unmoved.rects.iter().all(|kept| apart(area, kept))),
            "the canvas whose picture held is left alone"
        );
    }

    #[test]
    fn damage_falls_back_to_the_whole_frame_where_it_cannot_bound_a_change() {
        let base = damage_layout("", 0.0);
        assert_eq!(
            damage((&base, &[]), (&damage_layout("", 0.0), &[]), &[]),
            Damage::Nothing
        );
        for change in [
            "#turned { background: #533 }",
            "body { background: #000 }",
            "#words { backdrop-filter: blur(1px) }",
        ] {
            assert_eq!(
                damage((&base, &[]), (&damage_layout(change, 0.0), &[]), &[]),
                Damage::Everything,
                "{change}"
            );
        }
    }

    #[test]
    fn a_text_shadow_cast_into_view_paints_from_a_box_outside_it() {
        let document = Document::parse_markup("<html><body><div><p>shadow</p></div></body></html>")
            .expect("document should parse");
        let stylesheet = Stylesheet::parse(concat!(
            "html, body { margin: 0; background: #ffffff }",
            "div { height: 40px; overflow: hidden } p { margin: -80px 0 0; font: 16px sans-serif }",
            "p { color: #fff; text-shadow: 0 90px 0 #f00 }",
        ))
        .expect("stylesheet should parse");
        let canvas = paint(&crate::layout::layout(
            &document,
            &stylesheet,
            Size {
                width: 60.0,
                height: 40.0,
            },
        ));
        assert!(
            canvas
                .rgba_pixels()
                .chunks(4)
                .any(|pixel| pixel[0] > 200 && pixel[1] < 100),
            "the shadow should show in the viewport"
        );
    }

    #[test]
    fn a_clip_that_leaves_nothing_hides_the_children() {
        let document = Document::parse_markup(
            "<html><body><div id='outer'><div id='inner'><p>hidden</p></div></div></body></html>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(concat!(
            "html, body { margin: 0; background: #ffffff }",
            "#outer { width: 40px; height: 20px; overflow: hidden }",
            "#inner { margin-top: 30px; height: 10px; overflow: hidden }",
            "p { position: relative; top: -30px; margin: 0; height: 20px; background: #ff0000 }",
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
        assert!(
            canvas
                .rgba_pixels()
                .chunks(4)
                .all(|pixel| pixel[..3] == [255, 255, 255]),
            "nothing of the paragraph should show outside its clips"
        );
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
    use crate::dom::{Document, NodeId};
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
                background_clip_text: false,
                object_fit: crate::css::ObjectFit::Fill,
                object_position: None,
                top_layer: false,
                transform: None,
                backface_hidden: false,
                clip_path: None,
                radii: SQUARE_CORNERS,
                opacity: 1.0,
                shadows: Vec::new(),
                filters: Default::default(),
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
                sticky_inset: None,
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
                svg: None,
                scrollbar: Default::default(),
                stable_gutter: false,
                row_baseline: None,
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
    fn a_fixed_box_draws_grayscale_text_when_its_layer_is_not_opaque() {
        let document = Document::parse_markup(
            "<html><body><p id='plain'>a</p><p id='shadow'>a</p><p id='round'>a</p><p id='clear'>a</p></body></html>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(concat!(
            "p { position: fixed; width: 100px; height: 20px; background: #1c1c1c }",
            "#shadow { box-shadow: 0 2px 8px #000 } #round { border-radius: 8px }",
            "#clear { background: rgba(28, 28, 28, 0.9) }",
        ))
        .expect("stylesheet should parse");
        let layout = crate::layout::layout(
            &document,
            &stylesheet,
            Size {
                width: 200.0,
                height: 200.0,
            },
        );
        let mut fixed = Vec::new();
        let mut stack = vec![&layout.root];
        while let Some(layout_box) = stack.pop() {
            if layout_box.position == Position::Fixed {
                fixed.push(layout_box);
            }
            stack.extend(layout_box.children.iter().rev());
        }
        let layered = |scale: f32| {
            let canvas = Canvas::scaled(1.0, 1.0, scale, Color::WHITE);
            fixed
                .iter()
                .map(|layout_box| floats_in_own_layer(&canvas, layout_box))
                .collect::<Vec<_>>()
        };
        assert_eq!(layered(1.0), [false, true, false, true]);
        assert_eq!(layered(2.0), [false, true, true, true]);
    }

    #[test]
    fn background_clip_text_paints_the_background_through_the_glyphs_alone() {
        let document = Document::parse_markup(
            "<html><body><div id='a'>HH<span>H</span></div><div id='b'>HH</div></body></html>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(concat!(
            "html, body { margin: 0; background: #ffffff }",
            "div { width: 160px; height: 50px; font-size: 32px; color: #00ff00; background: #ff0000 }",
            "#a { -webkit-background-clip: text; background-clip: text; -webkit-text-fill-color: transparent }",
        ))
        .expect("stylesheet should parse");
        let canvas = paint(&crate::layout::layout(
            &document,
            &stylesheet,
            Size {
                width: 160.0,
                height: 100.0,
            },
        ));
        let count = |rows: std::ops::Range<usize>, wanted: fn([u8; 3]) -> bool| {
            rows.flat_map(|y| (0..160).map(move |x| (x, y)))
                .filter(|&(x, y)| {
                    let index = (y * canvas.width() as usize + x) * 4;
                    wanted([0, 1, 2].map(|channel| canvas.rgba_pixels()[index + channel]))
                })
                .count()
        };
        let red = |[r, g, b]: [u8; 3]| r > 200 && g < 60 && b < 60;
        let white = |[r, g, b]: [u8; 3]| r > 250 && g > 250 && b > 250;
        let green = |[r, g, _]: [u8; 3]| g > 200 && r < 60;
        let painted = count(0..50, red);
        assert!(
            painted > 40,
            "the glyphs show the background, {painted} red pixels"
        );
        assert!(
            count(0..50, white) > painted,
            "the box around the glyphs stays clear"
        );
        assert_eq!(
            count(0..50, green),
            0,
            "a transparent fill hides the text colour"
        );
        assert_eq!(
            count(50..100, white),
            0,
            "a border-box background fills its box"
        );
    }

    #[test]
    fn images_shrunk_past_half_sample_their_mipmaps() {
        // every fourth column white, four times smaller, is a quarter grey; bilinear taps
        // at the texel centres would only find the black columns
        let mut board = Image::new(64, 64);
        for (index, pixel) in board.pixels.iter_mut().enumerate() {
            let shade = if index % 4 == 3 { 255 } else { 0 };
            *pixel = Color::opaque(shade, shade, shade);
        }
        let mut canvas = Canvas::new(16, 16, Color::opaque(255, 0, 0));
        let rect = Rect {
            x: 0.0,
            y: 0.0,
            width: 16.0,
            height: 16.0,
        };
        paint_image(
            &mut canvas,
            ImageDraw {
                image: &board,
                placement: rect,
                area: Shape::from_rect(rect, CornerRadii::default()),
                clip: None,
                opacity: 1.0,
                repeat: (false, false),
                video: None,
            },
        );
        for pixel in canvas.rgba_pixels().as_chunks::<4>().0 {
            assert!(
                pixel[..3].iter().all(|channel| channel.abs_diff(64) <= 2),
                "{pixel:?}"
            );
        }
        assert_eq!((board.level(2).width, board.level(9).width), (16, 1));
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
    fn filters_and_backdrop_filters_land_on_the_pixels_chromium_draws() {
        let document = Document::parse_markup(concat!(
            "<html><body><div id='blurred'></div><div id='shadowed'></div>",
            "<div id='stripes'></div><div id='frosted'></div><div id='clip'><div></div></div>",
            "</body></html>",
        ))
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(concat!(
            "html, body { margin: 0; background: #fff } body > div { position: absolute }",
            "#blurred, #shadowed { width: 60px; height: 40px; background: #e01020; left: 20px }",
            "#blurred { top: 20px; filter: blur(4px) }",
            "#shadowed { top: 100px; filter: drop-shadow(4px 6px 6px #000) }",
            "#stripes { left: 200px; top: 20px; width: 200px; height: 120px;",
            " background: repeating-linear-gradient(90deg, #000 0 10px, #fff 10px 20px) }",
            "#frosted { left: 240px; top: 50px; width: 120px; height: 80px; border-radius: 12px;",
            " backdrop-filter: blur(6px) }",
            "#clip { left: 300px; top: 150px; width: 80px; height: 40px; overflow: hidden }",
            "#clip div { position: absolute; inset: 0; background: #000; filter: blur(18px) }",
        ))
        .expect("stylesheet should parse");
        let canvas = paint(&crate::layout::layout(
            &document,
            &stylesheet,
            Size {
                width: 420.0,
                height: 220.0,
            },
        ));
        let pixel = |x: usize, y: usize| {
            let index = (y * canvas.width() as usize + x) * 4;
            <[u8; 3]>::try_from(&canvas.rgba_pixels()[index..index + 3]).expect("a pixel")
        };
        // every expected value was read from edge rendering the same page in software
        let blurred = (8..24).step_by(2).map(|x| pixel(x, 40)).collect::<Vec<_>>();
        assert_eq!(
            blurred,
            [
                [255, 255, 255],
                [255, 253, 253],
                [254, 247, 247],
                [252, 232, 233],
                [249, 205, 209],
                [244, 168, 174],
                [238, 125, 133],
                [233, 83, 95],
            ]
        );
        let shadow = (140..156)
            .step_by(2)
            .map(|y| pixel(50, y)[0])
            .collect::<Vec<_>>();
        assert_eq!(shadow, [42, 69, 102, 136, 170, 200, 223, 239]);
        assert_eq!(pixel(50, 138), [224, 16, 32], "the box stays on its shadow");
        let frosted = (240..260).map(|x| pixel(x, 90)[0]).collect::<Vec<_>>();
        assert_eq!(
            frosted,
            [
                17, 20, 25, 32, 42, 55, 68, 83, 98, 112, 125, 137, 147, 154, 158, 158, 155, 149,
                141, 132
            ]
        );
        assert_eq!(
            pixel(240, 50),
            [0, 0, 0],
            "the rounded corner leaves the stripe behind it unfiltered"
        );
        assert_eq!(
            pixel(385, 170),
            [255, 255, 255],
            "the blur stays inside the overflow clip"
        );
        assert!(pixel(378, 170)[0] < 200);
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
    fn rounded_boxes_snap_to_device_pixels_like_square_ones() {
        let document = Document::parse_markup("<html><body><div id='track'></div></body></html>")
            .expect("document should parse");
        let stylesheet = Stylesheet::parse(
            "html, body { margin: 0; background: #ffffff } #track { margin: 10.5px 0 0 0.5px; width: 40px; height: 4px; border-radius: 2px; background: #ff0000 }",
        )
        .expect("stylesheet should parse");
        let layout = crate::layout::layout(
            &document,
            &stylesheet,
            Size {
                width: 48.0,
                height: 24.0,
            },
        );
        let canvas = paint(&layout);
        let pixel = |x: usize, y: usize| {
            let index = (y * canvas.width() as usize + x) * 4;
            canvas.rgba_pixels()[index..index + 3].to_vec()
        };
        let column = (8..18).map(|y| pixel(20, y)).collect::<Vec<_>>();
        let red = vec![255, 0, 0];
        let white = vec![255, 255, 255];
        assert_eq!(column[..3], [white.clone(), white.clone(), white.clone()]);
        assert_eq!(column[3..7], [red.clone(), red.clone(), red.clone(), red]);
        assert_eq!(column[7..], [white.clone(), white.clone(), white]);
        assert_eq!(pixel(0, 13), vec![255, 255, 255]);
        assert_ne!(
            pixel(1, 11),
            vec![255, 0, 0],
            "the corner should stay rounded"
        );
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
                background_clip_text: false,
                object_fit: crate::css::ObjectFit::Fill,
                object_position: None,
                top_layer: false,
                transform: None,
                backface_hidden: false,
                clip_path: None,
                radii: SQUARE_CORNERS,
                opacity: 1.0,
                shadows: Vec::new(),
                filters: Default::default(),
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
                sticky_inset: None,
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
                svg: None,
                scrollbar: Default::default(),
                stable_gutter: false,
                row_baseline: None,
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

    // a see-through surface writes its alpha, with colors as they are before the blend
    #[test]
    fn a_see_through_canvas_writes_a_png_with_straight_alpha() {
        let mut canvas = Canvas::new(4, 2, Color::transparent());
        canvas.fill_rect(0.0, 0.0, 2.0, 2.0, Color::rgba(200, 100, 40, 128));
        let path =
            std::env::temp_dir().join(format!("ceri-engine-clear-{}.png", std::process::id()));
        canvas.write_png(&path).expect("png should write");
        let image = crate::png::decode_png(&std::fs::read(&path).expect("png should read"))
            .expect("png should decode");
        std::fs::remove_file(path).expect("test png should be removable");
        let (filled, clear) = (image.pixels[0], image.pixels[3]);
        assert_eq!(clear.alpha, 0);
        assert_eq!(filled.alpha, 128);
        for (got, want) in [(filled.red, 200), (filled.green, 100), (filled.blue, 40)] {
            assert!(got.abs_diff(want) <= 1, "{filled:?}");
        }
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
                background_clip_text: false,
                object_fit: crate::css::ObjectFit::Fill,
                object_position: None,
                top_layer: false,
                transform: None,
                backface_hidden: false,
                clip_path: None,
                radii: SQUARE_CORNERS,
                opacity: 1.0,
                shadows: Vec::new(),
                filters: Default::default(),
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
                sticky_inset: None,
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
                svg: None,
                scrollbar: Default::default(),
                stable_gutter: false,
                row_baseline: None,
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
}
