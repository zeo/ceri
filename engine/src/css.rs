use crate::dom::{Document, Element, Node, NodePath};
use std::collections::{BTreeMap, BTreeSet, HashMap};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Display {
    Block,
    Flex,
    Grid,
    Inline,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Position {
    Static,
    Relative,
    Sticky,
    Absolute,
    Fixed,
}

impl Position {
    pub(crate) fn is_out_of_flow(self) -> bool {
        matches!(self, Self::Absolute | Self::Fixed)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Overflow {
    Visible,
    Hidden,
    Auto,
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

    pub fn mix(self, other: Self, ratio: f32) -> Self {
        let ratio = ratio.clamp(0.0, 1.0);
        let blend = |from: u8, to: u8| {
            (f32::from(from) + (f32::from(to) - f32::from(from)) * ratio).round() as u8
        };
        Self {
            red: blend(self.red, other.red),
            green: blend(self.green, other.green),
            blue: blend(self.blue, other.blue),
            alpha: blend(self.alpha, other.alpha),
        }
    }

    pub fn scale_alpha(self, factor: f32) -> Self {
        Self {
            alpha: (f32::from(self.alpha) * factor.clamp(0.0, 1.0)).round() as u8,
            ..self
        }
    }

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
                4 => Some(Self {
                    red: expand_nibble(hex.as_bytes()[0])?,
                    green: expand_nibble(hex.as_bytes()[1])?,
                    blue: expand_nibble(hex.as_bytes()[2])?,
                    alpha: expand_nibble(hex.as_bytes()[3])?,
                }),
                6 => Some(Self::opaque(
                    parse_hex_byte(&hex[0..2])?,
                    parse_hex_byte(&hex[2..4])?,
                    parse_hex_byte(&hex[4..6])?,
                )),
                8 => Some(Self {
                    red: parse_hex_byte(&hex[0..2])?,
                    green: parse_hex_byte(&hex[2..4])?,
                    blue: parse_hex_byte(&hex[4..6])?,
                    alpha: parse_hex_byte(&hex[6..8])?,
                }),
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
    Terms(LengthTerms),
    Clamp(LengthTerms, LengthTerms, LengthTerms),
    Smallest(LengthTerms, LengthTerms),
    Largest(LengthTerms, LengthTerms),
}

// a length is a sum of unit terms, so calc() and plain values share one resolver
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct LengthTerms {
    pub(crate) px: f32,
    pub(crate) em: f32,
    pub(crate) rem: f32,
    pub(crate) percent: f32,
    pub(crate) viewport_width: f32,
    pub(crate) viewport_height: f32,
    pub(crate) zero_width: f32,
}

impl LengthTerms {
    fn resolve(self, containing: f32, font_size: f32) -> f32 {
        let environment = layout_environment();
        self.px
            + self.em * font_size
            + self.rem * environment.root_font_size
            + self.percent * containing / 100.0
            + self.viewport_width * environment.viewport.width / 100.0
            + self.viewport_height * environment.viewport.height / 100.0
            + self.zero_width
                * crate::font::active_fonts().advance(
                    '0',
                    crate::font::FontStack::default(),
                    font_size,
                    false,
                )
    }

    fn is_finite(self) -> bool {
        [
            self.px,
            self.em,
            self.rem,
            self.percent,
            self.viewport_width,
            self.viewport_height,
            self.zero_width,
        ]
        .iter()
        .all(|value| value.is_finite())
    }
}

impl Length {
    pub(crate) const fn px(value: f32) -> Self {
        Self::Terms(LengthTerms {
            px: value,
            em: 0.0,
            rem: 0.0,
            percent: 0.0,
            viewport_width: 0.0,
            viewport_height: 0.0,
            zero_width: 0.0,
        })
    }

    pub(crate) const fn em(value: f32) -> Self {
        Self::Terms(LengthTerms {
            px: 0.0,
            em: value,
            rem: 0.0,
            percent: 0.0,
            viewport_width: 0.0,
            viewport_height: 0.0,
            zero_width: 0.0,
        })
    }

    pub(crate) fn resolve(self, containing: f32, font_size: f32) -> Option<f32> {
        match self {
            Self::Auto => None,
            Self::Terms(terms) => Some(terms.resolve(containing, font_size)),
            Self::Clamp(minimum, preferred, maximum) => {
                let minimum = minimum.resolve(containing, font_size);
                let maximum = maximum.resolve(containing, font_size);
                Some(
                    preferred
                        .resolve(containing, font_size)
                        .clamp(minimum.min(maximum), maximum.max(minimum)),
                )
            }
            Self::Smallest(first, second) => Some(
                first
                    .resolve(containing, font_size)
                    .min(second.resolve(containing, font_size)),
            ),
            Self::Largest(first, second) => Some(
                first
                    .resolve(containing, font_size)
                    .max(second.resolve(containing, font_size)),
            ),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LayoutEnvironment {
    pub(crate) viewport: crate::layout::Size,
    pub(crate) root_font_size: f32,
}

impl Default for LayoutEnvironment {
    fn default() -> Self {
        Self {
            viewport: crate::layout::Size {
                width: 800.0,
                height: 600.0,
            },
            root_font_size: 16.0,
        }
    }
}

thread_local! {
    static LAYOUT_ENVIRONMENT: std::cell::Cell<LayoutEnvironment> =
        const { std::cell::Cell::new(LayoutEnvironment {
            viewport: crate::layout::Size { width: 800.0, height: 600.0 },
            root_font_size: 16.0,
        }) };
}

pub(crate) struct ActiveEnvironment {
    previous: LayoutEnvironment,
}

impl Drop for ActiveEnvironment {
    fn drop(&mut self) {
        LAYOUT_ENVIRONMENT.with(|current| current.set(self.previous));
    }
}

// viewport and root-relative units resolve against the pass the layout runs in
pub(crate) fn use_layout_environment(environment: LayoutEnvironment) -> ActiveEnvironment {
    let previous = LAYOUT_ENVIRONMENT.with(|current| current.replace(environment));
    ActiveEnvironment { previous }
}

pub(crate) fn layout_environment() -> LayoutEnvironment {
    LAYOUT_ENVIRONMENT.with(|current| current.get())
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
            top: Length::px(0.0),
            right: Length::px(0.0),
            bottom: Length::px(0.0),
            left: Length::px(0.0),
        }
    }

    pub(crate) const fn auto() -> Self {
        Self {
            top: Length::Auto,
            right: Length::Auto,
            bottom: Length::Auto,
            left: Length::Auto,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct InheritedText {
    pub(crate) transform: TextTransform,
    pub(crate) letter_spacing: Length,
    pub(crate) family: crate::font::FontStack,
}

impl Default for InheritedText {
    fn default() -> Self {
        Self {
            transform: TextTransform::None,
            letter_spacing: Length::px(0.0),
            family: crate::font::FontStack::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextTransform {
    None,
    Uppercase,
    Lowercase,
    Capitalize,
}

impl TextTransform {
    pub(crate) fn apply(self, text: &str) -> String {
        match self {
            Self::None => text.to_string(),
            Self::Uppercase => text.to_uppercase(),
            Self::Lowercase => text.to_lowercase(),
            Self::Capitalize => text
                .split_inclusive(char::is_whitespace)
                .map(|word| {
                    let mut characters = word.chars();
                    match characters.next() {
                        Some(first) => {
                            first.to_uppercase().collect::<String>() + characters.as_str()
                        }
                        None => String::new(),
                    }
                })
                .collect(),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FlexDirection {
    Row,
    Column,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JustifyContent {
    FlexStart,
    Center,
    FlexEnd,
    SpaceBetween,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AlignItems {
    Stretch,
    FlexStart,
    Center,
    FlexEnd,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct StyleState {
    pub(crate) hovered_path: Option<NodePath>,
    pub(crate) focused_path: Option<NodePath>,
    pub(crate) checked_paths: BTreeSet<NodePath>,
    pub(crate) unchecked_paths: BTreeSet<NodePath>,
    pub(crate) disabled_paths: BTreeSet<NodePath>,
    pub(crate) scroll_offsets: BTreeMap<NodePath, (f32, f32)>,
    pub(crate) control_values: BTreeMap<NodePath, String>,
    pub(crate) text_selections: BTreeMap<NodePath, (usize, usize, bool)>,
    pub(crate) viewport_width: f32,
    pub(crate) viewport_height: f32,
    pub(crate) color_scheme: ColorScheme,
    pub(crate) reduced_motion: bool,
    pub(crate) custom_properties: BTreeMap<NodePath, BTreeMap<String, String>>,
    pub(crate) root_path: Option<NodePath>,
}

impl StyleState {
    fn is_hovered(&self, path: &[usize]) -> bool {
        self.hovered_path
            .as_deref()
            .is_some_and(|hovered| hovered.starts_with(path))
    }

    fn is_focused(&self, path: &[usize]) -> bool {
        self.focused_path.as_deref() == Some(path)
    }

    fn is_checked(&self, element: &Element, path: &[usize]) -> bool {
        if self.checked_paths.contains(path) {
            return true;
        }
        if self.unchecked_paths.contains(path) {
            false
        } else {
            element.attr("checked").is_some()
        }
    }

    fn is_disabled(&self, element: &Element, path: &[usize]) -> bool {
        self.disabled_paths.contains(path) || element.attrs.contains_key("disabled")
    }
}

pub(crate) struct StyleContext<'a> {
    pub(crate) element: &'a Element,
    pub(crate) path: &'a [usize],
    pub(crate) ancestors: &'a [&'a Element],
    pub(crate) parent_font_size: f32,
    pub(crate) parent_color: Color,
    pub(crate) parent_weight: FontWeight,
    pub(crate) parent_text: InheritedText,
    pub(crate) state: &'a StyleState,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum GridTrack {
    Auto,
    Px(f32),
    Percent(f32),
    Fr(f32),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct GridTemplate {
    pub tracks: [GridTrack; 8],
    pub count: usize,
}

impl GridTemplate {
    pub(crate) const fn empty() -> Self {
        Self {
            tracks: [GridTrack::Auto; 8],
            count: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GradientStop {
    pub color: Color,
    pub position: f32,
}

// a bounded linear gradient: an axis angle in degrees plus up to four stops
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gradient {
    pub angle: f32,
    pub stops: [GradientStop; 4],
    pub count: usize,
}

impl Gradient {
    pub fn scale_alpha(self, factor: f32) -> Self {
        let mut scaled = self;
        for stop in scaled.stops.iter_mut() {
            stop.color = stop.color.scale_alpha(factor);
        }
        scaled
    }

    pub(crate) fn color_at(&self, progress: f32) -> Color {
        if self.count == 0 {
            return Color::transparent();
        }
        let stops = &self.stops[..self.count];
        let progress = progress.clamp(0.0, 1.0);
        if progress <= stops[0].position {
            return stops[0].color;
        }
        for pair in stops.windows(2) {
            let (start, end) = (pair[0], pair[1]);
            if progress <= end.position {
                let span = (end.position - start.position).max(f32::EPSILON);
                let ratio = ((progress - start.position) / span).clamp(0.0, 1.0);
                return start.color.mix(end.color, ratio);
            }
        }
        stops[stops.len() - 1].color
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BoxShadow {
    pub offset_x: Length,
    pub offset_y: Length,
    pub blur: Length,
    pub spread: Length,
    pub color: Color,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Style {
    pub display: Display,
    pub position: Position,
    pub z_index: Option<i32>,
    pub inset: Edges,
    pub overflow: Overflow,
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
    pub text_transform: TextTransform,
    pub letter_spacing: Length,
    pub font_family: crate::font::FontStack,
    pub font_weight: FontWeight,
    pub flex_direction: FlexDirection,
    pub justify_content: JustifyContent,
    pub align_items: AlignItems,
    pub gap: Length,
    pub flex_grow: f32,
    pub grid_template_columns: GridTemplate,
    pub grid_template_rows: GridTemplate,
    pub border_radius: Length,
    pub opacity: f32,
    pub gradient: Option<Gradient>,
    pub shadow: Option<BoxShadow>,
}

impl Style {
    fn default_for(
        name: &str,
        parent_font_size: f32,
        parent_color: Color,
        parent_weight: FontWeight,
        parent_text: InheritedText,
    ) -> Self {
        let mut style = Self {
            display: default_display(name),
            position: Position::Static,
            z_index: None,
            inset: Edges::auto(),
            overflow: Overflow::Visible,
            width: Length::Auto,
            height: Length::Auto,
            margin: Edges::zero(),
            padding: Edges::zero(),
            background: Color::transparent(),
            color: parent_color,
            font_size: Length::px(parent_font_size),
            line_height: Length::em(1.25),
            border_width: 0.0,
            border_color: Color::transparent(),
            text_align: TextAlign::Left,
            text_transform: parent_text.transform,
            letter_spacing: parent_text.letter_spacing,
            font_family: parent_text.family,
            font_weight: parent_weight,
            flex_direction: FlexDirection::Row,
            justify_content: JustifyContent::FlexStart,
            align_items: AlignItems::Stretch,
            gap: Length::px(0.0),
            flex_grow: 0.0,
            grid_template_columns: GridTemplate::empty(),
            grid_template_rows: GridTemplate::empty(),
            border_radius: Length::px(0.0),
            opacity: 1.0,
            gradient: None,
            shadow: None,
        };
        match name {
            "body" => {
                style.margin = Edges::all(Length::px(8.0));
                style.background = Color::WHITE;
            }
            "h1" => {
                style.margin = Edges::vertical(Length::em(0.67));
                style.font_size = Length::em(2.0);
                style.font_weight = FontWeight::Bold;
            }
            "h2" => {
                style.margin = Edges::vertical(Length::em(0.83));
                style.font_size = Length::em(1.5);
                style.font_weight = FontWeight::Bold;
            }
            "p" => style.margin = Edges::vertical(Length::em(1.0)),
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
            right: Length::px(0.0),
            bottom: value,
            left: Length::px(0.0),
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
        parse_rules(&source, None, &mut rules, None)?;
        Ok(Self { rules })
    }

    // page rendering follows CSS error handling: an unsupported rule is dropped, the rest applies
    pub fn parse_recovering(source: &str) -> (Self, Vec<String>) {
        let source = strip_comments(source);
        let mut rules = Vec::new();
        let mut dropped = Vec::new();
        let _ = parse_rules(&source, None, &mut rules, Some(&mut dropped));
        (Self { rules }, dropped)
    }

    pub(crate) fn populate_custom_properties(&self, document: &Document, state: &mut StyleState) {
        state.custom_properties.clear();
        state.root_path = document
            .root
            .children
            .iter()
            .enumerate()
            .find_map(|(index, node)| match node {
                Node::Element(element) if element.name == "html" => Some(vec![index]),
                _ => None,
            })
            .or_else(|| {
                document
                    .root
                    .children
                    .iter()
                    .enumerate()
                    .find_map(|(index, node)| match node {
                        Node::Element(element)
                            if !matches!(
                                element.name.as_str(),
                                "head" | "style" | "script" | "link" | "meta" | "title"
                            ) =>
                        {
                            Some(vec![index])
                        }
                        _ => None,
                    })
            });
        let inherited = BTreeMap::new();
        let ancestors = Vec::new();
        for (index, node) in document.root.children.iter().enumerate() {
            let Node::Element(element) = node else {
                continue;
            };
            self.populate_element_custom_properties(
                element,
                &[index],
                &ancestors,
                &inherited,
                state,
            );
        }
    }

    fn populate_element_custom_properties(
        &self,
        element: &Element,
        path: &[usize],
        ancestors: &[&Element],
        inherited: &BTreeMap<String, String>,
        state: &mut StyleState,
    ) {
        let mut properties = inherited.clone();
        for (property, (_, _, value)) in self.cascaded_values(element, path, ancestors, state) {
            if !property.starts_with("--") {
                continue;
            }
            match value.trim().to_ascii_lowercase().as_str() {
                "initial" => {
                    properties.remove(&property);
                }
                "inherit" | "unset" => {}
                _ => {
                    properties.insert(property, value);
                }
            }
        }
        state
            .custom_properties
            .insert(path.to_vec(), properties.clone());
        let mut child_ancestors = ancestors.to_vec();
        child_ancestors.push(element);
        for (index, node) in element.children.iter().enumerate() {
            let Node::Element(child) = node else {
                continue;
            };
            let mut child_path = path.to_vec();
            child_path.push(index);
            self.populate_element_custom_properties(
                child,
                &child_path,
                &child_ancestors,
                &properties,
                state,
            );
        }
    }

    pub(crate) fn style_for(&self, context: StyleContext<'_>) -> Style {
        let StyleContext {
            element,
            path,
            ancestors,
            parent_font_size,
            parent_color,
            parent_weight,
            parent_text,
            state,
        } = context;
        let mut style = Style::default_for(
            &element.name,
            parent_font_size,
            parent_color,
            parent_weight,
            parent_text,
        );
        let values = self.cascaded_values(element, path, ancestors, state);
        let empty_properties = BTreeMap::new();
        let custom_properties = state
            .custom_properties
            .get(path)
            .unwrap_or(&empty_properties);
        for (property, (_, _, value)) in values {
            if property.starts_with("--") {
                continue;
            }
            let Some(value) = resolve_custom_value(&value, custom_properties) else {
                continue;
            };
            apply_declaration(&mut style, &property, &value);
        }
        style
    }

    fn cascaded_values(
        &self,
        element: &Element,
        path: &[usize],
        ancestors: &[&Element],
        state: &StyleState,
    ) -> HashMap<String, (Specificity, usize, String)> {
        let mut values: HashMap<String, (Specificity, usize, String)> = HashMap::new();
        for rule in &self.rules {
            if rule
                .media
                .as_ref()
                .is_some_and(|media| !media.matches(state))
            {
                continue;
            }
            let Some(specificity) = rule
                .selectors
                .iter()
                .filter(|selector| selector.matches(element, path, ancestors, state))
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
        let mut inline_dropped = Vec::new();
        if let Some(inline) = element.attr("style")
            && let Ok(declarations) = parse_declarations(inline, Some(&mut inline_dropped))
        {
            for declaration in declarations {
                values.insert(
                    declaration.property.clone(),
                    (Specificity::INLINE, usize::MAX, declaration.value),
                );
            }
        }
        values
    }

    #[cfg(test)]
    pub(crate) fn style_for_inherited_with_ancestors(
        &self,
        element: &Element,
        ancestors: &[&Element],
        parent_font_size: f32,
        parent_color: Color,
        parent_weight: FontWeight,
    ) -> Style {
        self.style_for(StyleContext {
            element,
            path: &[],
            ancestors,
            parent_font_size,
            parent_color,
            parent_weight,
            parent_text: InheritedText::default(),
            state: &StyleState::default(),
        })
    }
}

#[derive(Clone, Debug)]
struct Rule {
    selectors: Vec<Selector>,
    declarations: Vec<Declaration>,
    order: usize,
    media: Option<MediaCondition>,
}

#[derive(Clone, Debug, Default)]
struct MediaCondition {
    min_width: Option<f32>,
    max_width: Option<f32>,
    min_height: Option<f32>,
    max_height: Option<f32>,
    color_scheme: Option<ColorScheme>,
    reduced_motion: Option<bool>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorScheme {
    #[default]
    Light,
    Dark,
}

impl MediaCondition {
    fn matches(&self, state: &StyleState) -> bool {
        self.min_width
            .is_none_or(|value| state.viewport_width >= value)
            && self
                .max_width
                .is_none_or(|value| state.viewport_width <= value)
            && self
                .min_height
                .is_none_or(|value| state.viewport_height >= value)
            && self
                .max_height
                .is_none_or(|value| state.viewport_height <= value)
            && self
                .color_scheme
                .is_none_or(|scheme| scheme == state.color_scheme)
            && self
                .reduced_motion
                .is_none_or(|reduced| reduced == state.reduced_motion)
    }
}

#[derive(Clone, Debug)]
struct Declaration {
    property: String,
    value: String,
}

#[derive(Clone, Debug)]
pub(crate) struct Selector {
    parts: Vec<SelectorPart>,
}

#[derive(Clone, Debug)]
struct SelectorPart {
    combinator: Option<Combinator>,
    compound: CompoundSelector,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Combinator {
    Descendant,
    Child,
    NextSibling,
    SubsequentSibling,
}

#[derive(Clone, Debug)]
struct CompoundSelector {
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
    attributes: Vec<AttributeSelector>,
    pseudo_classes: Vec<PseudoClass>,
}

#[derive(Clone, Debug)]
enum AttributeSelector {
    Exists(String),
    Equals(String, String),
}

#[derive(Clone, Debug)]
enum PseudoClass {
    Root,
    Hover,
    Focus,
    Checked,
    Disabled,
    Enabled,
    FirstChild,
    LastChild,
    OnlyChild,
    NthChild(NthPattern),
    Not(Vec<Selector>),
}

// an+b as defined by Selectors Level 4, with odd and even folded into it
#[derive(Clone, Copy, Debug)]
struct NthPattern {
    step: i32,
    offset: i32,
}

impl NthPattern {
    fn parse(argument: &str) -> Result<Self, String> {
        let argument = argument.trim().to_ascii_lowercase();
        match argument.as_str() {
            "odd" => return Ok(Self { step: 2, offset: 1 }),
            "even" => return Ok(Self { step: 2, offset: 0 }),
            _ => {}
        }
        if let Ok(offset) = argument.parse::<i32>() {
            return Ok(Self { step: 0, offset });
        }
        let Some((step_text, offset_text)) = argument.split_once('n') else {
            return Err(format!("unsupported nth-child argument: {argument}"));
        };
        let step = match step_text.trim() {
            "" | "+" => 1,
            "-" => -1,
            value => value
                .parse::<i32>()
                .map_err(|_| format!("unsupported nth-child step: {value}"))?,
        };
        let offset = match offset_text.replace(' ', "").as_str() {
            "" => 0,
            value => value
                .parse::<i32>()
                .map_err(|_| format!("unsupported nth-child offset: {value}"))?,
        };
        Ok(Self { step, offset })
    }

    fn matches(&self, position: i32) -> bool {
        if self.step == 0 {
            return position == self.offset;
        }
        let distance = position - self.offset;
        distance % self.step == 0 && distance / self.step >= 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Specificity(u32, u32, u32);

impl Specificity {
    const INLINE: Self = Self(u32::MAX, u32::MAX, u32::MAX);
}

impl Selector {
    pub(crate) fn parse(source: &str) -> Result<Self, String> {
        let source = source.trim();
        if source.is_empty() {
            return Err("empty selector".to_string());
        }
        let mut cursor = 0;
        let mut parts = Vec::new();
        let mut combinator = None;
        while cursor < source.len() {
            let had_whitespace = skip_selector_whitespace(source, &mut cursor);
            if cursor >= source.len() {
                break;
            }
            let explicit = match source.as_bytes()[cursor] {
                b'>' => Some(Combinator::Child),
                b'+' => Some(Combinator::NextSibling),
                b'~' => Some(Combinator::SubsequentSibling),
                _ => None,
            };
            if let Some(explicit) = explicit {
                if parts.is_empty() || combinator.is_some() {
                    return Err(format!("invalid combinator in selector: {source}"));
                }
                combinator = Some(explicit);
                cursor += 1;
                continue;
            }
            if !parts.is_empty() && combinator.is_none() && had_whitespace {
                combinator = Some(Combinator::Descendant);
            }
            let compound = parse_compound_selector(source, &mut cursor)?;
            parts.push(SelectorPart {
                combinator: if parts.is_empty() {
                    None
                } else {
                    Some(combinator.take().unwrap_or(Combinator::Descendant))
                },
                compound,
            });
        }
        if parts.is_empty() || combinator.is_some() {
            return Err(format!("selector has no final compound: {source}"));
        }
        Ok(Self { parts })
    }

    pub(crate) fn parse_list(source: &str) -> Result<Vec<Self>, String> {
        split_selector_list(source)?
            .into_iter()
            .map(Self::parse)
            .collect()
    }

    pub(crate) fn matches(
        &self,
        element: &Element,
        path: &[usize],
        ancestors: &[&Element],
        state: &StyleState,
    ) -> bool {
        let Some(last) = self.parts.last() else {
            return false;
        };
        self.matches_part(
            self.parts.len() - 1,
            &last.compound,
            element,
            path,
            ancestors,
            state,
        )
    }

    fn matches_part(
        &self,
        index: usize,
        compound: &CompoundSelector,
        element: &Element,
        path: &[usize],
        ancestors: &[&Element],
        state: &StyleState,
    ) -> bool {
        if !compound.matches(element, path, ancestors, state) {
            return false;
        }
        if index == 0 {
            return true;
        }
        match self.parts[index].combinator {
            Some(Combinator::Child) => ancestors.last().is_some_and(|parent| {
                self.matches_part(
                    index - 1,
                    &self.parts[index - 1].compound,
                    parent,
                    &path[..path.len().saturating_sub(1)],
                    &ancestors[..ancestors.len() - 1],
                    state,
                )
            }),
            Some(Combinator::Descendant) => {
                ancestors
                    .iter()
                    .enumerate()
                    .rev()
                    .any(|(parent_index, parent)| {
                        self.matches_part(
                            index - 1,
                            &self.parts[index - 1].compound,
                            parent,
                            &path[..(parent_index + 1).min(path.len())],
                            &ancestors[..parent_index],
                            state,
                        )
                    })
            }
            Some(Combinator::NextSibling) | Some(Combinator::SubsequentSibling) => {
                let immediate = self.parts[index].combinator == Some(Combinator::NextSibling);
                let Some(parent) = ancestors.last() else {
                    return false;
                };
                let Some((&position, parent_path)) = path.split_last() else {
                    return false;
                };
                let mut siblings = element_children(parent)
                    .into_iter()
                    .take_while(|(child_index, _)| *child_index < position)
                    .collect::<Vec<_>>();
                siblings.reverse();
                if immediate {
                    siblings.truncate(1);
                }
                siblings.into_iter().any(|(child_index, sibling)| {
                    let mut sibling_path = parent_path.to_vec();
                    sibling_path.push(child_index);
                    self.matches_part(
                        index - 1,
                        &self.parts[index - 1].compound,
                        sibling,
                        &sibling_path,
                        ancestors,
                        state,
                    )
                })
            }
            None => false,
        }
    }

    fn specificity(&self) -> Specificity {
        self.parts
            .iter()
            .fold(Specificity(0, 0, 0), |specificity, part| {
                let part_specificity = part.compound.specificity();
                Specificity(
                    specificity.0 + part_specificity.0,
                    specificity.1 + part_specificity.1,
                    specificity.2 + part_specificity.2,
                )
            })
    }
}

impl CompoundSelector {
    fn matches(
        &self,
        element: &Element,
        path: &[usize],
        ancestors: &[&Element],
        state: &StyleState,
    ) -> bool {
        self.tag.as_deref().is_none_or(|tag| tag == element.name)
            && self
                .id
                .as_deref()
                .is_none_or(|id| element.attr("id") == Some(id))
            && self.classes.iter().all(|class| element.has_class(class))
            && self.attributes.iter().all(|attribute| match attribute {
                AttributeSelector::Exists(name) => element.attr(name).is_some(),
                AttributeSelector::Equals(name, value) => element.attr(name) == Some(value),
            })
            && self
                .pseudo_classes
                .iter()
                .all(|pseudo| pseudo.matches(element, path, ancestors, state))
    }

    fn specificity(&self) -> Specificity {
        let pseudo_specificity =
            self.pseudo_classes
                .iter()
                .fold(Specificity(0, 0, 0), |specificity, pseudo| {
                    let pseudo_specificity = pseudo.specificity();
                    Specificity(
                        specificity.0 + pseudo_specificity.0,
                        specificity.1 + pseudo_specificity.1,
                        specificity.2 + pseudo_specificity.2,
                    )
                });
        Specificity(
            u32::from(self.id.is_some()) + pseudo_specificity.0,
            (self.classes.len() + self.attributes.len()) as u32 + pseudo_specificity.1,
            u32::from(self.tag.is_some()) + pseudo_specificity.2,
        )
    }
}

impl PseudoClass {
    fn matches(
        &self,
        element: &Element,
        path: &[usize],
        ancestors: &[&Element],
        state: &StyleState,
    ) -> bool {
        match self {
            Self::Root => state.root_path.as_deref() == Some(path),
            Self::Hover => state.is_hovered(path),
            Self::Focus => state.is_focused(path),
            Self::Checked => {
                element.name == "input"
                    && element.attr("type").is_some_and(|kind| {
                        kind.eq_ignore_ascii_case("checkbox") || kind.eq_ignore_ascii_case("radio")
                    })
                    && state.is_checked(element, path)
            }
            Self::Disabled => state.is_disabled(element, path),
            Self::Enabled => !state.is_disabled(element, path),
            Self::FirstChild => child_position(path, ancestors) == Some(1),
            Self::LastChild => child_position(path, ancestors)
                .is_some_and(|position| position == element_child_count(ancestors).unwrap_or(0)),
            Self::OnlyChild => element_child_count(ancestors) == Some(1),
            Self::NthChild(pattern) => {
                child_position(path, ancestors).is_some_and(|position| pattern.matches(position))
            }
            Self::Not(selectors) => !selectors
                .iter()
                .any(|selector| selector.matches(element, path, ancestors, state)),
        }
    }

    fn specificity(&self) -> Specificity {
        match self {
            Self::Not(selectors) => selectors
                .iter()
                .map(Selector::specificity)
                .max()
                .unwrap_or(Specificity(0, 0, 0)),
            _ => Specificity(0, 1, 0),
        }
    }
}

fn element_children(parent: &Element) -> Vec<(usize, &Element)> {
    parent
        .children
        .iter()
        .enumerate()
        .filter_map(|(index, node)| match node {
            Node::Element(child) => Some((index, child)),
            Node::Text(_) => None,
        })
        .collect()
}

fn element_child_count(ancestors: &[&Element]) -> Option<i32> {
    ancestors
        .last()
        .map(|parent| element_children(parent).len() as i32)
}

fn child_position(path: &[usize], ancestors: &[&Element]) -> Option<i32> {
    let parent = ancestors.last()?;
    let &index = path.last()?;
    element_children(parent)
        .into_iter()
        .position(|(child_index, _)| child_index == index)
        .map(|position| position as i32 + 1)
}

fn skip_selector_whitespace(source: &str, cursor: &mut usize) -> bool {
    let start = *cursor;
    while *cursor < source.len() && source.as_bytes()[*cursor].is_ascii_whitespace() {
        *cursor += 1;
    }
    *cursor > start
}

fn parse_compound_selector(source: &str, cursor: &mut usize) -> Result<CompoundSelector, String> {
    let mut selector = CompoundSelector {
        tag: None,
        id: None,
        classes: Vec::new(),
        attributes: Vec::new(),
        pseudo_classes: Vec::new(),
    };
    let start = *cursor;
    while *cursor < source.len() {
        match source.as_bytes()[*cursor] {
            b'#' => {
                *cursor += 1;
                selector.id = Some(parse_selector_name(source, cursor)?);
            }
            b'.' => {
                *cursor += 1;
                selector.classes.push(parse_selector_name(source, cursor)?);
            }
            b':' => {
                *cursor += 1;
                let name = parse_selector_name(source, cursor)?;
                let pseudo = if name == "not" {
                    let argument = parse_function_argument(source, cursor)?;
                    PseudoClass::Not(Selector::parse_list(&argument)?)
                } else if name == "nth-child" {
                    let argument = parse_function_argument(source, cursor)?;
                    PseudoClass::NthChild(NthPattern::parse(&argument)?)
                } else {
                    match name.as_str() {
                        "root" => PseudoClass::Root,
                        "hover" => PseudoClass::Hover,
                        "focus" => PseudoClass::Focus,
                        "checked" => PseudoClass::Checked,
                        "disabled" => PseudoClass::Disabled,
                        "enabled" => PseudoClass::Enabled,
                        "focus-visible" | "focus-within" => PseudoClass::Focus,
                        "first-child" => PseudoClass::FirstChild,
                        "last-child" => PseudoClass::LastChild,
                        "only-child" => PseudoClass::OnlyChild,
                        _ => return Err(format!("unsupported pseudo-class: :{name}")),
                    }
                };
                selector.pseudo_classes.push(pseudo);
            }
            b'[' => {
                *cursor += 1;
                skip_selector_whitespace(source, cursor);
                let name = parse_selector_name(source, cursor)?;
                skip_selector_whitespace(source, cursor);
                let attribute = if *cursor < source.len() && source.as_bytes()[*cursor] == b']' {
                    *cursor += 1;
                    AttributeSelector::Exists(name)
                } else {
                    if *cursor >= source.len() || source.as_bytes()[*cursor] != b'=' {
                        return Err(format!("unsupported attribute selector: {source}"));
                    }
                    *cursor += 1;
                    skip_selector_whitespace(source, cursor);
                    let value = parse_attribute_value(source, cursor)?;
                    skip_selector_whitespace(source, cursor);
                    if *cursor >= source.len() || source.as_bytes()[*cursor] != b']' {
                        return Err(format!("unterminated attribute selector: {source}"));
                    }
                    *cursor += 1;
                    AttributeSelector::Equals(name, value)
                };
                selector.attributes.push(attribute);
            }
            b'*' => *cursor += 1,
            byte if byte.is_ascii_whitespace() || byte == b'>' => break,
            _ => {
                if selector.tag.is_some() {
                    return Err(format!("invalid compound selector: {source}"));
                }
                selector.tag = Some(parse_selector_name(source, cursor)?);
            }
        }
    }
    if *cursor == start {
        return Err(format!(
            "selector has no compound at byte {start}: {source}"
        ));
    }
    Ok(selector)
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

fn parse_attribute_value(source: &str, cursor: &mut usize) -> Result<String, String> {
    if *cursor >= source.len() {
        return Err(format!("attribute selector has no value: {source}"));
    }
    let quote = source.as_bytes()[*cursor];
    if quote == b'\'' || quote == b'"' {
        *cursor += 1;
        let start = *cursor;
        while *cursor < source.len() && source.as_bytes()[*cursor] != quote {
            *cursor += 1;
        }
        if *cursor >= source.len() {
            return Err(format!("unterminated attribute value: {source}"));
        }
        let value = source[start..*cursor].to_string();
        *cursor += 1;
        Ok(value)
    } else {
        let start = *cursor;
        while *cursor < source.len()
            && !source.as_bytes()[*cursor].is_ascii_whitespace()
            && source.as_bytes()[*cursor] != b']'
        {
            *cursor += 1;
        }
        (start != *cursor)
            .then(|| source[start..*cursor].to_string())
            .ok_or_else(|| format!("attribute selector has no value: {source}"))
    }
}

fn parse_rules(
    source: &str,
    media: Option<MediaCondition>,
    rules: &mut Vec<Rule>,
    mut dropped: Option<&mut Vec<String>>,
) -> Result<(), String> {
    let mut cursor = 0;
    while cursor < source.len() {
        let remaining = &source[cursor..];
        let Some(relative_open) = remaining.find('{') else {
            if remaining.trim().is_empty() {
                break;
            }
            return drop_or_fail(
                dropped.as_deref_mut(),
                "stylesheet has text outside a rule".to_string(),
            );
        };
        let open = cursor + relative_open;
        let header = source[cursor..open].trim();
        let close = match matching_brace(source, open) {
            Ok(close) => close,
            Err(error) => {
                drop_or_fail(dropped.as_deref_mut(), error)?;
                break;
            }
        };
        let body = &source[open + 1..close];
        cursor = close + 1;
        if header.is_empty() {
            drop_or_fail(
                dropped.as_deref_mut(),
                "stylesheet rule has no selector".to_string(),
            )?;
            continue;
        }
        if header
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("@media"))
        {
            if media.is_some() {
                drop_or_fail(
                    dropped.as_deref_mut(),
                    "nested media rules are not supported".to_string(),
                )?;
                continue;
            }
            let nested_media = match parse_media_condition(header) {
                Ok(condition) => condition,
                Err(error) => {
                    drop_or_fail(dropped.as_deref_mut(), error)?;
                    continue;
                }
            };
            parse_rules(body, Some(nested_media), rules, dropped.as_deref_mut())?;
            continue;
        }
        if header.starts_with('@') {
            drop_or_fail(
                dropped.as_deref_mut(),
                format!("unsupported at-rule: {header}"),
            )?;
            continue;
        }
        let selectors = match Selector::parse_list(header) {
            Ok(selectors) => selectors,
            Err(error) => {
                drop_or_fail(dropped.as_deref_mut(), format!("{header}: {error}"))?;
                continue;
            }
        };
        let declarations = match parse_declarations(body, dropped.as_deref_mut()) {
            Ok(declarations) => declarations,
            Err(error) => {
                drop_or_fail(dropped.as_deref_mut(), format!("{header}: {error}"))?;
                continue;
            }
        };
        rules.push(Rule {
            selectors,
            declarations,
            order: rules.len(),
            media: media.clone(),
        });
    }
    Ok(())
}

fn drop_or_fail(dropped: Option<&mut Vec<String>>, error: String) -> Result<(), String> {
    match dropped {
        Some(dropped) => {
            dropped.push(error);
            Ok(())
        }
        None => Err(error),
    }
}

fn matching_brace(source: &str, open: usize) -> Result<usize, String> {
    let bytes = source.as_bytes();
    let mut depth = 0;
    for (offset, byte) in bytes.iter().enumerate().skip(open) {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(offset);
                }
            }
            _ => {}
        }
    }
    Err("stylesheet rule has no closing brace".to_string())
}

fn parse_media_condition(header: &str) -> Result<MediaCondition, String> {
    let query = header
        .get(6..)
        .ok_or_else(|| format!("invalid media rule: {header}"))?
        .trim()
        .to_ascii_lowercase();
    let mut condition = MediaCondition::default();
    for raw_feature in query.split("and") {
        let feature = raw_feature.trim().trim_matches(['(', ')']).trim();
        if feature.is_empty()
            || feature.eq_ignore_ascii_case("all")
            || feature.eq_ignore_ascii_case("screen")
        {
            continue;
        }
        let Some((name, value)) = feature.split_once(':') else {
            return Err(format!("unsupported media feature: {feature}"));
        };
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        match name.as_str() {
            "prefers-color-scheme" => {
                condition.color_scheme = Some(match value {
                    "dark" => ColorScheme::Dark,
                    "light" => ColorScheme::Light,
                    other => return Err(format!("unsupported color scheme: {other}")),
                });
                continue;
            }
            "prefers-reduced-motion" => {
                condition.reduced_motion = Some(match value {
                    "reduce" => true,
                    "no-preference" => false,
                    other => return Err(format!("unsupported motion preference: {other}")),
                });
                continue;
            }
            _ => {}
        }
        let value = parse_media_dimension(value)?;
        match name.as_str() {
            "min-width" => condition.min_width = Some(value),
            "max-width" => condition.max_width = Some(value),
            "min-height" => condition.min_height = Some(value),
            "max-height" => condition.max_height = Some(value),
            _ => return Err(format!("unsupported media feature: {name}")),
        }
    }
    Ok(condition)
}

fn parse_media_dimension(value: &str) -> Result<f32, String> {
    let value = value.trim();
    let value = value.strip_suffix("px").unwrap_or(value).trim();
    let value = value
        .parse::<f32>()
        .map_err(|_| format!("invalid media dimension: {value}"))?;
    value
        .is_finite()
        .then_some(value.max(0.0))
        .ok_or_else(|| "media dimension must be finite".to_string())
}

fn parse_declarations(
    source: &str,
    mut dropped: Option<&mut Vec<String>>,
) -> Result<Vec<Declaration>, String> {
    let mut declarations = Vec::new();
    for chunk in source.split(';') {
        let chunk = chunk.trim();
        if chunk.is_empty() {
            continue;
        }
        let Some(colon) = chunk.find(':') else {
            drop_or_fail(
                dropped.as_deref_mut(),
                format!("declaration has no colon: {chunk}"),
            )?;
            continue;
        };
        let property_name = chunk[..colon].trim();
        let property = if property_name.starts_with("--") {
            property_name.to_string()
        } else {
            property_name.to_ascii_lowercase()
        };
        let value = chunk[colon + 1..].trim().to_string();
        if property.is_empty() || value.is_empty() {
            drop_or_fail(
                dropped.as_deref_mut(),
                format!("invalid declaration: {chunk}"),
            )?;
            continue;
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
                "flex" => Display::Flex,
                "grid" => Display::Grid,
                "inline" => Display::Inline,
                _ => Display::Block,
            };
        }
        "position" => {
            style.position = match value.trim().to_ascii_lowercase().as_str() {
                "relative" => Position::Relative,
                "sticky" => Position::Sticky,
                "absolute" => Position::Absolute,
                "fixed" => Position::Fixed,
                _ => Position::Static,
            };
        }
        "z-index" => style.z_index = parse_z_index(value),
        "inset" => style.inset = parse_edges(value),
        "top" => style.inset.top = parse_length(value).unwrap_or(style.inset.top),
        "right" => style.inset.right = parse_length(value).unwrap_or(style.inset.right),
        "bottom" => style.inset.bottom = parse_length(value).unwrap_or(style.inset.bottom),
        "left" => style.inset.left = parse_length(value).unwrap_or(style.inset.left),
        "overflow" | "overflow-x" | "overflow-y" => {
            style.overflow = match value.trim().to_ascii_lowercase().as_str() {
                "hidden" | "clip" => Overflow::Hidden,
                "auto" | "scroll" | "overlay" => Overflow::Auto,
                _ => Overflow::Visible,
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
        "background" | "background-color" | "background-image" => {
            if let Some(gradient) = parse_linear_gradient(value) {
                style.gradient = Some(gradient);
                return;
            }
            if value.trim().eq_ignore_ascii_case("none") {
                style.gradient = None;
                return;
            }
            if let Some(color) = Color::parse(value) {
                style.gradient = None;
                style.background = color;
            }
        }
        "border-radius" => {
            // the shorthand may carry four corners, and this slice paints one radius
            if let Some(radius) = value.split_ascii_whitespace().next().and_then(parse_length) {
                style.border_radius = radius;
            }
        }
        "opacity" => {
            if let Some(opacity) = parse_number(value) {
                style.opacity = opacity.clamp(0.0, 1.0);
            }
        }
        "box-shadow" => {
            style.shadow = parse_box_shadow(value);
        }
        "color" => style.color = Color::parse(value).unwrap_or(style.color),
        "font-size" => style.font_size = parse_length(value).unwrap_or(style.font_size),
        "line-height" => style.line_height = parse_line_height(value).unwrap_or(style.line_height),
        "font-weight" => {
            if value.trim().eq_ignore_ascii_case("bold") || value.trim() == "700" {
                style.font_weight = FontWeight::Bold;
            }
        }
        "flex-direction" => {
            style.flex_direction = if value.trim().eq_ignore_ascii_case("column") {
                FlexDirection::Column
            } else {
                FlexDirection::Row
            };
        }
        "justify-content" => {
            style.justify_content = match value.trim().to_ascii_lowercase().as_str() {
                "center" => JustifyContent::Center,
                "flex-end" | "end" => JustifyContent::FlexEnd,
                "space-between" => JustifyContent::SpaceBetween,
                _ => JustifyContent::FlexStart,
            };
        }
        "align-items" => {
            style.align_items = match value.trim().to_ascii_lowercase().as_str() {
                "flex-start" | "start" => AlignItems::FlexStart,
                "center" => AlignItems::Center,
                "flex-end" | "end" => AlignItems::FlexEnd,
                _ => AlignItems::Stretch,
            };
        }
        "gap" | "row-gap" | "column-gap" => style.gap = parse_length(value).unwrap_or(style.gap),
        "grid-template-columns" => style.grid_template_columns = parse_grid_template(value),
        "grid-template-rows" => style.grid_template_rows = parse_grid_template(value),
        "flex-grow" => style.flex_grow = parse_number(value).unwrap_or(style.flex_grow).max(0.0),
        "flex" => {
            if let Some(grow) = value.split_ascii_whitespace().find_map(parse_number) {
                style.flex_grow = grow.max(0.0);
            }
        }
        "text-align" => {
            style.text_align = match value.trim().to_ascii_lowercase().as_str() {
                "center" => TextAlign::Center,
                "right" => TextAlign::Right,
                _ => TextAlign::Left,
            };
        }
        "font-family" => {
            style.font_family = crate::font::intern_font_stack(value);
        }
        "text-transform" => {
            style.text_transform = match value.trim().to_ascii_lowercase().as_str() {
                "uppercase" => TextTransform::Uppercase,
                "lowercase" => TextTransform::Lowercase,
                "capitalize" => TextTransform::Capitalize,
                _ => TextTransform::None,
            };
        }
        "letter-spacing" => {
            if value.trim().eq_ignore_ascii_case("normal") {
                style.letter_spacing = Length::px(0.0);
            } else if let Some(length) = parse_length(value) {
                style.letter_spacing = length;
            }
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
    if let Some(expression) = value
        .strip_prefix("calc(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        return parse_length_sum(expression).map(Length::Terms);
    }
    if let Some(arguments) = value
        .strip_prefix("clamp(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let terms = parse_length_arguments(arguments, 3)?;
        return Some(Length::Clamp(terms[0], terms[1], terms[2]));
    }
    if let Some(arguments) = value
        .strip_prefix("min(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let terms = parse_length_arguments(arguments, 2)?;
        return Some(Length::Smallest(terms[0], terms[1]));
    }
    if let Some(arguments) = value
        .strip_prefix("max(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        let terms = parse_length_arguments(arguments, 2)?;
        return Some(Length::Largest(terms[0], terms[1]));
    }
    parse_length_unit(&value).map(Length::Terms)
}

fn parse_length_arguments(arguments: &str, expected: usize) -> Option<Vec<LengthTerms>> {
    let terms: Vec<LengthTerms> = arguments
        .split(',')
        .map(|argument| {
            let argument = argument.trim();
            match argument
                .strip_prefix("calc(")
                .and_then(|rest| rest.strip_suffix(')'))
            {
                Some(expression) => parse_length_sum(expression),
                None => parse_length_unit(argument),
            }
        })
        .collect::<Option<Vec<_>>>()?;
    (terms.len() == expected).then_some(terms)
}

fn parse_length_unit(value: &str) -> Option<LengthTerms> {
    let value = value.trim();
    let mut terms = LengthTerms::default();
    let (number, unit) = split_length_unit(value)?;
    match unit {
        "px" | "" => terms.px = number,
        "em" => terms.em = number,
        "rem" => terms.rem = number,
        "%" => terms.percent = number,
        "vw" => terms.viewport_width = number,
        "vh" => terms.viewport_height = number,
        "vmin" | "vmax" => {
            let environment = layout_environment();
            let smaller_is_width = environment.viewport.width <= environment.viewport.height;
            if (unit == "vmin") == smaller_is_width {
                terms.viewport_width = number;
            } else {
                terms.viewport_height = number;
            }
        }
        "ch" => terms.zero_width = number,
        _ => return None,
    }
    terms.is_finite().then_some(terms)
}

fn split_length_unit(value: &str) -> Option<(f32, &str)> {
    if value == "0" {
        return Some((0.0, "px"));
    }
    let split = value
        .find(|character: char| !matches!(character, '0'..='9' | '.' | '-' | '+'))
        .unwrap_or(value.len());
    let (number, unit) = value.split_at(split);
    let number = number.trim();
    if number.is_empty() {
        return None;
    }
    number.parse::<f32>().ok().map(|number| (number, unit))
}

fn parse_function_argument(source: &str, cursor: &mut usize) -> Result<String, String> {
    if *cursor >= source.len() || source.as_bytes()[*cursor] != b'(' {
        return Err(format!("functional pseudo-class has no argument: {source}"));
    }
    let open = *cursor;
    let mut depth = 0usize;
    let mut quote = None;
    let mut index = open;
    while index < source.len() {
        let byte = source.as_bytes()[index];
        if let Some(expected) = quote {
            if byte == expected {
                quote = None;
            } else if byte == b'\\' {
                index += 1;
            }
        } else if byte == b'\'' || byte == b'"' {
            quote = Some(byte);
        } else if byte == b'(' {
            depth += 1;
        } else if byte == b')' {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                *cursor = index + 1;
                return Ok(source[open + 1..index].to_string());
            }
        }
        index += 1;
    }
    Err(format!("unterminated functional pseudo-class: {source}"))
}

fn split_selector_list(source: &str) -> Result<Vec<&str>, String> {
    let mut selectors = Vec::new();
    let mut start = 0;
    let mut bracket_depth = 0usize;
    let mut parenthesis_depth = 0usize;
    let mut quote = None;
    let bytes = source.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if let Some(expected) = quote {
            if byte == expected {
                quote = None;
            } else if byte == b'\\' {
                index += 1;
            }
        } else if byte == b'\'' || byte == b'"' {
            quote = Some(byte);
        } else {
            match byte {
                b'[' => bracket_depth += 1,
                b']' => bracket_depth = bracket_depth.saturating_sub(1),
                b'(' => parenthesis_depth += 1,
                b')' => parenthesis_depth = parenthesis_depth.saturating_sub(1),
                b',' if bracket_depth == 0 && parenthesis_depth == 0 => {
                    let selector = source[start..index].trim();
                    if selector.is_empty() {
                        return Err(format!("selector list has an empty entry: {source}"));
                    }
                    selectors.push(selector);
                    start = index + 1;
                }
                _ => {}
            }
        }
        index += 1;
    }
    if quote.is_some() || bracket_depth != 0 || parenthesis_depth != 0 {
        return Err(format!("unterminated selector list: {source}"));
    }
    let selector = source[start..].trim();
    if selector.is_empty() {
        return Err(format!("selector list has an empty entry: {source}"));
    }
    selectors.push(selector);
    Ok(selectors)
}

fn parse_length_sum(expression: &str) -> Option<LengthTerms> {
    let expression = expression.trim();
    if expression.is_empty() {
        return None;
    }
    let bytes = expression.as_bytes();
    let mut cursor = 0;
    let mut total = LengthTerms::default();
    let mut saw_term = false;
    let mut sign = 1.0;
    while cursor < bytes.len() {
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= bytes.len() {
            break;
        }
        if matches!(bytes[cursor], b'+' | b'-') {
            sign = if bytes[cursor] == b'-' { -1.0 } else { 1.0 };
            cursor += 1;
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
        }
        let start = cursor;
        while cursor < bytes.len() && !matches!(bytes[cursor], b'+' | b'-') {
            cursor += 1;
        }
        let token = expression[start..cursor].trim();
        if token.is_empty() {
            return None;
        }
        let terms = parse_length_unit(token)?;
        total.px += sign * terms.px;
        total.em += sign * terms.em;
        total.rem += sign * terms.rem;
        total.percent += sign * terms.percent;
        total.viewport_width += sign * terms.viewport_width;
        total.viewport_height += sign * terms.viewport_height;
        total.zero_width += sign * terms.zero_width;
        if !total.is_finite() {
            return None;
        }
        saw_term = true;
        sign = 1.0;
    }
    saw_term.then_some(total)
}

fn parse_linear_gradient(value: &str) -> Option<Gradient> {
    let arguments = value
        .trim()
        .strip_prefix("linear-gradient(")
        .or_else(|| value.trim().strip_prefix("linear-gradient ("))?
        .strip_suffix(')')?;
    let parts = split_top_level(arguments);
    let mut parts = parts.iter().map(|part| part.trim()).peekable();
    let first = *parts.peek()?;
    let angle = if let Some(degrees) = first.strip_suffix("deg") {
        let angle = degrees.trim().parse::<f32>().ok()?;
        parts.next();
        angle
    } else if let Some(side) = first.strip_prefix("to ") {
        let angle = match side.trim().to_ascii_lowercase().as_str() {
            "top" => 0.0,
            "right" => 90.0,
            "bottom" => 180.0,
            "left" => 270.0,
            "bottom right" | "right bottom" => 135.0,
            "bottom left" | "left bottom" => 225.0,
            "top right" | "right top" => 45.0,
            "top left" | "left top" => 315.0,
            _ => return None,
        };
        parts.next();
        angle
    } else {
        180.0
    };

    let mut stops = [GradientStop {
        color: Color::transparent(),
        position: 0.0,
    }; 4];
    let mut count = 0;
    let entries: Vec<&str> = parts.collect();
    if entries.len() < 2 || entries.len() > stops.len() {
        return None;
    }
    for (index, entry) in entries.iter().enumerate() {
        let mut pieces = entry.rsplitn(2, ' ');
        let last = pieces.next()?.trim();
        let head = pieces.next().map(str::trim);
        let (color, position) = match (head, last.strip_suffix('%')) {
            (Some(head), Some(percent)) => (
                Color::parse(head)?,
                percent.trim().parse::<f32>().ok()? / 100.0,
            ),
            _ => (
                Color::parse(entry)?,
                index as f32 / (entries.len() - 1) as f32,
            ),
        };
        stops[count] = GradientStop {
            color,
            position: position.clamp(0.0, 1.0),
        };
        count += 1;
    }
    Some(Gradient {
        angle,
        stops,
        count,
    })
}

fn parse_box_shadow(value: &str) -> Option<BoxShadow> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("none") {
        return None;
    }
    // inset shadows and shadow lists stay outside this slice
    if value.to_ascii_lowercase().contains("inset") || value.contains(',') {
        return None;
    }
    let mut lengths = Vec::new();
    let mut color = Color::opaque(0, 0, 0);
    for token in value.split_ascii_whitespace() {
        if let Some(length) = parse_length(token) {
            lengths.push(length);
            continue;
        }
        color = Color::parse(token)?;
    }
    if lengths.len() < 2 || lengths.len() > 4 {
        return None;
    }
    Some(BoxShadow {
        offset_x: lengths[0],
        offset_y: lengths[1],
        blur: lengths.get(2).copied().unwrap_or(Length::px(0.0)),
        spread: lengths.get(3).copied().unwrap_or(Length::px(0.0)),
        color,
    })
}

// splits on commas that are not inside a function call
fn split_top_level(source: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0_usize;
    let mut start = 0;
    for (index, byte) in source.bytes().enumerate() {
        match byte {
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                parts.push(&source[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&source[start..]);
    parts
}

fn parse_line_height(value: &str) -> Option<Length> {
    let value = value.trim();
    if let Ok(multiplier) = value.parse::<f32>() {
        return Some(Length::em(multiplier));
    }
    parse_length(value)
}

fn parse_number(value: &str) -> Option<f32> {
    value.trim().parse().ok()
}

fn resolve_custom_value(value: &str, properties: &BTreeMap<String, String>) -> Option<String> {
    resolve_custom_value_inner(value, properties, &mut Vec::new())
}

fn resolve_custom_value_inner(
    value: &str,
    properties: &BTreeMap<String, String>,
    stack: &mut Vec<String>,
) -> Option<String> {
    let mut output = String::with_capacity(value.len());
    let mut cursor = 0;
    while let Some(relative_start) = find_var_function(&value[cursor..]) {
        let start = cursor + relative_start;
        output.push_str(&value[cursor..start]);
        let open = start + 3;
        let close = matching_parenthesis(value, open)?;
        let (name, fallback) = parse_var_arguments(&value[open + 1..close])?;
        let replacement = if stack.iter().any(|current| current == name) {
            fallback.and_then(|fallback| resolve_custom_value_inner(fallback, properties, stack))
        } else if let Some(raw) = properties.get(name) {
            stack.push(name.to_string());
            let resolved = resolve_custom_value_inner(raw, properties, stack);
            stack.pop();
            resolved.or_else(|| {
                fallback
                    .and_then(|fallback| resolve_custom_value_inner(fallback, properties, stack))
            })
        } else {
            fallback.and_then(|fallback| resolve_custom_value_inner(fallback, properties, stack))
        }?;
        output.push_str(&replacement);
        cursor = close + 1;
    }
    output.push_str(&value[cursor..]);
    Some(output)
}

fn find_var_function(value: &str) -> Option<usize> {
    let bytes = value.as_bytes();
    (0..bytes.len().saturating_sub(3))
        .find(|index| bytes[*index..*index + 4].eq_ignore_ascii_case(b"var("))
}

fn matching_parenthesis(value: &str, open: usize) -> Option<usize> {
    let mut depth = 0;
    for (index, byte) in value.as_bytes().iter().enumerate().skip(open) {
        match byte {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

fn parse_var_arguments(value: &str) -> Option<(&str, Option<&str>)> {
    let mut depth: usize = 0;
    let mut comma = None;
    for (index, byte) in value.as_bytes().iter().enumerate() {
        match byte {
            b'(' => depth += 1,
            b')' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => {
                comma = Some(index);
                break;
            }
            _ => {}
        }
    }
    let name_end = comma.unwrap_or(value.len());
    let name = value[..name_end].trim();
    if !name.starts_with("--") || name.len() == 2 {
        return None;
    }
    let fallback = comma
        .map(|index| value[index + 1..].trim())
        .filter(|fallback| !fallback.is_empty());
    Some((name, fallback))
}

fn parse_z_index(value: &str) -> Option<i32> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("auto") {
        return None;
    }
    value.parse().ok()
}

fn parse_grid_template(value: &str) -> GridTemplate {
    let mut template = GridTemplate::empty();
    let mut tracks = Vec::new();
    let value = value.replace(", ", ",");
    for token in value.split_ascii_whitespace() {
        if let Some(contents) = token
            .strip_prefix("repeat(")
            .and_then(|contents| contents.strip_suffix(')'))
            && let Some((count, track)) = contents.split_once(',')
            && let Ok(count) = count.trim().parse::<usize>()
            && let Some(track) = parse_grid_track(track.trim())
        {
            tracks.extend(std::iter::repeat_n(track, count));
            continue;
        }
        if let Some(track) = parse_grid_track(token) {
            tracks.push(track);
        }
    }
    for (index, track) in tracks.into_iter().take(template.tracks.len()).enumerate() {
        template.tracks[index] = track;
        template.count += 1;
    }
    template
}

fn parse_grid_track(value: &str) -> Option<GridTrack> {
    let value = value.trim().to_ascii_lowercase();
    if value == "auto" {
        return Some(GridTrack::Auto);
    }
    if let Some(value) = value.strip_suffix("fr") {
        return value
            .parse::<f32>()
            .ok()
            .filter(|value| *value > 0.0)
            .map(GridTrack::Fr);
    }
    if let Some(value) = value.strip_suffix("px") {
        return value.parse::<f32>().ok().map(GridTrack::Px);
    }
    if let Some(value) = value.strip_suffix('%') {
        return value.parse::<f32>().ok().map(GridTrack::Percent);
    }
    None
}

fn default_display(name: &str) -> Display {
    match name {
        "#document" | "html" | "body" | "address" | "article" | "aside" | "blockquote" | "div"
        | "dl" | "fieldset" | "figcaption" | "figure" | "footer" | "form" | "h1" | "h2" | "h3"
        | "h4" | "h5" | "h6" | "header" | "hgroup" | "hr" | "main" | "nav" | "ol" | "p" | "pre"
        | "section" | "table" | "ul" => Display::Block,
        "head" | "link" | "meta" | "noscript" | "script" | "style" | "template" | "title" => {
            Display::None
        }
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
        assert_eq!(parse_length("2em"), Some(Length::em(2.0)));
        assert_eq!(
            parse_length("68%"),
            Some(Length::Terms(LengthTerms {
                percent: 68.0,
                ..LengthTerms::default()
            }))
        );
        assert_eq!(
            parse_length("calc(100% - 20px)"),
            Some(Length::Terms(LengthTerms {
                px: -20.0,
                percent: 100.0,
                ..LengthTerms::default()
            }))
        );
        assert_eq!(
            parse_length("calc(100% - 20px)")
                .expect("calc length should parse")
                .resolve(200.0, 16.0),
            Some(180.0)
        );
    }

    #[test]
    fn parses_radius_opacity_shadow_and_gradient_declarations() {
        let stylesheet = Stylesheet::parse(
            ".card { border-radius: 18px 8px; opacity: 0.4; box-shadow: 0 10px 30px #00000080; background: linear-gradient(90deg, #ff0000 0%, #0000ff 100%); }",
        )
        .expect("stylesheet should parse");
        let mut card = Element::new("div");
        card.attrs.insert("class".to_string(), "card".to_string());
        let style = stylesheet.style_for(StyleContext {
            element: &card,
            path: &[0],
            ancestors: &[],
            parent_font_size: 16.0,
            parent_color: Color::BLACK,
            parent_weight: FontWeight::Normal,
            parent_text: InheritedText::default(),
            state: &StyleState::default(),
        });
        assert_eq!(style.border_radius.resolve(0.0, 16.0), Some(18.0));
        assert_eq!(style.opacity, 0.4);
        let shadow = style.shadow.expect("shadow should parse");
        assert_eq!(shadow.offset_x.resolve(0.0, 16.0), Some(0.0));
        assert_eq!(shadow.offset_y.resolve(0.0, 16.0), Some(10.0));
        assert_eq!(shadow.blur.resolve(0.0, 16.0), Some(30.0));
        assert_eq!(shadow.color.alpha, 128);
        let gradient = style.gradient.expect("gradient should parse");
        assert_eq!((gradient.angle, gradient.count), (90.0, 2));
        assert_eq!(gradient.color_at(0.0), Color::opaque(255, 0, 0));
        assert_eq!(gradient.color_at(1.0), Color::opaque(0, 0, 255));
        assert_eq!(gradient.color_at(0.5), Color::opaque(128, 0, 128));

        let none = Stylesheet::parse(".card { box-shadow: none; background: #101010; }")
            .expect("stylesheet should parse")
            .style_for(StyleContext {
                element: &card,
                path: &[0],
                ancestors: &[],
                parent_font_size: 16.0,
                parent_color: Color::BLACK,
                parent_weight: FontWeight::Normal,
                parent_text: InheritedText::default(),
                state: &StyleState::default(),
            });
        assert!(none.shadow.is_none());
        assert!(none.gradient.is_none());
        assert_eq!(none.background, Color::opaque(16, 16, 16));
    }

    #[test]
    fn resolves_root_relative_viewport_and_function_lengths() {
        let _environment = use_layout_environment(LayoutEnvironment {
            viewport: crate::layout::Size {
                width: 1000.0,
                height: 500.0,
            },
            root_font_size: 20.0,
        });
        let resolve = |source: &str| {
            parse_length(source)
                .unwrap_or_else(|| panic!("{source} should parse"))
                .resolve(400.0, 16.0)
        };
        assert_eq!(resolve("1.5rem"), Some(30.0));
        assert_eq!(resolve("2em"), Some(32.0));
        assert_eq!(resolve("10vw"), Some(100.0));
        assert_eq!(resolve("10vh"), Some(50.0));
        assert_eq!(resolve("10vmin"), Some(50.0));
        assert_eq!(resolve("10vmax"), Some(100.0));
        assert_eq!(resolve("50%"), Some(200.0));
        assert_eq!(resolve("clamp(10px, 5vw, 40px)"), Some(40.0));
        assert_eq!(resolve("clamp(10px, 1vw, 40px)"), Some(10.0));
        assert_eq!(resolve("min(30px, 2rem)"), Some(30.0));
        assert_eq!(resolve("max(30px, 2rem)"), Some(40.0));
        assert_eq!(resolve("calc(2rem + 5%)"), Some(60.0));
        assert_eq!(parse_length("3quirks"), None);
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
                .style_for_inherited_with_ancestors(
                    &element,
                    &[],
                    16.0,
                    Color::BLACK,
                    FontWeight::Normal
                )
                .color,
            Color::opaque(66, 211, 180)
        );
    }

    #[test]
    fn inherits_custom_properties_and_resolves_fallbacks() {
        let document = Document::parse(
            "<div id='theme' style='--Accent: #00ff00'><div id='card' style='background: var(--Accent); color: var(--missing, #0000ff); padding: var(--gap, 4px); --gap: 8px; --loop: var(--loop, #123456)'></div></div>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse("").expect("stylesheet should parse");
        let mut state = StyleState::default();
        stylesheet.populate_custom_properties(&document, &mut state);
        let theme = match &document.root.children[0] {
            Node::Element(element) => element,
            Node::Text(_) => panic!("theme should be an element"),
        };
        let card = match &theme.children[0] {
            Node::Element(element) => element,
            Node::Text(_) => panic!("card should be an element"),
        };
        let ancestors = [theme];
        let style = stylesheet.style_for(StyleContext {
            element: card,
            path: &[0, 0],
            ancestors: &ancestors,
            parent_font_size: 16.0,
            parent_color: Color::BLACK,
            parent_weight: FontWeight::Normal,
            parent_text: InheritedText::default(),
            state: &state,
        });
        assert_eq!(style.background, Color::opaque(0, 255, 0));
        assert_eq!(style.color, Color::opaque(0, 0, 255));
        assert_eq!(style.padding, Edges::all(Length::px(8.0)));
        assert_eq!(
            resolve_custom_value(
                "var(--loop, #abcdef)",
                state
                    .custom_properties
                    .get(&vec![0, 0])
                    .expect("card variables")
            ),
            Some("#123456".to_string())
        );
    }

    #[test]
    fn matches_root_and_resolves_calc_using_custom_properties() {
        let document =
            Document::parse("<html><div id='card'></div></html>").expect("document should parse");
        let stylesheet = Stylesheet::parse(
            ":root { --accent: #00ff00; } #card { width: calc(100% - 20px); color: var(--accent); }",
        )
        .expect("stylesheet should parse");
        let mut state = StyleState::default();
        stylesheet.populate_custom_properties(&document, &mut state);
        let html = match &document.root.children[0] {
            Node::Element(element) => element,
            Node::Text(_) => panic!("html should be an element"),
        };
        let card = match &html.children[0] {
            Node::Element(element) => element,
            Node::Text(_) => panic!("card should be an element"),
        };
        let style = stylesheet.style_for(StyleContext {
            element: card,
            path: &[0, 0],
            ancestors: &[html],
            parent_font_size: 16.0,
            parent_color: Color::BLACK,
            parent_weight: FontWeight::Normal,
            parent_text: InheritedText::default(),
            state: &state,
        });
        assert_eq!(style.width.resolve(100.0, 16.0), Some(80.0));
        assert_eq!(style.color, Color::opaque(0, 255, 0));
    }

    #[test]
    fn parses_four_edge_shorthand() {
        let edges = parse_edges("1px 2px 3px 4px");
        assert_eq!(edges.top, Length::px(1.0));
        assert_eq!(edges.right, Length::px(2.0));
        assert_eq!(edges.bottom, Length::px(3.0));
        assert_eq!(edges.left, Length::px(4.0));
    }

    #[test]
    fn parses_core_flex_properties() {
        let stylesheet = Stylesheet::parse(
            ".row { display: flex; flex-direction: column; justify-content: center; align-items: flex-end; gap: 12px; flex-grow: 2; }",
        )
        .expect("stylesheet should parse");
        let element = Element::new("div");
        let style = stylesheet.style_for_inherited_with_ancestors(
            &element,
            &[],
            16.0,
            Color::BLACK,
            FontWeight::Normal,
        );
        assert_eq!(style.display, Display::Block);
        let mut element = Element::new("div");
        element.attrs.insert("class".to_string(), "row".to_string());
        let style = stylesheet.style_for_inherited_with_ancestors(
            &element,
            &[],
            16.0,
            Color::BLACK,
            FontWeight::Normal,
        );
        assert_eq!(style.display, Display::Flex);
        assert_eq!(style.flex_direction, FlexDirection::Column);
        assert_eq!(style.justify_content, JustifyContent::Center);
        assert_eq!(style.align_items, AlignItems::FlexEnd);
        assert_eq!(style.gap, Length::px(12.0));
        assert_eq!(style.flex_grow, 2.0);
    }

    #[test]
    fn parses_position_and_overflow_properties() {
        let stylesheet = Stylesheet::parse(
            ".overlay { position: absolute; z-index: -2; inset: 10px 20px auto 30px; overflow: clip; }",
        )
        .expect("stylesheet should parse");
        let mut element = Element::new("div");
        element
            .attrs
            .insert("class".to_string(), "overlay".to_string());
        let style = stylesheet.style_for_inherited_with_ancestors(
            &element,
            &[],
            16.0,
            Color::BLACK,
            FontWeight::Normal,
        );
        assert_eq!(style.position, Position::Absolute);
        assert_eq!(style.z_index, Some(-2));
        assert_eq!(style.inset.top, Length::px(10.0));
        assert_eq!(style.inset.right, Length::px(20.0));
        assert_eq!(style.inset.bottom, Length::Auto);
        assert_eq!(style.inset.left, Length::px(30.0));
        assert_eq!(style.overflow, Overflow::Hidden);
        let auto =
            Stylesheet::parse(".scroll { overflow: auto; }").expect("auto stylesheet should parse");
        let mut scroll = Element::new("div");
        scroll
            .attrs
            .insert("class".to_string(), "scroll".to_string());
        assert_eq!(
            auto.style_for_inherited_with_ancestors(
                &scroll,
                &[],
                16.0,
                Color::BLACK,
                FontWeight::Normal,
            )
            .overflow,
            Overflow::Auto
        );
        let sticky = Stylesheet::parse(".sticky { position: sticky; top: 4px; }")
            .expect("sticky stylesheet should parse");
        let mut sticky_element = Element::new("div");
        sticky_element
            .attrs
            .insert("class".to_string(), "sticky".to_string());
        let sticky_style = sticky.style_for_inherited_with_ancestors(
            &sticky_element,
            &[],
            16.0,
            Color::BLACK,
            FontWeight::Normal,
        );
        assert_eq!(sticky_style.position, Position::Sticky);
        assert_eq!(sticky_style.inset.top, Length::px(4.0));
    }

    #[test]
    fn parses_core_grid_tracks() {
        let stylesheet = Stylesheet::parse(
            ".grid { display: grid; grid-template-columns: repeat(2, 1fr); grid-template-rows: 48px auto; gap: 12px; }",
        )
        .expect("stylesheet should parse");
        let mut element = Element::new("div");
        element
            .attrs
            .insert("class".to_string(), "grid".to_string());
        let style = stylesheet.style_for_inherited_with_ancestors(
            &element,
            &[],
            16.0,
            Color::BLACK,
            FontWeight::Normal,
        );
        assert_eq!(style.display, Display::Grid);
        assert_eq!(style.grid_template_columns.count, 2);
        assert_eq!(style.grid_template_columns.tracks[0], GridTrack::Fr(1.0));
        assert_eq!(style.grid_template_rows.count, 2);
        assert_eq!(style.grid_template_rows.tracks[0], GridTrack::Px(48.0));
        assert_eq!(style.gap, Length::px(12.0));
    }

    #[test]
    fn matches_descendant_and_child_selector_relationships() {
        let stylesheet =
            Stylesheet::parse(".shell .card { color: red; } .shell > .direct { color: blue; }")
                .expect("stylesheet should parse");
        let mut shell = Element::new("main");
        shell.attrs.insert("class".to_string(), "shell".to_string());
        let mut card = Element::new("section");
        card.attrs.insert("class".to_string(), "card".to_string());
        let mut direct = Element::new("p");
        direct
            .attrs
            .insert("class".to_string(), "direct".to_string());
        let card_style = stylesheet.style_for_inherited_with_ancestors(
            &card,
            &[&shell],
            16.0,
            Color::BLACK,
            FontWeight::Normal,
        );
        let direct_style = stylesheet.style_for_inherited_with_ancestors(
            &direct,
            &[&shell],
            16.0,
            Color::BLACK,
            FontWeight::Normal,
        );
        assert_eq!(card_style.color, Color::opaque(255, 0, 0));
        assert_eq!(direct_style.color, Color::opaque(0, 0, 255));
    }

    #[test]
    fn matches_selector_lists_and_not_with_argument_specificity() {
        let stylesheet = Stylesheet::parse(
            ".card, .panel { color: red; } .card:not(.muted) { color: blue; } .panel:not(.active, [aria-hidden='true']) { background: green; }",
        )
        .expect("selector list should parse");
        let mut card = Element::new("div");
        card.attrs.insert("class".to_string(), "card".to_string());
        let card_style = stylesheet.style_for_inherited_with_ancestors(
            &card,
            &[],
            16.0,
            Color::BLACK,
            FontWeight::Normal,
        );
        assert_eq!(card_style.color, Color::opaque(0, 0, 255));

        let mut muted = Element::new("div");
        muted
            .attrs
            .insert("class".to_string(), "card muted".to_string());
        let muted_style = stylesheet.style_for_inherited_with_ancestors(
            &muted,
            &[],
            16.0,
            Color::BLACK,
            FontWeight::Normal,
        );
        assert_eq!(muted_style.color, Color::opaque(255, 0, 0));

        let mut panel = Element::new("section");
        panel.attrs.insert("class".to_string(), "panel".to_string());
        let panel_style = stylesheet.style_for_inherited_with_ancestors(
            &panel,
            &[],
            16.0,
            Color::BLACK,
            FontWeight::Normal,
        );
        assert_eq!(panel_style.background, Color::opaque(0, 128, 0));

        let selectors = Selector::parse_list("[data-label='a,b'], .card:not(.muted)")
            .expect("nested selector commas should stay inside their arguments");
        assert_eq!(selectors.len(), 2);
        assert!(Selector::parse_list(".card,").is_err());
        assert!(Stylesheet::parse(".card, { color: red; }").is_err());
    }

    #[test]
    fn recovering_parse_keeps_valid_rules_and_reports_the_rest() {
        let (stylesheet, dropped) = Stylesheet::parse_recovering(
            r#"body::before { content: ""; } .card { color: red; } @keyframes signal { from { opacity: 0; } } p { color: } .last { color: blue; }"#,
        );
        assert_eq!(stylesheet.rules.len(), 3);
        assert_eq!(
            stylesheet
                .rules
                .iter()
                .map(|rule| rule.declarations.len())
                .collect::<Vec<_>>(),
            vec![1, 0, 1]
        );
        assert_eq!(dropped.len(), 3);
        assert!(dropped[0].starts_with("body::before"));
        assert!(dropped[1].starts_with("unsupported at-rule: @keyframes"));
        assert!(dropped[2].contains("invalid declaration: color:"));
        assert!(Stylesheet::parse("body::before { content: \"\"; }").is_err());
    }

    #[test]
    fn matches_sibling_combinators_and_structural_pseudo_classes() {
        let stylesheet = Stylesheet::parse(
            "p + p { color: red; } h1 ~ p { background-color: green; } li:first-child { color: blue; } li:last-child { color: yellow; } li:nth-child(2n) { border-width: 3px; }",
        )
        .expect("stylesheet should parse");
        let mut list = Element::new("ul");
        for _ in 0..3 {
            list.children.push(Node::Element(Element::new("li")));
        }
        let mut section = Element::new("section");
        section.children.push(Node::Element(Element::new("h1")));
        section.children.push(Node::Text(" ".to_string()));
        section.children.push(Node::Element(Element::new("p")));
        section.children.push(Node::Element(Element::new("p")));
        let state = StyleState::default();
        let style = |element: &Element, path: &[usize], ancestors: &[&Element]| {
            stylesheet.style_for(StyleContext {
                element,
                path,
                ancestors,
                parent_font_size: 16.0,
                parent_color: Color::BLACK,
                parent_weight: FontWeight::Normal,
                parent_text: InheritedText::default(),
                state: &state,
            })
        };

        let paragraph = Element::new("p");
        let first_paragraph = style(&paragraph, &[2], &[&section]);
        assert_eq!(first_paragraph.color, Color::BLACK);
        assert_eq!(first_paragraph.background, Color::opaque(0, 128, 0));
        let second_paragraph = style(&paragraph, &[3], &[&section]);
        assert_eq!(second_paragraph.color, Color::opaque(255, 0, 0));
        assert_eq!(second_paragraph.background, Color::opaque(0, 128, 0));

        let item = Element::new("li");
        assert_eq!(style(&item, &[0], &[&list]).color, Color::opaque(0, 0, 255));
        assert_eq!(
            style(&item, &[2], &[&list]).color,
            Color::opaque(255, 255, 0)
        );
        assert_eq!(style(&item, &[1], &[&list]).border_width, 3.0);
        assert_eq!(style(&item, &[0], &[&list]).border_width, 0.0);
    }

    #[test]
    fn media_queries_read_host_color_scheme_and_motion_preferences() {
        let stylesheet = Stylesheet::parse(
            "@media (prefers-color-scheme: dark) { p { color: red; } } @media (prefers-reduced-motion: reduce) { p { background-color: green; } }",
        )
        .expect("stylesheet should parse");
        let paragraph = Element::new("p");
        let style_with = |state: &StyleState| {
            stylesheet.style_for(StyleContext {
                element: &paragraph,
                path: &[0],
                ancestors: &[],
                parent_font_size: 16.0,
                parent_color: Color::BLACK,
                parent_weight: FontWeight::Normal,
                parent_text: InheritedText::default(),
                state,
            })
        };

        let light = StyleState::default();
        assert_eq!(style_with(&light).color, Color::BLACK);
        let dark = StyleState {
            color_scheme: ColorScheme::Dark,
            ..StyleState::default()
        };
        assert_eq!(style_with(&dark).color, Color::opaque(255, 0, 0));
        let still = StyleState {
            reduced_motion: true,
            ..StyleState::default()
        };
        assert_eq!(style_with(&still).background, Color::opaque(0, 128, 0));
    }

    #[test]
    fn matches_dynamic_pseudo_classes_from_style_state() {
        let stylesheet = Stylesheet::parse(
            "button:hover { color: red; } button:disabled { color: yellow; } button:enabled { background-color: green; } input:checked { color: blue; } input:disabled { color: yellow; }",
        )
        .expect("stylesheet should parse");
        let button = Element::new("button");
        let mut input = Element::new("input");
        input
            .attrs
            .insert("type".to_string(), "checkbox".to_string());
        input.attrs.insert("checked".to_string(), String::new());
        let mut state = StyleState {
            hovered_path: Some(vec![0]),
            ..StyleState::default()
        };
        let disabled_button = Element::new("button");
        state.disabled_paths.insert(vec![2]);
        assert_eq!(
            stylesheet
                .style_for(StyleContext {
                    element: &button,
                    path: &[0],
                    ancestors: &[],
                    parent_font_size: 16.0,
                    parent_color: Color::BLACK,
                    parent_weight: FontWeight::Normal,
                    parent_text: InheritedText::default(),
                    state: &state,
                })
                .color,
            Color::opaque(255, 0, 0)
        );
        assert_eq!(
            stylesheet
                .style_for(StyleContext {
                    element: &input,
                    path: &[1],
                    ancestors: &[],
                    parent_font_size: 16.0,
                    parent_color: Color::BLACK,
                    parent_weight: FontWeight::Normal,
                    parent_text: InheritedText::default(),
                    state: &state,
                })
                .color,
            Color::opaque(0, 0, 255)
        );
        assert_eq!(
            stylesheet
                .style_for(StyleContext {
                    element: &disabled_button,
                    path: &[2],
                    ancestors: &[],
                    parent_font_size: 16.0,
                    parent_color: Color::BLACK,
                    parent_weight: FontWeight::Normal,
                    parent_text: InheritedText::default(),
                    state: &state,
                })
                .color,
            Color::opaque(255, 255, 0)
        );
        assert_eq!(
            stylesheet
                .style_for(StyleContext {
                    element: &button,
                    path: &[3],
                    ancestors: &[],
                    parent_font_size: 16.0,
                    parent_color: Color::BLACK,
                    parent_weight: FontWeight::Normal,
                    parent_text: InheritedText::default(),
                    state: &state,
                })
                .background,
            Color::opaque(0, 128, 0)
        );
        state.unchecked_paths.insert(vec![1]);
        assert_eq!(
            stylesheet
                .style_for(StyleContext {
                    element: &input,
                    path: &[1],
                    ancestors: &[],
                    parent_font_size: 16.0,
                    parent_color: Color::BLACK,
                    parent_weight: FontWeight::Normal,
                    parent_text: InheritedText::default(),
                    state: &state,
                })
                .color,
            Color::BLACK
        );
    }

    #[test]
    fn matches_attribute_selectors() {
        let stylesheet = Stylesheet::parse(
            "[data-check] { color: red; } [data-check='module'] { background-color: blue; }",
        )
        .expect("attribute selectors should parse");
        let mut element = Element::new("p");
        element
            .attrs
            .insert("data-check".to_string(), "module".to_string());
        let style = stylesheet.style_for_inherited_with_ancestors(
            &element,
            &[],
            16.0,
            Color::BLACK,
            FontWeight::Normal,
        );
        assert_eq!(style.color, Color::opaque(255, 0, 0));
        assert_eq!(style.background, Color::opaque(0, 0, 255));
    }
}
