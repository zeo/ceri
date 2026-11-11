// clip-path basic shapes: parsed into a handle the copyable computed style can hold, then
// resolved against the border box once layout knows its size

use crate::border::{CornerLengths, SQUARE_CORNERS};
use crate::css::{Length, parse_length, split_top_level, split_words};
use crate::layout::{CornerRadii, Rect};
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Radius {
    Length(Length),
    ClosestSide,
    FarthestSide,
}

#[derive(Debug, PartialEq)]
enum Shape {
    // top, right, bottom and left
    Inset([Length; 4], Box<CornerLengths>),
    Circle(Radius, (Length, Length)),
    Ellipse((Radius, Radius), (Length, Length)),
    Polygon {
        even_odd: bool,
        points: Vec<(Length, Length)>,
    },
}

// a resolved clip relative to the border box corner
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ClipRegion {
    // circles and ellipses are rounded rects whose radii meet
    Rounded(Rect, CornerRadii),
    Polygon {
        even_odd: bool,
        points: Rc<[(f32, f32)]>,
    },
}

impl ClipRegion {
    // nothing inside it paints or takes the pointer, as with the inset(50%) hiding idiom
    pub(crate) fn is_empty(&self) -> bool {
        match self {
            Self::Rounded(rect, _) => rect.width <= 0.0 || rect.height <= 0.0,
            Self::Polygon { points, .. } => points.len() < 3,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct ClipPath(u32);

const MAX_CLIP_PATHS: usize = 4096;
// coverage walks every edge per pixel, so a polygon stays small enough to paint
const MAX_POLYGON_POINTS: usize = 256;

thread_local! {
    static CLIP_PATHS: RefCell<Vec<(String, Rc<Shape>)>> = const { RefCell::new(Vec::new()) };
}

impl ClipPath {
    // a basic shape with an optional border-box; other reference boxes and url() are invalid
    pub(crate) fn parse(value: &str) -> Option<Self> {
        let source = value.trim();
        if let Some(index) =
            CLIP_PATHS.with(|paths| paths.borrow().iter().position(|(key, _)| key == source))
        {
            return Some(Self(index as u32));
        }
        let mut shape = None;
        for word in split_words(source) {
            if word.eq_ignore_ascii_case("border-box") {
                continue;
            }
            let (name, arguments) = word.split_once('(')?;
            let arguments = arguments.strip_suffix(')')?;
            if shape.is_some() {
                return None;
            }
            shape = Some(match name.to_ascii_lowercase().as_str() {
                "inset" => parse_inset(arguments)?,
                "circle" => {
                    let (radii, center) = parse_radii_at(arguments)?;
                    match radii.as_slice() {
                        [] => Shape::Circle(Radius::ClosestSide, center),
                        [radius] => Shape::Circle(*radius, center),
                        _ => return None,
                    }
                }
                "ellipse" => {
                    let (radii, center) = parse_radii_at(arguments)?;
                    match radii.as_slice() {
                        [] => Shape::Ellipse((Radius::ClosestSide, Radius::ClosestSide), center),
                        [x, y] => Shape::Ellipse((*x, *y), center),
                        _ => return None,
                    }
                }
                "polygon" => parse_polygon(arguments)?,
                _ => return None,
            });
        }
        let shape = Rc::new(shape?);
        CLIP_PATHS.with(|paths| {
            let mut paths = paths.borrow_mut();
            if paths.len() >= MAX_CLIP_PATHS {
                return None;
            }
            paths.push((source.to_string(), shape));
            Some(Self(paths.len() as u32 - 1))
        })
    }

    // percentages of x run along the width and of y down the height; a circle's radius
    // resolves against the box diagonal over the square root of two
    pub(crate) fn resolve(self, width: f32, height: f32, font_size: f32) -> Option<ClipRegion> {
        let shape = CLIP_PATHS.with(|paths| {
            paths
                .borrow()
                .get(self.0 as usize)
                .map(|(_, shape)| shape.clone())
        })?;
        let x = |length: Length| length.resolve(width, font_size).unwrap_or(0.0);
        let y = |length: Length| length.resolve(height, font_size).unwrap_or(0.0);
        let ellipse = |(center_x, center_y): (f32, f32), radius_x: f32, radius_y: f32| {
            let rect = Rect {
                x: center_x - radius_x,
                y: center_y - radius_y,
                width: radius_x * 2.0,
                height: radius_y * 2.0,
            };
            ClipRegion::Rounded(rect, [(radius_x, radius_y); 4])
        };
        Some(match &*shape {
            Shape::Inset([top, right, bottom, left], radii) => {
                let rect = Rect {
                    x: x(*left),
                    y: y(*top),
                    width: (width - x(*left) - x(*right)).max(0.0),
                    height: (height - y(*top) - y(*bottom)).max(0.0),
                };
                let radii = crate::layout::resolve_corner_radii(
                    **radii,
                    rect.width,
                    rect.height,
                    font_size,
                );
                ClipRegion::Rounded(rect, radii)
            }
            Shape::Circle(radius, (center_x, center_y)) => {
                let center = (x(*center_x), y(*center_y));
                let sides = [center.0, width - center.0, center.1, height - center.1];
                let reference = width.hypot(height) / std::f32::consts::SQRT_2;
                let radius = extent(*radius, &sides, reference, font_size);
                ellipse(center, radius, radius)
            }
            Shape::Ellipse((radius_x, radius_y), (center_x, center_y)) => {
                let center = (x(*center_x), y(*center_y));
                let radius_x = extent(*radius_x, &[center.0, width - center.0], width, font_size);
                let radius_y = extent(*radius_y, &[center.1, height - center.1], height, font_size);
                ellipse(center, radius_x, radius_y)
            }
            Shape::Polygon { even_odd, points } => ClipRegion::Polygon {
                even_odd: *even_odd,
                points: points.iter().map(|(px, py)| (x(*px), y(*py))).collect(),
            },
        })
    }
}

fn extent(radius: Radius, sides: &[f32], reference: f32, font_size: f32) -> f32 {
    let distances = sides.iter().map(|side| side.abs());
    match radius {
        Radius::Length(length) => length.resolve(reference, font_size).unwrap_or(0.0),
        Radius::ClosestSide => distances.fold(f32::INFINITY, f32::min),
        Radius::FarthestSide => distances.fold(0.0, f32::max),
    }
    .max(0.0)
}

fn length(word: &str) -> Option<Length> {
    parse_length(word).filter(|length| *length != Length::Auto)
}

fn parse_inset(arguments: &str) -> Option<Shape> {
    let words = split_words(arguments);
    let round = words
        .iter()
        .position(|word| word.eq_ignore_ascii_case("round"));
    let (edges, radii) = words.split_at(round.unwrap_or(words.len()));
    let edges = edges
        .iter()
        .map(|word| length(word))
        .collect::<Option<Vec<_>>>()?;
    let radii = match radii {
        [] => SQUARE_CORNERS,
        [_, radii @ ..] => crate::border::parse_radii(&radii.join(" "))?,
    };
    Some(Shape::Inset(
        crate::border::expand(&edges)?,
        Box::new(radii),
    ))
}

// the radius words, then the position after `at`, centered when absent
fn parse_radii_at(arguments: &str) -> Option<(Vec<Radius>, (Length, Length))> {
    let words = split_words(arguments);
    let at = words
        .iter()
        .position(|word| word.eq_ignore_ascii_case("at"));
    let (radii, position) = words.split_at(at.unwrap_or(words.len()));
    let radii = radii
        .iter()
        .map(|word| match word.to_ascii_lowercase().as_str() {
            "closest-side" => Some(Radius::ClosestSide),
            "farthest-side" => Some(Radius::FarthestSide),
            _ => length(word)
                .filter(|length| length.resolve(1.0, 16.0).is_none_or(|px| px >= 0.0))
                .map(Radius::Length),
        })
        .collect::<Option<Vec<_>>>()?;
    let center = match position {
        [] => (Length::percent(50.0), Length::percent(50.0)),
        [_, position @ ..] => crate::css::parse_background_position(&position.join(" "))?,
    };
    Some((radii, center))
}

fn parse_polygon(arguments: &str) -> Option<Shape> {
    let mut parts = split_top_level(arguments);
    let rule = parts.first()?.trim().to_ascii_lowercase();
    let even_odd = rule == "evenodd";
    if even_odd || rule == "nonzero" {
        parts.remove(0);
    }
    if parts.is_empty() || parts.len() > MAX_POLYGON_POINTS {
        return None;
    }
    let points = parts
        .into_iter()
        .map(|part| match split_words(part).as_slice() {
            [px, py] => Some((length(px)?, length(py)?)),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Shape::Polygon { even_odd, points })
}

fn edges(points: &[(f32, f32)]) -> impl Iterator<Item = ((f32, f32), (f32, f32))> + '_ {
    points
        .iter()
        .copied()
        .zip(points.iter().copied().cycle().skip(1))
}

pub(crate) fn polygon_contains(points: &[(f32, f32)], even_odd: bool, point: (f32, f32)) -> bool {
    let winding: i32 = edges(points)
        .filter(|(from, to)| (from.1 <= point.1) != (to.1 <= point.1))
        .filter(|(from, to)| {
            from.0 + (point.1 - from.1) / (to.1 - from.1) * (to.0 - from.0) > point.0
        })
        .map(|(from, to)| if to.1 > from.1 { 1 } else { -1 })
        .sum();
    if even_odd {
        winding % 2 != 0
    } else {
        winding != 0
    }
}

// one pixel of feathering across each edge, matching the rounded masks
pub(crate) fn polygon_coverage(points: &[(f32, f32)], even_odd: bool, point: (f32, f32)) -> f32 {
    let nearest = edges(points)
        .map(|(from, to)| {
            let along = (to.0 - from.0, to.1 - from.1);
            let length = along.0 * along.0 + along.1 * along.1;
            let t = if length > 0.0 {
                (((point.0 - from.0) * along.0 + (point.1 - from.1) * along.1) / length)
                    .clamp(0.0, 1.0)
            } else {
                0.0
            };
            (point.0 - from.0 - along.0 * t).hypot(point.1 - from.1 - along.1 * t)
        })
        .fold(f32::INFINITY, f32::min);
    let distance = if polygon_contains(points, even_odd, point) {
        -nearest
    } else {
        nearest
    };
    (0.5 - distance).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(value: &str, width: f32, height: f32) -> Option<ClipRegion> {
        ClipPath::parse(value)?.resolve(width, height, 16.0)
    }

    #[test]
    fn resolves_basic_shapes_against_the_border_box() {
        let rect = |x, y, width, height| Rect {
            x,
            y,
            width,
            height,
        };
        assert_eq!(
            region("inset(10px 20%)", 100.0, 50.0),
            Some(ClipRegion::Rounded(
                rect(20.0, 10.0, 60.0, 30.0),
                SQUARE_CORNERS_PX
            ))
        );
        assert_eq!(
            region("inset(50%)", 100.0, 50.0),
            Some(ClipRegion::Rounded(
                rect(50.0, 25.0, 0.0, 0.0),
                SQUARE_CORNERS_PX
            ))
        );
        let Some(ClipRegion::Rounded(_, radii)) = region("inset(0 round 4px 8px)", 100.0, 50.0)
        else {
            panic!("inset with radii")
        };
        assert_eq!(radii, [(4.0, 4.0), (8.0, 8.0), (4.0, 4.0), (8.0, 8.0)]);
        assert_eq!(
            region("circle()", 100.0, 40.0),
            Some(ClipRegion::Rounded(
                rect(30.0, 0.0, 40.0, 40.0),
                [(20.0, 20.0); 4]
            ))
        );
        let Some(ClipRegion::Rounded(circle, _)) =
            region("circle(50% at left top) border-box", 60.0, 80.0)
        else {
            panic!("circle at a corner")
        };
        let radius = 50.0 / std::f32::consts::SQRT_2;
        assert!((circle.width - radius * 2.0).abs() < 0.01 && circle.x == -circle.width / 2.0);
        assert_eq!(
            region("ellipse(farthest-side 10px at 25% 50%)", 100.0, 40.0),
            Some(ClipRegion::Rounded(
                rect(-50.0, 10.0, 150.0, 20.0),
                [(75.0, 10.0); 4]
            ))
        );
        assert_eq!(
            region(
                "polygon(0 0, calc(100% - 14px) 0, 100% 14px, 100% 100%, 0 100%)",
                200.0,
                44.0
            ),
            Some(ClipRegion::Polygon {
                even_odd: false,
                points: Rc::from([
                    (0.0, 0.0),
                    (186.0, 0.0),
                    (200.0, 14.0),
                    (200.0, 44.0),
                    (0.0, 44.0)
                ]),
            })
        );
        for invalid in [
            "none",
            "url(#mask)",
            "inset()",
            "circle(-5px)",
            "polygon(evenodd)",
            "padding-box circle()",
            "circle() ellipse()",
        ] {
            assert_eq!(region(invalid, 10.0, 10.0), None, "{invalid}");
        }
    }

    const SQUARE_CORNERS_PX: CornerRadii = [(0.0, 0.0); 4];

    #[test]
    fn polygons_contain_points_by_their_fill_rule() {
        // a square drawn twice around, which nonzero fills and evenodd leaves hollow
        let twice = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)].repeat(2);
        assert!(polygon_contains(&twice, false, (5.0, 5.0)));
        assert!(!polygon_contains(&twice, true, (5.0, 5.0)));
        let corner_cut = [
            (0.0, 0.0),
            (6.0, 0.0),
            (10.0, 4.0),
            (10.0, 10.0),
            (0.0, 10.0),
        ];
        assert!(!polygon_contains(&corner_cut, false, (9.5, 0.5)));
        assert_eq!(polygon_coverage(&corner_cut, false, (5.0, 5.0)), 1.0);
        assert_eq!(polygon_coverage(&corner_cut, false, (9.5, 0.5)), 0.0);
        assert_eq!(polygon_coverage(&corner_cut, false, (0.0, 5.0)), 0.5);
    }
}
