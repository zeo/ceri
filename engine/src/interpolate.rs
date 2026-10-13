// blends two declared values for transitions and keyframes: values with the same shape
// mix number by number and color by color, transform lists fall back to a 2D matrix
// decomposition, and anything else switches halfway as CSS discrete animation does

use crate::css::Color;
use crate::transform::{Matrix, TransformOp, parse_transform_list};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Token<'a> {
    Number(f32, &'a str),
    Color(Color),
    Text(&'a str),
}

const LENGTH_UNITS: [&str; 9] = ["px", "em", "rem", "%", "vw", "vh", "vmin", "vmax", "ch"];

pub(crate) fn interpolate_value(
    property: &str,
    from: &str,
    to: &str,
    progress: f32,
) -> Option<String> {
    let (from, to) = (from.trim(), to.trim());
    if from == to {
        return Some(from.to_string());
    }
    if let (Some(start), Some(end)) = (Color::parse(from), Color::parse(to)) {
        return Some(color_text(start.mix(end, progress)));
    }
    if is_transform_property(property)
        && let Some(blended) = interpolate_transform(from, to, progress)
    {
        return Some(blended);
    }
    if let Some(blended) = interpolate_tokens(from, to, progress) {
        return Some(if property == "z-index" {
            blended
                .parse::<f32>()
                .map_or(blended, |value| format!("{}", value.round() as i32))
        } else {
            blended
        });
    }
    Some(if progress < 0.5 { from } else { to }.to_string())
}

fn is_transform_property(property: &str) -> bool {
    property == "transform"
}

fn color_text(color: Color) -> String {
    format!(
        "#{:02x}{:02x}{:02x}{:02x}",
        color.red, color.green, color.blue, color.alpha
    )
}

fn tokenize(value: &str) -> Option<Vec<Token<'_>>> {
    let bytes = value.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        let start = index;
        if byte.is_ascii_whitespace() {
            while index < bytes.len() && bytes[index].is_ascii_whitespace() {
                index += 1;
            }
            tokens.push(Token::Text(" "));
            continue;
        }
        let starts_number = byte.is_ascii_digit()
            || (byte == b'.' && bytes.get(index + 1).is_some_and(u8::is_ascii_digit))
            || ((byte == b'-' || byte == b'+')
                && bytes
                    .get(index + 1)
                    .is_some_and(|next| next.is_ascii_digit() || *next == b'.'));
        if starts_number {
            index += 1;
            while index < bytes.len() && (bytes[index].is_ascii_digit() || bytes[index] == b'.') {
                index += 1;
            }
            // an exponent only when digits follow, so units such as em stay units
            if index + 1 < bytes.len()
                && (bytes[index] == b'e' || bytes[index] == b'E')
                && (bytes[index + 1].is_ascii_digit()
                    || ((bytes[index + 1] == b'-' || bytes[index + 1] == b'+')
                        && bytes.get(index + 2).is_some_and(u8::is_ascii_digit)))
            {
                index += 2;
                while index < bytes.len() && bytes[index].is_ascii_digit() {
                    index += 1;
                }
            }
            let number = value[start..index].parse::<f32>().ok()?;
            let unit_start = index;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphabetic() || bytes[index] == b'%')
            {
                index += 1;
            }
            tokens.push(Token::Number(number, &value[unit_start..index]));
            continue;
        }
        if byte == b'#' {
            index += 1;
            while index < bytes.len() && bytes[index].is_ascii_hexdigit() {
                index += 1;
            }
            tokens.push(Token::Color(Color::parse(&value[start..index])?));
            continue;
        }
        if byte.is_ascii_alphabetic() || byte == b'-' || byte == b'_' {
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric()
                    || bytes[index] == b'-'
                    || bytes[index] == b'_')
            {
                index += 1;
            }
            let word = &value[start..index];
            let lowered = word.to_ascii_lowercase();
            if bytes.get(index) == Some(&b'(')
                && matches!(lowered.as_str(), "rgb" | "rgba" | "hsl" | "hsla")
            {
                let close = value[index..].find(')')? + index + 1;
                tokens.push(Token::Color(Color::parse(&value[start..close])?));
                index = close;
                continue;
            }
            match Color::parse(word) {
                Some(color) if bytes.get(index) != Some(&b'(') => tokens.push(Token::Color(color)),
                _ => tokens.push(Token::Text(word)),
            }
            continue;
        }
        index += value[index..].chars().next()?.len_utf8();
        tokens.push(Token::Text(&value[start..index]));
    }
    Some(tokens)
}

fn interpolate_tokens(from: &str, to: &str, progress: f32) -> Option<String> {
    let start = tokenize(from)?;
    let end = tokenize(to)?;
    if start.len() != end.len() {
        return None;
    }
    let mut output = String::new();
    for (left, right) in start.into_iter().zip(end) {
        match (left, right) {
            (Token::Text(left), Token::Text(right)) if left.eq_ignore_ascii_case(right) => {
                output.push_str(left);
            }
            (Token::Color(left), Token::Color(right)) => {
                output.push_str(&color_text(left.mix(right, progress)));
            }
            (Token::Number(left, left_unit), Token::Number(right, right_unit)) => {
                output.push_str(&blend_numbers(
                    (left, left_unit),
                    (right, right_unit),
                    progress,
                )?);
            }
            _ => return None,
        }
    }
    Some(output)
}

fn blend_numbers(
    (left, left_unit): (f32, &str),
    (right, right_unit): (f32, &str),
    progress: f32,
) -> Option<String> {
    let mix = |left: f32, right: f32| left + (right - left) * progress;
    if left_unit.eq_ignore_ascii_case(right_unit) {
        return Some(format!("{}{left_unit}", mix(left, right)));
    }
    // a bare zero takes the other side's unit
    if left == 0.0 && left_unit.is_empty() {
        return Some(format!("{}{right_unit}", mix(0.0, right)));
    }
    if right == 0.0 && right_unit.is_empty() {
        return Some(format!("{}{left_unit}", mix(left, 0.0)));
    }
    let is_length = |unit: &str| LENGTH_UNITS.contains(&unit.to_ascii_lowercase().as_str());
    if is_length(left_unit) && is_length(right_unit) {
        let first = left * (1.0 - progress);
        let second = right * progress;
        let (sign, second) = if second < 0.0 {
            ('-', -second)
        } else {
            ('+', second)
        };
        return Some(format!(
            "calc({first}{left_unit} {sign} {second}{right_unit})"
        ));
    }
    None
}

fn interpolate_transform(from: &str, to: &str, progress: f32) -> Option<String> {
    let none = |value: &str| value.eq_ignore_ascii_case("none");
    let (from, to) = match (none(from), none(to)) {
        (true, false) => (identity_list(to)?, to.to_string()),
        (false, true) => (from.to_string(), identity_list(from)?),
        _ => (from.to_string(), to.to_string()),
    };
    if let Some(blended) = interpolate_tokens(&from, &to, progress) {
        return Some(blended);
    }
    let start = decompose(&affine_of(&from)?)?;
    let end = decompose(&affine_of(&to)?)?;
    Some(recompose(&start.blend(&end, progress)))
}

// the same functions as a list, each with arguments that leave the element unchanged
fn identity_list(list: &str) -> Option<String> {
    parse_transform_list(list)?;
    let mut output = Vec::new();
    for (name, arguments) in crate::transform::functions(list)? {
        let lowered = name.to_ascii_lowercase();
        let identity = match lowered.as_str() {
            "translate" | "translatex" | "translatey" | "translatez" | "translate3d" => {
                vec!["0px"; arguments.len()]
            }
            "scale" | "scalex" | "scaley" | "scalez" | "scale3d" => vec!["1"; arguments.len()],
            "rotate" | "rotatex" | "rotatey" | "rotatez" | "skew" | "skewx" | "skewy" => {
                vec!["0deg"; arguments.len()]
            }
            "rotate3d" => {
                let mut axis = arguments[..3].to_vec();
                axis.push("0deg");
                axis
            }
            "matrix" => vec!["1", "0", "0", "1", "0", "0"],
            "matrix3d" => vec![
                "1", "0", "0", "0", "0", "1", "0", "0", "0", "0", "1", "0", "0", "0", "0", "1",
            ],
            _ => return None,
        };
        output.push(format!("{name}({})", identity.join(", ")));
    }
    Some(output.join(" "))
}

// a 2D affine matrix [a, b, c, d, e, f], when every function is planar and in pixels
fn affine_of(list: &str) -> Option<[f32; 6]> {
    let ops = parse_transform_list(list)?;
    let mut matrix = Matrix::IDENTITY;
    for op in &ops {
        match op {
            TransformOp::Translate(x, y, z) => {
                if *z != 0.0 || !is_pixels(*x) || !is_pixels(*y) {
                    return None;
                }
            }
            TransformOp::Scale(_, _, z) if *z != 1.0 => return None,
            TransformOp::Rotate(x, y, _, _) if *x != 0.0 || *y != 0.0 => return None,
            TransformOp::Perspective(_) => return None,
            _ => {}
        }
        matrix = matrix.multiply(&op.matrix(0.0, 0.0, 16.0));
    }
    let m = matrix.0;
    let planar = [2, 3, 6, 7, 8, 9, 11, 14]
        .iter()
        .all(|&index| m[index].abs() < 1e-6)
        && (m[10] - 1.0).abs() < 1e-6
        && (m[15] - 1.0).abs() < 1e-6;
    planar.then_some([m[0], m[1], m[4], m[5], m[12], m[13]])
}

fn is_pixels(length: crate::css::Length) -> bool {
    match length {
        crate::css::Length::Terms(terms) => {
            terms.em == 0.0
                && terms.rem == 0.0
                && terms.percent == 0.0
                && terms.viewport_width == 0.0
                && terms.viewport_height == 0.0
                && terms.zero_width == 0.0
        }
        _ => false,
    }
}

// rotation, then a shear, then scale, the factoring CSS uses to blend 2D matrices
#[derive(Clone, Copy, Debug)]
struct Decomposed {
    translate: (f32, f32),
    angle: f32,
    shear: f32,
    scale: (f32, f32),
}

impl Decomposed {
    fn blend(&self, other: &Self, progress: f32) -> Self {
        let mix = |left: f32, right: f32| left + (right - left) * progress;
        // the shorter way round
        let mut end_angle = other.angle;
        while end_angle - self.angle > std::f32::consts::PI {
            end_angle -= std::f32::consts::TAU;
        }
        while self.angle - end_angle > std::f32::consts::PI {
            end_angle += std::f32::consts::TAU;
        }
        Self {
            translate: (
                mix(self.translate.0, other.translate.0),
                mix(self.translate.1, other.translate.1),
            ),
            angle: mix(self.angle, end_angle),
            shear: mix(self.shear, other.shear),
            scale: (
                mix(self.scale.0, other.scale.0),
                mix(self.scale.1, other.scale.1),
            ),
        }
    }
}

fn decompose([a, b, c, d, e, f]: &[f32; 6]) -> Option<Decomposed> {
    let scale_x = (a * a + b * b).sqrt();
    if scale_x < 1e-9 {
        return None;
    }
    let angle = b.atan2(*a);
    let (sine, cosine) = angle.sin_cos();
    let scale_y = -sine * c + cosine * d;
    let shear = if scale_y.abs() < 1e-9 {
        0.0
    } else {
        (cosine * c + sine * d) / scale_y
    };
    Some(Decomposed {
        translate: (*e, *f),
        angle,
        shear,
        scale: (scale_x, scale_y),
    })
}

fn recompose(parts: &Decomposed) -> String {
    // a pure translation stays a translate, which keeps the cheap visual offset path
    if parts.angle.abs() < 1e-6
        && parts.shear.abs() < 1e-6
        && (parts.scale.0 - 1.0).abs() < 1e-6
        && (parts.scale.1 - 1.0).abs() < 1e-6
    {
        return format!(
            "translate({}px, {}px)",
            parts.translate.0, parts.translate.1
        );
    }
    let (sine, cosine) = parts.angle.sin_cos();
    let (scale_x, scale_y) = parts.scale;
    let shear = parts.shear * scale_y;
    let a = cosine * scale_x;
    let b = sine * scale_x;
    let c = cosine * shear - sine * scale_y;
    let d = sine * shear + cosine * scale_y;
    format!(
        "matrix({a}, {b}, {c}, {d}, {}, {})",
        parts.translate.0, parts.translate.1
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(property: &str, from: &str, to: &str, progress: f32) -> String {
        interpolate_value(property, from, to, progress).expect("a value always comes back")
    }

    #[test]
    fn matching_shapes_blend_number_by_number() {
        assert_eq!(at("width", "10px", "30px", 0.5), "20px");
        assert_eq!(at("opacity", "0", "1", 0.25), "0.25");
        assert_eq!(
            at("box-shadow", "0 2px 4px #000000", "0 6px 12px #ff0000", 0.5),
            "0 4px 8px #800000ff"
        );
        assert_eq!(
            at(
                "background",
                "linear-gradient(90deg, #000000, #ffffff)",
                "linear-gradient(270deg, #ffffff, #000000)",
                0.5
            ),
            "linear-gradient(180deg, #808080ff, #808080ff)"
        );
        assert_eq!(at("margin", "0 0", "10px 20px", 0.5), "5px 10px");
        assert_eq!(at("margin", "0", "10px 20px", 0.5), "10px 20px");
        assert_eq!(at("margin", "0 auto", "10px auto", 0.5), "5px auto");
    }

    #[test]
    fn mixed_units_meet_in_calc_and_mismatches_switch_halfway() {
        assert_eq!(at("left", "10px", "50%", 0.5), "calc(5px + 25%)");
        assert_eq!(at("display", "block", "flex", 0.4), "block");
        assert_eq!(at("display", "block", "flex", 0.6), "flex");
        assert_eq!(at("z-index", "1", "4", 0.5), "3");
    }

    #[test]
    fn transforms_blend_by_function_or_by_decomposed_matrix() {
        assert_eq!(
            at("transform", "rotate(0deg)", "rotate(90deg)", 0.5),
            "rotate(45deg)"
        );
        assert_eq!(at("transform", "none", "scale(2)", 0.5), "scale(1.5)");
        assert_eq!(
            at("transform", "translateX(10px) rotate(0)", "none", 0.5),
            "translateX(5px) rotate(0deg)"
        );
        let blended = at("transform", "rotate(0deg)", "scale(2) rotate(180deg)", 0.5);
        let parsed = affine_of(&blended).expect("a matrix comes back");
        // halfway the element has turned a quarter and grown by half
        assert!(
            parsed[0].abs() < 1e-4 && (parsed[1].abs() - 1.5).abs() < 1e-4,
            "{blended}"
        );
        let start = at(
            "transform",
            "translate(4px, 2px) rotate(30deg)",
            "skewX(20deg) scale(3)",
            0.0,
        );
        let matrix = affine_of(&start).unwrap();
        let expected = affine_of("translate(4px, 2px) rotate(30deg)").unwrap();
        for (left, right) in matrix.iter().zip(expected) {
            assert!((left - right).abs() < 1e-4, "{start}");
        }
    }
}
