// a canvas element's 2d context over a premultiplied RGBA bitmap, drawn the way Skia draws
// it on the cpu: rectangles cover pixels by area, and scaled pictures sample as chromium's
// image smoothing settings choose, measured against its output

use super::typed::{ElementKind, new_typed};
use super::*;
use crate::dom::Namespace;
use crate::image::Image;
use crate::layout::ImageLayer;
use crate::svg::{Affine, Coverage, IDENTITY, LineCap, LineJoin, Pen, Segment};
use std::cell::Cell;

type Rect4 = (f32, f32, f32, f32);
// a drawImage source's pixels and the width and height script measures it in
type Measured = (Bitmap, (f32, f32));

const METHODS: &[&str] = &[
    "save",
    "restore",
    "scale",
    "rotate",
    "translate",
    "transform",
    "setTransform",
    "resetTransform",
    "getTransform",
    "beginPath",
    "closePath",
    "moveTo",
    "lineTo",
    "bezierCurveTo",
    "quadraticCurveTo",
    "arc",
    "ellipse",
    "rect",
    "arcTo",
    "roundRect",
    "isPointInPath",
    "isPointInStroke",
    "fill",
    "stroke",
    "strokeRect",
    "clip",
    "setLineDash",
    "getLineDash",
    "createLinearGradient",
    "createRadialGradient",
    "createConicGradient",
    "createPattern",
    "fillText",
    "strokeText",
    "measureText",
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

// a gradient a context made: its line or its two circles in the user space of whatever
// later paints with it, and the stops script adds
pub(super) struct Gradient {
    // the start point and radius, then the end point and radius; a line has no radii, and a
    // conic gradient keeps its centre and start angle in the first
    from: (f32, f32, f32),
    to: (f32, f32, f32),
    shape: Shape,
    stops: Vec<(f32, Color)>,
}

#[derive(Clone, Copy, PartialEq)]
enum Shape {
    Linear,
    Radial,
    Conic,
}

// a CanvasPattern: the picture with alpha multiplied in, which axes repeat, and the matrix
// setTransform gave from pattern space to the user space of what paints with it
pub(super) struct Pattern {
    bitmap: Bitmap,
    repeat: (bool, bool),
    transform: Cell<[f32; 6]>,
}

pub(super) type PatternHandle = Rc<Pattern>;

impl Pattern {
    // the pixel the pattern puts at a point of user space, alpha already multiplied in,
    // filtered between its four nearest texels when smoothing is on; transparent past the
    // picture along an axis that does not repeat
    fn pixel_at(&self, point: (f32, f32), smooth: bool) -> Option<[u8; 4]> {
        let (width, height) = (self.bitmap.width as i64, self.bitmap.height as i64);
        if width == 0 || height == 0 {
            return None;
        }
        let (x, y) = crate::svg::map(crate::svg::invert(self.transform.get())?, point);
        let place = |at: i64, size: i64, repeat: bool| -> Option<i64> {
            if repeat {
                Some(at.rem_euclid(size))
            } else {
                (0..size).contains(&at).then_some(at)
            }
        };
        let texel = |column: i64, row: i64| -> Option<[u8; 4]> {
            let column = place(column, width, self.repeat.0)?;
            let row = place(row, height, self.repeat.1)?;
            Some(self.bitmap.pixels[(row * width + column) as usize])
        };
        if !smooth {
            return texel(x.floor() as i64, y.floor() as i64);
        }
        // skia's legacy bilinear, as drawImage samples: 16.16 fixed point with four bits of
        // subpixel weight
        let (fixed_x, fixed_y) = (Bitmap::fixed(x) - (1 << 15), Bitmap::fixed(y) - (1 << 15));
        let (left, top) = (fixed_x >> 16, fixed_y >> 16);
        let (sub_x, sub_y) = (((fixed_x >> 12) & 15) as u32, ((fixed_y >> 12) & 15) as u32);
        let corners = [
            (texel(left, top), (16 - sub_x) * (16 - sub_y)),
            (texel(left + 1, top), sub_x * (16 - sub_y)),
            (texel(left, top + 1), (16 - sub_x) * sub_y),
            (texel(left + 1, top + 1), sub_x * sub_y),
        ];
        if corners.iter().all(|(found, _)| found.is_none()) {
            return None;
        }
        Some(std::array::from_fn(|channel| {
            let total: u32 = corners
                .iter()
                .map(|(found, weight)| u32::from(found.unwrap_or([0; 4])[channel]) * weight)
                .sum();
            (total >> 8) as u8
        }))
    }
}

pub(super) type GradientHandle = Rc<RefCell<Gradient>>;

impl Gradient {
    // the colour at a point, none where a radial gradient reaches no circle or there are no
    // stops; channels are mixed as they stand, not premultiplied, as canvas gradients are
    fn color_at(&self, point: (f32, f32)) -> Option<Color> {
        let (first, last) = (self.stops.first()?, self.stops.last()?);
        let (dx, dy) = (self.to.0 - self.from.0, self.to.1 - self.from.1);
        let (px, py) = (point.0 - self.from.0, point.1 - self.from.1);
        let at = if self.shape == Shape::Conic {
            // the angle from the start angle, clockwise as the y axis runs down, in (0, 1] so
            // the seam itself takes the last stop as skia's sweep does
            let turn = (py.atan2(px) - self.from.2) / std::f32::consts::TAU;
            turn - turn.ceil() + 1.0
        } else if self.shape == Shape::Radial {
            // the largest t whose circle, between the two, passes through the point
            let dr = self.to.2 - self.from.2;
            let a = dx * dx + dy * dy - dr * dr;
            let b = px * dx + py * dy + self.from.2 * dr;
            let c = px * px + py * py - self.from.2 * self.from.2;
            // the circle through the point with the larger t, among those of a radius that
            // is not negative
            let grows = |t: f32| self.from.2 + t * dr >= 0.0;
            if a.abs() < 1e-6 {
                (b.abs() > 1e-6)
                    .then(|| c / (2.0 * b))
                    .filter(|&t| grows(t))?
            } else {
                let root = b * b - a * c;
                if root < 0.0 {
                    return None;
                }
                let (one, other) = ((b + root.sqrt()) / a, (b - root.sqrt()) / a);
                [one.max(other), one.min(other)]
                    .into_iter()
                    .find(|&t| grows(t))?
            }
        } else {
            let length = dx * dx + dy * dy;
            if length <= 0.0 {
                return None;
            }
            (px * dx + py * dy) / length
        };
        if at <= first.0 {
            return Some(first.1);
        }
        if at >= last.0 {
            return Some(last.1);
        }
        let after = self.stops.iter().position(|stop| stop.0 > at)?;
        let (low, high) = (self.stops[after - 1], self.stops[after]);
        let share = (at - low.0) / (high.0 - low.0);
        let mix = |from: u8, to: u8| {
            (f32::from(from) + (f32::from(to) - f32::from(from)) * share).round() as u8
        };
        Some(Color {
            red: mix(low.1.red, high.1.red),
            green: mix(low.1.green, high.1.green),
            blue: mix(low.1.blue, high.1.blue),
            alpha: mix(low.1.alpha, high.1.alpha),
        })
    }
}

// what a fill or a stroke takes its colour from
#[derive(Clone)]
enum Paint {
    Color(Color),
    // the gradient or pattern, and the object script holds it by
    Gradient(GradientHandle, Value),
    Pattern(PatternHandle, Value),
}

// how what is drawn meets what is there; each of these changes only where the shape lands
#[derive(Clone, Copy, PartialEq)]
enum Composite {
    SourceOver,
    DestinationOver,
    DestinationOut,
    SourceAtop,
    Xor,
    Lighter,
    Multiply,
    Screen,
    Darken,
    Lighten,
}

impl Composite {
    const NAMES: &[(&str, Self)] = &[
        ("source-over", Self::SourceOver),
        ("destination-over", Self::DestinationOver),
        ("destination-out", Self::DestinationOut),
        ("source-atop", Self::SourceAtop),
        ("xor", Self::Xor),
        ("lighter", Self::Lighter),
        ("multiply", Self::Multiply),
        ("screen", Self::Screen),
        ("darken", Self::Darken),
        ("lighten", Self::Lighten),
    ];

    // premultiplied source onto premultiplied backdrop, channels from 0 to 1
    fn blend(self, source: [f32; 4], backdrop: [f32; 4]) -> [f32; 4] {
        let (sa, ba) = (source[3], backdrop[3]);
        let alpha = match self {
            Self::DestinationOut => ba * (1.0 - sa),
            Self::SourceAtop => ba,
            Self::Xor => sa + ba - 2.0 * sa * ba,
            Self::Lighter => (sa + ba).min(1.0),
            _ => sa + ba - sa * ba,
        };
        let channel = |s: f32, b: f32| match self {
            Self::SourceOver => s + b * (1.0 - sa),
            Self::DestinationOver => b + s * (1.0 - ba),
            Self::DestinationOut => b * (1.0 - sa),
            Self::SourceAtop => s * ba + b * (1.0 - sa),
            Self::Xor => s * (1.0 - ba) + b * (1.0 - sa),
            Self::Lighter => (s + b).min(1.0),
            Self::Multiply => s * b + s * (1.0 - ba) + b * (1.0 - sa),
            Self::Screen => s + b - s * b,
            Self::Darken => (s * ba).min(b * sa) + s * (1.0 - ba) + b * (1.0 - sa),
            Self::Lighten => (s * ba).max(b * sa) + s * (1.0 - ba) + b * (1.0 - sa),
        };
        [
            channel(source[0], backdrop[0]),
            channel(source[1], backdrop[1]),
            channel(source[2], backdrop[2]),
            alpha,
        ]
    }
}

const DEFAULT_FONT: &str = "10px sans-serif";

// the value of an enumerated attribute among those it takes, which a setter keeps
fn keyword(value: &str, known: &[&'static str]) -> Option<&'static str> {
    known.iter().copied().find(|name| *name == value)
}

// the drawing state save pushes and restore pops
#[derive(Clone)]
struct DrawState {
    // user space to the bitmap's pixels
    transform: Affine,
    fill_style: Paint,
    stroke_style: Paint,
    line_width: f32,
    cap: LineCap,
    join: LineJoin,
    miter_limit: f32,
    dash: Vec<f32>,
    dash_offset: f32,
    alpha: f32,
    composite: Composite,
    // the font as script gave it, and its size in pixels, families and face
    font: (Rc<str>, f32, crate::font::FontStack, crate::font::FontStyle),
    text_align: &'static str,
    text_baseline: &'static str,
    direction: &'static str,
    // how much of each pixel of the bitmap drawing may reach, out of 255; none is all of it
    clip: Option<Rc<Vec<u8>>>,
    // the shadow a shape casts: its colour, how far it blurs and where it falls, in pixels of
    // the bitmap whatever the transform
    shadow_color: Color,
    shadow_blur: f32,
    shadow_offset: (f32, f32),
}

impl Default for DrawState {
    fn default() -> Self {
        Self {
            transform: IDENTITY,
            fill_style: Paint::Color(Color::BLACK),
            stroke_style: Paint::Color(Color::BLACK),
            line_width: 1.0,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            miter_limit: 10.0,
            dash: Vec::new(),
            dash_offset: 0.0,
            alpha: 1.0,
            composite: Composite::SourceOver,
            font: crate::css::canvas_font(DEFAULT_FONT).map_or(
                (
                    DEFAULT_FONT.into(),
                    10.0,
                    Default::default(),
                    crate::font::FontStyle::NORMAL,
                ),
                |(size, stack, style)| (DEFAULT_FONT.into(), size, stack, style),
            ),
            text_align: "start",
            text_baseline: "alphabetic",
            direction: "ltr",
            clip: None,
            shadow_color: Color::transparent(),
            shadow_blur: 0.0,
            shadow_offset: (0.0, 0.0),
        }
    }
}

// the inverse of a transform, none when it flattens the plane
fn inverse(matrix: Affine) -> Option<Affine> {
    let [a, b, c, d, e, f] = matrix;
    let determinant = a * d - b * c;
    (determinant.is_finite() && determinant.abs() > f32::EPSILON).then(|| {
        [
            d / determinant,
            -b / determinant,
            -c / determinant,
            a / determinant,
            (c * f - d * e) / determinant,
            (b * e - a * f) / determinant,
        ]
    })
}

fn map_segment(matrix: Affine, segment: Segment) -> Segment {
    let at = |point| crate::svg::map(matrix, point);
    match segment {
        Segment::Move(point) => Segment::Move(at(point)),
        Segment::Line(point) => Segment::Line(at(point)),
        Segment::Cubic(first, second, end) => Segment::Cubic(at(first), at(second), at(end)),
        Segment::Close => Segment::Close,
    }
}

// an arc of an ellipse as cubics, a quarter turn at most in each, from its start point on;
// the sweep is what the canvas arc rules leave of the two angles
fn arc_points(
    center: (f32, f32),
    radii: (f32, f32),
    rotation: f32,
    (start, end): (f32, f32),
    counterclockwise: bool,
) -> ((f32, f32), Vec<Segment>) {
    let turn = std::f32::consts::TAU;
    let sweep = if counterclockwise {
        if start - end >= turn {
            -turn
        } else {
            -(start - end).rem_euclid(turn)
        }
    } else if end - start >= turn {
        turn
    } else {
        (end - start).rem_euclid(turn)
    };
    let (sin, cos) = rotation.sin_cos();
    let place = |x: f32, y: f32| {
        let (x, y) = (x * radii.0, y * radii.1);
        (center.0 + x * cos - y * sin, center.1 + x * sin + y * cos)
    };
    let pieces = (sweep.abs() / std::f32::consts::FRAC_PI_2).ceil().max(1.0);
    let step = sweep / pieces;
    let reach = 4.0 / 3.0 * (step / 4.0).tan();
    let mut cubics = Vec::new();
    for piece in 0..pieces as usize {
        let (from, to) = (
            start + step * piece as f32,
            start + step * (piece + 1) as f32,
        );
        let ((from_sin, from_cos), (to_sin, to_cos)) = (from.sin_cos(), to.sin_cos());
        cubics.push(Segment::Cubic(
            place(from_cos - reach * from_sin, from_sin + reach * from_cos),
            place(to_cos + reach * to_sin, to_sin - reach * to_cos),
            place(to_cos, to_sin),
        ));
    }
    (place(start.cos(), start.sin()), cubics)
}

// a picture for a WebGL upload: width, height and straight-alpha pixels from the top row
pub(super) type TexturePixels = (u32, u32, Vec<[u8; 4]>);

// the pixels of an ImageBitmap, gone once it is closed
pub(crate) type ImageBitmapHandle = Rc<RefCell<Option<Bitmap>>>;

pub(super) struct CanvasContextData {
    node: NodeId,
    state: DrawState,
    saved: Vec<DrawState>,
    // the current path, already in the bitmap's pixels
    path: Vec<Segment>,
    // where the path's pen is and where its subpath began, in the bitmap's pixels
    pen: Option<((f32, f32), (f32, f32))>,
    smoothing_enabled: bool,
    smoothing_quality: Smoothing,
    bitmap: Bitmap,
    // the bitmap as paint reads it, rebuilt after the next change
    snapshot: Option<std::sync::Arc<Image>>,
    // pixels of another origin were drawn in without cors, so script may not read any back
    tainted: bool,
}

fn mul255(a: u32, b: u32) -> u32 {
    let product = a * b + 128;
    (product + (product >> 8)) >> 8
}

fn premultiply([red, green, blue, alpha]: [u8; 4]) -> [u8; 4] {
    let scale = |channel: u8| mul255(channel.into(), alpha.into()) as u8;
    [scale(red), scale(green), scale(blue), alpha]
}

// chromium's unpremultiply, skia's raster pipeline in float32: each channel over 255 times the
// reciprocal of alpha over 255, back to bytes rounding half to even; it matches every value
// getImageData returns in Edge, and premultiplying again gives back the same pixel
fn unpremultiply([red, green, blue, alpha]: [u8; 4]) -> [u8; 4] {
    if alpha == 0 {
        return [0; 4];
    }
    let inverse = (1.0_f64 / 255.0) as f32;
    let reciprocal = 1.0 / (f32::from(alpha) * inverse);
    let scale = |channel: u8| {
        ((f32::from(channel) * inverse * reciprocal) * 255.0)
            .round_ties_even()
            .min(255.0) as u8
    };
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
    fn new(node: NodeId, width: u32, height: u32) -> Self {
        Self {
            node,
            state: DrawState::default(),
            saved: Vec::new(),
            path: Vec::new(),
            pen: None,
            smoothing_enabled: true,
            smoothing_quality: Smoothing::Low,
            bitmap: Bitmap::new(width, height),
            snapshot: None,
            tainted: false,
        }
    }

    // a point of user space where the transform puts it now
    fn place(&self, point: (f32, f32)) -> (f32, f32) {
        crate::svg::map(self.state.transform, point)
    }

    fn move_to(&mut self, point: (f32, f32)) {
        let point = self.place(point);
        self.path.push(Segment::Move(point));
        self.pen = Some((point, point));
    }

    // a line to the point, or the start of a subpath at it when the path has none
    fn line_to(&mut self, point: (f32, f32)) {
        let placed = self.place(point);
        match self.pen {
            None => self.move_to(point),
            Some((_, start)) => {
                self.path.push(Segment::Line(placed));
                self.pen = Some((placed, start));
            }
        }
    }

    fn cubic_to(&mut self, first: (f32, f32), second: (f32, f32), end: (f32, f32)) {
        if self.pen.is_none() {
            self.move_to(first);
        }
        let (first, second, end) = (self.place(first), self.place(second), self.place(end));
        self.path.push(Segment::Cubic(first, second, end));
        self.pen = self.pen.map(|(_, start)| (end, start));
    }

    // a quadratic curve is the cubic whose controls sit two thirds of the way to its own
    fn quadratic_to(&mut self, control: (f32, f32), end: (f32, f32)) {
        if self.pen.is_none() {
            self.move_to(control);
        }
        let (control, end) = (self.place(control), self.place(end));
        let Some((from, start)) = self.pen else {
            return;
        };
        let toward = |point: (f32, f32)| {
            (
                point.0 + (control.0 - point.0) * 2.0 / 3.0,
                point.1 + (control.1 - point.1) * 2.0 / 3.0,
            )
        };
        self.path
            .push(Segment::Cubic(toward(from), toward(end), end));
        self.pen = Some((end, start));
    }

    fn close_path(&mut self) {
        if let Some((_, start)) = self.pen {
            self.path.push(Segment::Close);
            self.pen = Some((start, start));
        }
    }

    fn rect(&mut self, (x, y, width, height): Rect4) {
        self.move_to((x, y));
        self.line_to((x + width, y));
        self.line_to((x + width, y + height));
        self.line_to((x, y + height));
        self.close_path();
    }

    fn arc(
        &mut self,
        center: (f32, f32),
        radii: (f32, f32),
        rotation: f32,
        angles: (f32, f32),
        counterclockwise: bool,
    ) {
        let (start, cubics) = arc_points(center, radii, rotation, angles, counterclockwise);
        self.line_to(start);
        for cubic in cubics {
            if let Segment::Cubic(first, second, end) = cubic {
                self.cubic_to(first, second, end);
            }
        }
    }

    // a line toward the corner at the first point, rounded into the line toward the second
    fn arc_to(&mut self, corner: (f32, f32), toward: (f32, f32), radius: f32) {
        let Some((pen, _)) = self.pen else {
            self.move_to(corner);
            return;
        };
        let Some(back) = inverse(self.state.transform) else {
            return;
        };
        let from = crate::svg::map(back, pen);
        let (first, second) = (
            (from.0 - corner.0, from.1 - corner.1),
            (toward.0 - corner.0, toward.1 - corner.1),
        );
        let cross = first.0 * second.1 - first.1 * second.0;
        let (first_length, second_length) = (first.0.hypot(first.1), second.0.hypot(second.1));
        if radius == 0.0 || first_length == 0.0 || second_length == 0.0 || cross.abs() < 1e-6 {
            self.line_to(corner);
            return;
        }
        let (first, second) = (
            (first.0 / first_length, first.1 / first_length),
            (second.0 / second_length, second.1 / second_length),
        );
        let angle = (first.0 * second.0 + first.1 * second.1)
            .clamp(-1.0, 1.0)
            .acos();
        let reach = radius / (angle / 2.0).tan();
        let bisector = (first.0 + second.0, first.1 + second.1);
        let bisector_length = bisector.0.hypot(bisector.1);
        let middle = radius / (angle / 2.0).sin();
        let center = (
            corner.0 + bisector.0 / bisector_length * middle,
            corner.1 + bisector.1 / bisector_length * middle,
        );
        let start = (corner.0 + first.0 * reach, corner.1 + first.1 * reach);
        let end = (corner.0 + second.0 * reach, corner.1 + second.1 * reach);
        let angles = (
            (start.1 - center.1).atan2(start.0 - center.0),
            (end.1 - center.1).atan2(end.0 - center.0),
        );
        self.arc(center, (radius, radius), 0.0, angles, cross > 0.0);
    }

    // a rectangle with rounded corners, radii shrunk together where they would overlap
    fn round_rect(
        &mut self,
        (mut x, mut y, mut width, mut height): Rect4,
        mut radii: [(f32, f32); 4],
    ) {
        if width < 0.0 {
            x += width;
            width = -width;
            radii = [radii[1], radii[0], radii[3], radii[2]];
        }
        if height < 0.0 {
            y += height;
            height = -height;
            radii = [radii[3], radii[2], radii[1], radii[0]];
        }
        let [top_left, top_right, bottom_right, bottom_left] = radii;
        let fit = [
            width / (top_left.0 + top_right.0),
            width / (bottom_left.0 + bottom_right.0),
            height / (top_left.1 + bottom_left.1),
            height / (top_right.1 + bottom_right.1),
        ]
        .into_iter()
        .filter(|scale| scale.is_finite())
        .fold(1.0_f32, f32::min);
        let [top_left, top_right, bottom_right, bottom_left] =
            [top_left, top_right, bottom_right, bottom_left].map(|(rx, ry)| (rx * fit, ry * fit));
        let quarter = std::f32::consts::FRAC_PI_2;
        let corner = |context: &mut Self,
                      center: (f32, f32),
                      radii: (f32, f32),
                      from: f32,
                      point: (f32, f32)| {
            if radii.0 > 0.0 && radii.1 > 0.0 {
                context.arc(center, radii, 0.0, (from, from + quarter), false);
            } else {
                context.line_to(point);
            }
        };
        self.move_to((x + top_left.0, y));
        self.line_to((x + width - top_right.0, y));
        corner(
            self,
            (x + width - top_right.0, y + top_right.1),
            top_right,
            -quarter,
            (x + width, y),
        );
        self.line_to((x + width, y + height - bottom_right.1));
        corner(
            self,
            (x + width - bottom_right.0, y + height - bottom_right.1),
            bottom_right,
            0.0,
            (x + width, y + height),
        );
        self.line_to((x + bottom_left.0, y + height));
        corner(
            self,
            (x + bottom_left.0, y + height - bottom_left.1),
            bottom_left,
            quarter,
            (x, y + height),
        );
        self.line_to((x, y + top_left.1));
        corner(
            self,
            (x + top_left.0, y + top_left.1),
            top_left,
            2.0 * quarter,
            (x, y),
        );
        self.close_path();
        self.move_to((x, y));
    }

    fn replay(&mut self, ops: &[super::path2d::PathOp]) {
        use super::path2d::PathOp;
        for op in ops {
            match op {
                PathOp::Move(point) => self.move_to(*point),
                PathOp::Line(point) => self.line_to(*point),
                PathOp::Cubic(first, second, end) => self.cubic_to(*first, *second, *end),
                PathOp::Quadratic(control, end) => self.quadratic_to(*control, *end),
                PathOp::Close => self.close_path(),
                PathOp::Rect(rect) => self.rect(*rect),
                PathOp::Arc(center, radii, rotation, angles, counterclockwise) => {
                    self.arc(*center, *radii, *rotation, *angles, *counterclockwise)
                }
                PathOp::ArcTo(corner, toward, radius) => self.arc_to(*corner, *toward, *radius),
                PathOp::RoundRect(rect, radii) => self.round_rect(*rect, *radii),
                PathOp::Transformed(matrix, ops) => {
                    let saved = self.state.transform;
                    self.state.transform = crate::svg::multiply(saved, *matrix);
                    self.replay(ops);
                    self.state.transform = saved;
                }
            }
        }
    }

    // a Path2D as a path in the bitmap's pixels under the transform in force, the current
    // path untouched
    fn path_from(&mut self, ops: &[super::path2d::PathOp]) -> Vec<Segment> {
        let (path, pen) = (std::mem::take(&mut self.path), self.pen.take());
        self.replay(ops);
        let made = std::mem::replace(&mut self.path, path);
        self.pen = pen;
        made
    }

    // whether a point of the bitmap lies inside the path, or under its stroke
    fn hit(&self, path: &[Segment], point: (f32, f32), even_odd: bool, stroke: bool) -> bool {
        let (width, height) = self.size();
        if !(point.0 >= 0.0 && point.1 >= 0.0 && point.0 < width as f32 && point.1 < height as f32)
        {
            return false;
        }
        let coverage = if stroke {
            let Some(back) = inverse(self.state.transform) else {
                return false;
            };
            let user = path
                .iter()
                .map(|&segment| map_segment(back, segment))
                .collect::<Vec<_>>();
            let pen = Pen {
                width: self.state.line_width,
                cap: self.state.cap,
                join: self.state.join,
                miter_limit: self.state.miter_limit,
                dash: &self.state.dash,
                dash_offset: self.state.dash_offset,
            };
            crate::svg::path_coverage(&user, self.state.transform, Some(pen), false, self.size())
        } else {
            crate::svg::path_coverage(path, IDENTITY, None, even_odd, self.size())
        };
        let Some(coverage) = coverage else {
            return false;
        };
        let (x, y) = (
            point.0 as i64 - i64::from(coverage.left),
            point.1 as i64 - i64::from(coverage.top),
        );
        if x < 0 || y < 0 || x as usize >= coverage.width {
            return false;
        }
        coverage
            .values
            .get(y as usize * coverage.width + x as usize)
            .is_some_and(|&value| value >= 128)
    }

    // a shape and the shadow it casts beneath it
    fn paint(&mut self, coverage: Option<Coverage>, paint: &Paint) {
        if let Some(coverage) = &coverage {
            self.cast_shadow(coverage, paint);
        }
        self.lay(coverage, paint);
    }

    fn casts_shadow(&self) -> bool {
        let state = &self.state;
        state.shadow_color.alpha > 0
            && (state.shadow_blur > 0.0 || state.shadow_offset != (0.0, 0.0))
    }

    // the shape's own coverage moved by the offset and blurred, a gaussian whose deviation is
    // half the blur, laid in the shadow colour as any paint is
    fn cast_shadow(&mut self, coverage: &Coverage, paint: &Paint) {
        if !self.casts_shadow() || coverage.width == 0 {
            return;
        }
        let state = &self.state;
        let (color, sigma) = (state.shadow_color, state.shadow_blur / 2.0);
        let reach = (sigma * 3.0).ceil() as i32;
        let (shift_x, shift_y) = (
            state.shadow_offset.0.round() as i32,
            state.shadow_offset.1.round() as i32,
        );
        // a paint that is itself see-through casts a fainter shadow
        let strength = match paint {
            Paint::Color(color) => f32::from(color.alpha) / 255.0,
            Paint::Gradient(..) | Paint::Pattern(..) => 1.0,
        };
        let (wide, tall) = (
            coverage.width as i32,
            (coverage.values.len() / coverage.width) as i32,
        );
        let (width, height) = ((wide + 2 * reach) as usize, (tall + 2 * reach) as usize);
        let mut alpha = vec![0.0_f32; width * height];
        for (index, &value) in coverage.values.iter().enumerate() {
            let (x, y) = (index % coverage.width, index / coverage.width);
            alpha[(y + reach as usize) * width + x + reach as usize] = f32::from(value) * strength;
        }
        if sigma > 0.0 {
            let kernel: Vec<f32> = (-reach..=reach)
                .map(|step| (-(step * step) as f32 / (2.0 * sigma * sigma)).exp())
                .collect();
            let total: f32 = kernel.iter().sum();
            let pass = |from: &[f32], along_x: bool| {
                let mut to = vec![0.0_f32; from.len()];
                for y in 0..height as i32 {
                    for x in 0..width as i32 {
                        let mut sum = 0.0;
                        for (weight, step) in kernel.iter().zip(-reach..=reach) {
                            let (sx, sy) = if along_x {
                                (x + step, y)
                            } else {
                                (x, y + step)
                            };
                            if sx >= 0 && sy >= 0 && sx < width as i32 && sy < height as i32 {
                                sum += weight * from[sy as usize * width + sx as usize];
                            }
                        }
                        to[y as usize * width + x as usize] = sum / total;
                    }
                }
                to
            };
            alpha = pass(&pass(&alpha, true), false);
        }
        // the part of the shadow's box that lands on the bitmap
        let (left, top) = (
            coverage.left - reach + shift_x,
            coverage.top - reach + shift_y,
        );
        let columns = left.max(0)..(left + width as i32).min(self.bitmap.width as i32);
        let rows = top.max(0)..(top + height as i32).min(self.bitmap.height as i32);
        if columns.is_empty() || rows.is_empty() {
            return;
        }
        let mut values = Vec::with_capacity(columns.len() * rows.len());
        for row in rows.clone() {
            for column in columns.clone() {
                let at = (row - top) as usize * width + (column - left) as usize;
                values.push(alpha[at].round().clamp(0.0, 255.0) as u8);
            }
        }
        let shadow = Coverage {
            left: columns.start,
            top: rows.start,
            width: columns.len(),
            values,
        };
        self.lay(Some(shadow), &Paint::Color(color));
    }

    // a paint through a coverage onto the bitmap: thinned by the global alpha and the clip,
    // then composited as the state says
    fn lay(&mut self, coverage: Option<Coverage>, paint: &Paint) {
        let Some(coverage) = coverage else {
            return;
        };
        // a gradient is laid out in the user space of the moment it paints
        let back = inverse(self.state.transform);
        let smooth = self.smoothing_enabled;
        let alpha = self.state.alpha;
        let composite = self.state.composite;
        let clip = self.state.clip.clone();
        let stride = self.bitmap.width as usize;
        for (index, &value) in coverage.values.iter().enumerate() {
            let x = coverage.left as usize + index % coverage.width;
            let y = coverage.top as usize + index / coverage.width;
            let at = y * stride + x;
            let reach = clip.as_ref().map_or(255.0, |clip| f32::from(clip[at]));
            let amount = (f32::from(value) * alpha * reach / 255.0).round() as u32;
            if amount == 0 {
                continue;
            }
            let premultiplied = match paint {
                Paint::Color(color) => {
                    premultiply([color.red, color.green, color.blue, color.alpha])
                }
                Paint::Gradient(gradient, _) => {
                    let Some(back) = back else {
                        return;
                    };
                    let point = crate::svg::map(back, (x as f32 + 0.5, y as f32 + 0.5));
                    match gradient.borrow().color_at(point) {
                        Some(color) => {
                            premultiply([color.red, color.green, color.blue, color.alpha])
                        }
                        None => continue,
                    }
                }
                Paint::Pattern(pattern, _) => {
                    let Some(back) = back else {
                        return;
                    };
                    match pattern.pixel_at(
                        crate::svg::map(back, (x as f32 + 0.5, y as f32 + 0.5)),
                        smooth,
                    ) {
                        Some(pixel) => pixel,
                        None => continue,
                    }
                }
            };
            let source = scale(premultiplied, amount);
            let pixel = &mut self.bitmap.pixels[at];
            if composite == Composite::SourceOver {
                source_over(pixel, source);
            } else {
                let unit = |pixel: [u8; 4]| pixel.map(|channel| f32::from(channel) / 255.0);
                let blended = composite.blend(unit(source), unit(*pixel));
                *pixel = blended.map(|channel| (channel.clamp(0.0, 1.0) * 255.0).round() as u8);
            }
        }
        self.snapshot = None;
    }

    // the clip shrinks to where it and the path's inside both reach
    fn clip_to(&mut self, path: &[Segment], even_odd: bool) {
        let (width, height) = self.size();
        let mut next = vec![0_u8; width as usize * height as usize];
        if let Some(coverage) =
            crate::svg::path_coverage(path, IDENTITY, None, even_odd, self.size())
        {
            for (index, &value) in coverage.values.iter().enumerate() {
                let x = coverage.left as usize + index % coverage.width;
                let y = coverage.top as usize + index / coverage.width;
                let at = y * width as usize + x;
                let reach = self
                    .state
                    .clip
                    .as_ref()
                    .map_or(255, |clip| u32::from(clip[at]));
                next[at] = mul255(value.into(), reach) as u8;
            }
        }
        self.state.clip = Some(Rc::new(next));
    }

    fn size(&self) -> (u32, u32) {
        (self.bitmap.width, self.bitmap.height)
    }

    fn fill_path(&mut self, path: &[Segment], even_odd: bool) {
        let coverage = crate::svg::path_coverage(path, IDENTITY, None, even_odd, self.size());
        let paint = self.state.fill_style.clone();
        self.paint(coverage, &paint);
    }

    // the pen is as wide as the transform in force now makes it, so the path goes back to
    // user space to be outlined there
    fn stroke_path(&mut self, path: &[Segment]) {
        let Some(back) = inverse(self.state.transform) else {
            return;
        };
        let user = path
            .iter()
            .map(|&segment| map_segment(back, segment))
            .collect::<Vec<_>>();
        let pen = Pen {
            width: self.state.line_width,
            cap: self.state.cap,
            join: self.state.join,
            miter_limit: self.state.miter_limit,
            dash: &self.state.dash,
            dash_offset: self.state.dash_offset,
        };
        let coverage =
            crate::svg::path_coverage(&user, self.state.transform, Some(pen), false, self.size());
        let paint = self.state.stroke_style.clone();
        self.paint(coverage, &paint);
    }

    // a rectangle of user space as a path in the bitmap's pixels, the current path untouched
    fn rect_path(&self, (x, y, width, height): Rect4) -> Vec<Segment> {
        let corner = |x, y| self.place((x, y));
        vec![
            Segment::Move(corner(x, y)),
            Segment::Line(corner(x + width, y)),
            Segment::Line(corner(x + width, y + height)),
            Segment::Line(corner(x, y + height)),
            Segment::Close,
        ]
    }

    // whether rectangles can take the area-coverage path, which draws what skia draws for
    // an axis-aligned rectangle
    fn plain(&self) -> Option<Color> {
        let state = &self.state;
        match state.fill_style {
            Paint::Color(color)
                if state.transform == IDENTITY
                    && state.alpha == 1.0
                    && state.clip.is_none()
                    && state.composite == Composite::SourceOver
                    && !self.casts_shadow() =>
            {
                Some(color)
            }
            _ => None,
        }
    }

    // an attribute of the drawing state; a value outside what the attribute takes is ignored
    pub(super) fn set_drawing_property(&mut self, property: &str, text: &str, number: f32) {
        let state = &mut self.state;
        match property {
            "fillStyle" => {
                if let Some(color) = Color::parse(text) {
                    state.fill_style = Paint::Color(color);
                }
            }
            "strokeStyle" => {
                if let Some(color) = Color::parse(text) {
                    state.stroke_style = Paint::Color(color);
                }
            }
            "lineDashOffset" if number.is_finite() => state.dash_offset = number,
            "shadowColor" => {
                if let Some(color) = Color::parse(text) {
                    state.shadow_color = color;
                }
            }
            "shadowBlur" if number.is_finite() && number >= 0.0 => state.shadow_blur = number,
            "shadowOffsetX" if number.is_finite() => state.shadow_offset.0 = number,
            "shadowOffsetY" if number.is_finite() => state.shadow_offset.1 = number,
            "font" => {
                if let Some((size, stack, style)) = crate::css::canvas_font(text) {
                    state.font = (text.trim().into(), size, stack, style);
                }
            }
            "textAlign" => {
                let known = ["start", "end", "left", "right", "center"];
                state.text_align = keyword(text, &known).unwrap_or(state.text_align);
            }
            "textBaseline" => {
                let known = [
                    "top",
                    "hanging",
                    "middle",
                    "alphabetic",
                    "ideographic",
                    "bottom",
                ];
                state.text_baseline = keyword(text, &known).unwrap_or(state.text_baseline);
            }
            "direction" => {
                let known = ["ltr", "rtl", "inherit"];
                state.direction = keyword(text, &known).unwrap_or(state.direction);
            }
            "globalCompositeOperation" => {
                if let Some(&(_, composite)) =
                    Composite::NAMES.iter().find(|(name, _)| *name == text)
                {
                    state.composite = composite;
                }
            }
            "lineWidth" if number.is_finite() && number > 0.0 => state.line_width = number,
            "miterLimit" if number.is_finite() && number > 0.0 => state.miter_limit = number,
            "globalAlpha" if (0.0..=1.0).contains(&number) => state.alpha = number,
            "lineCap" => {
                state.cap = match text {
                    "butt" => LineCap::Butt,
                    "round" => LineCap::Round,
                    "square" => LineCap::Square,
                    _ => state.cap,
                }
            }
            "lineJoin" => {
                state.join = match text {
                    "miter" => LineJoin::Miter,
                    "round" => LineJoin::Round,
                    "bevel" => LineJoin::Bevel,
                    _ => state.join,
                }
            }
            _ => {}
        }
    }

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
            levels: Default::default(),
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

    // a PNG, a lossless WebP for "image/webp", or for "image/jpeg" a JPEG blended onto black,
    // which premultiplied colour already is
    fn encode(&self, mime: &str, quality: u8) -> Option<Vec<u8>> {
        if self.pixels.is_empty() {
            return None;
        }
        if mime == "image/webp" {
            let rgba = self.image_data(0, 0, self.width, self.height);
            return crate::webp::encode_webp(self.width, self.height, &rgba);
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
            .or_insert_with(|| Rc::new(RefCell::new(CanvasContextData::new(node, width, height))))
            .clone()
    }

    // setting width or height, even to the value it had, clears the bitmap and the context
    pub(super) fn reset_canvas(&mut self, node: NodeId) {
        self.resize_webgl(node);
        let Some(context) = self.canvas_contexts.get(&node) else {
            return;
        };
        let (width, height) = canvas_size(&self.document.borrow(), node);
        let mut context = context.borrow_mut();
        let smoothing = (context.smoothing_enabled, context.smoothing_quality);
        *context = CanvasContextData::new(node, width, height);
        (context.smoothing_enabled, context.smoothing_quality) = smoothing;
    }

    // a layer for each connected canvas with a bitmap, for paint to place like an img
    pub(crate) fn canvas_layers(&self) -> Vec<ImageLayer> {
        let mut layers = self.webgl_layers();
        let document = self.document.borrow();
        let drawn = self
            .canvas_contexts
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
            .collect::<Vec<_>>();
        layers.extend(drawn);
        layers
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

    // the encoder an export type picks: JPEG and WebP as named, PNG for anything else
    fn export_mime(&mut self, value: Option<&Value>) -> Result<&'static str, ScriptError> {
        Ok(match value {
            None | Some(Value::Undefined) => "image/png",
            Some(value) => match self
                .coerce_string(value.clone())?
                .to_ascii_lowercase()
                .as_str()
            {
                "image/jpeg" => "image/jpeg",
                "image/webp" => "image/webp",
                _ => "image/png",
            },
        })
    }

    // toDataURL(type, quality) and toBlob(callback, type, quality); types other than PNG,
    // JPEG and WebP fall back to PNG, and quality outside 0 to 1 means Chromium's default of 92
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
        let mime = self.export_mime(options.first())?;
        let quality = match options.get(1) {
            Some(&Value::Number(quality)) if (0.0..=1.0).contains(&quality) => {
                (quality * 100.0 + 0.5) as u8
            }
            _ => 92,
        };
        if self
            .canvas_contexts
            .get(&node)
            .is_some_and(|context| context.borrow().tainted)
        {
            return Err(self.throw_value(dom_exception(
                "SecurityError",
                &format!(
                    "Failed to execute '{name}' on 'HTMLCanvasElement': Tainted canvases may not be exported."
                ),
            )));
        }
        let encoded = match self.canvas_bitmap(node) {
            Some(bitmap) => bitmap.encode(mime, quality),
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
            return Ok(Value::Builtin(Builtin::CanvasMethod(Box::new((
                context.clone(),
                name,
            )))));
        }
        Ok(match property {
            "canvas" => {
                let node = context.borrow().node;
                self.offscreen_object(node)
                    .unwrap_or_else(|| self.node_value(node))
            }
            "fillStyle" | "strokeStyle" => {
                let context = context.borrow();
                let paint = match property {
                    "fillStyle" => &context.state.fill_style,
                    _ => &context.state.stroke_style,
                };
                match paint {
                    Paint::Color(color) => Value::String(color_css(*color).into()),
                    Paint::Gradient(_, object) | Paint::Pattern(_, object) => object.clone(),
                }
            }
            "lineDashOffset" => Value::Number(context.borrow().state.dash_offset.into()),
            "shadowColor" => Value::String(color_css(context.borrow().state.shadow_color).into()),
            "shadowBlur" => Value::Number(context.borrow().state.shadow_blur.into()),
            "shadowOffsetX" => Value::Number(context.borrow().state.shadow_offset.0.into()),
            "shadowOffsetY" => Value::Number(context.borrow().state.shadow_offset.1.into()),
            "font" => Value::String(context.borrow().state.font.0.to_string().into()),
            "textAlign" => Value::String(context.borrow().state.text_align.into()),
            "textBaseline" => Value::String(context.borrow().state.text_baseline.into()),
            "direction" => Value::String(context.borrow().state.direction.into()),
            "globalCompositeOperation" => {
                let composite = context.borrow().state.composite;
                let name = Composite::NAMES
                    .iter()
                    .find(|(_, other)| *other == composite);
                Value::String(name.map_or("source-over", |(name, _)| name).into())
            }
            "lineWidth" => Value::Number(context.borrow().state.line_width.into()),
            "miterLimit" => Value::Number(context.borrow().state.miter_limit.into()),
            "globalAlpha" => Value::Number(context.borrow().state.alpha.into()),
            "lineCap" => Value::String(
                match context.borrow().state.cap {
                    LineCap::Butt => "butt",
                    LineCap::Round => "round",
                    LineCap::Square => "square",
                }
                .into(),
            ),
            "lineJoin" => Value::String(
                match context.borrow().state.join {
                    LineJoin::Miter => "miter",
                    LineJoin::Round => "round",
                    LineJoin::Bevel => "bevel",
                }
                .into(),
            ),
            "imageSmoothingEnabled" => Value::Bool(context.borrow().smoothing_enabled),
            "imageSmoothingQuality" => {
                Value::String(context.borrow().smoothing_quality.name().into())
            }
            _ => Value::Undefined,
        })
    }

    // the fonts the page lays out with, or the built-in face where there is no page
    fn canvas_fonts(&self) -> Rc<crate::font::FontSet> {
        self.layout_inputs
            .as_ref()
            .map_or_else(crate::font::active_fonts, |inputs| {
                inputs.borrow().fonts.clone()
            })
    }

    // a line of text in the context's font: how wide it sets, and how far its face reaches
    // above and below the baseline
    fn canvas_text_metrics(&self, context: &CanvasContextHandle, text: &str) -> (f32, f32, f32) {
        let (_, size, stack, style) = context.borrow().state.font.clone();
        let fonts = self.canvas_fonts();
        let _active = crate::font::use_fonts(fonts.clone(), 1.0);
        let ascent = fonts.ascent(stack, size);
        (
            fonts.measure(text, stack, size, style),
            ascent,
            fonts.content_height(stack, size) - ascent,
        )
    }

    // fillText and strokeText: the line set from the point as the alignment and the baseline
    // say; upright text at one scale takes the glyphs text on the page takes, and any other
    // transform, or a stroke, takes their outlines
    fn draw_canvas_text(
        &mut self,
        context: &CanvasContextHandle,
        text: &str,
        (x, y): (f32, f32),
        stroke: bool,
    ) {
        let (width, ascent, descent) = self.canvas_text_metrics(context, text);
        let fonts = self.canvas_fonts();
        let mut context = context.borrow_mut();
        let state = context.state.clone();
        let (_, size, stack, style) = state.font;
        let x = x - match state.text_align {
            "center" => width / 2.0,
            "right" | "end" => width,
            _ => 0.0,
        };
        let y = y + match state.text_baseline {
            "top" => ascent,
            "hanging" => ascent * 0.8,
            "middle" => (ascent - descent) / 2.0,
            "bottom" | "ideographic" => -descent,
            _ => 0.0,
        };
        let [a, b, c, d, _, _] = state.transform;
        let upright = b == 0.0 && c == 0.0 && a == d && a > 0.0 && !stroke;
        let _active = crate::font::use_fonts(fonts.clone(), 1.0);
        if upright && !fonts.is_builtin() {
            let device_size = size * a;
            let origin = context.place((x, y));
            let shaped = fonts.shape(text, stack, device_size, style, true);
            let (canvas_width, canvas_height) = context.size();
            // the line casts one shadow, so the glyphs gather before any of them is laid
            let mut glyphs = Vec::new();
            for glyph in &shaped.glyphs {
                let Some(face) = &glyph.face else {
                    continue;
                };
                let pen = ((origin.0 + glyph.x) * 4.0).round() / 4.0;
                let Some(bitmap) = fonts.text_glyph(face, glyph.glyph, device_size, pen, 0.0)
                else {
                    continue;
                };
                // the part of the glyph's box that lands on the canvas
                let (left, top) = (
                    pen.floor() as i32 + bitmap.left,
                    origin.1.round() as i32 + bitmap.top,
                );
                let columns = left.max(0)..(left + bitmap.width as i32).min(canvas_width as i32);
                let rows = top.max(0)..(top + bitmap.height as i32).min(canvas_height as i32);
                if columns.is_empty() || rows.is_empty() {
                    continue;
                }
                let channels = if bitmap.subpixels { 3 } else { 1 };
                let mut values = Vec::with_capacity(columns.len() * rows.len());
                for row in rows.clone() {
                    for column in columns.clone() {
                        let at = ((row - top) as usize * bitmap.width + (column - left) as usize)
                            * channels;
                        let sum: u32 = bitmap.coverage[at..at + channels]
                            .iter()
                            .map(|&value| u32::from(value))
                            .sum();
                        values.push((sum / channels as u32) as u8);
                    }
                }
                glyphs.push(Coverage {
                    left: columns.start,
                    top: rows.start,
                    width: columns.len(),
                    values,
                });
            }
            if context.casts_shadow()
                && let (Some(left), Some(top)) = (
                    glyphs.iter().map(|glyph| glyph.left).min(),
                    glyphs.iter().map(|glyph| glyph.top).min(),
                )
            {
                let edge = |far: fn(&Coverage) -> i32| glyphs.iter().map(far).max().unwrap_or(0);
                let right = edge(|glyph| glyph.left + glyph.width as i32);
                let bottom =
                    edge(|glyph| glyph.top + (glyph.values.len() / glyph.width.max(1)) as i32);
                let width = (right - left) as usize;
                let mut values = vec![0_u8; width * (bottom - top) as usize];
                for glyph in &glyphs {
                    for (index, &value) in glyph.values.iter().enumerate() {
                        let x = (glyph.left - left) as usize + index % glyph.width;
                        let y = (glyph.top - top) as usize + index / glyph.width;
                        let slot = &mut values[y * width + x];
                        *slot = (*slot).max(value);
                    }
                }
                let line = Coverage {
                    left,
                    top,
                    width,
                    values,
                };
                context.cast_shadow(&line, &state.fill_style);
            }
            for glyph in glyphs {
                context.lay(Some(glyph), &state.fill_style);
            }
            return;
        }
        // the outlines as a path in user space, each glyph where the line sets it
        let shaped = fonts.shape(text, stack, size, style, true);
        let mut path = Vec::new();
        for glyph in &shaped.glyphs {
            let Some(face) = &glyph.face else {
                continue;
            };
            let mut last = None;
            for (from, to) in crate::font::glyph_edges(&face.face, glyph.glyph, size) {
                let at = |point: (f32, f32)| (x + glyph.x + point.0, y + point.1);
                if last != Some(from) {
                    path.push(Segment::Move(at(from)));
                }
                path.push(Segment::Line(at(to)));
                last = Some(to);
            }
        }
        let pen = stroke.then(|| Pen {
            width: state.line_width,
            cap: state.cap,
            join: state.join,
            miter_limit: state.miter_limit,
            dash: &state.dash,
            dash_offset: state.dash_offset,
        });
        let size = context.size();
        let coverage = crate::svg::path_coverage(&path, state.transform, pen, false, size);
        let paint = if stroke {
            &state.stroke_style
        } else {
            &state.fill_style
        };
        context.paint(coverage, paint);
    }

    // a fill or stroke style given as a gradient the context made; anything else is a colour
    pub(super) fn set_canvas_gradient(
        &mut self,
        context: &CanvasContextHandle,
        property: &str,
        value: &Value,
    ) -> bool {
        if !matches!(property, "fillStyle" | "strokeStyle") || !matches!(value, Value::Object(_)) {
            return false;
        }
        let paint = match (
            self.get_property(value, "addColorStop"),
            self.get_property(value, "setTransform"),
        ) {
            (Ok(Value::Builtin(Builtin::CanvasGradientAddColorStop(gradient))), _) => {
                Paint::Gradient(gradient, value.clone())
            }
            (_, Ok(Value::Builtin(Builtin::CanvasPatternSetTransform(pattern)))) => {
                Paint::Pattern(pattern, value.clone())
            }
            _ => return false,
        };
        let mut context = context.borrow_mut();
        match property {
            "fillStyle" => context.state.fill_style = paint,
            _ => context.state.stroke_style = paint,
        }
        true
    }

    // createPattern(image, repetition): null while the image has nothing to show
    fn create_pattern(&mut self, arguments: &[Value]) -> Result<Value, ScriptError> {
        if arguments.len() < 2 {
            return Err(self.type_error(format!(
                "Failed to execute 'createPattern' on 'CanvasRenderingContext2D': 2 arguments required, but only {} present.",
                arguments.len()
            )));
        }
        let repetition = match &arguments[1] {
            Value::Null => String::new(),
            value => self.coerce_string(value.clone())?,
        };
        let repeat = match repetition.as_str() {
            "" | "repeat" => (true, true),
            "repeat-x" => (true, false),
            "repeat-y" => (false, true),
            "no-repeat" => (false, false),
            _ => {
                return Err(self.throw_value(dom_exception(
                    "SyntaxError",
                    &format!("Failed to execute 'createPattern' on 'CanvasRenderingContext2D': The provided type ('{repetition}') is not one of 'repeat', 'no-repeat', 'repeat-x', or 'repeat-y'."),
                )));
            }
        };
        let Some((bitmap, _)) = self.canvas_source(&arguments[0])? else {
            return Ok(Value::Null);
        };
        let pattern = Rc::new(Pattern {
            bitmap,
            repeat,
            transform: Cell::new(IDENTITY),
        });
        Ok(self.host_object(pattern_object(pattern), "CanvasPattern"))
    }

    // CanvasPattern.setTransform(matrix): a DOMMatrix2DInit, the identity when left out
    pub(super) fn set_pattern_transform(
        &mut self,
        pattern: &PatternHandle,
        arguments: &[Value],
    ) -> Result<Value, ScriptError> {
        let mut matrix = IDENTITY;
        if let Some(init @ Value::Object(_)) = arguments.first() {
            for (index, (short, long)) in [
                ("a", "m11"),
                ("b", "m12"),
                ("c", "m21"),
                ("d", "m22"),
                ("e", "m41"),
                ("f", "m42"),
            ]
            .into_iter()
            .enumerate()
            {
                let value = match self.get_property(init, short)? {
                    Value::Undefined => self.get_property(init, long)?,
                    value => value,
                };
                if !matches!(value, Value::Undefined) {
                    matrix[index] = self.coerce_number(value)? as f32;
                }
            }
        }
        if matrix.iter().all(|value| value.is_finite()) {
            pattern.transform.set(matrix);
        }
        Ok(Value::Undefined)
    }

    // addColorStop(offset, colour): stops stay in offset order, a later one after its equals
    pub(super) fn add_color_stop(
        &mut self,
        gradient: &GradientHandle,
        arguments: &[Value],
    ) -> Result<Value, ScriptError> {
        let offset = arguments.first().cloned().unwrap_or(Value::Undefined);
        let offset = self.coerce_number(offset)? as f32;
        if !(0.0..=1.0).contains(&offset) {
            return Err(self.throw_value(dom_exception(
                "IndexSizeError",
                "Failed to execute 'addColorStop' on 'CanvasGradient': The provided value is outside the range (0.0, 1.0).",
            )));
        }
        let text = arguments
            .get(1)
            .map(Value::string_value)
            .unwrap_or_default();
        let Some(color) = Color::parse(&text) else {
            return Err(self.throw_value(dom_exception(
                "SyntaxError",
                &format!(
                    "Failed to execute 'addColorStop' on 'CanvasGradient': The value provided ('{text}') could not be parsed as a color."
                ),
            )));
        };
        let mut gradient = gradient.borrow_mut();
        let at = gradient.stops.partition_point(|stop| stop.0 <= offset);
        gradient.stops.insert(at, (offset, color));
        Ok(Value::Undefined)
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
        // how many numbers each of the path and transform calls takes
        let takes = match name {
            "scale" | "translate" | "moveTo" | "lineTo" => 2,
            "rotate" => 1,
            "transform" | "bezierCurveTo" => 6,
            "quadraticCurveTo" | "rect" | "strokeRect" | "fillRect" | "clearRect" => 4,
            "arc" | "arcTo" => 5,
            "ellipse" => 7,
            _ => 0,
        };
        if takes > 0 {
            let Some(numbers) = self.canvas_numbers(name, arguments, takes)? else {
                return Ok(Value::Undefined);
            };
            let n = |index: usize| numbers[index];
            let counterclockwise = arguments.get(takes).is_some_and(Value::truthy);
            let mut context = context.borrow_mut();
            let apply = |context: &mut CanvasContextData, matrix: Affine| {
                context.state.transform = crate::svg::multiply(context.state.transform, matrix);
            };
            match name {
                "scale" => apply(&mut context, [n(0), 0.0, 0.0, n(1), 0.0, 0.0]),
                "translate" => apply(&mut context, [1.0, 0.0, 0.0, 1.0, n(0), n(1)]),
                "rotate" => {
                    let (sin, cos) = n(0).sin_cos();
                    apply(&mut context, [cos, sin, -sin, cos, 0.0, 0.0]);
                }
                "transform" => apply(&mut context, [n(0), n(1), n(2), n(3), n(4), n(5)]),
                "moveTo" => context.move_to((n(0), n(1))),
                "lineTo" => context.line_to((n(0), n(1))),
                "bezierCurveTo" => context.cubic_to((n(0), n(1)), (n(2), n(3)), (n(4), n(5))),
                "quadraticCurveTo" => context.quadratic_to((n(0), n(1)), (n(2), n(3))),
                "rect" => context.rect((n(0), n(1), n(2), n(3))),
                "arcTo" => {
                    if n(4) < 0.0 {
                        drop(context);
                        return Err(self.throw_value(dom_exception(
                            "IndexSizeError",
                            "Failed to execute 'arcTo' on 'CanvasRenderingContext2D': The radius provided is negative.",
                        )));
                    }
                    context.arc_to((n(0), n(1)), (n(2), n(3)), n(4));
                }
                "arc" | "ellipse" => {
                    let (radii, rotation, angles) = match name {
                        "arc" => ((n(2), n(2)), 0.0, (n(3), n(4))),
                        _ => ((n(2), n(3)), n(4), (n(5), n(6))),
                    };
                    if radii.0 < 0.0 || radii.1 < 0.0 {
                        drop(context);
                        return Err(self.throw_value(dom_exception(
                            "IndexSizeError",
                            &format!(
                                "Failed to execute '{name}' on 'CanvasRenderingContext2D': The radius provided is negative."
                            ),
                        )));
                    }
                    context.arc((n(0), n(1)), radii, rotation, angles, counterclockwise);
                }
                "strokeRect" => {
                    let path = context.rect_path((n(0), n(1), n(2), n(3)));
                    context.stroke_path(&path);
                }
                "fillRect" if context.plain().is_some() => {
                    let fill = context.plain().unwrap_or(Color::BLACK);
                    context.bitmap.fill_rect((n(0), n(1), n(2), n(3)), fill);
                    context.snapshot = None;
                }
                "fillRect" => {
                    let path = context.rect_path((n(0), n(1), n(2), n(3)));
                    context.fill_path(&path, false);
                }
                "clearRect" if context.state.transform == IDENTITY => {
                    context.bitmap.clear_rect((n(0), n(1), n(2), n(3)));
                    context.snapshot = None;
                }
                _ => {
                    let path = context.rect_path((n(0), n(1), n(2), n(3)));
                    let size = context.size();
                    if let Some(coverage) =
                        crate::svg::path_coverage(&path, IDENTITY, None, false, size)
                    {
                        let stride = size.0 as usize;
                        for (index, &value) in coverage.values.iter().enumerate() {
                            let x = coverage.left as usize + index % coverage.width;
                            let y = coverage.top as usize + index / coverage.width;
                            let pixel = &mut context.bitmap.pixels[y * stride + x];
                            *pixel = scale(*pixel, 255 - u32::from(value));
                        }
                        context.snapshot = None;
                    }
                }
            }
            return Ok(Value::Undefined);
        }
        match name {
            "save" => {
                let mut context = context.borrow_mut();
                let state = context.state.clone();
                context.saved.push(state);
                Ok(Value::Undefined)
            }
            "restore" => {
                let mut context = context.borrow_mut();
                if let Some(state) = context.saved.pop() {
                    context.state = state;
                }
                Ok(Value::Undefined)
            }
            "resetTransform" => {
                context.borrow_mut().state.transform = IDENTITY;
                Ok(Value::Undefined)
            }
            // six numbers, a matrix that names them a to f, or nothing for the identity
            "setTransform" => {
                let mut matrix = IDENTITY;
                let first = arguments.first().cloned().unwrap_or(Value::Undefined);
                let named = matches!(first, Value::Object(_));
                if named || arguments.len() >= 6 {
                    for (index, key) in ["a", "b", "c", "d", "e", "f"].into_iter().enumerate() {
                        let value = match named {
                            true => self.get_property(&first, key)?,
                            false => arguments[index].clone(),
                        };
                        matrix[index] = self.coerce_number(value)? as f32;
                    }
                }
                if matrix.iter().all(|value| value.is_finite()) {
                    context.borrow_mut().state.transform = matrix;
                }
                Ok(Value::Undefined)
            }
            "getTransform" => {
                let matrix = context.borrow().state.transform;
                Ok(self.matrix_2d(matrix.map(f64::from)))
            }
            "beginPath" => {
                let mut context = context.borrow_mut();
                context.path.clear();
                context.pen = None;
                Ok(Value::Undefined)
            }
            "closePath" => {
                context.borrow_mut().close_path();
                Ok(Value::Undefined)
            }
            "fill" => {
                let even_odd = arguments
                    .iter()
                    .any(|argument| argument.string_value() == "evenodd");
                let given = arguments.first().and_then(Self::path_of);
                let mut context = context.borrow_mut();
                let path = match given {
                    Some(ops) => context.path_from(&ops.borrow()),
                    None => context.path.clone(),
                };
                context.fill_path(&path, even_odd);
                Ok(Value::Undefined)
            }
            "stroke" => {
                let given = arguments.first().and_then(Self::path_of);
                let mut context = context.borrow_mut();
                let path = match given {
                    Some(ops) => context.path_from(&ops.borrow()),
                    None => context.path.clone(),
                };
                context.stroke_path(&path);
                Ok(Value::Undefined)
            }
            "roundRect" => {
                let Some(numbers) = self.canvas_numbers(name, arguments, 4)? else {
                    return Ok(Value::Undefined);
                };
                let Some(radii) = self.round_rect_radii("roundRect", arguments.get(4))? else {
                    return Ok(Value::Undefined);
                };
                context
                    .borrow_mut()
                    .round_rect((numbers[0], numbers[1], numbers[2], numbers[3]), radii);
                Ok(Value::Undefined)
            }
            "isPointInPath" | "isPointInStroke" => {
                let given = arguments.first().and_then(Self::path_of);
                let rest = if given.is_some() {
                    arguments.get(1..).unwrap_or_default()
                } else {
                    arguments
                };
                let Some(point) = self.canvas_numbers(name, rest, 2)? else {
                    return Ok(Value::Bool(false));
                };
                let even_odd = rest
                    .get(2)
                    .is_some_and(|rule| rule.string_value() == "evenodd");
                let mut context = context.borrow_mut();
                let path = match given {
                    Some(ops) => context.path_from(&ops.borrow()),
                    None => context.path.clone(),
                };
                Ok(Value::Bool(context.hit(
                    &path,
                    (point[0], point[1]),
                    even_odd,
                    name == "isPointInStroke",
                )))
            }
            "fillText" | "strokeText" => {
                let text = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default();
                let rest = arguments.get(1..).unwrap_or_default();
                if let Some(&[x, y]) = self.canvas_numbers(name, rest, 2)?.as_deref() {
                    self.draw_canvas_text(context, &text, (x, y), name == "strokeText");
                }
                Ok(Value::Undefined)
            }
            "measureText" => {
                let text = arguments
                    .first()
                    .map(Value::string_value)
                    .unwrap_or_default();
                let (width, ascent, descent) = self.canvas_text_metrics(context, &text);
                let align = context.borrow().state.text_align;
                let left = match align {
                    "center" => width / 2.0,
                    "right" | "end" => width,
                    _ => 0.0,
                };
                let metrics = plain_object(
                    [
                        ("width", width),
                        ("actualBoundingBoxLeft", left),
                        ("actualBoundingBoxRight", width - left),
                        ("actualBoundingBoxAscent", ascent),
                        ("actualBoundingBoxDescent", descent),
                        ("fontBoundingBoxAscent", ascent),
                        ("fontBoundingBoxDescent", descent),
                        ("emHeightAscent", ascent),
                        ("emHeightDescent", descent),
                        ("hangingBaseline", ascent * 0.8),
                        ("alphabeticBaseline", 0.0),
                        ("ideographicBaseline", -descent),
                    ]
                    .map(|(key, value)| (key.to_string(), Value::Number(value.into()))),
                );
                Ok(self.host_object(metrics, "TextMetrics"))
            }
            "clip" => {
                let even_odd = arguments
                    .iter()
                    .any(|argument| argument.string_value() == "evenodd");
                let given = arguments.first().and_then(Self::path_of);
                let mut context = context.borrow_mut();
                let path = match given {
                    Some(ops) => context.path_from(&ops.borrow()),
                    None => context.path.clone(),
                };
                context.clip_to(&path, even_odd);
                Ok(Value::Undefined)
            }
            // an odd count repeats to make a whole pattern, and a negative or unreal length
            // leaves the pattern as it was
            "setLineDash" => {
                let list = arguments.first().cloned().unwrap_or(Value::Undefined);
                let mut dash = Vec::new();
                for length in self.iterate_to_vec(list)? {
                    dash.push(self.coerce_number(length)? as f32);
                }
                if dash
                    .iter()
                    .all(|length| length.is_finite() && *length >= 0.0)
                {
                    if dash.len() % 2 == 1 {
                        dash.extend_from_within(..);
                    }
                    context.borrow_mut().state.dash = dash;
                }
                Ok(Value::Undefined)
            }
            "getLineDash" => Ok(Value::Array(Rc::new(RefCell::new(
                context
                    .borrow()
                    .state
                    .dash
                    .iter()
                    .map(|&length| Value::Number(length.into()))
                    .collect(),
            )))),
            "createLinearGradient" | "createRadialGradient" => {
                let radial = name == "createRadialGradient";
                let count = if radial { 6 } else { 4 };
                let Some(numbers) = self.canvas_numbers(name, arguments, count)? else {
                    return Err(self.type_error(format!(
                        "Failed to execute '{name}' on 'CanvasRenderingContext2D': The provided double value is non-finite."
                    )));
                };
                let (from, to) = match radial {
                    true => (
                        (numbers[0], numbers[1], numbers[2]),
                        (numbers[3], numbers[4], numbers[5]),
                    ),
                    false => ((numbers[0], numbers[1], 0.0), (numbers[2], numbers[3], 0.0)),
                };
                Ok(self.host_object(
                    gradient_object(Rc::new(RefCell::new(Gradient {
                        from,
                        to,
                        shape: if radial { Shape::Radial } else { Shape::Linear },
                        stops: Vec::new(),
                    }))),
                    "CanvasGradient",
                ))
            }
            "createConicGradient" => {
                let Some(numbers) = self.canvas_numbers(name, arguments, 3)? else {
                    return Err(self.type_error(format!(
                        "Failed to execute '{name}' on 'CanvasRenderingContext2D': The provided double value is non-finite."
                    )));
                };
                Ok(self.host_object(
                    gradient_object(Rc::new(RefCell::new(Gradient {
                        from: (numbers[1], numbers[2], numbers[0]),
                        to: (numbers[1], numbers[2], 0.0),
                        shape: Shape::Conic,
                        stops: Vec::new(),
                    }))),
                    "CanvasGradient",
                ))
            }
            "createPattern" => self.create_pattern(arguments),
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
    // a picture of an ImageData, a canvas, an image, a video or an encoded image blob, kept
    // apart from what it was taken of
    pub(super) fn create_image_bitmap(
        &mut self,
        arguments: &[Value],
    ) -> Result<Value, ScriptError> {
        let source = arguments
            .first()
            .cloned()
            .unwrap_or(Value::Undefined)
            .resolve();
        let decoded = |bytes: &[u8], mime_type: &str| {
            crate::image::decode_image(bytes, mime_type)
                .ok()
                .map(|image| Bitmap::from_image(&image))
        };
        let bitmap = match &source {
            Value::Element(_) => self.canvas_source(&source)?.map(|(bitmap, _)| bitmap),
            offscreen if Self::offscreen_element(offscreen).is_some() => {
                self.canvas_source(&source)?.map(|(bitmap, _)| bitmap)
            }
            Value::Blob(blob) => decoded(&blob.bytes, &blob.mime_type),
            Value::File(file) => decoded(&file.bytes, &file.mime_type),
            data => {
                let width = self.get_property(data, "width")?;
                let width = self.coerce_uint32(width)?;
                let height = self.get_property(data, "height")?;
                let height = self.coerce_uint32(height)?;
                let bytes = match self.get_property(data, "data")? {
                    Value::TypedArray(array) => {
                        let array = array.borrow();
                        array
                            .buffer
                            .borrow()
                            .get(super::typed::byte_range(&array))
                            .map(<[u8]>::to_vec)
                    }
                    _ => None,
                };
                bytes
                    .filter(|bytes| {
                        width > 0 && bytes.len() == width as usize * height as usize * 4
                    })
                    .map(|bytes| {
                        let mut bitmap = Bitmap::new(width, height);
                        let whole = (0.0, 0.0, width as f32, height as f32);
                        bitmap.put_image_data(&bytes, (width, height), (0, 0), whole);
                        bitmap
                    })
            }
        };
        Ok(Value::Promise(match bitmap {
            Some(bitmap) => self.fulfilled_promise(self.image_bitmap_object(bitmap)),
            None => self.rejected_promise_value(dom_exception(
                "InvalidStateError",
                "The source image could not be decoded.",
            )),
        }))
    }

    // the pixels a canvas holds, when anything has drawn on it: its 2D bitmap or what its
    // WebGL context draws
    pub(super) fn canvas_bitmap(&self, node: NodeId) -> Option<Bitmap> {
        self.canvas_contexts
            .get(&node)
            .map(|context| context.borrow().bitmap.clone())
            .or_else(|| {
                self.webgl_image(node)
                    .map(|image| Bitmap::from_image(&image))
            })
    }

    // the pixels a WebGL upload takes from an image source: rows from the top, alpha not
    // multiplied in; None while the source has nothing to show
    pub(super) fn texture_source(
        &mut self,
        source: &Value,
    ) -> Result<Option<TexturePixels>, ScriptError> {
        let straight = |bitmap: Bitmap| {
            let pixels = bitmap
                .pixels
                .iter()
                .map(|&pixel| unpremultiply(pixel))
                .collect();
            (bitmap.width, bitmap.height, pixels)
        };
        let from_image = |image: std::sync::Arc<crate::image::Image>| {
            let pixels = image
                .pixels
                .iter()
                .map(|color| [color.red, color.green, color.blue, color.alpha])
                .collect();
            (image.width, image.height, pixels)
        };
        if let Value::Object(_) = source
            && Self::offscreen_element(source).is_none()
            && let Ok(Value::TypedArray(array)) = self.get_property(source, "data")
            && array.borrow().kind == ElementKind::Uint8Clamped
        {
            let width = self.get_property(source, "width")?;
            let width = self.coerce_uint32(width)?;
            let height = self.get_property(source, "height")?;
            let height = self.coerce_uint32(height)?;
            let array = array.borrow();
            let bytes = array.buffer.borrow();
            let bytes = &bytes[super::typed::byte_range(&array)];
            let pixels = bytes.as_chunks::<4>().0.to_vec();
            return Ok(Some((width, height, pixels)));
        }
        if let Value::Element(handle) = source {
            let node = handle.id;
            let document = self.document.borrow();
            let (image, video) = (
                is_html(&document, node, "img"),
                is_html(&document, node, "video"),
            );
            drop(document);
            if image {
                return Ok(self.image_content(node).ok().flatten().map(from_image));
            }
            if video {
                return Ok(self.video_content(node).map(from_image));
            }
        }
        Ok(self
            .canvas_source(source)?
            .map(|(bitmap, _)| straight(bitmap)))
    }

    // an ImageBitmap holding pixels until it is closed
    pub(super) fn image_bitmap_object(&self, bitmap: Bitmap) -> Value {
        let (width, height) = (bitmap.width, bitmap.height);
        let held = Rc::new(RefCell::new(Some(bitmap)));
        let bitmap = plain_object([
            ("width".to_string(), Value::Number(f64::from(width))),
            ("height".to_string(), Value::Number(f64::from(height))),
            (
                "close".to_string(),
                Value::Builtin(Builtin::ImageBitmapClose(held)),
            ),
        ]);
        self.host_object(bitmap, "ImageBitmap")
    }

    // a canvas's pixels as a PNG or JPEG blob, for OffscreenCanvas.convertToBlob
    pub(super) fn canvas_blob(
        &mut self,
        node: NodeId,
        kind: Value,
        quality: Value,
    ) -> Result<Value, ScriptError> {
        let mime = self.export_mime(Some(&kind))?;
        let quality = match quality {
            Value::Number(quality) if (0.0..=1.0).contains(&quality) => {
                (quality * 100.0 + 0.5) as u8
            }
            _ => 92,
        };
        if self
            .canvas_contexts
            .get(&node)
            .is_some_and(|context| context.borrow().tainted)
        {
            return Ok(Value::Promise(self.rejected_promise_value(dom_exception(
                "SecurityError",
                "Failed to execute 'convertToBlob' on 'OffscreenCanvas': Tainted OffscreenCanvas may not be exported.",
            ))));
        }
        let encoded = match self.canvas_bitmap(node) {
            Some(bitmap) => bitmap.encode(mime, quality),
            None => {
                let (width, height) = canvas_size(&self.document.borrow(), node);
                Bitmap::new(width, height).encode(mime, quality)
            }
        };
        Ok(Value::Promise(match encoded {
            Some(bytes) => self.fulfilled_promise(Value::Blob(Rc::new(BlobData {
                bytes,
                mime_type: mime.to_string(),
            }))),
            None => self.rejected_promise_value(dom_exception(
                "EncodingError",
                "Failed to execute 'convertToBlob' on 'OffscreenCanvas': Encoding of the canvas failed.",
            )),
        }))
    }

    fn canvas_source(&mut self, source: &Value) -> Result<Option<Measured>, ScriptError> {
        // an OffscreenCanvas draws as the canvas element behind it
        let behind = Self::offscreen_element(source).map(Value::Element);
        let source = behind.as_ref().unwrap_or(source);
        // an ImageBitmap is known by the close that carries its pixels
        if !matches!(source, Value::Element(_))
            && let Ok(Value::Builtin(Builtin::ImageBitmapClose(held))) =
                self.get_property(source, "close")
        {
            let Some(bitmap) = held.borrow().clone() else {
                return Err(self.throw_value(dom_exception(
                    "InvalidStateError",
                    "Failed to execute 'drawImage' on 'CanvasRenderingContext2D': The image source is detached.",
                )));
            };
            let size = (bitmap.width as f32, bitmap.height as f32);
            return Ok(Some((bitmap, size)));
        }
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
                Ok(self.canvas_bitmap(node))
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
        let taints = match &arguments[0] {
            Value::Element(handle) => self.taints_canvas(handle.id),
            _ => false,
        };
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
        context.tainted |= taints;
        Ok(())
    }

    // whether drawing the element leaves a canvas unreadable: a tainted canvas does, and so
    // does an image or a video of another origin that was not asked for with cors, which
    // only the app's own files answer
    fn taints_canvas(&self, node: NodeId) -> bool {
        if let Some(context) = self.canvas_contexts.get(&node) {
            return context.borrow().tainted;
        }
        let document = self.document.borrow();
        let source = document.attribute(node, "src").or_else(|| {
            document
                .element_children(node)
                .find(|&child| is_html(&document, child, "source"))
                .and_then(|child| document.attribute(child, "src"))
        });
        let Some(source) = source.map(str::trim) else {
            return false;
        };
        let scheme = |name: &str| {
            source
                .get(..name.len())
                .is_some_and(|start| start.eq_ignore_ascii_case(name))
        };
        if scheme("data:") || scheme("blob:") {
            return false;
        }
        let location = self.location.borrow();
        let base = crate::weburl::Url::parse(&location.to_string(), None);
        let Some(url) = crate::weburl::Url::parse(source, base.as_ref()) else {
            return false;
        };
        let origin = url.origin();
        origin != location.origin()
            && !(document.attribute(node, "crossorigin").is_some()
                && origin == crate::navigation::FILES_ORIGIN)
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
        if context.borrow().tainted {
            return Err(self.throw_value(dom_exception(
                "SecurityError",
                "Failed to execute 'getImageData' on 'CanvasRenderingContext2D': The canvas has been tainted by cross-origin data.",
            )));
        }
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

    // new ImageData(width, height), or new ImageData(data, width[, height]) over pixels a
    // typed array already holds
    pub(super) fn construct_image_data(
        &mut self,
        arguments: &[Value],
    ) -> Result<Value, ScriptError> {
        let Some(Value::TypedArray(array)) = arguments.first() else {
            return self.create_image_data(arguments.get(..2).unwrap_or(arguments));
        };
        let length = {
            let array = array.borrow();
            super::typed::byte_range(&array).len()
        };
        let width = arguments.get(1).cloned().unwrap_or(Value::Undefined);
        let width = self.coerce_uint32(width)? as usize;
        let fits = width != 0 && length != 0 && length % (width * 4) == 0;
        let height = if fits { length / (width * 4) } else { 0 };
        let asked = match arguments.get(2) {
            Some(height) => self.coerce_uint32(height.clone())? as usize,
            None => height,
        };
        if !fits || asked != height {
            return Err(self.throw_value(dom_exception(
                "IndexSizeError",
                "Failed to construct 'ImageData': The input data length is not a multiple of (4 * width), or does not match the height.",
            )));
        }
        Ok(plain_object([
            ("width".to_string(), Value::Number(width as f64)),
            ("height".to_string(), Value::Number(height as f64)),
            ("data".to_string(), Value::TypedArray(array.clone())),
            ("colorSpace".to_string(), Value::String("srgb".into())),
        ]))
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
                    .get(super::typed::byte_range(&array))
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

// a CanvasPattern as script holds it: its one method, which carries the pattern
fn pattern_object(pattern: PatternHandle) -> Value {
    plain_object([(
        "setTransform".to_string(),
        Value::Builtin(Builtin::CanvasPatternSetTransform(pattern)),
    )])
}

// a CanvasGradient as script holds it: its one method, which carries the gradient
fn gradient_object(gradient: GradientHandle) -> Value {
    plain_object([(
        "addColorStop".to_string(),
        Value::Builtin(Builtin::CanvasGradientAddColorStop(gradient)),
    )])
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
                Rc::new(RefCell::new(data.into())),
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
    fn paths_fill_and_stroke_under_the_transform_and_state_that_save_keeps() {
        let document = crate::dom::Document::parse(
            r#"<html><body><canvas id="c" width="20" height="20"></canvas><p id="status"></p></body></html>"#,
        )
        .expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r##"
                const c = document.getElementById("c").getContext("2d");
                const at = (x, y) => [...c.getImageData(x, y, 1, 1).data].join(",");
                const seen = [];
                c.fillStyle = "#f00";
                c.beginPath();
                c.moveTo(2, 2);
                c.lineTo(10, 2);
                c.lineTo(10, 10);
                c.lineTo(2, 10);
                c.fill();
                seen.push(at(5, 5), at(12, 5));
                c.save();
                c.translate(10, 10);
                c.scale(2, 2);
                c.fillStyle = "#00f";
                c.fillRect(1, 1, 2, 2);
                c.restore();
                seen.push(at(13, 13), at(17, 17), c.fillStyle);
                c.strokeStyle = "#0f0";
                c.lineWidth = 2;
                c.beginPath();
                c.moveTo(0, 15);
                c.lineTo(8, 15);
                c.stroke();
                seen.push(at(4, 14), at(4, 15), at(4, 17));
                c.globalAlpha = 0.5;
                c.fillStyle = "#000";
                c.fillRect(16, 0, 2, 2);
                seen.push(at(16, 0));
                c.globalAlpha = 1;
                c.beginPath();
                c.arc(10, 4, 3, 0, Math.PI * 2);
                c.fillStyle = "#fff";
                c.fill("evenodd");
                seen.push(at(10, 4), at(14, 4));
                c.setTransform(2, 0, 0, 2, 1, 1);
                const matrix = c.getTransform();
                c.resetTransform();
                c.lineWidth = -1;
                c.lineCap = "pointed";
                seen.push(matrix.a, matrix.e, c.getTransform().a, c.unknownThing, c.lineWidth, c.lineCap);
                c.restore();
                document.getElementById("status").textContent = seen.join(" ");
                "##,
            )
            .expect("canvas drawing should run");
        let document = document.borrow();
        let status = document.find_element_by_id("status").expect("status");
        assert_eq!(
            document.text_content(status),
            "255,0,0,255 0,0,0,0 0,0,255,255 0,0,0,0 #ff0000 0,255,0,255 0,255,0,255 0,0,0,0 0,0,0,128 255,255,255,255 0,0,0,0 2 1 1  2 butt"
        );
    }

    #[test]
    fn a_blob_url_loads_as_an_image_until_it_is_revoked() {
        let document = crate::dom::Document::parse(
            r#"<html><body><canvas id="c" width="6" height="4"></canvas><p id="status"></p></body></html>"#,
        )
        .expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime.set_location(
            crate::navigation::AppUrl::parse("https://app.ceri.invalid/").expect("url"),
        );
        runtime
            .execute(
                r##"
                const status = document.getElementById("status");
                const seen = [];
                const show = () => (status.textContent = seen.join("|"));
                const text = atob(document.getElementById("c").toDataURL().split(",")[1]);
                const bytes = Uint8Array.from(text, (c) => c.charCodeAt(0));
                const url = URL.createObjectURL(new Blob([bytes], { type: "image/png" }));
                seen.push(url.startsWith("blob:https://app.ceri.invalid/"), url.length);
                const first = new Image();
                first.onload = () => {
                    seen.push(first.naturalWidth + "x" + first.naturalHeight);
                    URL.revokeObjectURL(url);
                    const second = new Image();
                    second.onload = () => (seen.push("loaded again"), show());
                    second.onerror = () => (seen.push("gone"), show());
                    second.src = url + "#again";
                    show();
                };
                first.onerror = () => (seen.push("failed"), show());
                first.src = url;
                try { URL.createObjectURL("text"); } catch (error) { seen.push(error.name); }
                show();
                "##,
            )
            .expect("script should run");
        for _ in 0..4 {
            runtime.advance_time(1).expect("image tasks should run");
        }
        let status = document.borrow().find_element_by_id("status").unwrap();
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("true|66|TypeError|6x4|gone")
        );
    }

    #[test]
    fn an_image_bitmap_draws_until_it_is_closed() {
        let document = crate::dom::Document::parse(
            r#"<html><body><canvas id="c" width="8" height="8"></canvas><p id="status"></p></body></html>"#,
        )
        .expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r##"
                const c = document.getElementById("c").getContext("2d");
                const status = document.getElementById("status");
                const data = new ImageData(new Uint8ClampedArray([255, 0, 0, 255, 0, 0, 255, 128]), 2, 1);
                createImageBitmap(data).then((bitmap) => {
                    const seen = [bitmap.width + "x" + bitmap.height];
                    c.drawImage(bitmap, 3, 3);
                    seen.push([...c.getImageData(3, 3, 2, 1).data].join(","));
                    bitmap.close();
                    try { c.drawImage(bitmap, 0, 0); } catch (error) { seen.push(error.name); }
                    return createImageBitmap(document.getElementById("c")).then((copy) => {
                        seen.push(copy.width);
                        return createImageBitmap({}).catch((error) => seen.push(error.name));
                    }).then(() => (status.textContent = seen.join("|")));
                });
                "##,
            )
            .expect("script should run");
        runtime.advance_time(1).expect("promises should settle");
        let status = document.borrow().find_element_by_id("status").unwrap();
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some("2x1|255,0,0,255,0,0,255,128|InvalidStateError|8|InvalidStateError")
        );
    }

    #[test]
    fn a_shape_casts_its_shadow_moved_and_blurred() {
        let document = crate::dom::Document::parse(
            r#"<html><body><canvas id="c" width="40" height="40"></canvas><p id="status"></p></body></html>"#,
        )
        .expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r##"
                const c = document.getElementById("c").getContext("2d");
                const at = (x, y) => [...c.getImageData(x, y, 1, 1).data].join(",");
                c.fillStyle = "rgba(10, 20, 30, 0.5)";
                const seen = [c.shadowColor, c.fillStyle, c.shadowBlur, c.shadowOffsetX];
                c.shadowColor = "rgba(0, 0, 255, 0.5)";
                c.shadowOffsetX = 10;
                c.shadowOffsetY = 4;
                c.fillStyle = "#f00";
                c.fillRect(4, 4, 8, 8);
                // the shape, its shadow beside it, and the shape over its own shadow
                seen.push(at(5, 5), at(20, 14), at(11, 9), at(30, 30));
                c.clearRect(0, 0, 40, 40);
                c.shadowOffsetX = 0;
                c.shadowOffsetY = 0;
                c.shadowColor = "#000";
                c.shadowBlur = 8;
                c.fillRect(16, 16, 8, 8);
                // the blur spreads past the shape and fades with distance
                const near = c.getImageData(13, 20, 1, 1).data[3];
                const far = c.getImageData(8, 20, 1, 1).data[3];
                seen.push(near > far, far > 0, c.getImageData(1, 20, 1, 1).data[3], c.shadowBlur);
                c.shadowBlur = -1;
                seen.push(c.shadowBlur);
                document.getElementById("status").textContent = seen.join("|");
                "##,
            )
            .expect("script should run");
        let status = document.borrow().find_element_by_id("status").unwrap();
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some(
                "rgba(0, 0, 0, 0)|rgba(10, 20, 30, 0.5)|0|0|255,0,0,255|0,0,255,128|255,0,0,255|0,0,0,0|true|true|0|8|8"
            )
        );
    }

    #[test]
    fn gradients_dashes_clips_and_blend_modes_follow_the_drawing_state() {
        let document = crate::dom::Document::parse(
            r#"<html><body><canvas id="c" width="20" height="20"></canvas><p id="status"></p></body></html>"#,
        )
        .expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r##"
                const c = document.getElementById("c").getContext("2d");
                const at = (x, y) => [...c.getImageData(x, y, 1, 1).data].join(",");
                const seen = [];
                const fade = c.createLinearGradient(0, 0, 20, 0);
                fade.addColorStop(1, "#fff");
                fade.addColorStop(0, "#000");
                c.fillStyle = fade;
                c.fillRect(0, 0, 20, 4);
                seen.push(at(0, 1), at(19, 1), typeof c.fillStyle, c.fillStyle === fade);
                c.setLineDash([4]);
                seen.push(c.getLineDash().join("/"));
                c.setLineDash([1, -1]);
                seen.push(c.getLineDash().join("/"));
                c.strokeStyle = "#f00";
                c.lineWidth = 2;
                c.beginPath();
                c.moveTo(0, 7);
                c.lineTo(20, 7);
                c.stroke();
                seen.push(at(1, 7), at(5, 7), at(9, 7));
                c.save();
                c.beginPath();
                c.rect(0, 10, 4, 4);
                c.clip();
                c.fillStyle = "#0f0";
                c.fillRect(0, 10, 20, 4);
                c.restore();
                seen.push(at(2, 11), at(8, 11), c.fillStyle === fade);
                c.fillStyle = "#808080";
                c.fillRect(0, 16, 4, 4);
                c.globalCompositeOperation = "multiply";
                c.fillRect(0, 16, 4, 4);
                c.globalCompositeOperation = "no-such-mode";
                seen.push(at(1, 17), c.globalCompositeOperation);
                const ring = c.createRadialGradient(10, 10, 0, 10, 10, 10);
                ring.addColorStop(0, "#fff");
                ring.addColorStop(1, "#000");
                c.globalCompositeOperation = "source-over";
                c.fillStyle = ring;
                c.fillRect(0, 0, 20, 20);
                seen.push(at(10, 10), at(0, 10));
                try {
                    fade.addColorStop(2, "#000");
                } catch (error) {
                    seen.push(error.name);
                }
                document.getElementById("status").textContent = seen.join(" ");
                "##,
            )
            .expect("canvas drawing should run");
        let document = document.borrow();
        let status = document.find_element_by_id("status").expect("status");
        assert_eq!(
            document.text_content(status),
            "6,6,6,255 249,249,249,255 object true 4/4 4/4 255,0,0,255 0,0,0,0 255,0,0,255 0,255,0,255 0,0,0,0 true 64,64,64,255 multiply 237,237,237,255 12,12,12,255 IndexSizeError"
        );
    }

    #[test]
    fn patterns_tile_and_conic_gradients_turn_around_their_centre() {
        let document = crate::dom::Document::parse(
            r#"<html><body><canvas id="c" width="40" height="10"></canvas><p id="status"></p></body></html>"#,
        )
        .expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r##"
                const c = document.getElementById("c").getContext("2d");
                const at = (x, y) => [...c.getImageData(x, y, 1, 1).data].join(",");
                const seen = [];
                const tile = document.createElement("canvas");
                tile.width = 2;
                tile.height = 1;
                const t = tile.getContext("2d");
                t.fillStyle = "#f00";
                t.fillRect(0, 0, 1, 1);
                t.fillStyle = "#00f";
                t.fillRect(1, 0, 1, 1);
                const repeat = c.createPattern(tile, null);
                seen.push(String(repeat), repeat instanceof CanvasPattern);
                c.fillStyle = repeat;
                c.fillRect(0, 0, 4, 2);
                seen.push(at(0, 0), at(1, 1), at(2, 1), c.fillStyle === repeat);
                c.fillStyle = c.createPattern(tile, "no-repeat");
                c.translate(6, 0);
                c.fillRect(0, 0, 4, 2);
                c.setTransform(1, 0, 0, 1, 0, 0);
                seen.push(at(7, 0), at(9, 0));
                const scaled = c.createPattern(tile, "repeat-x");
                scaled.setTransform({ a: 2, d: 1 });
                c.fillStyle = scaled;
                c.imageSmoothingEnabled = false;
                c.fillRect(12, 0, 4, 2);
                seen.push(at(13, 0), at(14, 0), at(13, 1));
                try {
                    c.createPattern(tile, "diagonal");
                } catch (error) {
                    seen.push(error.name);
                }
                const turn = c.createConicGradient(0, 30, 5);
                turn.addColorStop(0, "#f00");
                turn.addColorStop(1, "#00f");
                seen.push(String(turn));
                c.fillStyle = turn;
                c.fillRect(20, 0, 20, 10);
                // right of the centre is the start, left of it half way round
                seen.push(at(38, 5), at(21, 5));
                document.getElementById("status").textContent = seen.join(" ");
                "##,
            )
            .expect("canvas drawing should run");
        let document = document.borrow();
        let status = document.find_element_by_id("status").expect("status");
        assert_eq!(
            document.text_content(status),
            "[object CanvasPattern] true 255,0,0,255 0,0,255,255 255,0,0,255 true 0,0,255,255 0,0,0,0 255,0,0,255 0,0,255,255 0,0,0,0 SyntaxError [object CanvasGradient] 253,0,2,255 130,0,125,255"
        );
    }

    #[test]
    fn text_settings_keep_what_they_understand_and_measure_in_the_font() {
        let document = crate::dom::Document::parse(
            r#"<html><body><canvas id="c" width="20" height="20"></canvas><p id="status"></p></body></html>"#,
        )
        .expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r##"
                const c = document.getElementById("c").getContext("2d");
                const seen = [c.font, c.textAlign, c.textBaseline, c.direction];
                const narrow = c.measureText("abcd").width;
                c.font = "bold 20px Arial";
                c.font = "nonsense";
                c.textAlign = "middle";
                c.textBaseline = "middle";
                seen.push(c.font, c.textAlign, c.textBaseline);
                const wide = c.measureText("abcd");
                seen.push(wide.width === narrow * 2, wide.width > 0, c.measureText("").width);
                seen.push(typeof wide.actualBoundingBoxAscent, wide.fontBoundingBoxAscent > 0);
                c.save();
                c.font = "8px serif";
                c.restore();
                seen.push(c.font);
                c.fillText("abcd", 2, 10);
                c.strokeText("abcd", 2, 10);
                c.fillText("abcd", NaN, 10);
                document.getElementById("status").textContent = seen.join("|");
                "##,
            )
            .expect("canvas text should run");
        let document = document.borrow();
        let status = document.find_element_by_id("status").expect("status");
        assert_eq!(
            document.text_content(status),
            "10px sans-serif|start|alphabetic|ltr|bold 20px Arial|start|middle|true|true|0|number|true|bold 20px Arial"
        );
    }

    #[test]
    fn media_of_another_origin_taints_a_canvas_unless_asked_for_with_cors() {
        let document = crate::dom::Document::parse(
            r#"<html><body>
            <img id="own" src="/a.png"><img id="data" src="data:image/png;base64,AAAA">
            <img id="files" src="https://files.ceri.invalid/a.png">
            <img id="cors" crossorigin="anonymous" src="https://files.ceri.invalid/a.png">
            <video id="video"><source src="https://files.ceri.invalid/v.mp4"></video>
            <video id="asked" crossorigin><source src="https://files.ceri.invalid/v.mp4"></video>
            <canvas id="from" width="2" height="2"></canvas><canvas id="to" width="2" height="2"></canvas>
            <p id="status"></p></body></html>"#,
        )
        .expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        let node = |id: &str| document.borrow().find_element_by_id(id).expect(id);
        let taints = ["own", "data", "files", "cors", "video", "asked"]
            .map(|id| runtime.taints_canvas(node(id)));
        assert_eq!(taints, [false, false, true, false, true, false]);

        // a tainted canvas taints the canvas it is drawn into, and neither gives pixels back
        runtime.canvas_context(node("from")).borrow_mut().tainted = true;
        runtime
            .execute(
                r#"
                const from = document.getElementById("from");
                const to = document.getElementById("to");
                const log = [];
                const attempt = (name, read) => {
                    try {
                        read();
                        log.push(name + ":read");
                    } catch (error) {
                        log.push(name + ":" + error.name);
                    }
                };
                attempt("before", () => to.getContext("2d").getImageData(0, 0, 1, 1));
                to.getContext("2d").drawImage(from, 0, 0);
                attempt("pixels", () => to.getContext("2d").getImageData(0, 0, 1, 1));
                attempt("url", () => to.toDataURL());
                attempt("blob", () => to.toBlob(() => {}));
                to.width = 2;
                attempt("reset", () => to.getContext("2d").getImageData(0, 0, 1, 1));
                document.getElementById("status").textContent = log.join(" ");
                "#,
            )
            .expect("script should run");
        let document = document.borrow();
        assert_eq!(
            document.text_content(node("status")),
            "before:read pixels:SecurityError url:SecurityError blob:SecurityError reset:read"
        );
    }

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
