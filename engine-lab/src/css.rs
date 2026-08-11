use crate::dom::Element;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Display {
    Block,
    Inline,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

impl Color {
    pub const BLACK: Self = Self::opaque(0, 0, 0);
    pub const WHITE: Self = Self::opaque(255, 255, 255);

    pub const fn opaque(red: u8, green: u8, blue: u8) -> Self {
        Self {
            red,
            green,
            blue,
            alpha: 255,
        }
    }

    pub const fn transparent() -> Self {
        Self {
            red: 0,
            green: 0,
            blue: 0,
            alpha: 0,
        }
    }

    pub fn parse(input: &str) -> Option<Self> {
        let input = input.trim().to_ascii_lowercase();
        if let Some(hex) = input.strip_prefix('#') {
            return match hex.len() {
                3 => Some(Self::opaque(
                    expand_nibble(hex.as_bytes()[0])?,
                    expand_nibble(hex.as_bytes()[1])?,
                    expand_nibble(hex.as_bytes()[2])?,
                )),
                6 => Some(Self::opaque(
                    parse_hex_byte(&hex[0..2])?,
                    parse_hex_byte(&hex[2..4])?,
                    parse_hex_byte(&hex[4..6])?,
                )),
                _ => None,
            };
        }
        match input.as_str() {
            "black" => Some(Self::BLACK),
            "white" => Some(Self::WHITE),
            "transparent" => Some(Self::transparent()),
            "red" => Some(Self::opaque(255, 0, 0)),
            "green" => Some(Self::opaque(0, 128, 0)),
            "blue" => Some(Self::opaque(0, 0, 255)),
            "yellow" => Some(Self::opaque(255, 255, 0)),
            "gray" | "grey" => Some(Self::opaque(128, 128, 128)),
            "rebeccapurple" => Some(Self::opaque(102, 51, 153)),
            _ => parse_rgb(&input),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Length {
    Auto,
    Px(f32),
    Em(f32),
    Percent(f32),
}

impl Length {
    pub(crate) fn resolve(self, containing: f32, font_size: f32) -> Option<f32> {
        match self {
            Self::Auto => None,
            Self::Px(value) => Some(value),
            Self::Em(value) => Some(value * font_size),
            Self::Percent(value) => Some(containing * value / 100.0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Edges {
    pub top: Length,
    pub right: Length,
    pub bottom: Length,
    pub left: Length,
}

impl Edges {
    pub(crate) const fn zero() -> Self {
        Self {
            top: Length::Px(0.0),
            right: Length::Px(0.0),
            bottom: Length::Px(0.0),
            left: Length::Px(0.0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextAlign {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FontWeight {
    Normal,
    Bold,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Style {
    pub display: Display,
    pub width: Length,
    pub height: Length,
    pub margin: Edges,
    pub padding: Edges,
    pub background: Color,
    pub color: Color,
    pub font_size: Length,
    pub line_height: Length,
    pub border_width: f32,
    pub border_color: Color,
    pub text_align: TextAlign,
    pub font_weight: FontWeight,
}

impl Style {
    fn default_for(
        name: &str,
        parent_font_size: f32,
        parent_color: Color,
        parent_weight: FontWeight,
    ) -> Self {
        let mut style = Self {
            display: default_display(name),
            width: Length::Auto,
            height: Length::Auto,
            margin: Edges::zero(),
            padding: Edges::zero(),
            background: Color::transparent(),
            color: parent_color,
            font_size: Length::Px(parent_font_size),
            line_height: Length::Em(1.25),
            border_width: 0.0,
            border_color: Color::transparent(),
            text_align: TextAlign::Left,
            font_weight: parent_weight,
        };
        match name {
            "body" => {
                style.margin = Edges::all(Length::Px(8.0));
                style.background = Color::WHITE;
            }
            "h1" => {
                style.margin = Edges::vertical(Length::Em(0.67));
                style.font_size = Length::Em(2.0);
                style.font_weight = FontWeight::Bold;
            }
            "h2" => {
                style.margin = Edges::vertical(Length::Em(0.83));
                style.font_size = Length::Em(1.5);
                style.font_weight = FontWeight::Bold;
            }
            "p" => style.margin = Edges::vertical(Length::Em(1.0)),
            "strong" | "b" => style.font_weight = FontWeight::Bold,
            _ => {}
        }
        style
    }
}

impl Edges {
    fn all(value: Length) -> Self {
        Self {
            top: value,
            right: value,
            bottom: value,
            left: value,
        }
    }

    fn vertical(value: Length) -> Self {
        Self {
            top: value,
            right: Length::Px(0.0),
            bottom: value,
            left: Length::Px(0.0),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Stylesheet {
    rules: Vec<Rule>,
}

impl Stylesheet {
    pub fn parse(source: &str) -> Result<Self, String> {
        let source = strip_comments(source);
        let mut rules = Vec::new();
        let mut cursor = 0;
        while let Some(relative_open) = source[cursor..].find('{') {
            let open = cursor + relative_open;
            let selector_text = source[cursor..open].trim();
            let Some(relative_close) = source[open + 1..].find('}') else {
                return Err("stylesheet rule has no closing brace".to_string());
            };
            let close = open + 1 + relative_close;
            let selectors = selector_text
                .split(',')
                .map(Selector::parse)
                .collect::<Result<Vec<_>, _>>()?;
            let declarations = parse_declarations(&source[open + 1..close])?;
            rules.push(Rule {
                selectors,
                declarations,
                order: rules.len(),
            });
            cursor = close + 1;
        }
        if !source[cursor..].trim().is_empty() {
            return Err("stylesheet has text outside a rule".to_string());
        }
        Ok(Self { rules })
    }

    pub(crate) fn style_for_inherited(
        &self,
        element: &Element,
        parent_font_size: f32,
        parent_color: Color,
        parent_weight: FontWeight,
    ) -> Style {
        let mut style =
            Style::default_for(&element.name, parent_font_size, parent_color, parent_weight);
        let mut values: HashMap<String, (Specificity, usize, String)> = HashMap::new();
        for rule in &self.rules {
            let Some(specificity) = rule
                .selectors
                .iter()
                .filter(|selector| selector.matches(element))
                .map(Selector::specificity)
                .max()
            else {
                continue;
            };
            for declaration in &rule.declarations {
                let candidate = (specificity, rule.order, declaration.value.clone());
                let replace = values
                    .get(&declaration.property)
                    .is_none_or(|current| (candidate.0, candidate.1) >= (current.0, current.1));
                if replace {
                    values.insert(declaration.property.clone(), candidate);
                }
            }
        }
        if let Some(inline) = element.attr("style")
            && let Ok(declarations) = parse_declarations(inline)
        {
            for declaration in declarations {
                values.insert(
                    declaration.property.clone(),
                    (Specificity::INLINE, usize::MAX, declaration.value),
                );
            }
        }
        for (property, (_, _, value)) in values {
            apply_declaration(&mut style, &property, &value);
        }
        style
    }
}

#[derive(Clone, Debug)]
struct Rule {
    selectors: Vec<Selector>,
    declarations: Vec<Declaration>,
    order: usize,
}

#[derive(Clone, Debug)]
struct Declaration {
    property: String,
    value: String,
}

#[derive(Clone, Debug)]
struct Selector {
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Specificity(u32, u32, u32);

impl Specificity {
    const INLINE: Self = Self(u32::MAX, u32::MAX, u32::MAX);
}

impl Selector {
    fn parse(source: &str) -> Result<Self, String> {
        let source = source.trim();
        if source.is_empty() {
            return Err("empty selector".to_string());
        }
        if source.contains(char::is_whitespace) {
            return Err(format!(
                "descendant selectors are not implemented: {source}"
            ));
        }
        let mut selector = Self {
            tag: None,
            id: None,
            classes: Vec::new(),
        };
        let mut cursor = 0;
        while cursor < source.len() {
            let byte = source.as_bytes()[cursor];
            match byte {
                b'#' => {
                    cursor += 1;
                    selector.id = Some(parse_selector_name(source, &mut cursor)?);
                }
                b'.' => {
                    cursor += 1;
                    selector
                        .classes
                        .push(parse_selector_name(source, &mut cursor)?);
                }
                b'*' => cursor += 1,
                _ => {
                    if selector.tag.is_some() {
                        return Err(format!("invalid selector: {source}"));
                    }
                    selector.tag = Some(parse_selector_name(source, &mut cursor)?);
                }
            }
        }
        Ok(selector)
    }

    fn matches(&self, element: &Element) -> bool {
        self.tag.as_deref().is_none_or(|tag| tag == element.name)
            && self
                .id
                .as_deref()
                .is_none_or(|id| element.attr("id") == Some(id))
            && self.classes.iter().all(|class| element.has_class(class))
    }

    fn specificity(&self) -> Specificity {
        Specificity(
            u32::from(self.id.is_some()),
            self.classes.len() as u32,
            u32::from(self.tag.is_some()),
        )
    }
}

fn parse_selector_name(source: &str, cursor: &mut usize) -> Result<String, String> {
    let start = *cursor;
    while *cursor < source.len() {
        let byte = source.as_bytes()[*cursor];
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') {
            *cursor += 1;
        } else {
            break;
        }
    }
    if *cursor == start {
        return Err(format!("selector component has no name: {source}"));
    }
    Ok(source[start..*cursor].to_ascii_lowercase())
}

fn parse_declarations(source: &str) -> Result<Vec<Declaration>, String> {
    let mut declarations = Vec::new();
    for chunk in source.split(';') {
        let chunk = chunk.trim();
        if chunk.is_empty() {
            continue;
        }
        let Some(colon) = chunk.find(':') else {
            return Err(format!("declaration has no colon: {chunk}"));
        };
        let property = chunk[..colon].trim().to_ascii_lowercase();
        let value = chunk[colon + 1..].trim().to_string();
        if property.is_empty() || value.is_empty() {
            return Err(format!("invalid declaration: {chunk}"));
        }
        declarations.push(Declaration { property, value });
    }
    Ok(declarations)
}

fn apply_declaration(style: &mut Style, property: &str, value: &str) {
    match property {
        "display" => {
            style.display = match value.trim().to_ascii_lowercase().as_str() {
                "none" => Display::None,
                "inline" => Display::Inline,
                _ => Display::Block,
            };
        }
        "width" => style.width = parse_length(value).unwrap_or(Length::Auto),
        "height" => style.height = parse_length(value).unwrap_or(Length::Auto),
        "margin" => style.margin = parse_edges(value),
        "margin-top" => style.margin.top = parse_length(value).unwrap_or(style.margin.top),
        "margin-right" => style.margin.right = parse_length(value).unwrap_or(style.margin.right),
        "margin-bottom" => style.margin.bottom = parse_length(value).unwrap_or(style.margin.bottom),
        "margin-left" => style.margin.left = parse_length(value).unwrap_or(style.margin.left),
        "padding" => style.padding = parse_edges(value),
        "padding-top" => style.padding.top = parse_length(value).unwrap_or(style.padding.top),
        "padding-right" => style.padding.right = parse_length(value).unwrap_or(style.padding.right),
        "padding-bottom" => {
            style.padding.bottom = parse_length(value).unwrap_or(style.padding.bottom)
        }
        "padding-left" => style.padding.left = parse_length(value).unwrap_or(style.padding.left),
        "background" | "background-color" => {
            style.background = Color::parse(value).unwrap_or(style.background)
        }
        "color" => style.color = Color::parse(value).unwrap_or(style.color),
        "font-size" => style.font_size = parse_length(value).unwrap_or(style.font_size),
        "line-height" => style.line_height = parse_line_height(value).unwrap_or(style.line_height),
        "font-weight" => {
            if value.trim().eq_ignore_ascii_case("bold") || value.trim() == "700" {
                style.font_weight = FontWeight::Bold;
            }
        }
        "text-align" => {
            style.text_align = match value.trim().to_ascii_lowercase().as_str() {
                "center" => TextAlign::Center,
                "right" => TextAlign::Right,
                _ => TextAlign::Left,
            };
        }
        "border" => parse_border(style, value),
        "border-width" => {
            style.border_width = parse_length(value)
                .and_then(|length| length.resolve(0.0, 16.0))
                .unwrap_or(style.border_width)
                .max(0.0);
        }
        "border-color" => style.border_color = Color::parse(value).unwrap_or(style.border_color),
        _ => {}
    }
}

fn parse_border(style: &mut Style, value: &str) {
    let mut width = None;
    let mut color = None;
    for token in value.split_ascii_whitespace() {
        if width.is_none() {
            width = parse_length(token).and_then(|length| length.resolve(0.0, 16.0));
        }
        if color.is_none() {
            color = Color::parse(token);
        }
    }
    if let Some(width) = width {
        style.border_width = width.max(0.0);
    }
    if let Some(color) = color {
        style.border_color = color;
    }
}

fn parse_edges(value: &str) -> Edges {
    let mut values = value
        .split_ascii_whitespace()
        .filter_map(parse_length)
        .collect::<Vec<_>>();
    if values.is_empty() {
        return Edges::zero();
    }
    if values.len() > 4 {
        values.truncate(4);
    }
    match values.as_slice() {
        [all] => Edges::all(*all),
        [vertical, horizontal] => Edges {
            top: *vertical,
            right: *horizontal,
            bottom: *vertical,
            left: *horizontal,
        },
        [top, horizontal, bottom] => Edges {
            top: *top,
            right: *horizontal,
            bottom: *bottom,
            left: *horizontal,
        },
        [top, right, bottom, left] => Edges {
            top: *top,
            right: *right,
            bottom: *bottom,
            left: *left,
        },
        _ => Edges::zero(),
    }
}

fn parse_length(value: &str) -> Option<Length> {
    let value = value.trim().to_ascii_lowercase();
    if value == "auto" {
        return Some(Length::Auto);
    }
    if let Some(value) = value.strip_suffix("px") {
        return value.parse().ok().map(Length::Px);
    }
    if let Some(value) = value.strip_suffix("em") {
        return value.parse().ok().map(Length::Em);
    }
    if let Some(value) = value.strip_suffix('%') {
        return value.parse().ok().map(Length::Percent);
    }
    (value == "0").then_some(Length::Px(0.0))
}

fn parse_line_height(value: &str) -> Option<Length> {
    let value = value.trim();
    if let Ok(multiplier) = value.parse::<f32>() {
        return Some(Length::Em(multiplier));
    }
    parse_length(value)
}

fn default_display(name: &str) -> Display {
    match name {
        "#document" | "html" | "body" | "address" | "article" | "aside" | "blockquote" | "div"
        | "dl" | "fieldset" | "figcaption" | "figure" | "footer" | "form" | "h1" | "h2" | "h3"
        | "h4" | "h5" | "h6" | "header" | "hgroup" | "hr" | "main" | "nav" | "ol" | "p" | "pre"
        | "section" | "table" | "ul" => Display::Block,
        "head" | "link" | "meta" | "script" | "style" | "title" => Display::None,
        _ => Display::Inline,
    }
}

fn strip_comments(source: &str) -> String {
    let mut output = String::with_capacity(source.len());
    let mut cursor = 0;
    while let Some(relative_start) = source[cursor..].find("/*") {
        let start = cursor + relative_start;
        output.push_str(&source[cursor..start]);
        let Some(relative_end) = source[start + 2..].find("*/") else {
            break;
        };
        cursor = start + 2 + relative_end + 2;
    }
    output.push_str(&source[cursor..]);
    output
}

fn parse_rgb(input: &str) -> Option<Color> {
    let input = input.strip_prefix("rgb(")?.strip_suffix(')')?;
    let values = input
        .split(',')
        .map(|value| value.trim().parse::<u8>().ok())
        .collect::<Option<Vec<_>>>()?;
    (values.len() == 3).then(|| Color::opaque(values[0], values[1], values[2]))
}

fn parse_hex_byte(value: &str) -> Option<u8> {
    u8::from_str_radix(value, 16).ok()
}

fn expand_nibble(value: u8) -> Option<u8> {
    let value = match value {
        b'0'..=b'9' => value - b'0',
        b'a'..=b'f' => value - b'a' + 10,
        b'A'..=b'F' => value - b'A' + 10,
        _ => return None,
    };
    Some(value * 17)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_colors_and_lengths() {
        assert_eq!(Color::parse("#42d3b4"), Some(Color::opaque(66, 211, 180)));
        assert_eq!(Color::parse("#abc"), Some(Color::opaque(170, 187, 204)));
        assert_eq!(parse_length("2em"), Some(Length::Em(2.0)));
        assert_eq!(parse_length("68%"), Some(Length::Percent(68.0)));
    }

    #[test]
    fn selector_specificity_controls_the_cascade() {
        let stylesheet = Stylesheet::parse(
            "p { color: red; } .accent { color: blue; } #hero { color: #42d3b4; }",
        )
        .expect("stylesheet should parse");
        let mut element = Element::new("p");
        element
            .attrs
            .insert("class".to_string(), "accent".to_string());
        element.attrs.insert("id".to_string(), "hero".to_string());
        assert_eq!(
            stylesheet
                .style_for_inherited(&element, 16.0, Color::BLACK, FontWeight::Normal)
                .color,
            Color::opaque(66, 211, 180)
        );
    }

    #[test]
    fn parses_four_edge_shorthand() {
        let edges = parse_edges("1px 2px 3px 4px");
        assert_eq!(edges.top, Length::Px(1.0));
        assert_eq!(edges.right, Length::Px(2.0));
        assert_eq!(edges.bottom, Length::Px(3.0));
        assert_eq!(edges.left, Length::Px(4.0));
    }
}
