use crate::css::{Color, Display, Length, Style, Visibility};
use crate::dom::{Document, Element, Namespace, NodeId};
use crate::image::Image;

type Point = (f32, f32);
// a b c d e f, mapping (x, y) to (a x + c y + e, b x + d y + f)
pub(crate) type Affine = [f32; 6];
pub(crate) const IDENTITY: Affine = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

// elements drawn or expanded for one scene, so nested use references stay bounded
const MAX_SCENE_ELEMENTS: usize = 20_000;
const MAX_DEPTH: usize = 64;
// group opacity deeper than this fades each shape instead of compositing a layer
const MAX_LAYERS: usize = 4;
const MAX_CURVE_STEPS: f32 = 256.0;
const TOLERANCE: f32 = 0.1;

// the svg properties, both as CSS and as presentation attributes
pub(crate) const PROPERTIES: &[&str] = &[
    "fill",
    "stroke",
    "stroke-width",
    "stroke-linecap",
    "stroke-linejoin",
    "stroke-miterlimit",
    "fill-rule",
    "fill-opacity",
    "stroke-opacity",
];

// attributes of svg elements that act as author style below every author rule
pub(crate) const PRESENTATION_ATTRIBUTES: &[&str] = &[
    "fill",
    "stroke",
    "stroke-width",
    "stroke-linecap",
    "stroke-linejoin",
    "stroke-miterlimit",
    "fill-rule",
    "fill-opacity",
    "stroke-opacity",
    "opacity",
    "color",
    "display",
    "visibility",
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Paint {
    None,
    Color(Color),
    // resolved against each element's own color when it paints
    CurrentColor,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LineCap {
    Butt,
    Round,
    Square,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LineJoin {
    Miter,
    Round,
    Bevel,
}

// the inherited svg painting properties
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Presentation {
    pub(crate) fill: Paint,
    pub(crate) stroke: Paint,
    pub(crate) stroke_width: Length,
    pub(crate) line_cap: LineCap,
    pub(crate) line_join: LineJoin,
    pub(crate) miter_limit: f32,
    pub(crate) even_odd: bool,
    pub(crate) fill_opacity: f32,
    pub(crate) stroke_opacity: f32,
}

impl Default for Presentation {
    fn default() -> Self {
        Self {
            fill: Paint::Color(Color::BLACK),
            stroke: Paint::None,
            stroke_width: Length::px(1.0),
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            miter_limit: 4.0,
            even_odd: false,
            fill_opacity: 1.0,
            stroke_opacity: 1.0,
        }
    }
}

pub(crate) fn apply_property(
    presentation: &mut Presentation,
    property: &str,
    value: &str,
    current: Color,
) {
    let value = value.trim();
    let keyword = value.to_ascii_lowercase();
    match property {
        "fill" => {
            if let Some(paint) = parse_paint(&keyword, current) {
                presentation.fill = paint;
            }
        }
        "stroke" => {
            if let Some(paint) = parse_paint(&keyword, current) {
                presentation.stroke = paint;
            }
        }
        "stroke-width" => {
            let width = parse_number(value)
                .map(Length::px)
                .or_else(|| crate::css::parse_length(value))
                .filter(|width| *width != Length::Auto);
            if let Some(width) = width {
                presentation.stroke_width = width;
            }
        }
        "stroke-linecap" => match keyword.as_str() {
            "butt" => presentation.line_cap = LineCap::Butt,
            "round" => presentation.line_cap = LineCap::Round,
            "square" => presentation.line_cap = LineCap::Square,
            _ => {}
        },
        "stroke-linejoin" => match keyword.as_str() {
            "miter" | "miter-clip" | "arcs" => presentation.line_join = LineJoin::Miter,
            "round" => presentation.line_join = LineJoin::Round,
            "bevel" => presentation.line_join = LineJoin::Bevel,
            _ => {}
        },
        "stroke-miterlimit" => {
            if let Some(limit) = parse_number(value).filter(|limit| *limit >= 1.0) {
                presentation.miter_limit = limit;
            }
        }
        "fill-rule" => match keyword.as_str() {
            "nonzero" => presentation.even_odd = false,
            "evenodd" => presentation.even_odd = true,
            _ => {}
        },
        "fill-opacity" | "stroke-opacity" => {
            let opacity = match value.strip_suffix('%') {
                Some(percent) => parse_number(percent).map(|percent| percent / 100.0),
                None => parse_number(value),
            };
            if let Some(opacity) = opacity.map(|opacity| opacity.clamp(0.0, 1.0)) {
                if property == "fill-opacity" {
                    presentation.fill_opacity = opacity;
                } else {
                    presentation.stroke_opacity = opacity;
                }
            }
        }
        _ => {}
    }
}

pub(crate) fn reset_property(presentation: &mut Presentation, from: &Presentation, property: &str) {
    match property {
        "fill" => presentation.fill = from.fill,
        "stroke" => presentation.stroke = from.stroke,
        "stroke-width" => presentation.stroke_width = from.stroke_width,
        "stroke-linecap" => presentation.line_cap = from.line_cap,
        "stroke-linejoin" => presentation.line_join = from.line_join,
        "stroke-miterlimit" => presentation.miter_limit = from.miter_limit,
        "fill-rule" => presentation.even_odd = from.even_odd,
        "fill-opacity" => presentation.fill_opacity = from.fill_opacity,
        "stroke-opacity" => presentation.stroke_opacity = from.stroke_opacity,
        _ => {}
    }
}

// a paint server reference draws its fallback color, or nothing without one
fn parse_paint(value: &str, current: Color) -> Option<Paint> {
    let value = match value.strip_prefix("url(") {
        Some(rest) => rest.split_once(')')?.1.trim(),
        None => value,
    };
    match value {
        "none" | "" => Some(Paint::None),
        "currentcolor" => Some(Paint::CurrentColor),
        _ => crate::colors::parse(value, current).map(Paint::Color),
    }
}

fn parse_number(value: &str) -> Option<f32> {
    let mut cursor = Cursor::new(value);
    let number = cursor.number()?;
    cursor.skip_separators();
    cursor.done().then_some(number)
}

// whitespace and comma separated numbers, all of them or none
fn parse_numbers(value: &str) -> Option<Vec<f32>> {
    let mut cursor = Cursor::new(value);
    let mut numbers = Vec::new();
    loop {
        cursor.skip_separators();
        if cursor.done() {
            return Some(numbers);
        }
        numbers.push(cursor.number()?);
    }
}

// x y width height; a missing or malformed attribute is ignored, while a box with no area
// is returned so it can turn rendering off
pub(crate) fn view_box(element: &Element) -> Option<(f32, f32, f32, f32)> {
    match parse_numbers(element.attr("viewBox")?)?.as_slice() {
        &[x, y, width, height] => Some((x, y, width, height)),
        _ => None,
    }
}

// the viewBox mapped onto a viewport of this size by preserveAspectRatio, or None when the
// viewBox has no area and nothing renders
fn viewport_transform(element: &Element, size: Point) -> Option<Affine> {
    let Some(view) = view_box(element) else {
        return Some(IDENTITY);
    };
    if view.2 <= 0.0 || view.3 <= 0.0 {
        return None;
    }
    Some(fit(
        view,
        size,
        element.attr("preserveAspectRatio").unwrap_or(""),
    ))
}

pub(crate) fn fit(view: (f32, f32, f32, f32), size: Point, aspect: &str) -> Affine {
    let (view_x, view_y, view_width, view_height) = view;
    let (scale_x, scale_y) = (size.0 / view_width, size.1 / view_height);
    let mut words = aspect.split_ascii_whitespace();
    let align = match words.next() {
        Some("defer") => words.next(),
        first => first,
    }
    .unwrap_or("xMidYMid");
    let slice = words.next() == Some("slice");
    if align == "none" {
        return [
            scale_x,
            0.0,
            0.0,
            scale_y,
            -view_x * scale_x,
            -view_y * scale_y,
        ];
    }
    let fraction = |part: &str| match part {
        "Min" => Some(0.0),
        "Mid" => Some(0.5),
        "Max" => Some(1.0),
        _ => None,
    };
    let parts = (align.len() == 8 && align.is_char_boundary(4))
        .then(|| (&align[..4], &align[4..]))
        .and_then(|(x, y)| {
            Some((
                fraction(x.strip_prefix('x')?)?,
                fraction(y.strip_prefix('Y')?)?,
            ))
        });
    let ((align_x, align_y), slice) = parts.map_or(((0.5, 0.5), false), |parts| (parts, slice));
    let scale = if slice {
        scale_x.max(scale_y)
    } else {
        scale_x.min(scale_y)
    };
    [
        scale,
        0.0,
        0.0,
        scale,
        (size.0 - view_width * scale) * align_x - view_x * scale,
        (size.1 - view_height * scale) * align_y - view_y * scale,
    ]
}

fn multiply(outer: Affine, inner: Affine) -> Affine {
    [
        outer[0] * inner[0] + outer[2] * inner[1],
        outer[1] * inner[0] + outer[3] * inner[1],
        outer[0] * inner[2] + outer[2] * inner[3],
        outer[1] * inner[2] + outer[3] * inner[3],
        outer[0] * inner[4] + outer[2] * inner[5] + outer[4],
        outer[1] * inner[4] + outer[3] * inner[5] + outer[5],
    ]
}

fn map(matrix: Affine, point: Point) -> Point {
    (
        matrix[0] * point.0 + matrix[2] * point.1 + matrix[4],
        matrix[1] * point.0 + matrix[3] * point.1 + matrix[5],
    )
}

fn translation(x: f32, y: f32) -> Affine {
    [1.0, 0.0, 0.0, 1.0, x, y]
}

// the transform attribute, which takes unitless numbers and rotation about a point
fn parse_transform(value: &str) -> Option<Affine> {
    let mut matrix = IDENTITY;
    let mut rest = value.trim();
    while !rest.is_empty() {
        let open = rest.find('(')?;
        let close = open + rest[open..].find(')')?;
        let arguments = parse_numbers(&rest[open + 1..close])?;
        let operation = match (rest[..open].trim(), arguments.as_slice()) {
            ("matrix", &[a, b, c, d, e, f]) => [a, b, c, d, e, f],
            ("translate", &[x]) => translation(x, 0.0),
            ("translate", &[x, y]) => translation(x, y),
            ("scale", &[scale]) => [scale, 0.0, 0.0, scale, 0.0, 0.0],
            ("scale", &[x, y]) => [x, 0.0, 0.0, y, 0.0, 0.0],
            ("rotate", &[angle]) => rotation(angle),
            ("rotate", &[angle, x, y]) => multiply(
                multiply(translation(x, y), rotation(angle)),
                translation(-x, -y),
            ),
            ("skewX", &[angle]) => [1.0, 0.0, angle.to_radians().tan(), 1.0, 0.0, 0.0],
            ("skewY", &[angle]) => [1.0, angle.to_radians().tan(), 0.0, 1.0, 0.0, 0.0],
            _ => return None,
        };
        matrix = multiply(matrix, operation);
        rest = rest[close + 1..].trim_start_matches(|character: char| {
            character.is_ascii_whitespace() || character == ','
        });
    }
    Some(matrix)
}

fn rotation(degrees: f32) -> Affine {
    let (sin, cos) = degrees.to_radians().sin_cos();
    [cos, sin, -sin, cos, 0.0, 0.0]
}

// a CSS transform on an svg child turns about its user space origin, as transform-box
// view-box and the svg transform-origin of 0 0 do
fn local_transform(element: &Element, style: &Style, font_size: f32) -> Affine {
    if let Some(list) = style.transform {
        let origin = (
            style
                .transform_origin
                .0
                .resolve(0.0, font_size)
                .unwrap_or(0.0),
            style
                .transform_origin
                .1
                .resolve(0.0, font_size)
                .unwrap_or(0.0),
        );
        let m = list.matrix(0.0, 0.0, font_size).about(origin.0, origin.1).0;
        return [m[0], m[1], m[4], m[5], m[12], m[13]];
    }
    element
        .attr("transform")
        .and_then(parse_transform)
        .unwrap_or(IDENTITY)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Segment {
    Move(Point),
    Line(Point),
    Cubic(Point, Point, Point),
    Close,
}

struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            bytes: text.as_bytes(),
            at: 0,
        }
    }

    fn done(&self) -> bool {
        self.at >= self.bytes.len()
    }

    fn skip_separators(&mut self) {
        while self
            .bytes
            .get(self.at)
            .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b',')
        {
            self.at += 1;
        }
    }

    fn digits(&mut self) -> bool {
        let from = self.at;
        while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
            self.at += 1;
        }
        self.at > from
    }

    // a number ends where the next can begin, so "1.5.5" is 1.5 then .5 and "1-2" is 1 then -2
    fn number(&mut self) -> Option<f32> {
        self.skip_separators();
        let start = self.at;
        if matches!(self.bytes.get(self.at), Some(b'+' | b'-')) {
            self.at += 1;
        }
        let whole = self.digits();
        let fraction = self.bytes.get(self.at) == Some(&b'.') && {
            self.at += 1;
            self.digits()
        };
        if !whole && !fraction {
            self.at = start;
            return None;
        }
        if matches!(self.bytes.get(self.at), Some(b'e' | b'E')) {
            let mark = self.at;
            self.at += 1;
            if matches!(self.bytes.get(self.at), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if !self.digits() {
                self.at = mark;
            }
        }
        std::str::from_utf8(&self.bytes[start..self.at])
            .ok()?
            .parse::<f32>()
            .ok()
            .filter(|number| number.is_finite())
    }

    // arc flags are single digits that need no separator after them
    fn flag(&mut self) -> Option<bool> {
        self.skip_separators();
        let flag = match self.bytes.get(self.at)? {
            b'0' => false,
            b'1' => true,
            _ => return None,
        };
        self.at += 1;
        Some(flag)
    }

    fn point(&mut self, origin: Point) -> Option<Point> {
        Some((self.number()? + origin.0, self.number()? + origin.1))
    }
}

#[derive(Default)]
struct PathBuilder {
    segments: Vec<Segment>,
    current: Point,
    start: Point,
    // the last control point of a cubic or quadratic, which a smooth command reflects
    cubic_control: Option<Point>,
    quad_control: Option<Point>,
}

impl PathBuilder {
    fn draw(&mut self, segment: Segment, end: Point) {
        // drawing after a close starts a new subpath at the closed one's start
        if self.segments.last() == Some(&Segment::Close) {
            self.segments.push(Segment::Move(self.start));
        }
        self.segments.push(segment);
        self.current = end;
    }

    fn reflect(&self, control: Option<Point>) -> Point {
        control.map_or(self.current, |control| {
            (
                2.0 * self.current.0 - control.0,
                2.0 * self.current.1 - control.1,
            )
        })
    }

    fn quadratic(&mut self, control: Point, end: Point) {
        let from = self.current;
        let first = (
            from.0 + 2.0 / 3.0 * (control.0 - from.0),
            from.1 + 2.0 / 3.0 * (control.1 - from.1),
        );
        let second = (
            end.0 + 2.0 / 3.0 * (control.0 - end.0),
            end.1 + 2.0 / 3.0 * (control.1 - end.1),
        );
        self.draw(Segment::Cubic(first, second, end), end);
    }

    // one command's arguments; nothing is drawn unless all of them parse
    fn command(&mut self, letter: u8, cursor: &mut Cursor<'_>) -> Option<()> {
        let origin = if letter.is_ascii_lowercase() {
            self.current
        } else {
            (0.0, 0.0)
        };
        let mut cubic_control = None;
        let mut quad_control = None;
        match letter.to_ascii_uppercase() {
            b'M' => {
                let point = cursor.point(origin)?;
                self.segments.push(Segment::Move(point));
                self.current = point;
                self.start = point;
            }
            b'L' => {
                let point = cursor.point(origin)?;
                self.draw(Segment::Line(point), point);
            }
            b'H' => {
                let point = (cursor.number()? + origin.0, self.current.1);
                self.draw(Segment::Line(point), point);
            }
            b'V' => {
                let point = (self.current.0, cursor.number()? + origin.1);
                self.draw(Segment::Line(point), point);
            }
            b'C' => {
                let first = cursor.point(origin)?;
                let second = cursor.point(origin)?;
                let end = cursor.point(origin)?;
                self.draw(Segment::Cubic(first, second, end), end);
                cubic_control = Some(second);
            }
            b'S' => {
                let first = self.reflect(self.cubic_control);
                let second = cursor.point(origin)?;
                let end = cursor.point(origin)?;
                self.draw(Segment::Cubic(first, second, end), end);
                cubic_control = Some(second);
            }
            b'Q' => {
                let control = cursor.point(origin)?;
                let end = cursor.point(origin)?;
                self.quadratic(control, end);
                quad_control = Some(control);
            }
            b'T' => {
                let control = self.reflect(self.quad_control);
                let end = cursor.point(origin)?;
                self.quadratic(control, end);
                quad_control = Some(control);
            }
            b'A' => {
                let radii = (cursor.number()?, cursor.number()?);
                let angle = cursor.number()?;
                let large = cursor.flag()?;
                let sweep = cursor.flag()?;
                let end = cursor.point(origin)?;
                self.arc(radii, angle, large, sweep, end);
            }
            b'Z' => {
                self.segments.push(Segment::Close);
                self.current = self.start;
            }
            _ => return None,
        }
        self.cubic_control = cubic_control;
        self.quad_control = quad_control;
        Some(())
    }

    // the endpoint to center conversion of the SVG 2 implementation notes, in quarter turns
    fn arc(&mut self, radii: Point, angle: f32, large: bool, sweep: bool, end: Point) {
        let from = self.current;
        if from == end {
            return;
        }
        let (mut rx, mut ry) = (f64::from(radii.0.abs()), f64::from(radii.1.abs()));
        if rx == 0.0 || ry == 0.0 {
            self.draw(Segment::Line(end), end);
            return;
        }
        let (sin, cos) = f64::from(angle).to_radians().sin_cos();
        let (x1, y1, x2, y2) = (
            f64::from(from.0),
            f64::from(from.1),
            f64::from(end.0),
            f64::from(end.1),
        );
        let (half_x, half_y) = ((x1 - x2) / 2.0, (y1 - y2) / 2.0);
        let x = cos * half_x + sin * half_y;
        let y = -sin * half_x + cos * half_y;
        let lambda = (x * x) / (rx * rx) + (y * y) / (ry * ry);
        if lambda > 1.0 {
            rx *= lambda.sqrt();
            ry *= lambda.sqrt();
        }
        let numerator = rx * rx * ry * ry - rx * rx * y * y - ry * ry * x * x;
        let denominator = rx * rx * y * y + ry * ry * x * x;
        let mut coefficient = (numerator / denominator).max(0.0).sqrt();
        if large == sweep {
            coefficient = -coefficient;
        }
        let center_x = coefficient * rx * y / ry;
        let center_y = -coefficient * ry * x / rx;
        let cx = cos * center_x - sin * center_y + (x1 + x2) / 2.0;
        let cy = sin * center_x + cos * center_y + (y1 + y2) / 2.0;
        let angle_between =
            |u: (f64, f64), v: (f64, f64)| (u.0 * v.1 - u.1 * v.0).atan2(u.0 * v.0 + u.1 * v.1);
        let start_vector = ((x - center_x) / rx, (y - center_y) / ry);
        let end_vector = ((-x - center_x) / rx, (-y - center_y) / ry);
        let start_angle = angle_between((1.0, 0.0), start_vector);
        let mut delta = angle_between(start_vector, end_vector);
        if !sweep && delta > 0.0 {
            delta -= std::f64::consts::TAU;
        } else if sweep && delta < 0.0 {
            delta += std::f64::consts::TAU;
        }
        let pieces = (delta.abs() / std::f64::consts::FRAC_PI_2 - 1e-9)
            .ceil()
            .max(1.0) as usize;
        let step = delta / pieces as f64;
        let k = 4.0 / 3.0 * (step / 4.0).tan();
        let place = |(ux, uy): (f64, f64)| {
            (
                (cx + rx * ux * cos - ry * uy * sin) as f32,
                (cy + rx * ux * sin + ry * uy * cos) as f32,
            )
        };
        for piece in 0..pieces {
            let (sin_a, cos_a) = (start_angle + step * piece as f64).sin_cos();
            let (sin_b, cos_b) = (start_angle + step * (piece + 1) as f64).sin_cos();
            let first = place((cos_a - k * sin_a, sin_a + k * cos_a));
            let second = place((cos_b + k * sin_b, sin_b - k * cos_b));
            let point = if piece + 1 == pieces {
                end
            } else {
                place((cos_b, sin_b))
            };
            self.draw(Segment::Cubic(first, second, point), point);
        }
    }
}

// the path data grammar; an error keeps the segments before it, as rendering does
pub(crate) fn parse_path(data: &str) -> Vec<Segment> {
    let mut cursor = Cursor::new(data);
    let mut path = PathBuilder::default();
    let mut repeat = None;
    loop {
        cursor.skip_separators();
        let Some(&byte) = cursor.bytes.get(cursor.at) else {
            break;
        };
        let letter = if byte.is_ascii_alphabetic() {
            cursor.at += 1;
            byte
        } else if let Some(letter) = repeat {
            letter
        } else {
            break;
        };
        if path.segments.is_empty() && !matches!(letter, b'M' | b'm') {
            break;
        }
        if path.command(letter, &mut cursor).is_none() {
            break;
        }
        // coordinates after a moveto are implicit linetos
        repeat = match letter {
            b'M' => Some(b'L'),
            b'm' => Some(b'l'),
            b'Z' | b'z' => None,
            letter => Some(letter),
        };
    }
    path.segments
}

const KAPPA: f32 = 0.552_284_8;

fn ellipse(cx: f32, cy: f32, rx: f32, ry: f32) -> Vec<Segment> {
    let (kx, ky) = (rx * KAPPA, ry * KAPPA);
    vec![
        Segment::Move((cx + rx, cy)),
        Segment::Cubic((cx + rx, cy + ky), (cx + kx, cy + ry), (cx, cy + ry)),
        Segment::Cubic((cx - kx, cy + ry), (cx - rx, cy + ky), (cx - rx, cy)),
        Segment::Cubic((cx - rx, cy - ky), (cx - kx, cy - ry), (cx, cy - ry)),
        Segment::Cubic((cx + kx, cy - ry), (cx + rx, cy - ky), (cx + rx, cy)),
        Segment::Close,
    ]
}

fn rounded_rect(x: f32, y: f32, width: f32, height: f32, rx: f32, ry: f32) -> Vec<Segment> {
    let (right, bottom) = (x + width, y + height);
    if rx <= 0.0 || ry <= 0.0 {
        return vec![
            Segment::Move((x, y)),
            Segment::Line((right, y)),
            Segment::Line((right, bottom)),
            Segment::Line((x, bottom)),
            Segment::Close,
        ];
    }
    let (kx, ky) = (rx * (1.0 - KAPPA), ry * (1.0 - KAPPA));
    vec![
        Segment::Move((x + rx, y)),
        Segment::Line((right - rx, y)),
        Segment::Cubic((right - kx, y), (right, y + ky), (right, y + ry)),
        Segment::Line((right, bottom - ry)),
        Segment::Cubic(
            (right, bottom - ky),
            (right - kx, bottom),
            (right - rx, bottom),
        ),
        Segment::Line((x + rx, bottom)),
        Segment::Cubic((x + kx, bottom), (x, bottom - ky), (x, bottom - ry)),
        Segment::Line((x, y + ry)),
        Segment::Cubic((x, y + ky), (x + kx, y), (x + rx, y)),
        Segment::Close,
    ]
}

// a coordinate or length attribute, with percentages of the viewport side it measures
fn length_attribute(element: &Element, name: &str, reference: f32, font_size: f32) -> Option<f32> {
    let value = element.attr(name)?.trim();
    parse_number(value)
        .or_else(|| crate::css::parse_length(value)?.resolve(reference, font_size))
        .filter(|value| value.is_finite())
}

fn shape_path(element: &Element, viewport: Point, font_size: f32) -> Option<Vec<Segment>> {
    let diagonal = (viewport.0.hypot(viewport.1)) / std::f32::consts::SQRT_2;
    let length = |name: &str, reference: f32| length_attribute(element, name, reference, font_size);
    let x = |name: &str| length(name, viewport.0).unwrap_or(0.0);
    let y = |name: &str| length(name, viewport.1).unwrap_or(0.0);
    match element.name.as_str() {
        "path" => Some(parse_path(element.attr("d")?)),
        "rect" => {
            let width = length("width", viewport.0)?;
            let height = length("height", viewport.1)?;
            if width <= 0.0 || height <= 0.0 {
                return None;
            }
            let rx = length("rx", viewport.0).filter(|rx| *rx >= 0.0);
            let ry = length("ry", viewport.1).filter(|ry| *ry >= 0.0);
            let (rx, ry) = match (rx, ry) {
                (Some(rx), Some(ry)) => (rx, ry),
                (Some(radius), None) | (None, Some(radius)) => (radius, radius),
                (None, None) => (0.0, 0.0),
            };
            Some(rounded_rect(
                x("x"),
                y("y"),
                width,
                height,
                rx.min(width / 2.0),
                ry.min(height / 2.0),
            ))
        }
        "circle" => {
            let radius = length("r", diagonal).filter(|radius| *radius > 0.0)?;
            Some(ellipse(x("cx"), y("cy"), radius, radius))
        }
        "ellipse" => {
            let rx = length("rx", viewport.0).filter(|rx| *rx > 0.0);
            let ry = length("ry", viewport.1).filter(|ry| *ry > 0.0);
            let (rx, ry) = (rx.or(ry)?, ry.or(rx)?);
            Some(ellipse(x("cx"), y("cy"), rx, ry))
        }
        "line" => Some(vec![
            Segment::Move((x("x1"), y("y1"))),
            Segment::Line((x("x2"), y("y2"))),
        ]),
        "polyline" | "polygon" => {
            let mut cursor = Cursor::new(element.attr("points")?);
            let mut segments = Vec::new();
            while let Some(point) = cursor.point((0.0, 0.0)) {
                segments.push(if segments.is_empty() {
                    Segment::Move(point)
                } else {
                    Segment::Line(point)
                });
            }
            if element.name == "polygon" && !segments.is_empty() {
                segments.push(Segment::Close);
            }
            Some(segments)
        }
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Stroke {
    color: Color,
    width: f32,
    cap: LineCap,
    join: LineJoin,
    miter_limit: f32,
}

#[derive(Clone, Debug, PartialEq)]
struct Shape {
    path: Vec<Segment>,
    transform: Affine,
    fill: Option<(Color, bool)>,
    stroke: Option<Stroke>,
}

#[derive(Clone, Debug, PartialEq)]
enum Item {
    Shape(Shape),
    // children composited together, then faded as one
    Group(f32, Vec<Item>),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Scene {
    // the content box the outer svg maps its viewBox onto, from the border box corner
    pub(crate) viewport: (f32, f32, f32, f32),
    items: Vec<Item>,
}

pub(crate) type StyleOf<'a> = &'a dyn Fn(NodeId, &Style, f32) -> (Style, f32);

#[derive(Clone, Copy)]
struct Frame {
    transform: Affine,
    // the nearest viewport in user units, which percentages refer to
    size: Point,
    depth: usize,
}

struct Walk<'a> {
    document: &'a Document,
    style_of: StyleOf<'a>,
    budget: usize,
    // use targets being expanded, so a reference back into itself stops
    using: Vec<NodeId>,
}

// the shapes an outer svg element draws into its content box
pub(crate) fn build(
    document: &Document,
    root: NodeId,
    root_style: (&Style, f32),
    viewport: (f32, f32, f32, f32),
    style_of: StyleOf<'_>,
) -> Scene {
    let mut items = Vec::new();
    if let Some(element) = document.element(root)
        && let Some(view) = viewport_transform(element, (viewport.2, viewport.3))
    {
        let mut walk = Walk {
            document,
            style_of,
            budget: MAX_SCENE_ELEMENTS,
            using: Vec::new(),
        };
        let frame = Frame {
            transform: multiply(translation(viewport.0, viewport.1), view),
            size: view_box(element).map_or((viewport.2, viewport.3), |view| (view.2, view.3)),
            depth: 0,
        };
        render_children(&mut walk, root, root_style, frame, &mut items);
    }
    Scene { viewport, items }
}

fn render_children(
    walk: &mut Walk<'_>,
    node: NodeId,
    style: (&Style, f32),
    frame: Frame,
    items: &mut Vec<Item>,
) {
    let children = walk.document.element_children(node).collect::<Vec<_>>();
    for child in children {
        render(walk, child, style, frame, items);
    }
}

fn render(
    walk: &mut Walk<'_>,
    node: NodeId,
    parent: (&Style, f32),
    frame: Frame,
    items: &mut Vec<Item>,
) {
    if walk.budget == 0 || frame.depth >= MAX_DEPTH {
        return;
    }
    walk.budget -= 1;
    let document = walk.document;
    let Some(element) = document
        .element(node)
        .filter(|element| element.namespace == Namespace::Svg)
    else {
        return;
    };
    let (style, font_size) = (walk.style_of)(node, parent.0, parent.1);
    if style.display == Display::None {
        return;
    }
    let frame = Frame {
        transform: multiply(frame.transform, local_transform(element, &style, font_size)),
        depth: frame.depth + 1,
        ..frame
    };
    let mut own = Vec::new();
    match element.name.as_str() {
        "g" | "a" => render_children(walk, node, (&style, font_size), frame, &mut own),
        "svg" => {
            let x = length_attribute(element, "x", frame.size.0, font_size).unwrap_or(0.0);
            let y = length_attribute(element, "y", frame.size.1, font_size).unwrap_or(0.0);
            let size = (
                length_attribute(element, "width", frame.size.0, font_size).unwrap_or(frame.size.0),
                length_attribute(element, "height", frame.size.1, font_size)
                    .unwrap_or(frame.size.1),
            );
            nested_viewport(
                walk,
                (node, element),
                (&style, font_size),
                frame,
                (x, y, size),
                &mut own,
            );
        }
        "use" => render_use(walk, element, (&style, font_size), frame, &mut own),
        _ => {
            if style.visibility == Visibility::Visible
                && let Some(path) = shape_path(element, frame.size, font_size)
            {
                own.push(Item::Shape(painted(path, frame, &style, font_size)));
            }
        }
    }
    if own.is_empty() {
        return;
    }
    if style.opacity < 1.0 {
        items.push(Item::Group(style.opacity.max(0.0), own));
    } else {
        items.extend(own);
    }
}

// a nested svg or a referenced symbol establishes a new viewport at x y with this size
fn nested_viewport(
    walk: &mut Walk<'_>,
    (node, element): (NodeId, &Element),
    style: (&Style, f32),
    frame: Frame,
    (x, y, size): (f32, f32, Point),
    items: &mut Vec<Item>,
) {
    if size.0 <= 0.0 || size.1 <= 0.0 {
        return;
    }
    let Some(view) = viewport_transform(element, size) else {
        return;
    };
    let frame = Frame {
        transform: multiply(frame.transform, multiply(translation(x, y), view)),
        size: view_box(element).map_or(size, |view| (view.2, view.3)),
        ..frame
    };
    render_children(walk, node, style, frame, items);
}

fn render_use(
    walk: &mut Walk<'_>,
    element: &Element,
    style: (&Style, f32),
    frame: Frame,
    items: &mut Vec<Item>,
) {
    let document = walk.document;
    let Some(target) = element
        .attr("href")
        .or_else(|| element.attr("xlink:href"))
        .and_then(|href| href.trim().strip_prefix('#'))
        .and_then(|id| document.find_element_by_id(id))
    else {
        return;
    };
    if walk.using.contains(&target) {
        return;
    }
    let length = |name: &str, reference: f32| length_attribute(element, name, reference, style.1);
    let x = length("x", frame.size.0).unwrap_or(0.0);
    let y = length("y", frame.size.1).unwrap_or(0.0);
    walk.using.push(target);
    match document.element(target) {
        Some(symbol)
            if symbol.namespace == Namespace::Svg
                && matches!(symbol.name.as_str(), "symbol" | "svg") =>
        {
            let (symbol_style, font_size) = (walk.style_of)(target, style.0, style.1);
            if symbol_style.display != Display::None {
                let side = |name: &str, reference: f32| {
                    length(name, reference)
                        .or_else(|| length_attribute(symbol, name, reference, font_size))
                        .unwrap_or(reference)
                };
                let size = (side("width", frame.size.0), side("height", frame.size.1));
                nested_viewport(
                    walk,
                    (target, symbol),
                    (&symbol_style, font_size),
                    frame,
                    (x, y, size),
                    items,
                );
            }
        }
        _ => {
            let frame = Frame {
                transform: multiply(frame.transform, translation(x, y)),
                ..frame
            };
            render(walk, target, style, frame, items);
        }
    }
    walk.using.pop();
}

fn painted(path: Vec<Segment>, frame: Frame, style: &Style, font_size: f32) -> Shape {
    let presentation = style.svg;
    let resolve = |paint: Paint, opacity: f32| {
        match paint {
            Paint::None => None,
            Paint::Color(color) => Some(color),
            Paint::CurrentColor => Some(style.color),
        }
        .map(|color| color.scale_alpha(opacity))
        .filter(|color| color.alpha > 0)
    };
    let diagonal = frame.size.0.hypot(frame.size.1) / std::f32::consts::SQRT_2;
    let width = presentation
        .stroke_width
        .resolve(diagonal, font_size)
        .unwrap_or(1.0);
    Shape {
        path,
        transform: frame.transform,
        fill: resolve(presentation.fill, presentation.fill_opacity)
            .map(|color| (color, presentation.even_odd)),
        stroke: resolve(presentation.stroke, presentation.stroke_opacity)
            .filter(|_| width > 0.0)
            .map(|color| Stroke {
                color,
                width,
                cap: presentation.line_cap,
                join: presentation.line_join,
                miter_limit: presentation.miter_limit,
            }),
    }
}

// paints the scene into the image, with `transform` mapping the scene's border box space to
// image pixels
pub(crate) fn rasterize(scene: &Scene, image: &mut Image, transform: Affine) {
    paint_items(&scene.items, image, transform, 1.0, 0);
}

fn paint_items(items: &[Item], image: &mut Image, transform: Affine, fade: f32, layers: usize) {
    for item in items {
        match item {
            Item::Shape(shape) => paint_shape(shape, image, transform, fade),
            Item::Group(opacity, children) if layers >= MAX_LAYERS => {
                paint_items(children, image, transform, fade * opacity, layers);
            }
            Item::Group(opacity, children) => {
                let mut layer = Image::new(image.width, image.height);
                paint_items(children, &mut layer, transform, 1.0, layers + 1);
                for (index, color) in layer.pixels.iter().enumerate() {
                    if color.alpha > 0 {
                        let x = (index % image.width as usize) as i32;
                        let y = (index / image.width as usize) as i32;
                        image.blend_pixel(x, y, color.scale_alpha(fade * opacity));
                    }
                }
            }
        }
    }
}

fn paint_shape(shape: &Shape, image: &mut Image, transform: Affine, fade: f32) {
    let matrix = multiply(transform, shape.transform);
    let subpaths = flatten(&shape.path, matrix);
    if let Some((color, even_odd)) = shape.fill {
        let mut edges = Vec::new();
        for (points, _) in &subpaths {
            let device = points.iter().map(|&point| map(matrix, point));
            let next = device.clone().cycle().skip(1);
            edges.extend(device.zip(next));
        }
        fill_edges(image, &edges, even_odd, color.scale_alpha(fade));
    }
    if let Some(stroke) = shape.stroke {
        let scale = matrix[0].hypot(matrix[1]).max(matrix[2].hypot(matrix[3]));
        let device_radius = stroke.width / 2.0 * scale;
        let disc_steps = if device_radius > TOLERANCE {
            (std::f32::consts::PI / (1.0 - TOLERANCE / device_radius).acos()).ceil()
        } else {
            8.0
        }
        .clamp(8.0, 128.0) as usize;
        let mut polygons = Vec::new();
        for (points, closed) in &subpaths {
            stroke_polygons(points, *closed, &stroke, disc_steps, &mut polygons);
        }
        let mut edges = Vec::new();
        for polygon in polygons {
            let device = polygon
                .into_iter()
                .map(|point| map(matrix, point))
                .collect::<Vec<_>>();
            crate::font::push_polygon(&mut edges, &device);
        }
        fill_edges(image, &edges, false, stroke.color.scale_alpha(fade));
    }
}

fn fill_edges(image: &mut Image, edges: &[crate::font::Segment], even_odd: bool, color: Color) {
    if edges.is_empty() || color.alpha == 0 {
        return;
    }
    let (mut left, mut top, mut right, mut bottom) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for &(from, to) in edges {
        for point in [from, to] {
            left = left.min(point.0);
            top = top.min(point.1);
            right = right.max(point.0);
            bottom = bottom.max(point.1);
        }
    }
    if !(left.is_finite() && top.is_finite() && right.is_finite() && bottom.is_finite()) {
        return;
    }
    let left = left.floor().max(0.0) as i32;
    let top = top.floor().max(0.0) as i32;
    let right = right.ceil().min(image.width as f32) as i32;
    let bottom = bottom.ceil().min(image.height as f32) as i32;
    if right <= left || bottom <= top {
        return;
    }
    let (width, height) = ((right - left) as usize, (bottom - top) as usize);
    let coverage =
        crate::font::coverage_grid(edges, (left as f32, top as f32), width, height, even_odd);
    for (index, &value) in coverage.iter().enumerate() {
        if value > 0 {
            image.blend_pixel(
                left + (index % width) as i32,
                top + (index / width) as i32,
                color.scale_alpha(f32::from(value) / 255.0),
            );
        }
    }
}

// subpaths in user space as point lists, each flagged when it closes; curves are cut into
// steps fine enough for the device scale of `matrix`
fn flatten(path: &[Segment], matrix: Affine) -> Vec<(Vec<Point>, bool)> {
    let mut subpaths = Vec::new();
    let mut points: Vec<Point> = Vec::new();
    for segment in path {
        match *segment {
            Segment::Move(point) => {
                if points.len() > 1 {
                    subpaths.push((std::mem::take(&mut points), false));
                }
                points.clear();
                points.push(point);
            }
            Segment::Line(point) => points.push(point),
            Segment::Cubic(first, second, end) => {
                let from = points.last().copied().unwrap_or(first);
                let device = [from, first, second, end].map(|point| map(matrix, point));
                let bend = |a: Point, b: Point, c: Point| {
                    (a.0 - 2.0 * b.0 + c.0).hypot(a.1 - 2.0 * b.1 + c.1)
                };
                let deviation = bend(device[0], device[1], device[2])
                    .max(bend(device[1], device[2], device[3]));
                let steps = (0.75 * deviation / TOLERANCE)
                    .sqrt()
                    .ceil()
                    .clamp(1.0, MAX_CURVE_STEPS);
                let steps = if steps.is_finite() { steps as usize } else { 1 };
                for step in 1..=steps {
                    let t = step as f32 / steps as f32;
                    let u = 1.0 - t;
                    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                    points.push((
                        a * from.0 + b * first.0 + c * second.0 + d * end.0,
                        a * from.1 + b * first.1 + c * second.1 + d * end.1,
                    ));
                }
            }
            Segment::Close => {
                if !points.is_empty() {
                    subpaths.push((std::mem::take(&mut points), true));
                }
            }
        }
    }
    if points.len() > 1 {
        subpaths.push((points, false));
    }
    subpaths
}

fn unit(from: Point, to: Point) -> Point {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let length = dx.hypot(dy);
    (dx / length, dy / length)
}

fn disc(center: Point, radius: f32, steps: usize) -> Vec<Point> {
    (0..steps)
        .map(|step| {
            let angle = step as f32 * std::f32::consts::TAU / steps as f32;
            (
                center.0 + radius * angle.cos(),
                center.1 + radius * angle.sin(),
            )
        })
        .collect()
}

// the stroke outline as polygons whose union is the stroked area: a quad per edge, plus
// joins and caps
fn stroke_polygons(
    points: &[Point],
    closed: bool,
    stroke: &Stroke,
    disc_steps: usize,
    polygons: &mut Vec<Vec<Point>>,
) {
    let half = stroke.width / 2.0;
    let mut points = points.to_vec();
    points.dedup_by(|a, b| (a.0 - b.0).hypot(a.1 - b.1) < 1e-6);
    if closed && points.len() > 1 && points.first() == points.last() {
        points.pop();
    }
    // a zero length subpath shows only its caps
    if let &[point] = points.as_slice() {
        match stroke.cap {
            LineCap::Butt => {}
            LineCap::Round => polygons.push(disc(point, half, disc_steps)),
            LineCap::Square => polygons.push(vec![
                (point.0 - half, point.1 - half),
                (point.0 + half, point.1 - half),
                (point.0 + half, point.1 + half),
                (point.0 - half, point.1 + half),
            ]),
        }
        return;
    }
    if points.len() < 2 {
        return;
    }
    let count = points.len();
    let edges = if closed { count } else { count - 1 };
    for index in 0..edges {
        let (from, to) = (points[index], points[(index + 1) % count]);
        let direction = unit(from, to);
        let normal = (-direction.1 * half, direction.0 * half);
        polygons.push(vec![
            (from.0 + normal.0, from.1 + normal.1),
            (to.0 + normal.0, to.1 + normal.1),
            (to.0 - normal.0, to.1 - normal.1),
            (from.0 - normal.0, from.1 - normal.1),
        ]);
    }
    let joints = if closed { 0..count } else { 1..count - 1 };
    for index in joints {
        let vertex = points[index];
        let before = points[(index + count - 1) % count];
        let after = points[(index + 1) % count];
        join(
            vertex,
            unit(before, vertex),
            unit(vertex, after),
            stroke,
            disc_steps,
            polygons,
        );
    }
    if !closed {
        let start = unit(points[1], points[0]);
        let end = unit(points[count - 2], points[count - 1]);
        cap(points[0], start, stroke, disc_steps, polygons);
        cap(points[count - 1], end, stroke, disc_steps, polygons);
    }
}

fn join(
    vertex: Point,
    incoming: Point,
    outgoing: Point,
    stroke: &Stroke,
    disc_steps: usize,
    polygons: &mut Vec<Vec<Point>>,
) {
    let half = stroke.width / 2.0;
    let cross = incoming.0 * outgoing.1 - incoming.1 * outgoing.0;
    let dot = incoming.0 * outgoing.0 + incoming.1 * outgoing.1;
    if cross.abs() < 1e-6 && dot > 0.0 {
        return;
    }
    if stroke.join == LineJoin::Round {
        polygons.push(disc(vertex, half, disc_steps));
        return;
    }
    // the outer corner lies on the side the path turns away from
    let side = if cross > 0.0 { -half } else { half };
    let first = (vertex.0 - incoming.1 * side, vertex.1 + incoming.0 * side);
    let second = (vertex.0 - outgoing.1 * side, vertex.1 + outgoing.0 * side);
    let ratio = 1.0 / ((1.0 + dot) / 2.0).sqrt();
    if stroke.join == LineJoin::Miter && ratio <= stroke.miter_limit {
        let bisector = unit(
            vertex,
            ((first.0 + second.0) / 2.0, (first.1 + second.1) / 2.0),
        );
        let tip = (
            vertex.0 + bisector.0 * half * ratio,
            vertex.1 + bisector.1 * half * ratio,
        );
        polygons.push(vec![vertex, first, tip, second]);
    } else {
        polygons.push(vec![vertex, first, second]);
    }
}

fn cap(
    point: Point,
    direction: Point,
    stroke: &Stroke,
    disc_steps: usize,
    polygons: &mut Vec<Vec<Point>>,
) {
    let half = stroke.width / 2.0;
    match stroke.cap {
        LineCap::Butt => {}
        LineCap::Round => polygons.push(disc(point, half, disc_steps)),
        LineCap::Square => {
            let normal = (-direction.1 * half, direction.0 * half);
            let reach = (direction.0 * half, direction.1 * half);
            polygons.push(vec![
                (point.0 + normal.0, point.1 + normal.1),
                (point.0 + normal.0 + reach.0, point.1 + normal.1 + reach.1),
                (point.0 - normal.0 + reach.0, point.1 - normal.1 + reach.1),
                (point.0 - normal.0, point.1 - normal.1),
            ]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Segment::{Close, Cubic, Line, Move};

    fn near(actual: Point, expected: Point) -> bool {
        (actual.0 - expected.0).abs() < 1e-3 && (actual.1 - expected.1).abs() < 1e-3
    }

    fn end_of(segment: Segment) -> Point {
        match segment {
            Move(point) | Line(point) | Cubic(_, _, point) => point,
            Close => panic!("a close has no end point"),
        }
    }

    #[test]
    fn path_data_takes_compact_numbers_implicit_commands_and_relative_steps() {
        assert_eq!(
            parse_path("M.5.5l9 9M9.5.5l-9 9"),
            vec![
                Move((0.5, 0.5)),
                Line((9.5, 9.5)),
                Move((9.5, 0.5)),
                Line((0.5, 9.5))
            ]
        );
        assert_eq!(
            parse_path("M1-2 3-4 2e1.5m1 1 1 1"),
            vec![
                Move((1.0, -2.0)),
                Line((3.0, -4.0)),
                Line((20.0, 0.5)),
                Move((21.0, 1.5)),
                Line((22.0, 2.5))
            ]
        );
        assert_eq!(
            parse_path("M2.5 2.5V.5h7v7h-2M.5 2.5h7v7h-7z"),
            vec![
                Move((2.5, 2.5)),
                Line((2.5, 0.5)),
                Line((9.5, 0.5)),
                Line((9.5, 7.5)),
                Line((7.5, 7.5)),
                Move((0.5, 2.5)),
                Line((7.5, 2.5)),
                Line((7.5, 9.5)),
                Line((0.5, 9.5)),
                Close
            ]
        );
    }

    #[test]
    fn path_data_stops_at_the_first_error_and_reopens_after_close() {
        assert_eq!(parse_path("M0 0L1"), vec![Move((0.0, 0.0))]);
        assert!(parse_path("L1 1").is_empty());
        assert_eq!(
            parse_path("M0 0L1 1 x 2 2"),
            vec![Move((0.0, 0.0)), Line((1.0, 1.0))]
        );
        assert_eq!(
            parse_path("M1 1h2z l1 1"),
            vec![
                Move((1.0, 1.0)),
                Line((3.0, 1.0)),
                Close,
                Move((1.0, 1.0)),
                Line((2.0, 2.0))
            ]
        );
    }

    #[test]
    fn smooth_curves_reflect_the_previous_control_point() {
        let cubic = parse_path("M0 0C1 0 2 1 3 3S5 6 6 6");
        assert_eq!(cubic[2], Cubic((4.0, 5.0), (5.0, 6.0), (6.0, 6.0)));
        let quadratic = parse_path("M0 0Q3 3 6 0T12 0");
        let Cubic(first, second, end) = quadratic[2] else {
            panic!("a quadratic becomes a cubic");
        };
        assert!(near(first, (8.0, -2.0)));
        assert!(near(second, (10.0, -2.0)));
        assert_eq!(end, (12.0, 0.0));
    }

    #[test]
    fn arcs_follow_the_flags_and_grow_radii_that_cannot_reach() {
        let half_turn = parse_path("M0 0A5 5 0 0 1 10 0");
        assert_eq!(half_turn.len(), 3);
        assert!(near(end_of(half_turn[1]), (5.0, -5.0)));
        assert_eq!(end_of(half_turn[2]), (10.0, 0.0));
        let scaled = parse_path("M0 0a1 1 0 0010 0");
        assert_eq!(scaled.len(), 3);
        assert!(near(end_of(scaled[1]), (5.0, 5.0)));
        let large = parse_path("M0 0A5 5 0 1 1 5 5");
        assert_eq!(large.len(), 4, "three quarters of a turn");
        assert!(near(end_of(large[1]), (5.0, -5.0)));
        assert!(near(end_of(large[2]), (10.0, 0.0)));
        assert_eq!(end_of(large[3]), (5.0, 5.0));
        assert_eq!(parse_path("M1 1A0 3 0 0 0 4 4")[1], Line((4.0, 4.0)));
        assert_eq!(parse_path("M1 1A3 3 0 0 0 1 1").len(), 1);
        let rotated = parse_path("M0 0A10 5 90 0 1 0 20");
        assert!(near(end_of(rotated[1]), (5.0, 10.0)));
    }

    #[test]
    fn view_boxes_align_meet_slice_and_stretch() {
        let view = (0.0, 0.0, 16.0, 16.0);
        assert_eq!(fit(view, (32.0, 16.0), ""), [1.0, 0.0, 0.0, 1.0, 8.0, 0.0]);
        assert_eq!(
            fit(view, (32.0, 16.0), "xMaxYMin meet"),
            [1.0, 0.0, 0.0, 1.0, 16.0, 0.0]
        );
        assert_eq!(
            fit(view, (16.0, 8.0), "xMinYMax slice"),
            [1.0, 0.0, 0.0, 1.0, 0.0, -8.0]
        );
        assert_eq!(
            fit((2.0, 4.0, 8.0, 8.0), (16.0, 8.0), "none"),
            [2.0, 0.0, 0.0, 1.0, -4.0, -4.0]
        );
        assert_eq!(
            fit(view, (32.0, 16.0), "defer xMinYMin"),
            [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]
        );
        assert_eq!(
            fit(view, (32.0, 16.0), "bogus slice"),
            fit(view, (32.0, 16.0), "")
        );
    }

    #[test]
    fn transform_attributes_compose_left_to_right() {
        let matrix = parse_transform("translate(10 0) scale(2), rotate(90 1 1)")
            .expect("transform should parse");
        assert!(near(map(matrix, (1.0, 1.0)), (12.0, 2.0)));
        assert!(near(map(matrix, (2.0, 1.0)), (12.0, 4.0)));
        let skew = parse_transform("skewX(45)").expect("skew should parse");
        assert!(near(map(skew, (0.0, 2.0)), (2.0, 2.0)));
        assert!(parse_transform("translate(1 2").is_none());
        assert!(parse_transform("spin(4)").is_none());
    }
}
