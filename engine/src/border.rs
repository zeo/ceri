// border sides, corner radii and outlines: their values and the properties that set them

use crate::css::{Color, Length, Style, parse_length, split_words};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BorderStyle {
    #[default]
    None,
    Hidden,
    Solid,
    Dashed,
    Dotted,
    Double,
    Groove,
    Ridge,
    Inset,
    Outset,
}

impl BorderStyle {
    fn parse(word: &str) -> Option<Self> {
        Some(match word.to_ascii_lowercase().as_str() {
            "none" => Self::None,
            "hidden" => Self::Hidden,
            "solid" => Self::Solid,
            "dashed" => Self::Dashed,
            "dotted" => Self::Dotted,
            "double" => Self::Double,
            "groove" => Self::Groove,
            "ridge" => Self::Ridge,
            "inset" => Self::Inset,
            "outset" => Self::Outset,
            _ => return None,
        })
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Hidden => "hidden",
            Self::Solid => "solid",
            Self::Dashed => "dashed",
            Self::Dotted => "dotted",
            Self::Double => "double",
            Self::Groove => "groove",
            Self::Ridge => "ridge",
            Self::Inset => "inset",
            Self::Outset => "outset",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BorderSide {
    pub width: f32,
    pub style: BorderStyle,
    // none follows the element's color
    pub color: Option<Color>,
}

impl BorderSide {
    pub(crate) const INITIAL: Self = Self {
        width: 3.0,
        style: BorderStyle::None,
        color: None,
    };

    // a side with no style takes no space
    pub(crate) fn used_width(self) -> f32 {
        if matches!(self.style, BorderStyle::None | BorderStyle::Hidden) {
            0.0
        } else {
            self.width
        }
    }
}

// an outline draws outside the border box and takes no space
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OutlineStyle {
    pub side: BorderSide,
    pub offset: f32,
}

impl OutlineStyle {
    pub(crate) const INITIAL: Self = Self {
        side: BorderSide::INITIAL,
        offset: 0.0,
    };
}

// corner radii as horizontal and vertical lengths, from the top-left corner clockwise
pub(crate) type CornerLengths = [(Length, Length); 4];

pub(crate) const SQUARE_CORNERS: CornerLengths = [(Length::px(0.0), Length::px(0.0)); 4];

fn parse_width(word: &str) -> Option<f32> {
    match word.to_ascii_lowercase().as_str() {
        "thin" => Some(1.0),
        "medium" => Some(3.0),
        "thick" => Some(5.0),
        _ => parse_length(word)
            .filter(|length| !length.has_percent())
            .and_then(|length| length.resolve(0.0, 16.0))
            .filter(|width| *width >= 0.0),
    }
}

// width, style and color in any order; what is left out goes back to its initial value
fn parse_side(value: &str, color: &dyn Fn(&str) -> Option<Color>) -> Option<BorderSide> {
    let mut side = BorderSide::INITIAL;
    for word in split_words(value) {
        if let Some(style) = BorderStyle::parse(word) {
            side.style = style;
        } else if let Some(width) = parse_width(word) {
            side.width = width;
        } else if let Some(parsed) = color(word) {
            side.color = Some(parsed);
        } else {
            return None;
        }
    }
    Some(side)
}

// one to four values for top, right, bottom and left, or for the corners from top-left
pub(crate) fn expand<T: Copy>(values: &[T]) -> Option<[T; 4]> {
    Some(match *values {
        [all] => [all; 4],
        [vertical, horizontal] => [vertical, horizontal, vertical, horizontal],
        [top, horizontal, bottom] => [top, horizontal, bottom, horizontal],
        [top, right, bottom, left] => [top, right, bottom, left],
        _ => return None,
    })
}

fn parse_each<T>(value: &str, parse: impl Fn(&str) -> Option<T>) -> Option<Vec<T>> {
    split_words(value).into_iter().map(parse).collect()
}

pub(crate) fn parse_radii(value: &str) -> Option<CornerLengths> {
    let (horizontal, vertical) = match value.split_once('/') {
        Some((horizontal, vertical)) => (horizontal, Some(vertical)),
        None => (value, None),
    };
    let radius = |word: &str| parse_length(word).filter(|length| *length != Length::Auto);
    let horizontal = expand(&parse_each(horizontal, radius)?)?;
    let vertical = match vertical {
        Some(vertical) => expand(&parse_each(vertical, radius)?)?,
        None => horizontal,
    };
    Some([0, 1, 2, 3].map(|corner| (horizontal[corner], vertical[corner])))
}

fn parse_corner(value: &str) -> Option<(Length, Length)> {
    let radius = |word: &str| parse_length(word).filter(|length| *length != Length::Auto);
    match parse_each(value, radius)?.as_slice() {
        [both] => Some((*both, *both)),
        [horizontal, vertical] => Some((*horizontal, *vertical)),
        _ => None,
    }
}

const SIDES: [&str; 4] = ["top", "right", "bottom", "left"];

// the border, border-radius and outline properties; false when the property is another one
pub(crate) fn apply(
    style: &mut Style,
    property: &str,
    value: &str,
    color: &dyn Fn(&str) -> Option<Color>,
) -> bool {
    let sides = &mut style.border;
    match property {
        "border" => {
            if let Some(side) = parse_side(value, color) {
                *sides = [side; 4];
            }
        }
        "border-width" => {
            if let Some(widths) = parse_each(value, parse_width).as_deref().and_then(expand) {
                for (side, width) in sides.iter_mut().zip(widths) {
                    side.width = width;
                }
            }
        }
        "border-style" => {
            if let Some(styles) = parse_each(value, BorderStyle::parse)
                .as_deref()
                .and_then(expand)
            {
                for (side, border_style) in sides.iter_mut().zip(styles) {
                    side.style = border_style;
                }
            }
        }
        "border-color" => {
            if let Some(colors) = parse_each(value, color).as_deref().and_then(expand) {
                for (side, side_color) in sides.iter_mut().zip(colors) {
                    side.color = Some(side_color);
                }
            }
        }
        "border-radius" => {
            if let Some(radii) = parse_radii(value) {
                style.border_radius = radii;
            }
        }
        "outline" => {
            if let Some(side) = parse_side(value, color) {
                style.outline.side = side;
            }
        }
        "outline-width" => {
            if let Some(width) = parse_width(value.trim()) {
                style.outline.side.width = width;
            }
        }
        "outline-style" => {
            // auto draws as the platform's focus ring, which here is solid
            let value = value.trim();
            let parsed = if value.eq_ignore_ascii_case("auto") {
                Some(BorderStyle::Solid)
            } else {
                BorderStyle::parse(value)
            };
            if let Some(outline_style) = parsed {
                style.outline.side.style = outline_style;
            }
        }
        "outline-color" => {
            if let Some(parsed) = color(value.trim()) {
                style.outline.side.color = Some(parsed);
            }
        }
        "outline-offset" => {
            if let Some(offset) = parse_length(value).and_then(|length| length.resolve(0.0, 16.0)) {
                style.outline.offset = offset;
            }
        }
        _ => {
            let Some(rest) = property.strip_prefix("border-") else {
                return false;
            };
            let corner = match rest {
                "top-left-radius" => Some(0),
                "top-right-radius" => Some(1),
                "bottom-right-radius" => Some(2),
                "bottom-left-radius" => Some(3),
                _ => None,
            };
            if let Some(corner) = corner {
                if let Some(radius) = parse_corner(value) {
                    style.border_radius[corner] = radius;
                }
                return true;
            }
            let (side_name, part) = rest.split_once('-').unwrap_or((rest, ""));
            let Some(index) = SIDES.iter().position(|name| *name == side_name) else {
                return false;
            };
            let side = &mut sides[index];
            match part {
                "" => {
                    if let Some(parsed) = parse_side(value, color) {
                        *side = parsed;
                    }
                }
                "width" => {
                    if let Some(width) = parse_width(value.trim()) {
                        side.width = width;
                    }
                }
                "style" => {
                    if let Some(border_style) = BorderStyle::parse(value.trim()) {
                        side.style = border_style;
                    }
                }
                "color" => {
                    if let Some(parsed) = color(value.trim()) {
                        side.color = Some(parsed);
                    }
                }
                _ => return false,
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sides_in_any_order_and_elliptical_corner_radii() {
        let side = parse_side("dashed 2px #ff0000", &|word| {
            crate::colors::parse(word, Color::BLACK)
        })
        .expect("side should parse");
        assert_eq!(side.style, BorderStyle::Dashed);
        assert_eq!(side.width, 2.0);
        assert_eq!(side.color, Some(Color::opaque(255, 0, 0)));
        let medium = parse_side("solid", &|_| None).expect("style alone should parse");
        assert_eq!(medium.used_width(), 3.0);
        assert_eq!(parse_side("solid wobbly", &|_| None), None);

        let radii = parse_radii("10px 20px / 5px").expect("radii should parse");
        assert_eq!(radii[0], (Length::px(10.0), Length::px(5.0)));
        assert_eq!(radii[1], (Length::px(20.0), Length::px(5.0)));
        assert_eq!(radii[2], (Length::px(10.0), Length::px(5.0)));
        assert_eq!(expand(&[1, 2, 3]), Some([1, 2, 3, 2]));
    }
}
