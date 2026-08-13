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

    pub(crate) const fn auto() -> Self {
        Self {
            top: Length::Auto,
            right: Length::Auto,
            bottom: Length::Auto,
            left: Length::Auto,
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
    pub(crate) scroll_offsets: BTreeMap<NodePath, (f32, f32)>,
    pub(crate) viewport_width: f32,
    pub(crate) viewport_height: f32,
    pub(crate) custom_properties: BTreeMap<NodePath, BTreeMap<String, String>>,
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
}

pub(crate) struct StyleContext<'a> {
    pub(crate) element: &'a Element,
    pub(crate) path: &'a [usize],
    pub(crate) ancestors: &'a [&'a Element],
    pub(crate) parent_font_size: f32,
    pub(crate) parent_color: Color,
    pub(crate) parent_weight: FontWeight,
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
    pub font_weight: FontWeight,
    pub flex_direction: FlexDirection,
    pub justify_content: JustifyContent,
    pub align_items: AlignItems,
    pub gap: Length,
    pub flex_grow: f32,
    pub grid_template_columns: GridTemplate,
    pub grid_template_rows: GridTemplate,
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
            font_size: Length::Px(parent_font_size),
            line_height: Length::Em(1.25),
            border_width: 0.0,
            border_color: Color::transparent(),
            text_align: TextAlign::Left,
            font_weight: parent_weight,
            flex_direction: FlexDirection::Row,
            justify_content: JustifyContent::FlexStart,
            align_items: AlignItems::Stretch,
            gap: Length::Px(0.0),
            flex_grow: 0.0,
            grid_template_columns: GridTemplate::empty(),
            grid_template_rows: GridTemplate::empty(),
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
        parse_rules(&source, None, &mut rules)?;
        Ok(Self { rules })
    }

    pub(crate) fn populate_custom_properties(&self, document: &Document, state: &mut StyleState) {
        state.custom_properties.clear();
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
            state,
        } = context;
        let mut style =
            Style::default_for(&element.name, parent_font_size, parent_color, parent_weight);
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PseudoClass {
    Hover,
    Focus,
    Checked,
    Disabled,
    Enabled,
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
            if source.as_bytes()[cursor] == b'>' {
                if parts.is_empty() || combinator.is_some() {
                    return Err(format!("invalid child combinator in selector: {source}"));
                }
                combinator = Some(Combinator::Child);
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
        if !compound.matches(element, path, state) {
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
    fn matches(&self, element: &Element, path: &[usize], state: &StyleState) -> bool {
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
                .all(|pseudo| pseudo.matches(element, path, state))
    }

    fn specificity(&self) -> Specificity {
        Specificity(
            u32::from(self.id.is_some()),
            (self.classes.len() + self.attributes.len() + self.pseudo_classes.len()) as u32,
            u32::from(self.tag.is_some()),
        )
    }
}

impl PseudoClass {
    fn matches(self, element: &Element, path: &[usize], state: &StyleState) -> bool {
        match self {
            Self::Hover => state.is_hovered(path),
            Self::Focus => state.is_focused(path),
            Self::Checked => {
                element.name == "input"
                    && element.attr("type").is_some_and(|kind| {
                        kind.eq_ignore_ascii_case("checkbox")
                            || kind.eq_ignore_ascii_case("radio")
                    })
                    && state.is_checked(element, path)
            }
            Self::Disabled => element.attrs.contains_key("disabled"),
            Self::Enabled => !element.attrs.contains_key("disabled"),
        }
    }
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
                let pseudo = match name.as_str() {
                    "hover" => PseudoClass::Hover,
                    "focus" => PseudoClass::Focus,
                    "checked" => PseudoClass::Checked,
                    "disabled" => PseudoClass::Disabled,
                    "enabled" => PseudoClass::Enabled,
                    _ => return Err(format!("unsupported pseudo-class: :{name}")),
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
) -> Result<(), String> {
    let mut cursor = 0;
    while cursor < source.len() {
        let remaining = &source[cursor..];
        let Some(relative_open) = remaining.find('{') else {
            if remaining.trim().is_empty() {
                break;
            }
            return Err("stylesheet has text outside a rule".to_string());
        };
        let open = cursor + relative_open;
        let header = source[cursor..open].trim();
        if header.is_empty() {
            return Err("stylesheet rule has no selector".to_string());
        }
        let close = matching_brace(source, open)?;
        let body = &source[open + 1..close];
        if header
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("@media"))
        {
            let nested_media = parse_media_condition(header)?;
            if media.is_some() {
                return Err("nested media rules are not supported".to_string());
            }
            parse_rules(body, Some(nested_media), rules)?;
        } else if header.starts_with('@') {
            return Err(format!("unsupported at-rule: {header}"));
        } else {
            let selectors = header
                .split(',')
                .map(Selector::parse)
                .collect::<Result<Vec<_>, _>>()?;
            let declarations = parse_declarations(body)?;
            rules.push(Rule {
                selectors,
                declarations,
                order: rules.len(),
                media: media.clone(),
            });
        }
        cursor = close + 1;
    }
    Ok(())
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
        let value = parse_media_dimension(value)?;
        match name.trim().to_ascii_lowercase().as_str() {
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
        let property_name = chunk[..colon].trim();
        let property = if property_name.starts_with("--") {
            property_name.to_string()
        } else {
            property_name.to_ascii_lowercase()
        };
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
        return value.parse::<f32>().ok().filter(|value| *value > 0.0).map(GridTrack::Fr);
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
            state: &state,
        });
        assert_eq!(style.background, Color::opaque(0, 255, 0));
        assert_eq!(style.color, Color::opaque(0, 0, 255));
        assert_eq!(style.padding, Edges::all(Length::Px(8.0)));
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
    fn parses_four_edge_shorthand() {
        let edges = parse_edges("1px 2px 3px 4px");
        assert_eq!(edges.top, Length::Px(1.0));
        assert_eq!(edges.right, Length::Px(2.0));
        assert_eq!(edges.bottom, Length::Px(3.0));
        assert_eq!(edges.left, Length::Px(4.0));
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
        assert_eq!(style.gap, Length::Px(12.0));
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
        assert_eq!(style.inset.top, Length::Px(10.0));
        assert_eq!(style.inset.right, Length::Px(20.0));
        assert_eq!(style.inset.bottom, Length::Auto);
        assert_eq!(style.inset.left, Length::Px(30.0));
        assert_eq!(style.overflow, Overflow::Hidden);
        let auto = Stylesheet::parse(".scroll { overflow: auto; }")
            .expect("auto stylesheet should parse");
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
        assert_eq!(sticky_style.inset.top, Length::Px(4.0));
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
        assert_eq!(style.gap, Length::Px(12.0));
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
    fn matches_dynamic_pseudo_classes_from_style_state() {
        let stylesheet = Stylesheet::parse(
            "button:hover { color: red; } input:checked { color: blue; } input:disabled { color: yellow; }",
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
        assert_eq!(
            stylesheet
                .style_for(StyleContext {
                    element: &button,
                    path: &[0],
                    ancestors: &[],
                    parent_font_size: 16.0,
                    parent_color: Color::BLACK,
                    parent_weight: FontWeight::Normal,
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
                    state: &state,
                })
                .color,
            Color::opaque(0, 0, 255)
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
