use crate::dom::{Document, Element, NodeId};
use crate::grid::{Areas, GridLine, GridPlacement, Tracks, parse_line, parse_placement};
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Display {
    Block,
    Flex,
    Grid,
    Inline,
    // laid out as a block, then placed on the line like a word
    InlineBlock,
    // a flex or grid container that sits on the line like an inline-block
    InlineFlex,
    InlineGrid,
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

    pub const fn rgba(red: u8, green: u8, blue: u8, alpha: u8) -> Self {
        Self {
            red,
            green,
            blue,
            alpha,
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

    // a color that needs no element: currentColor has nothing to stand for here
    pub fn parse(input: &str) -> Option<Self> {
        if input.trim().eq_ignore_ascii_case("currentcolor") {
            return None;
        }
        crate::colors::parse(input, Self::BLACK)
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
    // a percentage needs a definite basis, which an auto-height parent does not give
    pub(crate) fn has_percent(self) -> bool {
        match self {
            Self::Auto => false,
            Self::Terms(terms) => terms.percent != 0.0,
            Self::Clamp(minimum, preferred, maximum) => {
                minimum.percent != 0.0 || preferred.percent != 0.0 || maximum.percent != 0.0
            }
            Self::Smallest(left, right) | Self::Largest(left, right) => {
                left.percent != 0.0 || right.percent != 0.0
            }
        }
    }

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

    pub(crate) const fn percent(value: f32) -> Self {
        Self::Terms(LengthTerms {
            px: 0.0,
            em: 0.0,
            rem: 0.0,
            percent: value,
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

// whether width and height include padding and border
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BoxSizing {
    ContentBox,
    BorderBox,
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
    RowReverse,
    Column,
    ColumnReverse,
}

impl FlexDirection {
    pub(crate) fn is_row(self) -> bool {
        matches!(self, Self::Row | Self::RowReverse)
    }

    pub(crate) fn is_reverse(self) -> bool {
        matches!(self, Self::RowReverse | Self::ColumnReverse)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FlexWrap {
    NoWrap,
    Wrap,
    WrapReverse,
}

// align-content: how the lines of a wrapping flex container share its cross size
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AlignContent {
    Stretch,
    FlexStart,
    FlexEnd,
    Center,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum JustifyContent {
    FlexStart,
    Center,
    FlexEnd,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AlignItems {
    Stretch,
    FlexStart,
    Center,
    FlexEnd,
    // first-baseline alignment, which lines items up by their tops for now
    Baseline,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TransitionState {
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) started: u64,
    pub(crate) finishes: u64,
    pub(crate) delay: u64,
    pub(crate) duration: u64,
    pub(crate) timing: Timing,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct StyleState {
    pub(crate) hovered: Option<NodeId>,
    pub(crate) focused: Option<NodeId>,
    // the element the primary pointer button is pressed on
    pub(crate) active: Option<NodeId>,
    pub(crate) checked: BTreeSet<NodeId>,
    pub(crate) unchecked: BTreeSet<NodeId>,
    pub(crate) disabled: BTreeSet<NodeId>,
    pub(crate) scroll_offsets: BTreeMap<NodeId, (f32, f32)>,
    pub(crate) control_values: BTreeMap<NodeId, String>,
    pub(crate) text_selections: BTreeMap<NodeId, (usize, usize, bool)>,
    pub(crate) viewport_width: f32,
    pub(crate) viewport_height: f32,
    pub(crate) color_scheme: ColorScheme,
    pub(crate) reduced_motion: bool,
    pub(crate) clock: u64,
    pub(crate) transitions: BTreeMap<(NodeId, AnimatableProperty), TransitionState>,
    pub(crate) custom_properties: HashMap<NodeId, BTreeMap<String, String>>,
    pub(crate) root: Option<NodeId>,
    // natural pixel sizes of decoded images, keyed by their element
    pub(crate) image_sizes: BTreeMap<NodeId, (u32, u32)>,
    // when each element's current animation began, on the page clock
    pub(crate) animation_starts: BTreeMap<NodeId, u64>,
}

impl StyleState {
    fn is_hovered(&self, document: &Document, node: NodeId) -> bool {
        self.hovered
            .is_some_and(|hovered| document.is_inclusive_ancestor(node, hovered))
    }

    fn is_checked(&self, element: &Element, node: NodeId) -> bool {
        if self.checked.contains(&node) {
            return true;
        }
        if self.unchecked.contains(&node) {
            false
        } else {
            element.attr("checked").is_some()
        }
    }

    fn is_disabled(&self, element: &Element, node: NodeId) -> bool {
        self.disabled.contains(&node) || element.attrs.contains_key("disabled")
    }
}

// the element being styled, or the generated box of one of its pseudo-elements
#[derive(Clone, Copy)]
pub(crate) struct StyleContext<'a> {
    pub(crate) document: &'a Document,
    pub(crate) node: NodeId,
    pub(crate) pseudo: Option<PseudoElement>,
    pub(crate) parent_font_size: f32,
    pub(crate) parent_color: Color,
    pub(crate) parent_weight: FontWeight,
    pub(crate) parent_text: InheritedText,
    pub(crate) state: &'a StyleState,
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
    pub min_width: Length,
    pub max_width: Length,
    pub min_height: Length,
    pub max_height: Length,
    pub box_sizing: BoxSizing,
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
    // None is auto: the item follows its container's align-items
    pub align_self: Option<AlignItems>,
    pub align_content: AlignContent,
    pub flex_wrap: FlexWrap,
    // the gap between columns, and between flex items in a row
    pub gap: Length,
    pub row_gap: Length,
    pub flex_grow: f32,
    pub flex_shrink: f32,
    pub flex_basis: Length,
    pub order: i32,
    pub grid_template_columns: Tracks,
    pub grid_template_rows: Tracks,
    pub grid_template_areas: Areas,
    // implicit track sizes, cycled; none means auto
    pub grid_auto_columns: Tracks,
    pub grid_auto_rows: Tracks,
    pub grid_auto_flow_column: bool,
    pub grid_auto_flow_dense: bool,
    pub grid_column: GridPlacement,
    pub grid_row: GridPlacement,
    pub justify_items: AlignItems,
    pub justify_self: Option<AlignItems>,
    pub transitions: [Option<Transition>; MAX_TRANSITIONS],
    // only translation is expressed today, which the visual offset model already carries
    pub translate: (Length, Length),
    pub border_radius: Length,
    pub opacity: f32,
    pub gradient: Option<Gradient>,
    pub shadow: Option<BoxShadow>,
    // width over height, from aspect-ratio or a replaced element's natural size
    pub aspect_ratio: Option<f32>,
    pub object_fit: ObjectFit,
    pub background_image: Option<crate::image::ImageSource>,
    pub background_size: BackgroundSize,
    pub background_position: (Length, Length),
    pub background_repeat: BackgroundRepeat,
    // anything beyond a pure translation, which keeps using the visual offset path
    pub transform: Option<crate::transform::TransformList>,
    pub translate_property: Option<crate::transform::TransformList>,
    pub rotate_property: Option<crate::transform::TransformList>,
    pub scale_property: Option<crate::transform::TransformList>,
    pub transform_origin: (Length, Length),
    pub perspective: Option<f32>,
    pub perspective_origin: (Length, Length),
    pub backface_hidden: bool,
}

impl Style {
    pub(crate) fn has_transform(&self) -> bool {
        self.transform.is_some()
            || self.translate_property.is_some()
            || self.rotate_property.is_some()
            || self.scale_property.is_some()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum BackgroundSize {
    Auto,
    Cover,
    Contain,
    // an auto side keeps the image's ratio
    Explicit(Length, Length),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BackgroundRepeat {
    pub(crate) x: bool,
    pub(crate) y: bool,
}

// a resolved background image layer, carried on the layout box for paint
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BackgroundImage {
    pub(crate) source: crate::image::ImageSource,
    pub(crate) size: BackgroundSize,
    pub(crate) position: (Length, Length),
    pub(crate) repeat: BackgroundRepeat,
}

impl Style {
    pub(crate) fn background_layer(&self) -> Option<BackgroundImage> {
        Some(BackgroundImage {
            source: self.background_image?,
            size: self.background_size,
            position: self.background_position,
            repeat: self.background_repeat,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ObjectFit {
    Fill,
    Contain,
    Cover,
    None,
    ScaleDown,
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
            min_width: Length::Auto,
            max_width: Length::Auto,
            min_height: Length::Auto,
            max_height: Length::Auto,
            box_sizing: BoxSizing::ContentBox,
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
            align_self: None,
            align_content: AlignContent::Stretch,
            flex_wrap: FlexWrap::NoWrap,
            gap: Length::px(0.0),
            row_gap: Length::px(0.0),
            flex_grow: 0.0,
            flex_shrink: 1.0,
            flex_basis: Length::Auto,
            order: 0,
            grid_template_columns: Tracks::NONE,
            grid_template_rows: Tracks::NONE,
            grid_template_areas: Areas::NONE,
            grid_auto_columns: Tracks::NONE,
            grid_auto_rows: Tracks::NONE,
            grid_auto_flow_column: false,
            grid_auto_flow_dense: false,
            grid_column: GridPlacement::AUTO,
            grid_row: GridPlacement::AUTO,
            justify_items: AlignItems::Stretch,
            justify_self: None,
            transitions: [None; MAX_TRANSITIONS],
            translate: (Length::px(0.0), Length::px(0.0)),
            border_radius: Length::px(0.0),
            opacity: 1.0,
            gradient: None,
            shadow: None,
            aspect_ratio: None,
            object_fit: ObjectFit::Fill,
            background_image: None,
            background_size: BackgroundSize::Auto,
            background_position: (Length::px(0.0), Length::px(0.0)),
            background_repeat: BackgroundRepeat { x: true, y: true },
            transform: None,
            translate_property: None,
            rotate_property: None,
            scale_property: None,
            transform_origin: (Length::percent(50.0), Length::percent(50.0)),
            perspective: None,
            perspective_origin: (Length::percent(50.0), Length::percent(50.0)),
            backface_hidden: false,
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
    keyframes: Vec<Keyframes>,
    index: RuleIndex,
    // rules that declare a transition or an animation, checked before a full cascade
    motion_rules: Vec<usize>,
    // shadow root style elements, by their text
    shadow_sheets: RefCell<HashMap<String, Rc<Stylesheet>>>,
}

fn motion_rules(rules: &[Rule]) -> Vec<usize> {
    rules
        .iter()
        .enumerate()
        .filter(|(_, rule)| {
            rule.declarations.iter().any(|declaration| {
                matches!(
                    declaration.property.as_str(),
                    "transition" | "animation" | "animation-name"
                )
            })
        })
        .map(|(index, _)| index)
        .collect()
}

// rules bucketed by the rightmost id, class or tag, so an element only tries plausible rules
#[derive(Clone, Debug, Default)]
struct RuleIndex {
    by_id: HashMap<String, Vec<usize>>,
    by_class: HashMap<String, Vec<usize>>,
    by_tag: HashMap<String, Vec<usize>>,
    universal: Vec<usize>,
}

impl RuleIndex {
    fn build(rules: &[Rule]) -> Self {
        let mut index = Self::default();
        for (position, rule) in rules.iter().enumerate() {
            for selector in &rule.selectors {
                let Some(subject) = selector.parts.last().map(|part| &part.compound) else {
                    continue;
                };
                if let Some(id) = subject.id.as_ref() {
                    index.by_id.entry(id.clone()).or_default().push(position);
                } else if let Some(class) = subject.classes.first() {
                    index
                        .by_class
                        .entry(class.clone())
                        .or_default()
                        .push(position);
                } else if let Some(tag) = subject.tag.as_ref() {
                    index.by_tag.entry(tag.clone()).or_default().push(position);
                } else {
                    index.universal.push(position);
                }
            }
        }
        for bucket in index
            .by_id
            .values_mut()
            .chain(index.by_class.values_mut())
            .chain(index.by_tag.values_mut())
            .chain(std::iter::once(&mut index.universal))
        {
            bucket.sort_unstable();
            bucket.dedup();
        }
        index
    }

    fn candidates(&self, subject: &Element) -> Vec<usize> {
        let mut candidates = self.universal.clone();
        if let Some(bucket) = self.by_tag.get(&subject.name) {
            candidates.extend_from_slice(bucket);
        }
        if let Some(id) = subject.attr("id")
            && let Some(bucket) = self.by_id.get(id)
        {
            candidates.extend_from_slice(bucket);
        }
        for class in subject.classes() {
            if let Some(bucket) = self.by_class.get(class) {
                candidates.extend_from_slice(bucket);
            }
        }
        candidates.sort_unstable();
        candidates.dedup();
        candidates
    }
}

#[derive(Clone, Debug)]
struct Keyframes {
    name: String,
    stops: Vec<KeyframeStop>,
}

#[derive(Clone, Debug)]
struct KeyframeStop {
    progress: f32,
    declarations: Vec<Declaration>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Timing {
    Linear,
    CubicBezier(f32, f32, f32, f32),
    Steps(u32, StepPosition),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StepPosition {
    Start,
    End,
    None,
    Both,
}

impl Timing {
    pub(crate) const EASE: Self = Self::CubicBezier(0.25, 0.1, 0.25, 1.0);

    pub(crate) fn apply(self, progress: f32) -> f32 {
        let progress = progress.clamp(0.0, 1.0);
        match self {
            Self::Linear => progress,
            Self::CubicBezier(x1, y1, x2, y2) => cubic_bezier(x1, y1, x2, y2, progress),
            Self::Steps(count, position) => {
                let count = count.max(1) as f32;
                let (jumps, offset) = match position {
                    StepPosition::End => (count, 0.0),
                    StepPosition::Start => (count, 1.0),
                    StepPosition::None => ((count - 1.0).max(1.0), 0.0),
                    StepPosition::Both => (count + 1.0, 1.0),
                };
                if progress >= 1.0 {
                    return 1.0;
                }
                (((progress * count).floor() + offset) / jumps).clamp(0.0, 1.0)
            }
        }
    }
}

// solves x(t) = progress on the curve from (0, 0) to (1, 1), then returns y(t)
fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, progress: f32) -> f32 {
    let curve = |a: f32, b: f32, t: f32| {
        let inverse = 1.0 - t;
        3.0 * inverse * inverse * t * a + 3.0 * inverse * t * t * b + t * t * t
    };
    let slope = |a: f32, b: f32, t: f32| {
        let inverse = 1.0 - t;
        3.0 * inverse * inverse * a + 6.0 * inverse * t * (b - a) + 3.0 * t * t * (1.0 - b)
    };
    let mut t = progress;
    for _ in 0..8 {
        let error = curve(x1, x2, t) - progress;
        if error.abs() < 1e-6 {
            return curve(y1, y2, t);
        }
        let derivative = slope(x1, x2, t);
        if derivative.abs() < 1e-6 {
            break;
        }
        t = (t - error / derivative).clamp(0.0, 1.0);
    }
    // bisection when Newton steps stall on a flat stretch
    let (mut low, mut high) = (0.0_f32, 1.0_f32);
    t = progress;
    for _ in 0..32 {
        let x = curve(x1, x2, t);
        if (x - progress).abs() < 1e-6 {
            break;
        }
        if x < progress {
            low = t;
        } else {
            high = t;
        }
        t = (low + high) / 2.0;
    }
    curve(y1, y2, t)
}

fn parse_timing(value: &str) -> Option<Timing> {
    let lowered = value.trim().to_ascii_lowercase();
    let arguments = |name: &str| {
        lowered
            .strip_prefix(name)
            .and_then(|rest| rest.strip_prefix('('))
            .and_then(|rest| rest.strip_suffix(')'))
            .map(|inner| inner.split(',').map(str::trim).collect::<Vec<_>>())
    };
    Some(match lowered.as_str() {
        "linear" => Timing::Linear,
        "ease" => Timing::EASE,
        "ease-in" => Timing::CubicBezier(0.42, 0.0, 1.0, 1.0),
        "ease-out" => Timing::CubicBezier(0.0, 0.0, 0.58, 1.0),
        "ease-in-out" => Timing::CubicBezier(0.42, 0.0, 0.58, 1.0),
        "step-start" => Timing::Steps(1, StepPosition::Start),
        "step-end" => Timing::Steps(1, StepPosition::End),
        _ => {
            if let Some(values) = arguments("cubic-bezier") {
                let numbers = values
                    .iter()
                    .map(|value| value.parse::<f32>().ok().filter(|value| value.is_finite()))
                    .collect::<Option<Vec<_>>>()?;
                let [x1, y1, x2, y2] = numbers.as_slice() else {
                    return None;
                };
                // the x coordinates must stay inside the unit interval
                if !(0.0..=1.0).contains(x1) || !(0.0..=1.0).contains(x2) {
                    return None;
                }
                Timing::CubicBezier(*x1, *y1, *x2, *y2)
            } else if let Some(values) = arguments("steps") {
                let count = values
                    .first()?
                    .parse::<u32>()
                    .ok()
                    .filter(|count| *count > 0)?;
                let position = match values.get(1).copied() {
                    None | Some("end") | Some("jump-end") => StepPosition::End,
                    Some("start") | Some("jump-start") => StepPosition::Start,
                    Some("jump-none") if count > 1 => StepPosition::None,
                    Some("jump-both") => StepPosition::Both,
                    _ => return None,
                };
                Timing::Steps(count, position)
            } else {
                return None;
            }
        }
    })
}

// the properties a transition can drive in this engine; All stands for every one of them
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum AnimatableProperty {
    All,
    Color,
    Background,
    BackgroundColor,
    BackgroundPosition,
    BackgroundSize,
    BorderColor,
    BorderRadius,
    BorderWidth,
    BoxShadow,
    Opacity,
    Width,
    Height,
    Margin,
    MarginTop,
    MarginRight,
    MarginBottom,
    MarginLeft,
    Padding,
    PaddingTop,
    PaddingRight,
    PaddingBottom,
    PaddingLeft,
    Inset,
    Top,
    Right,
    Bottom,
    Left,
    Gap,
    FlexGrow,
    ZIndex,
    LetterSpacing,
    FontSize,
    LineHeight,
    Transform,
    TransformOrigin,
    Translate,
    Rotate,
    Scale,
    Perspective,
}

const ANIMATABLE: [(AnimatableProperty, &str); 39] = [
    (AnimatableProperty::Color, "color"),
    (AnimatableProperty::Background, "background"),
    (AnimatableProperty::BackgroundColor, "background-color"),
    (
        AnimatableProperty::BackgroundPosition,
        "background-position",
    ),
    (AnimatableProperty::BackgroundSize, "background-size"),
    (AnimatableProperty::BorderColor, "border-color"),
    (AnimatableProperty::BorderRadius, "border-radius"),
    (AnimatableProperty::BorderWidth, "border-width"),
    (AnimatableProperty::BoxShadow, "box-shadow"),
    (AnimatableProperty::Opacity, "opacity"),
    (AnimatableProperty::Width, "width"),
    (AnimatableProperty::Height, "height"),
    (AnimatableProperty::Margin, "margin"),
    (AnimatableProperty::MarginTop, "margin-top"),
    (AnimatableProperty::MarginRight, "margin-right"),
    (AnimatableProperty::MarginBottom, "margin-bottom"),
    (AnimatableProperty::MarginLeft, "margin-left"),
    (AnimatableProperty::Padding, "padding"),
    (AnimatableProperty::PaddingTop, "padding-top"),
    (AnimatableProperty::PaddingRight, "padding-right"),
    (AnimatableProperty::PaddingBottom, "padding-bottom"),
    (AnimatableProperty::PaddingLeft, "padding-left"),
    (AnimatableProperty::Inset, "inset"),
    (AnimatableProperty::Top, "top"),
    (AnimatableProperty::Right, "right"),
    (AnimatableProperty::Bottom, "bottom"),
    (AnimatableProperty::Left, "left"),
    (AnimatableProperty::Gap, "gap"),
    (AnimatableProperty::FlexGrow, "flex-grow"),
    (AnimatableProperty::ZIndex, "z-index"),
    (AnimatableProperty::LetterSpacing, "letter-spacing"),
    (AnimatableProperty::FontSize, "font-size"),
    (AnimatableProperty::LineHeight, "line-height"),
    (AnimatableProperty::Transform, "transform"),
    (AnimatableProperty::TransformOrigin, "transform-origin"),
    (AnimatableProperty::Translate, "translate"),
    (AnimatableProperty::Rotate, "rotate"),
    (AnimatableProperty::Scale, "scale"),
    (AnimatableProperty::Perspective, "perspective"),
];

impl AnimatableProperty {
    pub(crate) fn name(self) -> &'static str {
        ANIMATABLE
            .iter()
            .find(|(property, _)| *property == self)
            .map_or("all", |(_, name)| name)
    }

    fn parse(name: &str) -> Option<Self> {
        if name == "all" {
            return Some(Self::All);
        }
        ANIMATABLE
            .iter()
            .find(|(_, candidate)| *candidate == name)
            .map(|(property, _)| *property)
    }

    // the concrete properties this one covers
    pub(crate) fn expand(self) -> impl Iterator<Item = Self> {
        let all = self == Self::All;
        ANIMATABLE
            .iter()
            .map(|(property, _)| *property)
            .filter(move |property| all || *property == self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Transition {
    pub(crate) property: AnimatableProperty,
    pub(crate) duration: u64,
    pub(crate) delay: u64,
    pub(crate) timing: Timing,
}

pub(crate) const MAX_TRANSITIONS: usize = 6;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Animation {
    pub(crate) name: String,
    pub(crate) duration: u64,
    pub(crate) delay: u64,
    pub(crate) timing: Timing,
    pub(crate) iterations: Option<f32>,
    pub(crate) alternate: bool,
}

impl Stylesheet {
    fn has_generated_content(&self) -> bool {
        self.rules.iter().any(|rule| {
            rule.selectors.iter().any(|selector| {
                selector
                    .parts
                    .last()
                    .is_some_and(|part| part.compound.pseudo_element.is_some())
            })
        })
    }
}

impl Stylesheet {
    pub fn parse(source: &str) -> Result<Self, String> {
        let source = strip_comments(source);
        let mut rules = Vec::new();
        let mut keyframes = Vec::new();
        parse_rules(&source, None, &mut rules, &mut keyframes, None)?;
        let index = RuleIndex::build(&rules);
        let motion_rules = motion_rules(&rules);
        Ok(Self {
            rules,
            keyframes,
            index,
            motion_rules,
            shadow_sheets: RefCell::default(),
        })
    }

    // page rendering follows CSS error handling: an unsupported rule is dropped, the rest applies
    pub fn parse_recovering(source: &str) -> (Self, Vec<String>) {
        let source = strip_comments(source);
        let mut rules = Vec::new();
        let mut keyframes = Vec::new();
        let mut dropped = Vec::new();
        let _ = parse_rules(
            &source,
            None,
            &mut rules,
            &mut keyframes,
            Some(&mut dropped),
        );
        let index = RuleIndex::build(&rules);
        let motion_rules = motion_rules(&rules);
        (
            Self {
                rules,
                keyframes,
                index,
                motion_rules,
                shadow_sheets: RefCell::default(),
            },
            dropped,
        )
    }

    // the animated values of an element at the current clock, blended between keyframes
    pub(crate) fn animated_values(
        &self,
        animation: &Animation,
        now: u64,
        base: &BTreeMap<String, String>,
    ) -> Vec<(String, String)> {
        let Some(keyframes) = self
            .keyframes
            .iter()
            .find(|frames| frames.name == animation.name)
        else {
            return Vec::new();
        };
        if animation.duration == 0 || keyframes.stops.is_empty() {
            return Vec::new();
        }
        let keyframes = with_synthesized_endpoints(keyframes, base);
        let keyframes = &keyframes;
        if keyframes.stops.len() < 2 {
            return Vec::new();
        }
        let elapsed = now.saturating_sub(animation.delay);
        if now < animation.delay {
            return keyframes
                .stops
                .first()
                .map(|stop| declaration_pairs(&stop.declarations))
                .unwrap_or_default();
        }
        let cycle = elapsed as f32 / animation.duration as f32;
        if let Some(iterations) = animation.iterations
            && cycle >= iterations
        {
            let ended = if animation.alternate && (iterations.ceil() as u64).is_multiple_of(2) {
                keyframes.stops.first()
            } else {
                keyframes.stops.last()
            };
            return ended
                .map(|stop| declaration_pairs(&stop.declarations))
                .unwrap_or_default();
        }
        let mut progress = cycle.fract();
        if animation.alternate && !(cycle.floor() as u64).is_multiple_of(2) {
            progress = 1.0 - progress;
        }
        let progress = animation.timing.apply(progress);
        let mut values = Vec::new();
        for pair in keyframes.stops.windows(2) {
            let (start, end) = (&pair[0], &pair[1]);
            if progress > end.progress && end.progress < 1.0 {
                continue;
            }
            let span = (end.progress - start.progress).max(f32::EPSILON);
            let local = ((progress - start.progress) / span).clamp(0.0, 1.0);
            for declaration in &start.declarations {
                let target = end
                    .declarations
                    .iter()
                    .find(|other| other.property == declaration.property);
                let value = match target {
                    Some(target) => crate::interpolate::interpolate_value(
                        &declaration.property,
                        &declaration.value,
                        &target.value,
                        local,
                    )
                    .unwrap_or_else(|| declaration.value.clone()),
                    None => declaration.value.clone(),
                };
                values.push((declaration.property.clone(), value));
            }
            break;
        }
        values
    }

    pub(crate) fn populate_custom_properties(&self, document: &Document, state: &mut StyleState) {
        state.custom_properties.clear();
        let top = document
            .element_children(document.root())
            .collect::<Vec<_>>();
        state.root = top
            .iter()
            .copied()
            .find(|&node| {
                document
                    .element(node)
                    .is_some_and(|element| element.name == "html")
            })
            .or_else(|| {
                top.iter().copied().find(|&node| {
                    document.element(node).is_some_and(|element| {
                        !matches!(
                            element.name.as_str(),
                            "head" | "style" | "script" | "link" | "meta" | "title"
                        )
                    })
                })
            });
        let inherited = BTreeMap::new();
        for node in top {
            self.populate_element_custom_properties(document, node, &inherited, state);
        }
    }

    fn populate_element_custom_properties(
        &self,
        document: &Document,
        node: NodeId,
        inherited: &BTreeMap<String, String>,
        state: &mut StyleState,
    ) {
        let mut properties = inherited.clone();
        let mut changed = false;
        for (property, (_, _, value)) in self.cascaded_values(document, node, None, state) {
            if !property.starts_with("--") {
                continue;
            }
            changed = true;
            match value.trim().to_ascii_lowercase().as_str() {
                "initial" => {
                    properties.remove(property.as_ref());
                }
                "inherit" | "unset" => {}
                _ => {
                    properties.insert(property.into_owned(), value.into_owned());
                }
            }
        }
        if changed || inherited.is_empty() {
            state.custom_properties.insert(node, properties.clone());
        }
        for child in document.element_children(node).collect::<Vec<_>>() {
            self.populate_element_custom_properties(document, child, &properties, state);
        }
    }

    // the content string of a generated box, which is what decides whether it exists
    pub(crate) fn generated_content(&self, context: StyleContext<'_>) -> Option<String> {
        if !self.has_generated_content() {
            return None;
        }
        let values = self.cascaded_values(
            context.document,
            context.node,
            context.pseudo,
            context.state,
        );
        let empty = BTreeMap::new();
        let custom_properties =
            inherited_custom_properties(context.state, context.document, context.node)
                .unwrap_or(&empty);
        let (_, _, raw) = values.get("content")?;
        let resolved = resolve_custom_value(raw, custom_properties)?;
        let resolved = resolved.trim();
        if resolved.eq_ignore_ascii_case("none") || resolved.eq_ignore_ascii_case("normal") {
            return None;
        }
        let quoted = resolved
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .or_else(|| {
                resolved
                    .strip_prefix('\'')
                    .and_then(|rest| rest.strip_suffix('\''))
            })?;
        Some(quoted.to_string())
    }

    pub(crate) fn style_for(&self, context: StyleContext<'_>) -> Style {
        let StyleContext {
            document,
            node,
            pseudo,
            parent_font_size,
            parent_color,
            parent_weight,
            parent_text,
            state,
        } = context;
        let name = match pseudo {
            Some(pseudo) => pseudo.tag(),
            None => document
                .element(node)
                .map_or("", |element| element.name.as_str()),
        };
        let mut style = Style::default_for(
            name,
            parent_font_size,
            parent_color,
            parent_weight,
            parent_text,
        );
        let values = self.cascaded_values(document, node, pseudo, state);
        let empty_properties = BTreeMap::new();
        let custom_properties =
            inherited_custom_properties(state, document, node).unwrap_or(&empty_properties);
        // motion state belongs to the element itself, not to its generated boxes
        let own = pseudo.is_none().then_some(node);
        let mut animation = None;
        let mut base_values = BTreeMap::new();
        // declarations apply in cascade order, so a later shorthand overrides an earlier
        // longhand and the reverse; color comes first for currentColor
        let mut values = values
            .into_iter()
            .filter(|(property, _)| !property.starts_with("--"))
            .collect::<Vec<_>>();
        values.sort_unstable_by_key(|(property, (rank, order, _))| {
            (property.as_ref() != "color", *rank, *order)
        });
        for (property, (_, _, value)) in values {
            let Some(value) = resolve_custom_value(&value, custom_properties) else {
                continue;
            };
            if property == "animation" || property == "animation-name" {
                animation = parse_animation(&value);
                continue;
            }
            apply_declaration(&mut style, &property, &value);
            base_values.insert(property.into_owned(), value);
        }
        // a running transition replaces the declared value while it is in flight
        if let Some(node) = own
            && !state.transitions.is_empty()
            && !state.reduced_motion
        {
            let range = (node, AnimatableProperty::All)..=(node, AnimatableProperty::Perspective);
            for ((_, property), active) in state.transitions.range(range) {
                let elapsed = state.clock.saturating_sub(active.started);
                if elapsed < active.delay {
                    apply_declaration(&mut style, property.name(), &active.from);
                    continue;
                }
                let progress = if active.duration == 0 {
                    1.0
                } else {
                    ((elapsed - active.delay) as f32 / active.duration as f32).min(1.0)
                };
                if progress >= 1.0 {
                    continue;
                }
                if let Some(value) = crate::interpolate::interpolate_value(
                    property.name(),
                    &active.from,
                    &active.to,
                    active.timing.apply(progress),
                ) {
                    apply_declaration(&mut style, property.name(), &value);
                }
            }
        }
        // animated values win over the normal cascade, as the animation origin requires
        if let Some(animation) = animation.filter(|_| !state.reduced_motion) {
            let started = own
                .and_then(|node| state.animation_starts.get(&node).copied())
                .unwrap_or(0);
            let now = state.clock.saturating_sub(started);
            for (property, value) in self.animated_values(&animation, now, &base_values) {
                let Some(value) = resolve_custom_value(&value, custom_properties) else {
                    continue;
                };
                apply_declaration(&mut style, &property, &value);
            }
        }
        // an image with no declared size takes its natural size; with one side
        // declared, the other follows the natural ratio
        if let Some(&(width, height)) = own.and_then(|node| state.image_sizes.get(&node))
            && name == "img"
        {
            style
                .aspect_ratio
                .get_or_insert(width as f32 / height as f32);
            if style.width == Length::Auto && style.height == Length::Auto {
                style.width = Length::px(width as f32);
                style.height = Length::px(height as f32);
            }
        }
        style
    }

    pub(crate) fn has_transitions(&self) -> bool {
        self.rules.iter().any(|rule| {
            rule.declarations
                .iter()
                .any(|declaration| declaration.property == "transition")
        })
    }

    // an element's style with the inherited values of its ancestors, as layout computes it,
    // and its font size in pixels
    pub(crate) fn computed_style(
        &self,
        document: &Document,
        node: NodeId,
        state: &StyleState,
    ) -> Option<(Style, f32)> {
        document.element(node)?;
        let mut chain = std::iter::once(node)
            .chain(document.ancestors(node))
            .filter(|&current| document.element(current).is_some())
            .collect::<Vec<_>>();
        chain.reverse();
        let mut font_size = 16.0;
        let mut color = Color::BLACK;
        let mut weight = FontWeight::Normal;
        let mut text = InheritedText::default();
        let mut computed = None;
        for current in chain {
            let style = self.style_for(StyleContext {
                document,
                node: current,
                pseudo: None,
                parent_font_size: font_size,
                parent_color: color,
                parent_weight: weight,
                parent_text: text,
                state,
            });
            font_size = style
                .font_size
                .resolve(font_size, font_size)
                .unwrap_or(font_size)
                .max(1.0);
            color = style.color;
            weight = style.font_weight;
            text = InheritedText {
                transform: style.text_transform,
                letter_spacing: style.letter_spacing,
                family: style.font_family,
            };
            computed = Some((style, font_size));
        }
        computed
    }

    // declared values after custom property substitution, from one cascade
    pub(crate) fn declared_values(&self, context: StyleContext<'_>) -> BTreeMap<String, String> {
        let values = self.cascaded_values(
            context.document,
            context.node,
            context.pseudo,
            context.state,
        );
        let empty = BTreeMap::new();
        let custom_properties =
            inherited_custom_properties(context.state, context.document, context.node)
                .unwrap_or(&empty);
        values
            .into_iter()
            .filter_map(|(property, (_, _, raw))| {
                Some((
                    property.into_owned(),
                    resolve_custom_value(&raw, custom_properties)?,
                ))
            })
            .collect()
    }

    // the winning declaration of each property among the rules that match, borrowed from
    // this sheet
    fn borrowed_declarations<'a>(
        &'a self,
        element: &Element,
        document: &Document,
        node: NodeId,
        pseudo: Option<PseudoElement>,
        state: &StyleState,
        host_only: bool,
    ) -> HashMap<Cow<'a, str>, (Specificity, usize, Cow<'a, str>)> {
        let mut values: HashMap<Cow<'a, str>, (Specificity, usize, Cow<'a, str>)> = HashMap::new();
        for position in self.index.candidates(element) {
            let Some(rule) = self.rules.get(position) else {
                continue;
            };
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
                .filter(|selector| !host_only || selector.names_host())
                .filter(|selector| selector.matches(document, node, pseudo, state))
                .map(Selector::specificity)
                .max()
            else {
                continue;
            };
            // later declarations in a rule follow earlier ones in the cascade order
            for (position, declaration) in rule.declarations.iter().enumerate() {
                let candidate = (
                    specificity.important(declaration.important),
                    (rule.order << 16) | position,
                    Cow::Borrowed(declaration.value.as_str()),
                );
                let replace = values
                    .get(declaration.property.as_str())
                    .is_none_or(|current| (candidate.0, candidate.1) >= (current.0, current.1));
                if replace {
                    values.insert(Cow::Borrowed(declaration.property.as_str()), candidate);
                }
            }
        }
        values
    }

    fn matched_declarations(
        &self,
        element: &Element,
        document: &Document,
        node: NodeId,
        pseudo: Option<PseudoElement>,
        state: &StyleState,
        host_only: bool,
    ) -> Vec<(String, (Specificity, usize, String))> {
        self.borrowed_declarations(element, document, node, pseudo, state, host_only)
            .into_iter()
            .map(|(property, (specificity, order, value))| {
                (
                    property.into_owned(),
                    (specificity, order, value.into_owned()),
                )
            })
            .collect()
    }

    // the parsed style elements of a shadow root, parsed once per distinct text
    fn shadow_sheet(&self, document: &Document, root: NodeId) -> Option<Rc<Stylesheet>> {
        let text = document.shadow_style_text(root);
        if text.trim().is_empty() {
            return None;
        }
        let mut sheets = self.shadow_sheets.borrow_mut();
        if let Some(sheet) = sheets.get(&text) {
            return Some(sheet.clone());
        }
        let sheet = Rc::new(Stylesheet::parse_recovering(&text).0);
        sheets.insert(text, sheet.clone());
        Some(sheet)
    }

    // a cheap check before a full cascade: only rules that declare motion, or inline style
    pub(crate) fn may_animate(
        &self,
        document: &Document,
        node: NodeId,
        state: &StyleState,
    ) -> bool {
        let Some(element) = document.element(node) else {
            return false;
        };
        if element
            .attr("style")
            .is_some_and(|inline| inline.contains("transition") || inline.contains("animation"))
        {
            return true;
        }
        self.motion_rules.iter().any(|&index| {
            let rule = &self.rules[index];
            rule.media.as_ref().is_none_or(|media| media.matches(state))
                && rule
                    .selectors
                    .iter()
                    .any(|selector| selector.matches(document, node, None, state))
        })
    }

    pub(crate) fn has_keyframes(&self, name: &str) -> bool {
        self.keyframes.iter().any(|frames| frames.name == name)
    }

    pub(crate) fn has_animations(&self) -> bool {
        !self.keyframes.is_empty()
            && self.rules.iter().any(|rule| {
                rule.declarations.iter().any(|declaration| {
                    declaration.property == "animation" || declaration.property == "animation-name"
                })
            })
    }

    fn cascaded_values<'a>(
        &'a self,
        document: &'a Document,
        node: NodeId,
        pseudo: Option<PseudoElement>,
        state: &StyleState,
    ) -> HashMap<Cow<'a, str>, (Specificity, usize, Cow<'a, str>)> {
        let mut values: HashMap<Cow<'a, str>, (Specificity, usize, Cow<'a, str>)> = HashMap::new();
        // a generated box carries the id, classes and tag of the element that owns it
        let Some(element) = document.element(node) else {
            return values;
        };
        // a shadow tree is styled by its own style elements, and its host also takes the
        // :host rules there, below anything the outer document says
        let shadows = document.has_shadow_roots();
        let root = if shadows {
            document.tree_root(node)
        } else {
            document.root()
        };
        let in_shadow = shadows && document.shadow_host(root).is_some();
        values = if in_shadow {
            self.shadow_sheet(document, root)
                .map(|sheet| {
                    sheet
                        .matched_declarations(element, document, node, pseudo, state, false)
                        .into_iter()
                        .map(|(property, (specificity, order, value))| {
                            (
                                Cow::Owned(property),
                                (specificity, order, Cow::Owned(value)),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default()
        } else {
            self.borrowed_declarations(element, document, node, pseudo, state, false)
        };
        if shadows
            && let Some(shadow) = document.shadow_root(node)
            && let Some(sheet) = self.shadow_sheet(document, shadow)
        {
            for (property, (specificity, order, value)) in
                sheet.matched_declarations(element, document, node, pseudo, state, true)
            {
                values.entry(Cow::Owned(property)).or_insert((
                    specificity,
                    order,
                    Cow::Owned(value),
                ));
            }
        }
        let mut inline_dropped = Vec::new();
        if let Some(inline) = element.attr("style").filter(|_| pseudo.is_none())
            && let Ok(declarations) = parse_declarations(inline, Some(&mut inline_dropped))
        {
            for (position, declaration) in declarations.into_iter().enumerate() {
                let specificity = if declaration.important {
                    Specificity::INLINE_IMPORTANT
                } else {
                    Specificity::INLINE
                };
                let replace = values
                    .get(declaration.property.as_str())
                    .is_none_or(|current| specificity >= current.0);
                if replace {
                    values.insert(
                        Cow::Owned(declaration.property),
                        (
                            specificity,
                            (usize::MAX >> 1) | position,
                            Cow::Owned(declaration.value),
                        ),
                    );
                }
            }
        }
        values
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
    important: bool,
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
    pseudo_element: Option<PseudoElement>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PseudoElement {
    Before,
    After,
    // a pseudo-element the engine does not draw, such as ::placeholder; its rules parse and
    // never apply
    Unrendered,
}

impl PseudoElement {
    pub(crate) fn tag(self) -> &'static str {
        match self {
            Self::Before => "::before",
            Self::After => "::after",
            Self::Unrendered => "::unrendered",
        }
    }
}

#[derive(Clone, Debug)]
enum AttributeSelector {
    Exists(String),
    Match {
        name: String,
        operator: AttributeOperator,
        value: String,
        // the `i` flag compares ASCII case-insensitively
        ignore_case: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AttributeOperator {
    Equals,
    // ~= a whitespace-separated word
    Includes,
    // |= the value or the value followed by a hyphen
    DashMatch,
    Prefix,
    Suffix,
    Substring,
}

impl AttributeSelector {
    fn matches(&self, element: &Element) -> bool {
        match self {
            Self::Exists(name) => element.attr(name).is_some(),
            Self::Match {
                name,
                operator,
                value,
                ignore_case,
            } => {
                let Some(actual) = element.attr(name) else {
                    return false;
                };
                let (actual, value) = if *ignore_case {
                    (actual.to_ascii_lowercase(), value.to_ascii_lowercase())
                } else {
                    (actual.to_string(), value.clone())
                };
                match operator {
                    AttributeOperator::Equals => actual == value,
                    AttributeOperator::Includes => {
                        !value.is_empty()
                            && actual.split_ascii_whitespace().any(|word| word == value)
                    }
                    AttributeOperator::DashMatch => {
                        actual == value
                            || actual
                                .strip_prefix(value.as_str())
                                .is_some_and(|rest| rest.starts_with('-'))
                    }
                    AttributeOperator::Prefix => !value.is_empty() && actual.starts_with(&value),
                    AttributeOperator::Suffix => !value.is_empty() && actual.ends_with(&value),
                    AttributeOperator::Substring => !value.is_empty() && actual.contains(&value),
                }
            }
        }
    }
}

// a selector inside :has(), with the combinator that relates it to the anchor element
#[derive(Clone, Debug)]
struct RelativeSelector {
    combinator: Combinator,
    selector: Selector,
}

#[derive(Clone, Debug)]
enum PseudoClass {
    Root,
    Hover,
    Focus,
    FocusWithin,
    Active,
    Checked,
    Disabled,
    Enabled,
    FirstChild,
    LastChild,
    OnlyChild,
    // an+b among the siblings, optionally only those matching an `of` selector list
    NthChild(NthPattern, Vec<Selector>),
    NthLastChild(NthPattern, Vec<Selector>),
    FirstOfType,
    LastOfType,
    OnlyOfType,
    NthOfType(NthPattern),
    NthLastOfType(NthPattern),
    Empty,
    PlaceholderShown,
    Link,
    // a :visited link never matches, so history cannot be read through style
    Visited,
    ReadOnly,
    ReadWrite,
    Required,
    Optional,
    Defined,
    Not(Vec<Selector>),
    Is(Vec<Selector>),
    // as :is() but with no specificity
    Where(Vec<Selector>),
    Has(Vec<RelativeSelector>),
    // the shadow host, seen from its shadow tree's styles, optionally matching a compound
    Host(Vec<Selector>),
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

// importance and inline style sit above every selector's specificity: normal rules, then
// inline style, then !important rules, then !important inline style
const IMPORTANT: u32 = 1 << 30;

impl Specificity {
    const INLINE: Self = Self(1 << 29, 0, 0);
    const INLINE_IMPORTANT: Self = Self(u32::MAX, u32::MAX, u32::MAX);

    fn important(self, important: bool) -> Self {
        if important {
            Self(self.0.saturating_add(IMPORTANT), self.1, self.2)
        } else {
            self
        }
    }
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

    // a forgiving list, as :is() and :where() take it: parts that do not parse drop out
    fn parse_forgiving_list(source: &str) -> Vec<Self> {
        split_selector_list(source)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|part| Self::parse(part).ok())
            .collect()
    }

    pub(crate) fn parse_list(source: &str) -> Result<Vec<Self>, String> {
        split_selector_list(source)?
            .into_iter()
            .map(Self::parse)
            .collect()
    }

    pub(crate) fn matches(
        &self,
        document: &Document,
        node: NodeId,
        pseudo: Option<PseudoElement>,
        state: &StyleState,
    ) -> bool {
        let Some(last) = self.parts.last() else {
            return false;
        };
        last.compound.pseudo_element == pseudo
            && self.matches_part(self.parts.len() - 1, document, node, state)
    }

    fn matches_part(
        &self,
        index: usize,
        document: &Document,
        node: NodeId,
        state: &StyleState,
    ) -> bool {
        let Some(element) = document.element(node) else {
            return false;
        };
        if !self.parts[index]
            .compound
            .matches(element, document, node, state)
        {
            return false;
        }
        if index == 0 {
            return true;
        }
        let previous = |candidate: NodeId| self.matches_part(index - 1, document, candidate, state);
        // from inside a shadow tree the host is reachable only through :host
        let host = || {
            if !document.has_shadow_roots() {
                return None;
            }
            document
                .shadow_host(document.tree_root(node))
                .map(|(host, _)| host)
                .filter(|_| self.parts[index - 1].compound.names_host())
        };
        match self.parts[index].combinator {
            Some(Combinator::Child) => document
                .parent_element(node)
                .or_else(host)
                .is_some_and(previous),
            Some(Combinator::Descendant) => document
                .ancestors(node)
                .filter(|&ancestor| document.is_element(ancestor))
                .chain(host())
                .any(previous),
            Some(Combinator::NextSibling) => {
                document.element_sibling(node, false).is_some_and(previous)
            }
            Some(Combinator::SubsequentSibling) => {
                let mut sibling = document.element_sibling(node, false);
                while let Some(candidate) = sibling {
                    if previous(candidate) {
                        return true;
                    }
                    sibling = document.element_sibling(candidate, false);
                }
                false
            }
            None => false,
        }
    }

    // a selector whose subject is the shadow host, as :host rules are
    fn names_host(&self) -> bool {
        self.parts
            .last()
            .is_some_and(|part| part.compound.names_host())
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
    fn names_host(&self) -> bool {
        self.pseudo_classes
            .iter()
            .any(|pseudo| matches!(pseudo, PseudoClass::Host(_)))
    }

    fn matches(
        &self,
        element: &Element,
        document: &Document,
        node: NodeId,
        state: &StyleState,
    ) -> bool {
        self.tag.as_deref().is_none_or(|tag| tag == element.name)
            && self
                .id
                .as_deref()
                .is_none_or(|id| element.attr("id") == Some(id))
            && self.classes.iter().all(|class| element.has_class(class))
            && self
                .attributes
                .iter()
                .all(|attribute| attribute.matches(element))
            && self
                .pseudo_classes
                .iter()
                .all(|pseudo| pseudo.matches(element, document, node, state))
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
            u32::from(self.tag.is_some())
                + u32::from(self.pseudo_element.is_some())
                + pseudo_specificity.2,
        )
    }
}

impl PseudoClass {
    fn matches(
        &self,
        element: &Element,
        document: &Document,
        node: NodeId,
        state: &StyleState,
    ) -> bool {
        match self {
            Self::Root => state.root.map_or_else(
                || document.document_element() == Some(node),
                |root| root == node,
            ),
            Self::Hover => state.is_hovered(document, node),
            Self::Focus => state.focused == Some(node),
            Self::Checked => {
                element.name == "input"
                    && element.attr("type").is_some_and(|kind| {
                        kind.eq_ignore_ascii_case("checkbox") || kind.eq_ignore_ascii_case("radio")
                    })
                    && state.is_checked(element, node)
            }
            Self::Disabled => state.is_disabled(element, node),
            Self::Enabled => !state.is_disabled(element, node),
            Self::FirstChild => document.element_sibling(node, false).is_none(),
            Self::LastChild => document.element_sibling(node, true).is_none(),
            Self::OnlyChild => {
                document.element_sibling(node, false).is_none()
                    && document.element_sibling(node, true).is_none()
            }

            Self::FocusWithin => state
                .focused
                .is_some_and(|focused| document.is_inclusive_ancestor(node, focused)),
            Self::Active => state
                .active
                .is_some_and(|active| document.is_inclusive_ancestor(node, active)),
            Self::NthChild(pattern, of) | Self::NthLastChild(pattern, of) => {
                let last = matches!(self, Self::NthLastChild(..));
                let counts = |candidate: NodeId| {
                    of.is_empty()
                        || of
                            .iter()
                            .any(|selector| selector.matches(document, candidate, None, state))
                };
                counts(node)
                    && sibling_position(document, node, last, counts)
                        .is_some_and(|position| pattern.matches(position))
            }
            Self::FirstOfType | Self::LastOfType | Self::OnlyOfType => {
                let same = |candidate: NodeId| {
                    document
                        .element(candidate)
                        .is_some_and(|other| other.name == element.name)
                };
                let first = sibling_position(document, node, false, same) == Some(1);
                let last = sibling_position(document, node, true, same) == Some(1);
                match self {
                    Self::FirstOfType => first,
                    Self::LastOfType => last,
                    _ => first && last,
                }
            }
            Self::NthOfType(pattern) | Self::NthLastOfType(pattern) => {
                let same = |candidate: NodeId| {
                    document
                        .element(candidate)
                        .is_some_and(|other| other.name == element.name)
                };
                sibling_position(document, node, matches!(self, Self::NthLastOfType(_)), same)
                    .is_some_and(|position| pattern.matches(position))
            }
            Self::Empty => document.children(node).iter().all(|&child| {
                matches!(document.kind(child), Some(crate::dom::NodeKind::Comment(_)))
            }),
            Self::PlaceholderShown => {
                matches!(element.name.as_str(), "input" | "textarea")
                    && element.attr("placeholder").is_some()
                    && state.control_values.get(&node).map_or_else(
                        || element.attr("value").unwrap_or_default().is_empty(),
                        String::is_empty,
                    )
            }
            Self::Link => {
                matches!(element.name.as_str(), "a" | "area") && element.attr("href").is_some()
            }
            Self::Visited => false,
            Self::ReadWrite | Self::ReadOnly => {
                let text_input = element.name == "input"
                    && !matches!(
                        element
                            .attr("type")
                            .unwrap_or("text")
                            .to_ascii_lowercase()
                            .as_str(),
                        "checkbox"
                            | "radio"
                            | "button"
                            | "submit"
                            | "reset"
                            | "image"
                            | "file"
                            | "hidden"
                            | "range"
                            | "color"
                    );
                let editable = ((text_input || element.name == "textarea")
                    && element.attr("readonly").is_none()
                    && !state.is_disabled(element, node))
                    || element.attr("contenteditable").is_some_and(|value| {
                        value.is_empty() || value.eq_ignore_ascii_case("true")
                    });
                editable == matches!(self, Self::ReadWrite)
            }
            Self::Required | Self::Optional => {
                let form_control = matches!(element.name.as_str(), "input" | "select" | "textarea");
                form_control
                    && (element.attr("required").is_some() == matches!(self, Self::Required))
            }
            Self::Defined => true,
            Self::Is(selectors) | Self::Where(selectors) => selectors
                .iter()
                .any(|selector| selector.matches(document, node, None, state)),
            Self::Has(relatives) => relatives.iter().any(|relative| {
                let matching =
                    |candidate: NodeId| relative.selector.matches(document, candidate, None, state);
                match relative.combinator {
                    Combinator::Descendant => document
                        .descendants(node)
                        .filter(|&candidate| document.is_element(candidate))
                        .any(matching),
                    Combinator::Child => document.element_children(node).any(matching),
                    Combinator::NextSibling => {
                        document.element_sibling(node, true).is_some_and(matching)
                    }
                    Combinator::SubsequentSibling => {
                        let mut sibling = document.element_sibling(node, true);
                        while let Some(candidate) = sibling {
                            if matching(candidate) {
                                return true;
                            }
                            sibling = document.element_sibling(candidate, true);
                        }
                        false
                    }
                }
            }),
            Self::Not(selectors) => !selectors
                .iter()
                .any(|selector| selector.matches(document, node, None, state)),
            Self::Host(selectors) => {
                document.shadow_root(node).is_some()
                    && (selectors.is_empty()
                        || selectors
                            .iter()
                            .any(|selector| selector.matches(document, node, None, state)))
            }
        }
    }

    fn specificity(&self) -> Specificity {
        let most = |selectors: &[Selector]| {
            selectors
                .iter()
                .map(Selector::specificity)
                .max()
                .unwrap_or(Specificity(0, 0, 0))
        };
        match self {
            Self::Not(selectors) | Self::Is(selectors) => most(selectors),
            Self::Where(_) => Specificity(0, 0, 0),
            Self::Has(relatives) => relatives
                .iter()
                .map(|relative| relative.selector.specificity())
                .max()
                .unwrap_or(Specificity(0, 0, 0)),
            Self::NthChild(_, of) | Self::NthLastChild(_, of) => {
                let of = most(of);
                Specificity(of.0, of.1 + 1, of.2)
            }
            _ => Specificity(0, 1, 0),
        }
    }
}

// the 1-based position of a node among its element siblings that count, from either end
fn sibling_position(
    document: &Document,
    node: NodeId,
    from_end: bool,
    counts: impl Fn(NodeId) -> bool,
) -> Option<i32> {
    let parent = document.parent(node)?;
    let mut siblings = document
        .element_children(parent)
        .filter(|&child| counts(child))
        .collect::<Vec<_>>();
    if from_end {
        siblings.reverse();
    }
    siblings
        .iter()
        .position(|&child| child == node)
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
        pseudo_element: None,
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
                let double = *cursor < source.len() && source.as_bytes()[*cursor] == b':';
                if double {
                    *cursor += 1;
                }
                let name = parse_selector_name(source, cursor)?;
                let element = match name.as_str() {
                    "before" => Some(PseudoElement::Before),
                    "after" => Some(PseudoElement::After),
                    "placeholder"
                    | "selection"
                    | "marker"
                    | "backdrop"
                    | "first-line"
                    | "first-letter"
                    | "file-selector-button"
                    | "cue"
                    | "spelling-error"
                    | "grammar-error"
                    | "target-text"
                    | "highlight"
                    | "part"
                    | "slotted" => Some(PseudoElement::Unrendered),
                    vendor if double && vendor.starts_with('-') => Some(PseudoElement::Unrendered),
                    _ => None,
                };
                if let Some(element) = element {
                    if source.as_bytes().get(*cursor) == Some(&b'(') {
                        parse_function_argument(source, cursor)?;
                    }
                    if selector.pseudo_element.is_some() {
                        return Err(format!("selector has two pseudo elements: {source}"));
                    }
                    selector.pseudo_element = Some(element);
                    continue;
                }
                if double {
                    return Err(format!("unsupported pseudo element: ::{name}"));
                }
                let functional = source.as_bytes().get(*cursor) == Some(&b'(');
                let pseudo = if name == "not" {
                    let argument = parse_function_argument(source, cursor)?;
                    PseudoClass::Not(Selector::parse_list(&argument)?)
                } else if functional && matches!(name.as_str(), "is" | "matches" | "-webkit-any") {
                    let argument = parse_function_argument(source, cursor)?;
                    PseudoClass::Is(Selector::parse_forgiving_list(&argument))
                } else if functional && name == "where" {
                    let argument = parse_function_argument(source, cursor)?;
                    PseudoClass::Where(Selector::parse_forgiving_list(&argument))
                } else if functional && name == "has" {
                    let argument = parse_function_argument(source, cursor)?;
                    PseudoClass::Has(parse_relative_list(&argument)?)
                } else if functional
                    && matches!(
                        name.as_str(),
                        "nth-child" | "nth-last-child" | "nth-of-type" | "nth-last-of-type"
                    )
                {
                    let argument = parse_function_argument(source, cursor)?;
                    let (pattern, of) = match argument.find(" of ") {
                        Some(split) if name.ends_with("child") => (
                            &argument[..split],
                            Selector::parse_list(&argument[split + 4..])?,
                        ),
                        _ => (argument.as_str(), Vec::new()),
                    };
                    let pattern = NthPattern::parse(pattern)?;
                    match name.as_str() {
                        "nth-child" => PseudoClass::NthChild(pattern, of),
                        "nth-last-child" => PseudoClass::NthLastChild(pattern, of),
                        "nth-of-type" => PseudoClass::NthOfType(pattern),
                        _ => PseudoClass::NthLastOfType(pattern),
                    }
                } else if name == "host" && source.as_bytes().get(*cursor) == Some(&b'(') {
                    let argument = parse_function_argument(source, cursor)?;
                    PseudoClass::Host(Selector::parse_list(&argument)?)
                } else if name == "host" {
                    PseudoClass::Host(Vec::new())
                } else {
                    match name.as_str() {
                        "root" | "scope" => PseudoClass::Root,
                        "hover" => PseudoClass::Hover,
                        // focus moved by the keyboard or by script shows a ring, so every
                        // focus is visible focus here
                        "focus" | "focus-visible" => PseudoClass::Focus,
                        "focus-within" => PseudoClass::FocusWithin,
                        "active" => PseudoClass::Active,
                        "checked" => PseudoClass::Checked,
                        "disabled" => PseudoClass::Disabled,
                        "enabled" => PseudoClass::Enabled,
                        "first-child" => PseudoClass::FirstChild,
                        "last-child" => PseudoClass::LastChild,
                        "only-child" => PseudoClass::OnlyChild,
                        "first-of-type" => PseudoClass::FirstOfType,
                        "last-of-type" => PseudoClass::LastOfType,
                        "only-of-type" => PseudoClass::OnlyOfType,
                        "empty" => PseudoClass::Empty,
                        "placeholder-shown" => PseudoClass::PlaceholderShown,
                        "link" | "any-link" => PseudoClass::Link,
                        "visited" => PseudoClass::Visited,
                        "read-only" => PseudoClass::ReadOnly,
                        "read-write" => PseudoClass::ReadWrite,
                        "required" => PseudoClass::Required,
                        "optional" => PseudoClass::Optional,
                        "defined" => PseudoClass::Defined,
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
                let name = name.to_ascii_lowercase();
                let attribute = if *cursor < source.len() && source.as_bytes()[*cursor] == b']' {
                    *cursor += 1;
                    AttributeSelector::Exists(name)
                } else {
                    let rest = &source[*cursor..];
                    let (operator, width) = if rest.starts_with('=') {
                        (AttributeOperator::Equals, 1)
                    } else if rest.starts_with("~=") {
                        (AttributeOperator::Includes, 2)
                    } else if rest.starts_with("|=") {
                        (AttributeOperator::DashMatch, 2)
                    } else if rest.starts_with("^=") {
                        (AttributeOperator::Prefix, 2)
                    } else if rest.starts_with("$=") {
                        (AttributeOperator::Suffix, 2)
                    } else if rest.starts_with("*=") {
                        (AttributeOperator::Substring, 2)
                    } else {
                        return Err(format!("unsupported attribute selector: {source}"));
                    };
                    *cursor += width;
                    skip_selector_whitespace(source, cursor);
                    let value = parse_attribute_value(source, cursor)?;
                    skip_selector_whitespace(source, cursor);
                    let mut ignore_case = false;
                    if let Some(&flag) = source.as_bytes().get(*cursor)
                        && matches!(flag, b'i' | b'I' | b's' | b'S')
                    {
                        ignore_case = matches!(flag, b'i' | b'I');
                        *cursor += 1;
                        skip_selector_whitespace(source, cursor);
                    }
                    if *cursor >= source.len() || source.as_bytes()[*cursor] != b']' {
                        return Err(format!("unterminated attribute selector: {source}"));
                    }
                    *cursor += 1;
                    AttributeSelector::Match {
                        name,
                        operator,
                        value,
                        ignore_case,
                    }
                };
                selector.attributes.push(attribute);
            }
            b'*' => *cursor += 1,
            byte if byte.is_ascii_whitespace() || byte == b'>' => break,
            _ => {
                if selector.tag.is_some() {
                    return Err(format!("invalid compound selector: {source}"));
                }
                selector.tag = Some(parse_selector_name(source, cursor)?.to_ascii_lowercase());
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

// an identifier as written: ids and classes keep their case, and backslash escapes and
// non-ASCII characters are part of the name
fn parse_selector_name(source: &str, cursor: &mut usize) -> Result<String, String> {
    let mut name = String::new();
    while let Some(character) = source[*cursor..].chars().next() {
        if character.is_ascii_alphanumeric()
            || matches!(character, '-' | '_')
            || !character.is_ascii()
        {
            name.push(character);
            *cursor += character.len_utf8();
        } else if character == '\\' {
            *cursor += 1;
            let hex = source[*cursor..]
                .chars()
                .take_while(char::is_ascii_hexdigit)
                .take(6)
                .collect::<String>();
            if hex.is_empty() {
                let Some(escaped) = source[*cursor..].chars().next() else {
                    break;
                };
                name.push(escaped);
                *cursor += escaped.len_utf8();
            } else {
                *cursor += hex.len();
                let code = u32::from_str_radix(&hex, 16).unwrap_or(0xfffd);
                name.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                if source[*cursor..].starts_with(' ') {
                    *cursor += 1;
                }
            }
        } else {
            break;
        }
    }
    if name.is_empty() {
        return Err(format!("selector component has no name: {source}"));
    }
    Ok(name)
}

// the argument of :has(): relative selectors that may start with a combinator
fn parse_relative_list(source: &str) -> Result<Vec<RelativeSelector>, String> {
    split_selector_list(source)?
        .into_iter()
        .map(|part| {
            let part = part.trim();
            let (combinator, rest) = if let Some(rest) = part.strip_prefix('>') {
                (Combinator::Child, rest)
            } else if let Some(rest) = part.strip_prefix('+') {
                (Combinator::NextSibling, rest)
            } else if let Some(rest) = part.strip_prefix('~') {
                (Combinator::SubsequentSibling, rest)
            } else {
                (Combinator::Descendant, part)
            };
            Ok(RelativeSelector {
                combinator,
                selector: Selector::parse(rest.trim())?,
            })
        })
        .collect()
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

// css fills a missing 0% or 100% keyframe with the element's own value for that property
fn with_synthesized_endpoints(keyframes: &Keyframes, base: &BTreeMap<String, String>) -> Keyframes {
    let mut stops = keyframes.stops.clone();
    let animated: Vec<String> = {
        let mut properties: Vec<String> = stops
            .iter()
            .flat_map(|stop| stop.declarations.iter().map(|entry| entry.property.clone()))
            .collect();
        properties.sort();
        properties.dedup();
        properties
    };
    let endpoint = |progress: f32| KeyframeStop {
        progress,
        declarations: animated
            .iter()
            .filter_map(|property| {
                base.get(property).map(|value| Declaration {
                    property: property.clone(),
                    value: value.clone(),
                    important: false,
                })
            })
            .collect(),
    };
    if stops.first().is_none_or(|stop| stop.progress > 0.0) {
        stops.insert(0, endpoint(0.0));
    }
    if stops.last().is_none_or(|stop| stop.progress < 1.0) {
        stops.push(endpoint(1.0));
    }
    Keyframes {
        name: keyframes.name.clone(),
        stops,
    }
}

fn declaration_pairs(declarations: &[Declaration]) -> Vec<(String, String)> {
    declarations
        .iter()
        .map(|declaration| (declaration.property.clone(), declaration.value.clone()))
        .collect()
}

// colors, numbers, lengths and translate transforms blend, anything else steps at the midpoint
fn parse_keyframes(header: &str, body: &str) -> Option<Keyframes> {
    let name = header
        .get("@keyframes".len()..)?
        .trim()
        .trim_matches(['"', '\''])
        .trim()
        .to_string();
    if name.is_empty() {
        return None;
    }
    let mut stops = Vec::new();
    let mut cursor = 0;
    while let Some(relative_open) = body[cursor..].find('{') {
        let open = cursor + relative_open;
        let selector = body[cursor..open].trim().to_ascii_lowercase();
        let close = matching_brace(body, open).ok()?;
        let declarations =
            parse_declarations(&body[open + 1..close], Some(&mut Vec::new())).ok()?;
        cursor = close + 1;
        for stop in selector.split(',') {
            let stop = stop.trim();
            let progress = match stop {
                "from" => 0.0,
                "to" => 1.0,
                other => match other.strip_suffix('%') {
                    Some(percent) => percent.trim().parse::<f32>().ok()? / 100.0,
                    None => continue,
                },
            };
            stops.push(KeyframeStop {
                progress: progress.clamp(0.0, 1.0),
                declarations: declarations.clone(),
            });
        }
    }
    if stops.is_empty() {
        return None;
    }
    stops.sort_by(|left, right| {
        left.progress
            .partial_cmp(&right.progress)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Some(Keyframes { name, stops })
}

// transform accepts translate functions, and rotation, scale and skew remain gates
fn parse_translate(value: &str) -> Option<(Length, Length)> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("none") {
        return Some((Length::px(0.0), Length::px(0.0)));
    }
    let mut total = (Length::px(0.0), Length::px(0.0));
    let mut seen = false;
    for (name, arguments) in crate::transform::functions(value)? {
        let name = name.to_ascii_lowercase();
        let mut parts = arguments.into_iter();
        let first = parts.next().filter(|part| !part.is_empty())?;
        let second = parts.next();
        if parts.next().is_some() {
            return None;
        }
        let (x, y) = match name.as_str() {
            "translate" => (Some(first), second),
            "translatex" => (Some(first), None),
            "translatey" => (None, Some(first)),
            _ => return None,
        };
        if let Some(x) = x {
            total.0 = add_lengths(total.0, parse_length(x)?)?;
        }
        if let Some(y) = y {
            total.1 = add_lengths(total.1, parse_length(y)?)?;
        }
        seen = true;
    }
    seen.then_some(total)
}

fn add_lengths(left: Length, right: Length) -> Option<Length> {
    match (left, right) {
        (Length::Terms(left), Length::Terms(right)) => Some(Length::Terms(LengthTerms {
            px: left.px + right.px,
            em: left.em + right.em,
            rem: left.rem + right.rem,
            percent: left.percent + right.percent,
            viewport_width: left.viewport_width + right.viewport_width,
            viewport_height: left.viewport_height + right.viewport_height,
            zero_width: left.zero_width + right.zero_width,
        })),
        _ => None,
    }
}

// transition: <property> <duration> [timing] [delay], repeated for each comma separated entry
fn parse_transitions(value: &str) -> [Option<Transition>; MAX_TRANSITIONS] {
    let mut transitions = [None; MAX_TRANSITIONS];
    if value.trim().eq_ignore_ascii_case("none") {
        return transitions;
    }
    let mut next = 0;
    for entry in split_top_level(value) {
        let mut property = None;
        let mut durations = Vec::new();
        let mut timing = Timing::EASE;
        for token in split_words(entry) {
            let lowered = token.to_ascii_lowercase();
            if let Some(duration) = parse_duration(&lowered) {
                durations.push(duration);
            } else if let Some(parsed) = parse_timing(&lowered) {
                timing = parsed;
            } else if let Some(parsed) = AnimatableProperty::parse(&lowered) {
                property = Some(parsed);
            }
        }
        // a missing property means all, as the shorthand's initial value does
        let property = property.unwrap_or(AnimatableProperty::All);
        let Some(&duration) = durations.first() else {
            continue;
        };
        if next >= transitions.len() {
            break;
        }
        transitions[next] = Some(Transition {
            property,
            duration,
            delay: durations.get(1).copied().unwrap_or(0),
            timing,
        });
        next += 1;
    }
    transitions
}

pub(crate) fn animation_from(values: &BTreeMap<String, String>) -> Option<Animation> {
    values
        .get("animation")
        .or_else(|| values.get("animation-name"))
        .and_then(|value| parse_animation(value))
}

pub(crate) fn transitions_from(
    values: &BTreeMap<String, String>,
) -> [Option<Transition>; MAX_TRANSITIONS] {
    values
        .get("transition")
        .map_or([None; MAX_TRANSITIONS], |value| parse_transitions(value))
}

fn parse_animation(value: &str) -> Option<Animation> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("none") {
        return None;
    }
    let mut name = None;
    let mut durations = Vec::new();
    let mut timing = Timing::EASE;
    let mut iterations = Some(1.0);
    let mut alternate = false;
    for token in split_words(value) {
        let lowered = token.to_ascii_lowercase();
        if let Some(duration) = parse_duration(&lowered) {
            durations.push(duration);
            continue;
        }
        if let Some(parsed) = parse_timing(&lowered) {
            timing = parsed;
            continue;
        }
        match lowered.as_str() {
            "infinite" => iterations = None,
            "alternate" | "alternate-reverse" => alternate = true,
            "normal" | "reverse" | "forwards" | "backwards" | "both" | "none" | "running"
            | "paused" => {}
            other => {
                if let Ok(count) = other.parse::<f32>() {
                    iterations = Some(count.max(0.0));
                } else if name.is_none() {
                    name = Some(token.to_string());
                }
            }
        }
    }
    Some(Animation {
        name: name?,
        duration: durations.first().copied().unwrap_or(0),
        delay: durations.get(1).copied().unwrap_or(0),
        timing,
        iterations,
        alternate,
    })
}

fn parse_duration(value: &str) -> Option<u64> {
    if let Some(seconds) = value.strip_suffix("ms") {
        return seconds.trim().parse::<f32>().ok().map(|value| value as u64);
    }
    if let Some(seconds) = value.strip_suffix('s') {
        return seconds
            .trim()
            .parse::<f32>()
            .ok()
            .map(|value| (value * 1000.0) as u64);
    }
    None
}

fn parse_rules(
    source: &str,
    media: Option<MediaCondition>,
    rules: &mut Vec<Rule>,
    keyframes: &mut Vec<Keyframes>,
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
            parse_rules(
                body,
                Some(nested_media),
                rules,
                keyframes,
                dropped.as_deref_mut(),
            )?;
            continue;
        }
        if header
            .get(..10)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("@keyframes"))
        {
            match parse_keyframes(header, body) {
                Some(frames) => keyframes.push(frames),
                None => drop_or_fail(
                    dropped.as_deref_mut(),
                    format!("unsupported keyframes rule: {header}"),
                )?,
            }
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

// a media query list from script, such as matchMedia's; one that does not parse matches
// nothing
pub(crate) fn media_query_matches(query: &str, state: &StyleState) -> bool {
    query.split(',').any(|part| {
        parse_media_condition(&format!("@media {}", part.trim()))
            .is_ok_and(|condition| condition.matches(state))
    })
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
        let mut value = chunk[colon + 1..].trim();
        let mut important = false;
        if let Some(bang) = value.rfind('!')
            && value[bang + 1..].trim().eq_ignore_ascii_case("important")
        {
            important = true;
            value = value[..bang].trim_end();
        }
        let value = value.to_string();
        if property.is_empty() || value.is_empty() {
            drop_or_fail(
                dropped.as_deref_mut(),
                format!("invalid declaration: {chunk}"),
            )?;
            continue;
        }
        declarations.push(Declaration {
            property,
            value,
            important,
        });
    }
    Ok(declarations)
}

thread_local! {
    // the element's color while its declarations apply, for currentColor
    static CURRENT_COLOR: std::cell::Cell<Color> = const { std::cell::Cell::new(Color::BLACK) };
}

fn parse_color(value: &str) -> Option<Color> {
    crate::colors::parse(value, CURRENT_COLOR.with(std::cell::Cell::get))
}

fn apply_declaration(style: &mut Style, property: &str, value: &str) {
    CURRENT_COLOR.with(|current| current.set(style.color));
    match property {
        "display" => {
            style.display = match value.trim().to_ascii_lowercase().as_str() {
                "none" => Display::None,
                "flex" => Display::Flex,
                "grid" => Display::Grid,
                "inline" => Display::Inline,
                "inline-block" => Display::InlineBlock,
                "inline-flex" => Display::InlineFlex,
                "inline-grid" => Display::InlineGrid,
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
        // none and auto both read as no limit
        "min-width" => style.min_width = parse_length(value).unwrap_or(Length::Auto),
        "max-width" => style.max_width = parse_length(value).unwrap_or(Length::Auto),
        "min-height" => style.min_height = parse_length(value).unwrap_or(Length::Auto),
        "max-height" => style.max_height = parse_length(value).unwrap_or(Length::Auto),
        "box-sizing" => {
            style.box_sizing = if value.trim().eq_ignore_ascii_case("border-box") {
                BoxSizing::BorderBox
            } else {
                BoxSizing::ContentBox
            };
        }
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
                style.background_image = None;
                return;
            }
            if value.trim().eq_ignore_ascii_case("none") {
                style.gradient = None;
                if property != "background-color" {
                    style.background_image = None;
                }
                return;
            }
            if let Some(color) = parse_color(value) {
                style.gradient = None;
                style.background = color;
                if property == "background" {
                    style.background_image = None;
                }
                return;
            }
            if property != "background-color" {
                apply_background_layer(style, value, property == "background");
            }
        }
        "background-size" => {
            if let Some(size) = parse_background_size(value) {
                style.background_size = size;
            }
        }
        "background-position" => {
            if let Some(position) = parse_background_position(value) {
                style.background_position = position;
            }
        }
        "background-repeat" => {
            if let Some(repeat) = parse_background_repeat(value) {
                style.background_repeat = repeat;
            }
        }
        "aspect-ratio" => {
            if let Some(ratio) = parse_aspect_ratio(value) {
                style.aspect_ratio = ratio;
            }
        }
        "object-fit" => {
            style.object_fit = match value.trim().to_ascii_lowercase().as_str() {
                "contain" => ObjectFit::Contain,
                "cover" => ObjectFit::Cover,
                "none" => ObjectFit::None,
                "scale-down" => ObjectFit::ScaleDown,
                _ => ObjectFit::Fill,
            };
        }
        "transform" => {
            if value.trim().eq_ignore_ascii_case("none") {
                style.translate = (Length::px(0.0), Length::px(0.0));
                style.transform = None;
            } else if let Some(translate) = parse_translate(value) {
                style.translate = translate;
                style.transform = None;
            } else if let Some(list) = crate::transform::parse_transform_list(value)
                .and_then(|ops| crate::transform::TransformList::intern(value.trim(), ops))
            {
                style.translate = (Length::px(0.0), Length::px(0.0));
                style.transform = Some(list);
            }
        }
        "translate" | "rotate" | "scale" => {
            let parsed = if value.trim().eq_ignore_ascii_case("none") {
                Some(None)
            } else {
                parse_individual_transform(property, value)
                    .and_then(|op| {
                        crate::transform::TransformList::intern(
                            &format!("{property}:{}", value.trim()),
                            vec![op],
                        )
                    })
                    .map(Some)
            };
            if let Some(list) = parsed {
                match property {
                    "translate" => style.translate_property = list,
                    "rotate" => style.rotate_property = list,
                    _ => style.scale_property = list,
                }
            }
        }
        "transform-origin" => {
            if let Some(origin) = parse_origin(value) {
                style.transform_origin = origin;
            }
        }
        "perspective" => {
            let value = value.trim();
            if value.eq_ignore_ascii_case("none") {
                style.perspective = None;
            } else if let Some(Length::Terms(terms)) = parse_length(value)
                && terms.px > 0.0
            {
                style.perspective = Some(terms.px);
            }
        }
        "perspective-origin" => {
            if let Some(origin) = parse_origin(value) {
                style.perspective_origin = origin;
            }
        }
        "backface-visibility" => {
            style.backface_hidden = value.trim().eq_ignore_ascii_case("hidden");
        }
        "transition" => {
            style.transitions = parse_transitions(value);
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
        "color" => style.color = parse_color(value).unwrap_or(style.color),
        "font-size" => style.font_size = parse_length(value).unwrap_or(style.font_size),
        "line-height" => style.line_height = parse_line_height(value).unwrap_or(style.line_height),
        "font-weight" => {
            if value.trim().eq_ignore_ascii_case("bold") || value.trim() == "700" {
                style.font_weight = FontWeight::Bold;
            }
        }
        "flex-direction" => {
            if let Some(direction) = parse_flex_direction(value) {
                style.flex_direction = direction;
            }
        }
        "flex-wrap" => {
            if let Some(wrap) = parse_flex_wrap(value) {
                style.flex_wrap = wrap;
            }
        }
        "flex-flow" => {
            for word in value.split_ascii_whitespace() {
                if let Some(direction) = parse_flex_direction(word) {
                    style.flex_direction = direction;
                } else if let Some(wrap) = parse_flex_wrap(word) {
                    style.flex_wrap = wrap;
                }
            }
        }
        "justify-content" => {
            style.justify_content = match last_keyword(value).as_str() {
                "center" => JustifyContent::Center,
                "flex-end" | "end" | "right" => JustifyContent::FlexEnd,
                "space-between" => JustifyContent::SpaceBetween,
                "space-around" => JustifyContent::SpaceAround,
                "space-evenly" => JustifyContent::SpaceEvenly,
                _ => JustifyContent::FlexStart,
            };
        }
        "align-items" => style.align_items = parse_align(value).unwrap_or(AlignItems::Stretch),
        "align-self" => style.align_self = parse_align(value),
        "align-content" => {
            style.align_content = match last_keyword(value).as_str() {
                "flex-start" | "start" => AlignContent::FlexStart,
                "flex-end" | "end" => AlignContent::FlexEnd,
                "center" => AlignContent::Center,
                "space-between" => AlignContent::SpaceBetween,
                "space-around" => AlignContent::SpaceAround,
                "space-evenly" => AlignContent::SpaceEvenly,
                _ => AlignContent::Stretch,
            };
        }
        "gap" | "grid-gap" => {
            let mut parts = value.split_ascii_whitespace().filter_map(parse_length);
            if let Some(row) = parts.next() {
                style.row_gap = row;
                style.gap = parts.next().unwrap_or(row);
            }
        }
        "row-gap" | "grid-row-gap" => style.row_gap = parse_length(value).unwrap_or(style.row_gap),
        "column-gap" | "grid-column-gap" => style.gap = parse_length(value).unwrap_or(style.gap),
        "flex-shrink" => {
            style.flex_shrink = parse_number(value).unwrap_or(style.flex_shrink).max(0.0);
        }
        "flex-basis" => {
            style.flex_basis = if value.trim().eq_ignore_ascii_case("content") {
                Length::Auto
            } else {
                parse_length(value).unwrap_or(style.flex_basis)
            };
        }
        "order" => {
            if let Ok(order) = value.trim().parse::<i32>() {
                style.order = order;
            }
        }
        "grid-template-columns" => {
            style.grid_template_columns =
                Tracks::parse(value).unwrap_or(style.grid_template_columns);
        }
        "grid-template-rows" => {
            style.grid_template_rows = Tracks::parse(value).unwrap_or(style.grid_template_rows);
        }
        "grid-template-areas" => {
            style.grid_template_areas = Areas::parse(value).unwrap_or(style.grid_template_areas);
        }
        "grid-template" | "grid" => apply_grid_template(style, value),
        "grid-auto-columns" => {
            style.grid_auto_columns = Tracks::parse(value).unwrap_or(style.grid_auto_columns);
        }
        "grid-auto-rows" => {
            style.grid_auto_rows = Tracks::parse(value).unwrap_or(style.grid_auto_rows);
        }
        "grid-auto-flow" => {
            let value = value.to_ascii_lowercase();
            style.grid_auto_flow_column = value.contains("column");
            style.grid_auto_flow_dense = value.contains("dense");
        }
        "grid-column" => style.grid_column = parse_placement(value).unwrap_or(style.grid_column),
        "grid-row" => style.grid_row = parse_placement(value).unwrap_or(style.grid_row),
        "grid-column-start" => {
            style.grid_column.start = parse_line(value).unwrap_or(style.grid_column.start);
        }
        "grid-column-end" => {
            style.grid_column.end = parse_line(value).unwrap_or(style.grid_column.end);
        }
        "grid-row-start" => {
            style.grid_row.start = parse_line(value).unwrap_or(style.grid_row.start)
        }
        "grid-row-end" => style.grid_row.end = parse_line(value).unwrap_or(style.grid_row.end),
        "grid-area" => {
            let parts = value.split('/').collect::<Vec<_>>();
            let lines = parts
                .iter()
                .map(|part| parse_line(part))
                .collect::<Option<Vec<_>>>();
            if let Some(lines) = lines.filter(|lines| (1..=4).contains(&lines.len())) {
                // omitted lines copy a named start, or are auto
                let copy = |line: GridLine| match line {
                    GridLine::Named(..) => line,
                    _ => GridLine::Auto,
                };
                let row_start = lines[0];
                let column_start = lines.get(1).copied().unwrap_or(copy(row_start));
                let row_end = lines.get(2).copied().unwrap_or(copy(row_start));
                let column_end = lines.get(3).copied().unwrap_or(copy(column_start));
                style.grid_row = GridPlacement {
                    start: row_start,
                    end: row_end,
                };
                style.grid_column = GridPlacement {
                    start: column_start,
                    end: column_end,
                };
            }
        }
        "justify-items" => {
            style.justify_items = parse_align(value).unwrap_or(AlignItems::Stretch);
        }
        "justify-self" => style.justify_self = parse_align(value),
        "place-items" => {
            let mut parts = value.split_ascii_whitespace().map(parse_align);
            if let Some(align) = parts.next() {
                let align = align.unwrap_or(AlignItems::Stretch);
                style.align_items = align;
                style.justify_items = parts.next().flatten().unwrap_or(align);
            }
        }
        "place-self" => {
            let mut parts = value.split_ascii_whitespace().map(parse_align);
            if let Some(align) = parts.next() {
                style.align_self = align;
                style.justify_self = parts.next().unwrap_or(align);
            }
        }
        "flex-grow" => style.flex_grow = parse_number(value).unwrap_or(style.flex_grow).max(0.0),
        "flex" => {
            if let Some((grow, shrink, basis)) = parse_flex_shorthand(value) {
                style.flex_grow = grow;
                style.flex_shrink = shrink;
                style.flex_basis = basis;
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
        "border-color" => style.border_color = parse_color(value).unwrap_or(style.border_color),
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
            color = parse_color(token);
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

pub(crate) fn parse_length(value: &str) -> Option<Length> {
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
                parse_color(head)?,
                percent.trim().parse::<f32>().ok()? / 100.0,
            ),
            _ => (
                parse_color(entry)?,
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
        color = parse_color(token)?;
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

// an element without its own entry reads its nearest ancestor's properties
fn inherited_custom_properties<'a>(
    state: &'a StyleState,
    document: &Document,
    node: NodeId,
) -> Option<&'a BTreeMap<String, String>> {
    std::iter::once(node)
        .chain(document.ancestors(node))
        .find_map(|candidate| state.custom_properties.get(&candidate))
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

// grid-template and grid: rows and columns around a slash, optionally with quoted area rows
fn apply_grid_template(style: &mut Style, value: &str) {
    let value = value.trim();
    if value.eq_ignore_ascii_case("none") {
        style.grid_template_rows = Tracks::NONE;
        style.grid_template_columns = Tracks::NONE;
        style.grid_template_areas = Areas::NONE;
        return;
    }
    let (rows, columns) = match value.rsplit_once('/') {
        Some((rows, columns)) => (rows.trim(), columns.trim()),
        None => (value, "none"),
    };
    let auto_flow = |side: &str| {
        let lower = side.to_ascii_lowercase();
        lower.contains("auto-flow").then(|| {
            let dense = lower.contains("dense");
            let sizes = lower.replace("auto-flow", "").replace("dense", "");
            (
                dense,
                Tracks::parse(if sizes.trim().is_empty() {
                    "none"
                } else {
                    &sizes
                }),
            )
        })
    };
    if let Some((dense, sizes)) = auto_flow(rows) {
        style.grid_auto_flow_column = false;
        style.grid_auto_flow_dense = dense;
        style.grid_auto_rows = sizes.unwrap_or(Tracks::NONE);
        style.grid_template_rows = Tracks::NONE;
        style.grid_template_columns = Tracks::parse(columns).unwrap_or(Tracks::NONE);
        style.grid_template_areas = Areas::NONE;
        return;
    }
    if let Some((dense, sizes)) = auto_flow(columns) {
        style.grid_auto_flow_column = true;
        style.grid_auto_flow_dense = dense;
        style.grid_auto_columns = sizes.unwrap_or(Tracks::NONE);
        style.grid_template_columns = Tracks::NONE;
        style.grid_template_rows = Tracks::parse(rows).unwrap_or(Tracks::NONE);
        style.grid_template_areas = Areas::NONE;
        return;
    }
    if rows.contains(['"', '\'']) {
        // each quoted row may be followed by its size
        let mut areas = String::new();
        let mut sizes = Vec::new();
        for (index, part) in rows.split(['"', '\'']).enumerate() {
            if index % 2 == 1 {
                areas.push_str(&format!("\"{part}\" "));
                sizes.push("auto".to_string());
            } else if let Some(size) = sizes.last_mut() {
                let size_text = part
                    .split_ascii_whitespace()
                    .filter(|word| !word.starts_with('[') && !word.ends_with(']'))
                    .collect::<Vec<_>>()
                    .join(" ");
                if !size_text.is_empty() {
                    *size = size_text;
                }
            }
        }
        if let Some(areas) = Areas::parse(&areas) {
            style.grid_template_areas = areas;
            style.grid_template_rows = Tracks::parse(&sizes.join(" ")).unwrap_or(Tracks::NONE);
            style.grid_template_columns = Tracks::parse(columns).unwrap_or(Tracks::NONE);
        }
        return;
    }
    if let (Some(rows), Some(columns)) = (Tracks::parse(rows), Tracks::parse(columns)) {
        style.grid_template_rows = rows;
        style.grid_template_columns = columns;
        style.grid_template_areas = Areas::NONE;
    }
}

// the translate, rotate and scale properties, which apply before transform in that order
fn parse_individual_transform(
    property: &str,
    value: &str,
) -> Option<crate::transform::TransformOp> {
    use crate::transform::{TransformOp, parse_angle};
    let words = value.split_whitespace().collect::<Vec<_>>();
    let number = |word: &str| -> Option<f32> {
        match word.strip_suffix('%') {
            Some(percent) => percent.parse::<f32>().ok().map(|value| value / 100.0),
            None => word.parse::<f32>().ok(),
        }
        .filter(|value| value.is_finite())
    };
    match (property, words.as_slice()) {
        ("translate", [x]) => Some(TransformOp::Translate(
            parse_length(x)?,
            Length::px(0.0),
            0.0,
        )),
        ("translate", [x, y]) => Some(TransformOp::Translate(
            parse_length(x)?,
            parse_length(y)?,
            0.0,
        )),
        ("translate", [x, y, z]) => match parse_length(z)? {
            Length::Terms(terms) => Some(TransformOp::Translate(
                parse_length(x)?,
                parse_length(y)?,
                terms.px,
            )),
            _ => None,
        },
        ("rotate", [angle]) => Some(TransformOp::Rotate(0.0, 0.0, 1.0, parse_angle(angle)?)),
        ("rotate", [axis, angle]) => {
            let angle = parse_angle(angle)?;
            match axis.to_ascii_lowercase().as_str() {
                "x" => Some(TransformOp::Rotate(1.0, 0.0, 0.0, angle)),
                "y" => Some(TransformOp::Rotate(0.0, 1.0, 0.0, angle)),
                "z" => Some(TransformOp::Rotate(0.0, 0.0, 1.0, angle)),
                _ => None,
            }
        }
        ("rotate", [x, y, z, angle]) => Some(TransformOp::Rotate(
            number(x)?,
            number(y)?,
            number(z)?,
            parse_angle(angle)?,
        )),
        ("scale", [both]) => Some(TransformOp::Scale(number(both)?, number(both)?, 1.0)),
        ("scale", [x, y]) => Some(TransformOp::Scale(number(x)?, number(y)?, 1.0)),
        ("scale", [x, y, z]) => Some(TransformOp::Scale(number(x)?, number(y)?, number(z)?)),
        _ => None,
    }
}

// transform-origin and perspective-origin: keywords or lengths, with a centered default axis
fn parse_origin(value: &str) -> Option<(Length, Length)> {
    let words = value
        .split_whitespace()
        .take(2)
        .collect::<Vec<_>>()
        .join(" ");
    parse_background_position(&words)
}

// splits on whitespace outside parentheses, so url(...) and functions stay whole
fn split_words(value: &str) -> Vec<&str> {
    let mut words = Vec::new();
    let mut depth = 0_usize;
    let mut start = None;
    for (index, character) in value.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if character.is_whitespace() && depth == 0 {
            if let Some(begin) = start.take() {
                words.push(&value[begin..index]);
            }
        } else if start.is_none() {
            start = Some(index);
        }
    }
    if let Some(begin) = start {
        words.push(&value[begin..]);
    }
    words
}

fn parse_url(word: &str) -> Option<&str> {
    let inner = word
        .get(..4)
        .filter(|prefix| prefix.eq_ignore_ascii_case("url("))
        .and_then(|_| word[4..].strip_suffix(')'))?
        .trim();
    Some(inner.trim_matches(['"', '\'']))
}

// the image, color, position, size and repeat parts of a background declaration
fn apply_background_layer(style: &mut Style, value: &str, shorthand: bool) {
    let layer = split_top_level(value).into_iter().next().unwrap_or(value);
    // the size follows a slash outside parentheses, so a slash inside url() stays put
    let mut depth = 0_usize;
    let slash = layer.char_indices().find_map(|(index, character)| {
        match character {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            '/' if depth == 0 => return Some(index),
            _ => {}
        }
        None
    });
    let (place, size) = match slash {
        Some(index) => (&layer[..index], Some(&layer[index + 1..])),
        None => (layer, None),
    };
    // after the slash, only the leading one or two words are the size
    let mut size_words = Vec::new();
    let mut trailing = Vec::new();
    for word in size.map(split_words).unwrap_or_default() {
        let is_size = matches!(
            word.to_ascii_lowercase().as_str(),
            "auto" | "cover" | "contain"
        ) || parse_length(word).is_some();
        if is_size && size_words.len() < 2 && trailing.is_empty() {
            size_words.push(word);
        } else {
            trailing.push(word);
        }
    }
    let mut image = None;
    let mut positions = Vec::new();
    let mut repeat = None;
    for word in split_words(place).into_iter().chain(trailing) {
        if let Some(url) = parse_url(word) {
            image = crate::image::intern_image_source(url);
        } else if let Some(parsed) = parse_background_repeat(word) {
            repeat = Some(parsed);
        } else if let Some(color) = parse_color(word) {
            if shorthand {
                style.background = color;
            }
        } else {
            positions.push(word);
        }
    }
    if image.is_none() && !shorthand {
        return;
    }
    style.gradient = None;
    style.background_image = image;
    if shorthand {
        style.background_position = parse_background_position(&positions.join(" "))
            .unwrap_or((Length::px(0.0), Length::px(0.0)));
        style.background_repeat = repeat.unwrap_or(BackgroundRepeat { x: true, y: true });
        style.background_size =
            parse_background_size(&size_words.join(" ")).unwrap_or(BackgroundSize::Auto);
    }
}

fn parse_background_size(value: &str) -> Option<BackgroundSize> {
    let value = value.trim().to_ascii_lowercase();
    match value.as_str() {
        "cover" => return Some(BackgroundSize::Cover),
        "contain" => return Some(BackgroundSize::Contain),
        "auto" | "auto auto" => return Some(BackgroundSize::Auto),
        _ => {}
    }
    let mut words = value.split_whitespace();
    let parse = |word: &str| {
        if word == "auto" {
            Some(Length::Auto)
        } else {
            parse_length(word)
        }
    };
    let width = parse(words.next()?)?;
    let height = words.next().map(parse).unwrap_or(Some(Length::Auto))?;
    Some(BackgroundSize::Explicit(width, height))
}

// keywords become percentages; one value centers the other axis
fn parse_background_position(value: &str) -> Option<(Length, Length)> {
    let percent = |amount: f32| {
        Length::Terms(LengthTerms {
            percent: amount,
            ..LengthTerms::default()
        })
    };
    let words = split_words(value.trim());
    if words.is_empty() || words.len() > 2 {
        return None;
    }
    let mut horizontal = None;
    let mut vertical = None;
    let mut loose = Vec::new();
    for word in &words {
        match word.to_ascii_lowercase().as_str() {
            "left" => horizontal = Some(percent(0.0)),
            "right" => horizontal = Some(percent(100.0)),
            "top" => vertical = Some(percent(0.0)),
            "bottom" => vertical = Some(percent(100.0)),
            "center" => loose.push(percent(50.0)),
            other => loose.push(parse_length(other)?),
        }
    }
    let mut loose = loose.into_iter();
    let horizontal = horizontal.or_else(|| loose.next()).unwrap_or(percent(50.0));
    let vertical = vertical.or_else(|| loose.next()).unwrap_or(percent(50.0));
    Some((horizontal, vertical))
}

fn parse_background_repeat(value: &str) -> Option<BackgroundRepeat> {
    let repeat = |x, y| Some(BackgroundRepeat { x, y });
    match value.trim().to_ascii_lowercase().as_str() {
        "repeat" | "repeat repeat" => repeat(true, true),
        "no-repeat" | "no-repeat no-repeat" => repeat(false, false),
        "repeat-x" | "repeat no-repeat" => repeat(true, false),
        "repeat-y" | "no-repeat repeat" => repeat(false, true),
        _ => None,
    }
}

// "16 / 9", "1.5" or "auto 4 / 3"; Some(None) clears a declared ratio
fn parse_aspect_ratio(value: &str) -> Option<Option<f32>> {
    let value = value.trim().to_ascii_lowercase();
    let ratio = value.trim_start_matches("auto").trim();
    if ratio.is_empty() {
        return (value == "auto").then_some(None);
    }
    let (width, height) = match ratio.split_once('/') {
        Some((width, height)) => (
            width.trim().parse::<f32>().ok()?,
            height.trim().parse::<f32>().ok()?,
        ),
        None => (ratio.parse::<f32>().ok()?, 1.0),
    };
    (width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0)
        .then_some(Some(width / height))
}

// flex-direction keywords
fn parse_flex_direction(value: &str) -> Option<FlexDirection> {
    Some(match value.trim().to_ascii_lowercase().as_str() {
        "row" => FlexDirection::Row,
        "row-reverse" => FlexDirection::RowReverse,
        "column" => FlexDirection::Column,
        "column-reverse" => FlexDirection::ColumnReverse,
        _ => return None,
    })
}

fn parse_flex_wrap(value: &str) -> Option<FlexWrap> {
    Some(match value.trim().to_ascii_lowercase().as_str() {
        "nowrap" => FlexWrap::NoWrap,
        "wrap" => FlexWrap::Wrap,
        "wrap-reverse" => FlexWrap::WrapReverse,
        _ => return None,
    })
}

// the last word of an alignment value, which drops prefixes such as `safe` and `unsafe`
fn last_keyword(value: &str) -> String {
    value
        .split_ascii_whitespace()
        .last()
        .unwrap_or_default()
        .to_ascii_lowercase()
}

// align-items and align-self; auto and normal give None
fn parse_align(value: &str) -> Option<AlignItems> {
    Some(match last_keyword(value).as_str() {
        "flex-start" | "start" | "self-start" => AlignItems::FlexStart,
        "center" => AlignItems::Center,
        "flex-end" | "end" | "self-end" => AlignItems::FlexEnd,
        "baseline" => AlignItems::Baseline,
        "stretch" => AlignItems::Stretch,
        _ => return None,
    })
}

// the flex shorthand: none, auto, initial, or up to two numbers and a basis
fn parse_flex_shorthand(value: &str) -> Option<(f32, f32, Length)> {
    match value.trim().to_ascii_lowercase().as_str() {
        "none" => return Some((0.0, 0.0, Length::Auto)),
        "auto" => return Some((1.0, 1.0, Length::Auto)),
        "initial" => return Some((0.0, 1.0, Length::Auto)),
        _ => {}
    }
    let mut numbers = Vec::new();
    let mut basis = None;
    for word in value.split_ascii_whitespace() {
        if let Some(number) = parse_number(word) {
            numbers.push(number);
        } else if word.eq_ignore_ascii_case("content") {
            basis = Some(Length::Auto);
        } else {
            basis = Some(parse_length(word)?);
        }
    }
    let grow = numbers.first().copied();
    let shrink = numbers.get(1).copied();
    if grow.is_none() && basis.is_none() {
        return None;
    }
    // a bare flex factor sets the basis to zero, so items share all the space
    let basis = basis.unwrap_or(if grow.is_some() {
        Length::px(0.0)
    } else {
        Length::Auto
    });
    Some((
        grow.unwrap_or(1.0).max(0.0),
        shrink.unwrap_or(1.0).max(0.0),
        basis,
    ))
}

fn default_display(name: &str) -> Display {
    match name {
        "#document" | "html" | "body" | "address" | "article" | "aside" | "blockquote" | "div"
        | "dl" | "dd" | "dt" | "details" | "summary" | "fieldset" | "figcaption" | "figure"
        | "footer" | "form" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "header" | "hgroup"
        | "hr" | "li" | "legend" | "main" | "menu" | "nav" | "ol" | "p" | "pre" | "search"
        | "section" | "table" | "ul" | "center" | "dialog" => Display::Block,
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

#[cfg(test)]
mod tests {
    use super::*;

    // an element placed under its ancestors, outermost first, in a fresh document
    fn placed(element: &Element, ancestors: &[&Element]) -> (Document, NodeId) {
        let mut document = Document::new();
        let mut parent = document.root();
        for element in ancestors.iter().copied().chain(std::iter::once(element)) {
            let node = document.adopt_element(element.clone());
            document
                .append_child(parent, node)
                .expect("an element fits under its parent");
            parent = node;
        }
        (document, parent)
    }

    fn context<'a>(placed: &'a (Document, NodeId), state: &'a StyleState) -> StyleContext<'a> {
        document_context(&placed.0, placed.1, state)
    }

    fn document_context<'a>(
        document: &'a Document,
        node: NodeId,
        state: &'a StyleState,
    ) -> StyleContext<'a> {
        StyleContext {
            document,
            node,
            pseudo: None,
            parent_font_size: 16.0,
            parent_color: Color::BLACK,
            parent_weight: FontWeight::Normal,
            parent_text: InheritedText::default(),
            state,
        }
    }

    fn style_within(
        stylesheet: &Stylesheet,
        element: &Element,
        ancestors: &[&Element],
        parent_font_size: f32,
        parent_color: Color,
        parent_weight: FontWeight,
    ) -> Style {
        let placed = placed(element, ancestors);
        stylesheet.style_for(StyleContext {
            parent_font_size,
            parent_color,
            parent_weight,
            ..context(&placed, &StyleState::default())
        })
    }

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
    fn animations_sample_keyframes_against_the_clock() {
        let stylesheet = Stylesheet::parse(
            "@keyframes slide { from { width: 40px; background: #000000 } to { width: 240px; background: #ffffff } } .bar { animation: slide 1000ms linear infinite }",
        )
        .expect("stylesheet should parse");
        assert!(stylesheet.has_animations());
        let mut bar = Element::new("div");
        bar.attrs.insert("class".to_string(), "bar".to_string());
        let style_at = |clock: u64, reduced_motion: bool| {
            stylesheet.style_for(context(
                &placed(&bar, &[]),
                &StyleState {
                    clock,
                    reduced_motion,
                    ..StyleState::default()
                },
            ))
        };

        assert_eq!(style_at(0, false).width.resolve(0.0, 16.0), Some(40.0));
        assert_eq!(style_at(250, false).width.resolve(0.0, 16.0), Some(90.0));
        assert_eq!(
            style_at(500, false).background,
            Color::opaque(128, 128, 128)
        );
        // an infinite animation wraps, so the second cycle repeats the first
        assert_eq!(style_at(1250, false).width.resolve(0.0, 16.0), Some(90.0));
        // a motion preference stops the animation from applying at all
        assert_eq!(style_at(500, true).width, Length::Auto);

        let stepped = Stylesheet::parse(
            "@keyframes fade { 0% { opacity: 1 } 100% { opacity: 0 } } .bar { animation: fade 800ms steps(2, end) infinite }",
        )
        .expect("stylesheet should parse");
        let opacity_at = |clock: u64| {
            stepped
                .style_for(context(
                    &placed(&bar, &[]),
                    &StyleState {
                        clock,
                        ..StyleState::default()
                    },
                ))
                .opacity
        };
        assert_eq!(opacity_at(100), 1.0);
        assert_eq!(opacity_at(500), 0.5);

        let once = Stylesheet::parse(
            "@keyframes grow { from { width: 10px } to { width: 20px } } .bar { animation: grow 100ms linear }",
        )
        .expect("stylesheet should parse");
        let ended = once.style_for(context(
            &placed(&bar, &[]),
            &StyleState {
                clock: 5_000,
                ..StyleState::default()
            },
        ));
        assert_eq!(ended.width.resolve(0.0, 16.0), Some(20.0));
    }

    #[test]
    fn animations_and_transitions_blend_translate_transforms() {
        let stylesheet = Stylesheet::parse(
            "@keyframes slide { from { transform: translateX(0px) } to { transform: translateX(80px) } } .bar { animation: slide 1000ms linear infinite; transform: translate(0px, 0px) }",
        )
        .expect("stylesheet should parse");
        let mut bar = Element::new("div");
        bar.attrs.insert("class".to_string(), "bar".to_string());
        let translate_at = |clock: u64| {
            stylesheet
                .style_for(context(
                    &placed(&bar, &[]),
                    &StyleState {
                        clock,
                        ..StyleState::default()
                    },
                ))
                .translate
        };
        assert_eq!(translate_at(250), (Length::px(20.0), Length::px(0.0)));
        // percentages stay relative to the box and mixed units meet in calc()
        let stepped = Stylesheet::parse(
            "@keyframes drift { from { transform: translate(10%, 0px) } to { transform: translate(30%, 4em) } } .bar { animation: drift 1000ms linear infinite }",
        )
        .expect("stylesheet should parse")
        .style_for(context(&placed(&bar, &[]), &StyleState {
                clock: 500,
                ..StyleState::default()
            }));
        let (x, y) = stepped.translate;
        assert_eq!(x.resolve(200.0, 16.0), Some(40.0));
        assert_eq!(y.resolve(200.0, 16.0), Some(32.0));

        let transitioned = Stylesheet::parse(
            ".card { transform: translate(0px, 0px); transition: transform 400ms linear } .card.lift { transform: translateY(-24px) }",
        )
        .expect("stylesheet should parse");
        let mut card = Element::new("div");
        card.attrs.insert("class".to_string(), "card".to_string());
        card.attrs.insert("id".to_string(), "hero".to_string());
        // a lone element lands on the same node in every fresh document
        let card_node = placed(&card, &[]).1;
        let mut style_at = |clock: u64, lifted: bool| {
            if lifted {
                card.attrs
                    .insert("class".to_string(), "card lift".to_string());
            } else {
                card.attrs.insert("class".to_string(), "card".to_string());
            }
            transitioned.style_for(context(
                &placed(&card, &[]),
                &StyleState {
                    clock,
                    transitions: BTreeMap::from([(
                        (card_node, AnimatableProperty::Transform),
                        TransitionState {
                            from: "translate(0px, 0px)".to_string(),
                            to: "translateY(-24px)".to_string(),
                            started: 0,
                            finishes: 400,
                            delay: 0,
                            duration: 400,
                            timing: Timing::Linear,
                        },
                    )]),
                    ..StyleState::default()
                },
            ))
        };
        let (x, y) = style_at(100, true).translate;
        assert_eq!(x.resolve(100.0, 16.0), Some(0.0));
        assert_eq!(y.resolve(100.0, 16.0), Some(-6.0));
    }

    #[test]
    fn timing_functions_follow_their_curves_and_step_positions() {
        let close = |left: f32, right: f32| (left - right).abs() < 1e-3;
        assert!(
            close(Timing::EASE.apply(0.5), 0.8024),
            "{}",
            Timing::EASE.apply(0.5)
        );
        assert!(close(parse_timing("ease-in-out").unwrap().apply(0.5), 0.5));
        assert!(close(parse_timing("ease-in").unwrap().apply(0.25), 0.0935));
        assert!(close(
            parse_timing("cubic-bezier(0, 0, 1, 1)").unwrap().apply(0.3),
            0.3
        ));
        // y values outside the unit range overshoot, as back-out curves do
        assert!(
            parse_timing("cubic-bezier(0.3, 1.5, 0.6, 1)")
                .unwrap()
                .apply(0.6)
                > 1.0
        );
        assert!(parse_timing("cubic-bezier(1.5, 0, 0, 1)").is_none());
        let steps = |value: &str, progress: f32| parse_timing(value).unwrap().apply(progress);
        assert_eq!(steps("steps(4)", 0.3), 0.25);
        assert_eq!(steps("steps(4, jump-start)", 0.3), 0.5);
        assert_eq!(steps("steps(5, jump-none)", 0.3), 0.25);
        assert_eq!(steps("steps(3, jump-both)", 0.0), 0.25);
        assert_eq!(steps("step-end", 0.99), 0.0);
        assert_eq!(steps("step-start", 0.01), 1.0);
        assert!(parse_timing("steps(1, jump-none)").is_none());
    }

    #[test]
    fn transition_all_is_one_entry_that_covers_every_property() {
        let transitions = parse_transitions("all 200ms ease-out, opacity 1s 50ms steps(2)");
        let first = transitions[0].expect("all");
        assert_eq!(first.property, AnimatableProperty::All);
        assert!(
            first
                .property
                .expand()
                .any(|property| property == AnimatableProperty::Transform)
        );
        assert!(first.property.expand().count() > 30);
        let second = transitions[1].expect("opacity");
        assert_eq!(
            (second.property, second.duration, second.delay),
            (AnimatableProperty::Opacity, 1000, 50)
        );
        assert_eq!(second.timing, Timing::Steps(2, StepPosition::End));
        // a shorthand entry without a property name applies to all of them
        assert_eq!(
            parse_transitions("300ms")[0].expect("bare").property,
            AnimatableProperty::All
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
        let style = stylesheet.style_for(context(&placed(&card, &[]), &StyleState::default()));
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
            .style_for(context(&placed(&card, &[]), &StyleState::default()));
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
            style_within(
                &stylesheet,
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
        let card = document.find_element_by_id("card").expect("card exists");
        let style = stylesheet.style_for(document_context(&document, card, &state));
        assert_eq!(style.background, Color::opaque(0, 255, 0));
        assert_eq!(style.color, Color::opaque(0, 0, 255));
        assert_eq!(style.padding, Edges::all(Length::px(8.0)));
        assert_eq!(
            resolve_custom_value(
                "var(--loop, #abcdef)",
                state.custom_properties.get(&card).expect("card variables")
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
        let card = document.find_element_by_id("card").expect("card exists");
        let style = stylesheet.style_for(document_context(&document, card, &state));
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
        let style = style_within(
            &stylesheet,
            &element,
            &[],
            16.0,
            Color::BLACK,
            FontWeight::Normal,
        );
        assert_eq!(style.display, Display::Block);
        let mut element = Element::new("div");
        element.attrs.insert("class".to_string(), "row".to_string());
        let style = style_within(
            &stylesheet,
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
        let style = style_within(
            &stylesheet,
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
            style_within(&auto, &scroll, &[], 16.0, Color::BLACK, FontWeight::Normal,).overflow,
            Overflow::Auto
        );
        let sticky = Stylesheet::parse(".sticky { position: sticky; top: 4px; }")
            .expect("sticky stylesheet should parse");
        let mut sticky_element = Element::new("div");
        sticky_element
            .attrs
            .insert("class".to_string(), "sticky".to_string());
        let sticky_style = style_within(
            &sticky,
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
        let style = style_within(
            &stylesheet,
            &element,
            &[],
            16.0,
            Color::BLACK,
            FontWeight::Normal,
        );
        assert_eq!(style.display, Display::Grid);
        let columns = style.grid_template_columns.list();
        assert_eq!(columns.tracks.len(), 2);
        assert_eq!(columns.tracks[0].max, crate::grid::Breadth::Fr(1.0));
        let rows = style.grid_template_rows.list();
        assert_eq!(rows.tracks.len(), 2);
        assert_eq!(
            rows.tracks[0].max,
            crate::grid::Breadth::Length(Length::px(48.0))
        );
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
        let card_style = style_within(
            &stylesheet,
            &card,
            &[&shell],
            16.0,
            Color::BLACK,
            FontWeight::Normal,
        );
        let direct_style = style_within(
            &stylesheet,
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
        let card_style = style_within(
            &stylesheet,
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
        let muted_style = style_within(
            &stylesheet,
            &muted,
            &[],
            16.0,
            Color::BLACK,
            FontWeight::Normal,
        );
        assert_eq!(muted_style.color, Color::opaque(255, 0, 0));

        let mut panel = Element::new("section");
        panel.attrs.insert("class".to_string(), "panel".to_string());
        let panel_style = style_within(
            &stylesheet,
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
            r#"p::nonsense { color: red; } .card { color: red; } @supports (display: grid) { p { color: red } } p { color: } .last { color: blue; }"#,
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
        assert!(dropped[0].starts_with("p::nonsense"));
        assert!(dropped[1].starts_with("unsupported at-rule: @supports"));
        assert!(dropped[2].contains("invalid declaration: color:"));
        assert!(Stylesheet::parse("p::nonsense { color: red; }").is_err());
        assert!(Stylesheet::parse("p::first-line { color: red; }").is_ok());
        assert!(Stylesheet::parse("body::before { content: \"\"; }").is_ok());
    }

    #[test]
    fn matches_sibling_combinators_and_structural_pseudo_classes() {
        let stylesheet = Stylesheet::parse(
            "p + p { color: red; } h1 ~ p { background-color: green; } li:first-child { color: blue; } li:last-child { color: yellow; } li:nth-child(2n) { border-width: 3px; }",
        )
        .expect("stylesheet should parse");
        let document = Document::parse(
            "<ul><li></li><li></li><li></li></ul><section><h1></h1> <p></p><p></p></section>",
        )
        .expect("document should parse");
        let state = StyleState::default();
        let nodes = |selector: &str| {
            document
                .find_elements_by_selector(selector)
                .expect("selector parses")
        };
        let style = |node: NodeId| stylesheet.style_for(document_context(&document, node, &state));

        let paragraphs = nodes("p");
        let first_paragraph = style(paragraphs[0]);
        assert_eq!(first_paragraph.color, Color::BLACK);
        assert_eq!(first_paragraph.background, Color::opaque(0, 128, 0));
        let second_paragraph = style(paragraphs[1]);
        assert_eq!(second_paragraph.color, Color::opaque(255, 0, 0));
        assert_eq!(second_paragraph.background, Color::opaque(0, 128, 0));

        let items = nodes("li");
        assert_eq!(style(items[0]).color, Color::opaque(0, 0, 255));
        assert_eq!(style(items[2]).color, Color::opaque(255, 255, 0));
        assert_eq!(style(items[1]).border_width, 3.0);
        assert_eq!(style(items[0]).border_width, 0.0);
    }

    #[test]
    fn media_queries_read_host_color_scheme_and_motion_preferences() {
        let stylesheet = Stylesheet::parse(
            "@media (prefers-color-scheme: dark) { p { color: red; } } @media (prefers-reduced-motion: reduce) { p { background-color: green; } }",
        )
        .expect("stylesheet should parse");
        let paragraph = Element::new("p");
        let style_with =
            |state: &StyleState| stylesheet.style_for(context(&placed(&paragraph, &[]), state));

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
        let document = Document::parse(
            "<button id='hovered'></button><input id='box' type='checkbox' checked><button id='off'></button>",
        )
        .expect("document should parse");
        let id = |value: &str| document.find_element_by_id(value).expect("element exists");
        let (button, input, disabled_button) = (id("hovered"), id("box"), id("off"));
        let mut state = StyleState {
            hovered: Some(button),
            ..StyleState::default()
        };
        state.disabled.insert(disabled_button);
        let style = |node: NodeId, state: &StyleState| {
            stylesheet.style_for(document_context(&document, node, state))
        };
        assert_eq!(style(button, &state).color, Color::opaque(255, 0, 0));
        assert_eq!(style(input, &state).color, Color::opaque(0, 0, 255));
        assert_eq!(
            style(disabled_button, &state).color,
            Color::opaque(255, 255, 0)
        );
        assert_eq!(style(button, &state).background, Color::opaque(0, 128, 0));
        state.unchecked.insert(input);
        assert_eq!(style(input, &state).color, Color::BLACK);
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
        let style = style_within(
            &stylesheet,
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
