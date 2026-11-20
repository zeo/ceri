// conditions that decide whether rules apply: media queries, container queries and @supports

use crate::css::{ColorScheme, StyleState};
use crate::dom::{Document, NodeId};

// a boolean combination of leaf tests, as media, container and supports conditions share it
#[derive(Clone, Debug)]
enum Condition<T> {
    Leaf(T),
    Not(Box<Condition<T>>),
    And(Vec<Condition<T>>),
    Or(Vec<Condition<T>>),
    // a test this engine does not know, which is false
    Unknown,
}

impl<T> Condition<T> {
    fn evaluate(&self, test: &dyn Fn(&T) -> bool) -> bool {
        match self {
            Self::Leaf(leaf) => test(leaf),
            Self::Not(inner) => !inner.evaluate(test),
            Self::And(parts) => parts.iter().all(|part| part.evaluate(test)),
            Self::Or(parts) => parts.iter().any(|part| part.evaluate(test)),
            Self::Unknown => false,
        }
    }
}

// splits on a keyword outside parentheses, such as and, or and commas
fn split_top_level<'a>(text: &'a str, separator: &str) -> Vec<&'a str> {
    let lower = text.to_ascii_lowercase();
    let mut parts = Vec::new();
    let mut depth = 0_usize;
    let mut start = 0;
    let mut index = 0;
    let bytes = text.as_bytes();
    while index < bytes.len() {
        match bytes[index] {
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            _ if depth == 0 && lower[index..].starts_with(separator) => {
                parts.push(&text[start..index]);
                index += separator.len();
                start = index;
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    parts.push(&text[start..]);
    parts
}

fn parse_condition<T>(text: &str, leaf: &dyn Fn(&str) -> Option<T>) -> Condition<T> {
    let text = text.trim();
    if let Some(rest) = strip_keyword(text, "not") {
        return Condition::Not(Box::new(parse_in_parens(rest, leaf)));
    }
    let ands = split_top_level(text, " and ");
    if ands.len() > 1 {
        return Condition::And(
            ands.into_iter()
                .map(|part| parse_in_parens(part, leaf))
                .collect(),
        );
    }
    let ors = split_top_level(text, " or ");
    if ors.len() > 1 {
        return Condition::Or(
            ors.into_iter()
                .map(|part| parse_in_parens(part, leaf))
                .collect(),
        );
    }
    parse_in_parens(text, leaf)
}

fn parse_in_parens<T>(text: &str, leaf: &dyn Fn(&str) -> Option<T>) -> Condition<T> {
    let text = text.trim();
    if !is_wrapped(text) {
        return leaf(text).map_or(Condition::Unknown, Condition::Leaf);
    }
    let inner = text[1..text.len() - 1].trim();
    if inner.starts_with('(') || strip_keyword(inner, "not").is_some() {
        return parse_condition(inner, leaf);
    }
    leaf(inner).map_or(Condition::Unknown, Condition::Leaf)
}

// whether the opening parenthesis is matched by the final character
fn is_wrapped(text: &str) -> bool {
    if !text.starts_with('(') {
        return false;
    }
    let mut depth = 0_usize;
    for (index, byte) in text.bytes().enumerate() {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return index == text.len() - 1;
                }
            }
            _ => {}
        }
    }
    false
}

fn strip_keyword<'a>(text: &'a str, keyword: &str) -> Option<&'a str> {
    let head = text.get(..keyword.len())?;
    let rest = &text[keyword.len()..];
    (head.eq_ignore_ascii_case(keyword) && rest.starts_with([' ', '('])).then_some(rest)
}

// a closed or open bound on a size
#[derive(Clone, Copy, Debug, Default)]
struct Range {
    min: Option<(f32, bool)>,
    max: Option<(f32, bool)>,
}

impl Range {
    fn contains(&self, value: f32) -> bool {
        self.min.is_none_or(|(bound, inclusive)| {
            if inclusive {
                value >= bound
            } else {
                value > bound
            }
        }) && self.max.is_none_or(|(bound, inclusive)| {
            if inclusive {
                value <= bound
            } else {
                value < bound
            }
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Contrast {
    NoPreference,
    More,
    Less,
    Custom,
}

#[derive(Clone, Copy, Debug)]
enum Feature {
    Width(Range),
    Height(Range),
    AspectRatio(Range),
    Portrait(bool),
    ColorScheme(ColorScheme),
    ReducedMotion(bool),
    ForcedColors(bool),
    Contrast(Contrast),
    Hover(bool),
    FinePointer(bool),
    Always(bool),
}

fn parse_dimension(value: &str) -> Option<f32> {
    let value = value.trim().to_ascii_lowercase();
    if value == "0" {
        return Some(0.0);
    }
    let split = value
        .find(|character: char| {
            !(character.is_ascii_digit() || matches!(character, '.' | '-' | '+'))
        })
        .unwrap_or(value.len());
    let number = value[..split].parse::<f32>().ok()?;
    let scale = match &value[split..] {
        "px" => 1.0,
        "em" | "rem" => 16.0,
        _ => return None,
    };
    let size = number * scale;
    size.is_finite().then_some(size)
}

fn parse_ratio(value: &str) -> Option<f32> {
    let (width, height) = match value.split_once('/') {
        Some((width, height)) => (
            width.trim().parse::<f32>().ok()?,
            height.trim().parse::<f32>().ok()?,
        ),
        None => (value.trim().parse::<f32>().ok()?, 1.0),
    };
    (height > 0.0).then_some(width / height)
}

// a size feature by name, from min-, max- or plain forms and from range comparisons
fn size_feature(name: &str, range: Range) -> Option<Feature> {
    Some(match name {
        "width" | "inline-size" => Feature::Width(range),
        "height" | "block-size" => Feature::Height(range),
        "aspect-ratio" => Feature::AspectRatio(range),
        _ => return None,
    })
}

fn parse_feature(text: &str) -> Option<Feature> {
    let text = text.trim().to_ascii_lowercase();
    if text.contains(['<', '>', '=']) && !text.contains(':') {
        return parse_range_feature(&text);
    }
    let Some((name, value)) = text.split_once(':') else {
        return Some(match text.as_str() {
            "hover" | "any-hover" | "pointer" | "any-pointer" | "color" | "grid" => {
                Feature::Always(text != "grid")
            }
            "monochrome" | "inverted-colors" => Feature::Always(false),
            _ => return None,
        });
    };
    let (name, value) = (name.trim(), value.trim());
    let measured = |value: &str, name: &str| {
        if name.ends_with("aspect-ratio") {
            parse_ratio(value)
        } else {
            parse_dimension(value)
        }
    };
    if let Some(base) = name.strip_prefix("min-") {
        let bound = measured(value, base)?;
        return size_feature(
            base,
            Range {
                min: Some((bound, true)),
                max: None,
            },
        );
    }
    if let Some(base) = name.strip_prefix("max-") {
        let bound = measured(value, base)?;
        return size_feature(
            base,
            Range {
                min: None,
                max: Some((bound, true)),
            },
        );
    }
    Some(match name {
        "width" | "height" | "inline-size" | "block-size" | "aspect-ratio" => {
            let exact = measured(value, name)?;
            size_feature(
                name,
                Range {
                    min: Some((exact, true)),
                    max: Some((exact, true)),
                },
            )?
        }
        "orientation" => Feature::Portrait(match value {
            "portrait" => true,
            "landscape" => false,
            _ => return None,
        }),
        "prefers-color-scheme" => Feature::ColorScheme(match value {
            "dark" => ColorScheme::Dark,
            "light" => ColorScheme::Light,
            _ => return None,
        }),
        "prefers-reduced-motion" => Feature::ReducedMotion(match value {
            "reduce" => true,
            "no-preference" => false,
            _ => return None,
        }),
        "hover" | "any-hover" => Feature::Hover(value == "hover"),
        "pointer" | "any-pointer" => Feature::FinePointer(value == "fine"),
        "forced-colors" => Feature::ForcedColors(match value {
            "active" => true,
            "none" => false,
            _ => return None,
        }),
        "prefers-contrast" => Feature::Contrast(match value {
            "no-preference" => Contrast::NoPreference,
            "more" => Contrast::More,
            "less" => Contrast::Less,
            "custom" => Contrast::Custom,
            _ => return None,
        }),
        "prefers-reduced-transparency" => Feature::Always(value == "no-preference"),
        "display-mode" => Feature::Always(value == "standalone"),
        "scripting" => Feature::Always(value == "enabled"),
        _ => return None,
    })
}

// (width >= 600px), (400px <= width < 900px) and the like
fn parse_range_feature(text: &str) -> Option<Feature> {
    let mut tokens = Vec::new();
    let mut rest = text.trim();
    while !rest.is_empty() {
        let operator = ["<=", ">=", "<", ">", "="]
            .into_iter()
            .map(|operator| (rest.find(operator), operator))
            .filter_map(|(position, operator)| position.map(|position| (position, operator)))
            .min_by_key(|(position, operator)| (*position, std::cmp::Reverse(operator.len())));
        match operator {
            Some((position, operator)) => {
                tokens.push(rest[..position].trim());
                tokens.push(operator);
                rest = rest[position + operator.len()..].trim();
            }
            None => {
                tokens.push(rest);
                rest = "";
            }
        }
    }
    let mut range = Range::default();
    let mut name = None;
    let mut apply = |operator: &str, value: f32| match operator {
        "<" => range.max = Some((value, false)),
        "<=" => range.max = Some((value, true)),
        ">" => range.min = Some((value, false)),
        ">=" => range.min = Some((value, true)),
        _ => {
            range.min = Some((value, true));
            range.max = Some((value, true));
        }
    };
    let is_name = |token: &str| {
        token
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_alphabetic())
    };
    match tokens.as_slice() {
        [left, operator, right] if is_name(left) => {
            name = Some(*left);
            apply(operator, parse_bound(left, right)?);
        }
        [left, operator, right] if is_name(right) => {
            name = Some(*right);
            apply(flip(operator), parse_bound(right, left)?);
        }
        [low, first, middle, second, high] if is_name(middle) => {
            name = Some(*middle);
            apply(flip(first), parse_bound(middle, low)?);
            apply(second, parse_bound(middle, high)?);
        }
        _ => {}
    }
    size_feature(name?, range)
}

// the same comparison seen from the other side
fn flip(operator: &str) -> &str {
    match operator {
        "<" => ">",
        "<=" => ">=",
        ">" => "<",
        ">=" => "<=",
        other => other,
    }
}

fn parse_bound(name: &str, value: &str) -> Option<f32> {
    if name.ends_with("aspect-ratio") {
        parse_ratio(value)
    } else {
        parse_dimension(value)
    }
}

#[derive(Clone, Debug)]
struct MediaQuery {
    negated: bool,
    // whether the media type covers a screen
    screen: bool,
    condition: Option<Condition<Feature>>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct MediaQueryList(Vec<MediaQuery>);

impl MediaQueryList {
    pub(crate) fn parse(text: &str) -> Self {
        let text = text.trim();
        if text.is_empty() {
            return Self(vec![MediaQuery {
                negated: false,
                screen: true,
                condition: None,
            }]);
        }
        Self(
            split_top_level(text, ",")
                .into_iter()
                .map(parse_media_query)
                .collect(),
        )
    }

    pub(crate) fn matches(&self, state: &StyleState) -> bool {
        let viewport = (state.viewport_width, state.viewport_height);
        self.0.iter().any(|query| {
            let matched = query.screen
                && query.condition.as_ref().is_none_or(|condition| {
                    condition.evaluate(&|feature| media_feature(feature, viewport, state))
                });
            matched != query.negated
        })
    }
}

fn parse_media_query(text: &str) -> MediaQuery {
    let text = text.trim();
    if text.starts_with('(')
        || strip_keyword(text, "not").is_some_and(|rest| rest.trim_start().starts_with('('))
    {
        return MediaQuery {
            negated: false,
            screen: true,
            condition: Some(parse_condition(text, &parse_feature)),
        };
    }
    let mut words = text.splitn(2, char::is_whitespace);
    let mut first = words.next().unwrap_or("").to_ascii_lowercase();
    let mut rest = words.next().unwrap_or("").trim();
    let mut negated = false;
    if first == "not" || first == "only" {
        negated = first == "not";
        let mut words = rest.splitn(2, char::is_whitespace);
        first = words.next().unwrap_or("").to_ascii_lowercase();
        rest = words.next().unwrap_or("").trim();
    }
    let screen = matches!(first.as_str(), "all" | "screen");
    let condition =
        strip_keyword(rest, "and").map(|condition| parse_condition(condition, &parse_feature));
    let malformed = !rest.is_empty() && condition.is_none();
    MediaQuery {
        negated,
        screen: screen && !malformed,
        condition,
    }
}

fn media_feature(feature: &Feature, (width, height): (f32, f32), state: &StyleState) -> bool {
    match *feature {
        Feature::Width(range) => range.contains(width),
        Feature::Height(range) => range.contains(height),
        Feature::AspectRatio(range) => height > 0.0 && range.contains(width / height),
        Feature::Portrait(portrait) => (height >= width) == portrait,
        Feature::ColorScheme(scheme) => scheme == state.color_scheme,
        Feature::ReducedMotion(reduced) => reduced == state.reduced_motion,
        Feature::ForcedColors(forced) => forced == state.forced_colors.is_some(),
        // a forced palette of very high or very low contrast asks for more or for less, as
        // chromium reads it, and any other for custom
        Feature::Contrast(asked) => {
            asked
                == match state.forced_colors.map(|colors| colors.contrast()) {
                    None => Contrast::NoPreference,
                    Some(ratio) if ratio >= 7.0 => Contrast::More,
                    Some(ratio) if ratio <= 2.5 => Contrast::Less,
                    Some(_) => Contrast::Custom,
                }
        }
        // a desktop window has a mouse
        Feature::Hover(hover) | Feature::FinePointer(hover) => hover,
        Feature::Always(result) => result,
    }
}

// a container's size and name, recorded by the layout that sized it
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ContainerSize {
    pub(crate) width: f32,
    pub(crate) height: f32,
    // false for inline-size containers, which answer no height queries
    pub(crate) both_axes: bool,
    pub(crate) name: Option<u32>,
}

#[derive(Clone, Debug)]
pub(crate) struct ContainerQuery {
    name: Option<u32>,
    condition: Condition<Feature>,
}

impl ContainerQuery {
    pub(crate) fn parse(text: &str) -> Self {
        let text = text.trim();
        let (name, condition) = match text.split_once(char::is_whitespace) {
            Some((name, rest)) if !name.starts_with('(') && !name.eq_ignore_ascii_case("not") => {
                (Some(crate::grid::intern_name(name)), rest)
            }
            _ => (None, text),
        };
        Self {
            name,
            condition: parse_condition(condition, &parse_feature),
        }
    }

    // the nearest ancestor container with the name, if any, answers the query
    pub(crate) fn matches(&self, document: &Document, node: NodeId, state: &StyleState) -> bool {
        let Some(container) = document.ancestors(node).find_map(|ancestor| {
            state
                .container_sizes
                .get(&ancestor)
                .filter(|size| self.name.is_none_or(|name| size.name == Some(name)))
        }) else {
            return false;
        };
        self.condition.evaluate(&|feature| match *feature {
            Feature::Width(range) => range.contains(container.width),
            Feature::Height(range) => container.both_axes && range.contains(container.height),
            Feature::AspectRatio(range) => {
                container.both_axes
                    && container.height > 0.0
                    && range.contains(container.width / container.height)
            }
            Feature::Portrait(portrait) => {
                container.both_axes && (container.height >= container.width) == portrait
            }
            _ => false,
        })
    }
}

// @supports: declarations this engine applies, selectors it parses, and their combinations
pub(crate) fn supports(text: &str) -> bool {
    parse_condition(text, &|leaf: &str| {
        if let Some(selector) = leaf
            .strip_prefix("selector(")
            .and_then(|rest| rest.strip_suffix(')'))
        {
            return Some(crate::css::Selector::parse_list(selector).is_ok());
        }
        let (property, value) = leaf.split_once(':')?;
        Some(crate::css::supports_declaration(
            property.trim(),
            value.trim(),
        ))
    })
    .evaluate(&|supported| *supported)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_queries_read_types_ranges_negation_and_lists() {
        let state = StyleState {
            viewport_width: 800.0,
            viewport_height: 600.0,
            ..StyleState::default()
        };
        let matches = |query: &str| MediaQueryList::parse(query).matches(&state);
        assert!(matches("screen and (min-width: 50em)"));
        assert!(!matches("print"));
        assert!(matches("not print"));
        assert!(matches("print, (orientation: landscape)"));
        assert!(matches("(400px < width <= 800px)"));
        assert!(!matches("(width < 800px)"));
        assert!(matches("(600px >= height)"));
        assert!(matches("(min-aspect-ratio: 4/3)"));
        assert!(!matches("(not (hover)) or (max-width: 10px)"));
        assert!(!matches("(unknown-feature: 1)"));
        assert!(matches(
            "only screen and ((prefers-reduced-motion: no-preference) and (pointer: fine))"
        ));
        assert!(supports("(display: flex) and (not (display: contents))"));
        assert!(!supports("(colour: red)"));
        assert!(supports("selector(a > b) or (display: nonsense)"));
        assert!(supports(
            "(font-variant-numeric: tabular-nums slashed-zero) and (font-kerning: none)"
        ));
        assert!(supports("(font-feature-settings: 'ss01' on, 'salt' 2)"));
        assert!(!supports(
            "(font-variant-numeric: tabular-nums proportional-nums)"
        ));
        assert!(!supports("(font-variant-ligatures: fancy)"));
        assert!(!supports("(font-feature-settings: ss01)"));
    }
}
