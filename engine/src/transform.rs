// CSS transform functions and the 4x4 matrices they resolve to; points map through
// homogeneous coordinates, so perspective flattens onto the page like a projected plane

use crate::css::{Length, parse_length};
use std::cell::RefCell;
use std::rc::Rc;

// column-major, so `m[column * 4 + row]` as in CSS matrix3d()
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Matrix(pub(crate) [f32; 16]);

impl Matrix {
    pub(crate) const IDENTITY: Self = Self([
        1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ]);

    fn at(&self, row: usize, column: usize) -> f32 {
        self.0[column * 4 + row]
    }

    pub(crate) fn multiply(&self, other: &Self) -> Self {
        let mut output = [0.0_f32; 16];
        for column in 0..4 {
            for row in 0..4 {
                output[column * 4 + row] = (0..4)
                    .map(|index| self.at(row, index) * other.at(index, column))
                    .sum();
            }
        }
        Self(output)
    }

    pub(crate) fn translate(x: f32, y: f32, z: f32) -> Self {
        let mut matrix = Self::IDENTITY;
        matrix.0[12] = x;
        matrix.0[13] = y;
        matrix.0[14] = z;
        matrix
    }

    fn scale(x: f32, y: f32, z: f32) -> Self {
        let mut matrix = Self::IDENTITY;
        matrix.0[0] = x;
        matrix.0[5] = y;
        matrix.0[10] = z;
        matrix
    }

    // rotation about a unit axis, clockwise on screen for positive z angles as CSS requires
    fn rotate(axis: (f32, f32, f32), angle: f32) -> Self {
        let length = (axis.0 * axis.0 + axis.1 * axis.1 + axis.2 * axis.2).sqrt();
        if length == 0.0 || !length.is_finite() {
            return Self::IDENTITY;
        }
        let (x, y, z) = (axis.0 / length, axis.1 / length, axis.2 / length);
        let (sine, cosine) = angle.sin_cos();
        let inverse = 1.0 - cosine;
        Self([
            cosine + x * x * inverse,
            y * x * inverse + z * sine,
            z * x * inverse - y * sine,
            0.0,
            x * y * inverse - z * sine,
            cosine + y * y * inverse,
            z * y * inverse + x * sine,
            0.0,
            x * z * inverse + y * sine,
            y * z * inverse - x * sine,
            cosine + z * z * inverse,
            0.0,
            0.0,
            0.0,
            0.0,
            1.0,
        ])
    }

    fn skew(x: f32, y: f32) -> Self {
        let mut matrix = Self::IDENTITY;
        matrix.0[4] = x.tan();
        matrix.0[1] = y.tan();
        matrix
    }

    pub(crate) fn perspective(distance: f32) -> Self {
        let mut matrix = Self::IDENTITY;
        if distance > 0.0 {
            matrix.0[11] = -1.0 / distance;
        }
        matrix
    }

    fn affine(a: f32, b: f32, c: f32, d: f32, e: f32, f: f32) -> Self {
        Self([
            a, b, 0.0, 0.0, c, d, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, e, f, 0.0, 1.0,
        ])
    }

    // the same transform applied about a point instead of the coordinate origin
    pub(crate) fn about(&self, x: f32, y: f32) -> Self {
        Self::translate(x, y, 0.0)
            .multiply(self)
            .multiply(&Self::translate(-x, -y, 0.0))
    }

    pub(crate) fn is_identity(&self) -> bool {
        self.0
            .iter()
            .zip(Self::IDENTITY.0)
            .all(|(value, identity)| (value - identity).abs() < 1e-6)
    }

    // the x and y offset when the matrix does nothing but move the plane
    pub(crate) fn translation(&self) -> Option<(f32, f32)> {
        let mut rest = *self;
        rest.0[12] = 0.0;
        rest.0[13] = 0.0;
        rest.is_identity().then_some((self.0[12], self.0[13]))
    }

    // maps a point on the z = 0 plane; None when it lands behind the viewer
    pub(crate) fn map_point(&self, x: f32, y: f32) -> Option<(f32, f32)> {
        self.map_point_z(x, y).map(|(px, py, _)| (px, py))
    }

    // maps a point on z = 0 to projected (x, y, z); larger z is closer to the viewer
    pub(crate) fn map_point_z(&self, x: f32, y: f32) -> Option<(f32, f32, f32)> {
        let w = self.at(3, 0) * x + self.at(3, 1) * y + self.at(3, 3);
        if w <= 1e-6 {
            return None;
        }
        Some((
            (self.at(0, 0) * x + self.at(0, 1) * y + self.at(0, 3)) / w,
            (self.at(1, 0) * x + self.at(1, 1) * y + self.at(1, 3)) / w,
            (self.at(2, 0) * x + self.at(2, 1) * y + self.at(2, 3)) / w,
        ))
    }

    // the plane-to-plane map that a flattened element applies: drops z, keeps x, y and w
    pub(crate) fn flattened(&self) -> [f32; 9] {
        [
            self.at(0, 0),
            self.at(0, 1),
            self.at(0, 3),
            self.at(1, 0),
            self.at(1, 1),
            self.at(1, 3),
            self.at(3, 0),
            self.at(3, 1),
            self.at(3, 3),
        ]
    }

    // finds the point on the element plane that projects onto a screen point
    pub(crate) fn unmap_point(&self, x: f32, y: f32) -> Option<(f32, f32)> {
        let inverse = invert3(self.flattened())?;
        let w = inverse[6] * x + inverse[7] * y + inverse[8];
        if w.abs() <= 1e-9 {
            return None;
        }
        let local = (
            (inverse[0] * x + inverse[1] * y + inverse[2]) / w,
            (inverse[3] * x + inverse[4] * y + inverse[5]) / w,
        );
        // the forward map must also land in front of the viewer
        self.map_point(local.0, local.1)?;
        Some(local)
    }

    // largest stretch along either screen axis, which sets the resolution a layer needs
    pub(crate) fn max_scale(&self) -> f32 {
        let column = |index: usize| (self.at(0, index).powi(2) + self.at(1, index).powi(2)).sqrt();
        column(0).max(column(1))
    }

    // the back face shows when m33 of the 4x4 inverse is negative, which holds under
    // perspective and off-origin transforms while separating a turn about y from a 2d mirror
    pub(crate) fn shows_back(&self) -> bool {
        let determinant = self.determinant();
        let cofactor = det3(self.flattened());
        determinant.abs() > 1e-12
            && determinant.is_finite()
            && cofactor.is_finite()
            && cofactor / determinant < 0.0
    }

    fn determinant(&self) -> f32 {
        let m = |row, col| self.at(row, col);
        let sub = |c0, c1, c2| {
            det3([
                m(1, c0),
                m(1, c1),
                m(1, c2),
                m(2, c0),
                m(2, c1),
                m(2, c2),
                m(3, c0),
                m(3, c1),
                m(3, c2),
            ])
        };
        m(0, 0) * sub(1, 2, 3) - m(0, 1) * sub(0, 2, 3) + m(0, 2) * sub(0, 1, 3)
            - m(0, 3) * sub(0, 1, 2)
    }
}

fn det3(m: [f32; 9]) -> f32 {
    m[0] * (m[4] * m[8] - m[5] * m[7]) - m[1] * (m[3] * m[8] - m[5] * m[6])
        + m[2] * (m[3] * m[7] - m[4] * m[6])
}

fn invert3(m: [f32; 9]) -> Option<[f32; 9]> {
    let cofactor = [
        m[4] * m[8] - m[5] * m[7],
        m[2] * m[7] - m[1] * m[8],
        m[1] * m[5] - m[2] * m[4],
        m[5] * m[6] - m[3] * m[8],
        m[0] * m[8] - m[2] * m[6],
        m[2] * m[3] - m[0] * m[5],
        m[3] * m[7] - m[4] * m[6],
        m[1] * m[6] - m[0] * m[7],
        m[0] * m[4] - m[1] * m[3],
    ];
    let determinant = m[0] * cofactor[0] + m[1] * cofactor[3] + m[2] * cofactor[6];
    if determinant.abs() < 1e-9 || !determinant.is_finite() {
        return None;
    }
    Some(cofactor.map(|value| value / determinant))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum TransformOp {
    Translate(Length, Length, f32),
    Scale(f32, f32, f32),
    Rotate(f32, f32, f32, f32),
    Skew(f32, f32),
    Perspective(f32),
    Matrix(Matrix),
}

impl TransformOp {
    // percentages in translations refer to the border box
    pub(crate) fn matrix(&self, width: f32, height: f32, font_size: f32) -> Matrix {
        match *self {
            Self::Translate(x, y, z) => Matrix::translate(
                x.resolve(width, font_size).unwrap_or(0.0),
                y.resolve(height, font_size).unwrap_or(0.0),
                z,
            ),
            Self::Scale(x, y, z) => Matrix::scale(x, y, z),
            Self::Rotate(x, y, z, angle) => Matrix::rotate((x, y, z), angle),
            Self::Skew(x, y) => Matrix::skew(x, y),
            Self::Perspective(distance) => Matrix::perspective(distance),
            Self::Matrix(matrix) => matrix,
        }
    }
}

// a parsed function list kept outside the copyable computed style
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct TransformList(u32);

thread_local! {
    static TRANSFORM_LISTS: RefCell<crate::intern::Table<[TransformOp]>> =
        RefCell::new(crate::intern::Table::new());
}

impl TransformList {
    pub(crate) fn intern(source: &str, ops: Vec<TransformOp>) -> Self {
        TRANSFORM_LISTS.with(|lists| Self(lists.borrow_mut().insert(source, ops.into())))
    }

    pub(crate) fn ops(self) -> Rc<[TransformOp]> {
        TRANSFORM_LISTS
            .with(|lists| lists.borrow().get(self.0))
            .unwrap_or_else(|| Rc::from(Vec::new()))
    }

    pub(crate) fn matrix(self, width: f32, height: f32, font_size: f32) -> Matrix {
        self.ops().iter().fold(Matrix::IDENTITY, |matrix, op| {
            matrix.multiply(&op.matrix(width, height, font_size))
        })
    }
}

pub(crate) fn parse_angle(value: &str) -> Option<f32> {
    let value = value.trim().to_ascii_lowercase();
    let (number, factor) = if let Some(number) = value.strip_suffix("deg") {
        (number, std::f32::consts::PI / 180.0)
    } else if let Some(number) = value.strip_suffix("grad") {
        (number, std::f32::consts::PI / 200.0)
    } else if let Some(number) = value.strip_suffix("rad") {
        (number, 1.0)
    } else if let Some(number) = value.strip_suffix("turn") {
        (number, std::f32::consts::TAU)
    } else if value == "0" {
        ("0", 0.0)
    } else {
        return None;
    };
    let angle = number.trim().parse::<f32>().ok()? * factor;
    angle.is_finite().then_some(angle)
}

fn parse_number(value: &str) -> Option<f32> {
    let value = value.trim();
    let number = match value.strip_suffix('%') {
        Some(percent) => percent.trim().parse::<f32>().ok()? / 100.0,
        None => value.parse::<f32>().ok()?,
    };
    number.is_finite().then_some(number)
}

fn parse_pixels(value: &str) -> Option<f32> {
    match parse_length(value)? {
        Length::Terms(terms)
            if terms.em == 0.0
                && terms.rem == 0.0
                && terms.percent == 0.0
                && terms.viewport_width == 0.0
                && terms.viewport_height == 0.0
                && terms.zero_width == 0.0 =>
        {
            Some(terms.px)
        }
        _ => None,
    }
}

// one function from a transform list, such as rotate(45deg) or matrix(1, 0, 0, 1, 10, 0)
fn parse_function(name: &str, arguments: &[&str]) -> Option<TransformOp> {
    let count = arguments.len();
    let number = |index: usize| arguments.get(index).and_then(|value| parse_number(value));
    let angle = |index: usize| arguments.get(index).and_then(|value| parse_angle(value));
    let length = |index: usize| arguments.get(index).and_then(|value| parse_length(value));
    let zero = Length::px(0.0);
    Some(match (name, count) {
        ("translate", 1) => TransformOp::Translate(length(0)?, zero, 0.0),
        ("translate", 2) => TransformOp::Translate(length(0)?, length(1)?, 0.0),
        ("translatex", 1) => TransformOp::Translate(length(0)?, zero, 0.0),
        ("translatey", 1) => TransformOp::Translate(zero, length(0)?, 0.0),
        ("translatez", 1) => TransformOp::Translate(zero, zero, parse_pixels(arguments[0])?),
        ("translate3d", 3) => {
            TransformOp::Translate(length(0)?, length(1)?, parse_pixels(arguments[2])?)
        }
        ("scale", 1) => TransformOp::Scale(number(0)?, number(0)?, 1.0),
        ("scale", 2) => TransformOp::Scale(number(0)?, number(1)?, 1.0),
        ("scalex", 1) => TransformOp::Scale(number(0)?, 1.0, 1.0),
        ("scaley", 1) => TransformOp::Scale(1.0, number(0)?, 1.0),
        ("scalez", 1) => TransformOp::Scale(1.0, 1.0, number(0)?),
        ("scale3d", 3) => TransformOp::Scale(number(0)?, number(1)?, number(2)?),
        ("rotate" | "rotatez", 1) => TransformOp::Rotate(0.0, 0.0, 1.0, angle(0)?),
        ("rotatex", 1) => TransformOp::Rotate(1.0, 0.0, 0.0, angle(0)?),
        ("rotatey", 1) => TransformOp::Rotate(0.0, 1.0, 0.0, angle(0)?),
        ("rotate3d", 4) => TransformOp::Rotate(number(0)?, number(1)?, number(2)?, angle(3)?),
        ("skew", 1) => TransformOp::Skew(angle(0)?, 0.0),
        ("skew", 2) => TransformOp::Skew(angle(0)?, angle(1)?),
        ("skewx", 1) => TransformOp::Skew(angle(0)?, 0.0),
        ("skewy", 1) => TransformOp::Skew(0.0, angle(0)?),
        ("perspective", 1) => TransformOp::Perspective(match arguments[0].trim() {
            "none" => 0.0,
            value => parse_pixels(value)?,
        }),
        ("matrix", 6) => {
            let values = (0..6).map(number).collect::<Option<Vec<_>>>()?;
            TransformOp::Matrix(Matrix::affine(
                values[0], values[1], values[2], values[3], values[4], values[5],
            ))
        }
        ("matrix3d", 16) => {
            let values = (0..16).map(number).collect::<Option<Vec<_>>>()?;
            TransformOp::Matrix(Matrix(values.try_into().ok()?))
        }
        _ => return None,
    })
}

pub(crate) const MAX_TRANSFORM_FUNCTIONS: usize = 16;

// a space-separated list of name(arguments) functions; parentheses nest, so calc() and
// var() arguments stay whole, and arguments split only on top-level commas
pub(crate) fn functions(value: &str) -> Option<Vec<(&str, Vec<&str>)>> {
    let mut output = Vec::new();
    let mut rest = value.trim();
    while !rest.is_empty() {
        let open = rest.find('(')?;
        let mut depth = 0_usize;
        let mut close = None;
        let mut commas = Vec::new();
        for (index, character) in rest[open..].char_indices() {
            match character {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(open + index);
                        break;
                    }
                }
                ',' if depth == 1 => commas.push(open + index),
                _ => {}
            }
        }
        let close = close?;
        let mut arguments = Vec::new();
        let mut start = open + 1;
        for comma in commas {
            arguments.push(rest[start..comma].trim());
            start = comma + 1;
        }
        arguments.push(rest[start..close].trim());
        output.push((rest[..open].trim(), arguments));
        rest = rest[close + 1..].trim_start();
    }
    Some(output)
}

// the whole list, or None when any function is unknown or malformed
pub(crate) fn parse_transform_list(value: &str) -> Option<Vec<TransformOp>> {
    let mut ops = Vec::new();
    for (name, arguments) in functions(value)? {
        ops.push(parse_function(&name.to_ascii_lowercase(), &arguments)?);
        if ops.len() > MAX_TRANSFORM_FUNCTIONS {
            return None;
        }
    }
    (!ops.is_empty()).then_some(ops)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(actual: (f32, f32), expected: (f32, f32)) -> bool {
        (actual.0 - expected.0).abs() < 1e-3 && (actual.1 - expected.1).abs() < 1e-3
    }

    fn matrix_of(value: &str) -> Matrix {
        parse_transform_list(value)
            .expect("transform should parse")
            .iter()
            .fold(Matrix::IDENTITY, |matrix, op| {
                matrix.multiply(&op.matrix(100.0, 50.0, 16.0))
            })
    }

    #[test]
    fn functions_compose_left_to_right_like_css() {
        let point = matrix_of("translate(10px, 5px) rotate(90deg)")
            .map_point(1.0, 0.0)
            .expect("point maps");
        assert!(close(point, (10.0, 6.0)), "{point:?}");
        let point = matrix_of("scale(2) translateX(50%)")
            .map_point(0.0, 0.0)
            .unwrap();
        assert!(close(point, (100.0, 0.0)), "{point:?}");
        let point = matrix_of("matrix(1, 0, 0, 1, 7, -3)")
            .map_point(1.0, 1.0)
            .unwrap();
        assert!(close(point, (8.0, -2.0)), "{point:?}");
        let point = matrix_of("skewX(45deg)").map_point(0.0, 10.0).unwrap();
        assert!(close(point, (10.0, 10.0)), "{point:?}");
        let point = matrix_of("rotate(0.25turn)").map_point(0.0, 1.0).unwrap();
        assert!(close(point, (-1.0, 0.0)), "{point:?}");
    }

    #[test]
    fn rejects_unknown_functions_and_bad_arguments() {
        assert!(parse_transform_list("rotate(45)").is_none());
        assert!(parse_transform_list("wobble(1)").is_none());
        assert!(parse_transform_list("scale(a)").is_none());
        assert!(parse_transform_list("translate(1px, 2px, 3px)").is_none());
        assert!(parse_transform_list(&"scale(1) ".repeat(17)).is_none());
    }

    #[test]
    fn inverse_mapping_undoes_affine_and_perspective_maps() {
        for value in [
            "rotate(30deg) scale(1.5, 0.5) translate(4px, -2px)",
            "perspective(400px) rotateY(40deg)",
            "skew(10deg, 20deg)",
        ] {
            let matrix = matrix_of(value);
            let screen = matrix.map_point(12.0, -7.0).expect("forward map");
            let local = matrix.unmap_point(screen.0, screen.1).expect("inverse map");
            assert!(close(local, (12.0, -7.0)), "{value}: {local:?}");
        }
        assert!(matrix_of("scale(0)").unmap_point(1.0, 1.0).is_none());
    }

    #[test]
    fn perspective_shrinks_receding_points_and_reports_back_faces() {
        let receding = matrix_of("perspective(100px) translateZ(-100px)");
        let point = receding.map_point(10.0, 0.0).unwrap();
        assert!(close(point, (5.0, 0.0)), "{point:?}");
        assert!(matrix_of("rotateY(180deg)").shows_back());
        assert!(!matrix_of("rotateY(20deg)").shows_back());
        assert!(!matrix_of("rotate(180deg)").shows_back());
        assert!((matrix_of("scale(3, 2)").max_scale() - 3.0).abs() < 1e-5);
    }

    #[test]
    fn map_point_z_orders_depth_and_shows_back_handles_off_origin_perspective() {
        let near = matrix_of("perspective(500px) translateZ(60px)")
            .map_point_z(0.0, 0.0)
            .expect("near maps");
        let far = matrix_of("perspective(500px) translateZ(-60px)")
            .map_point_z(0.0, 0.0)
            .expect("far maps");
        assert!(near.2 > far.2, "near={near:?} far={far:?}");

        let perspective = Matrix::perspective(500.0).about(500.0, 0.0);
        let turned_back = perspective
            .multiply(&Matrix::rotate((0.0, 1.0, 0.0), 100.0_f32.to_radians()).about(500.0, 0.0));
        let turned_front = perspective
            .multiply(&Matrix::rotate((0.0, 1.0, 0.0), 20.0_f32.to_radians()).about(500.0, 0.0));
        assert!(turned_back.shows_back());
        assert!(!turned_front.shows_back());
        assert!(!matrix_of("scaleX(-1)").shows_back());

        let flip = Matrix::rotate((0.0, 1.0, 0.0), std::f32::consts::PI).about(200.0, 200.0);
        let back_face = Matrix::perspective(600.0)
            .about(200.0, 200.0)
            .multiply(&flip)
            .multiply(&flip);
        assert!(!back_face.shows_back());
    }
}
