use crate::bidi::{BidiClass, bidi_class, mirrored, resolve_levels, visual_order};
use crate::border::{BorderSide, BorderStyle, CornerLengths};
use crate::css::{
    AlignContent, AlignItems, Color, Display, Edges, FontWeight, InheritedText, JustifyContent,
    Length, LengthTerms, Overflow, Position, PseudoElement, Style, StyleContext, StyleState,
    Stylesheet, TextAlign, inherited_text,
};
use crate::css::{
    Clear, ContentSize, Cursor, Float, ListStyleType, OverflowWrap, ScrollbarWidth, SpaceCollapse,
    VerticalAlign, Visibility,
};
use crate::dom::{Document, Element, Namespace, NodeId, NodeKind};
use crate::font::{FontStack, FontStyle};
use crate::grid::{Breadth, GridLine, GridPlacement, TrackList, TrackSize, Tracks};
use crate::image::Image;
use crate::linebreak::{WordBreak, break_opportunities};
use crate::media::ContainerSize;
use crate::range::RangeBounds;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    pub fn bottom(self) -> f32 {
        self.y + self.height
    }

    pub fn right(self) -> f32 {
        self.x + self.width
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextRun {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub font_size: f32,
    pub letter_spacing: f32,
    pub font_stack: FontStack,
    pub color: Color,
    pub font: FontStyle,
    // drawn right to left, with its glyphs shaped in logical order
    pub rtl: bool,
    pub(crate) decoration: crate::css::TextDecoration,
    // DOM text node that produced this run, so prose can be selected by pointer;
    // node_offset is where the word starts, past any leading space in text
    pub node: Option<NodeId>,
    pub node_offset: usize,
    // UTF-16 length of the source word, which text-transform may change in text
    pub source_len: usize,
}

impl TextRun {
    fn prefix_units(&self) -> usize {
        usize::from(self.text.starts_with(' '))
    }

    // the node offset that a UTF-16 index inside this run's text stands for
    pub(crate) fn node_offset_at(&self, index: usize) -> usize {
        self.node_offset
            + index
                .saturating_sub(self.prefix_units())
                .min(self.source_len)
    }

    // the run text indices covered by a node-local selection span; the leading
    // space joins the selection when the span starts before the word
    pub(crate) fn selected_units(
        &self,
        start: usize,
        end: usize,
        continued: bool,
    ) -> Option<(usize, usize)> {
        let prefix = self.prefix_units();
        let length = self.text.encode_utf16().count();
        // the end of the source word maps to the end of the drawn text, which
        // text-transform may have lengthened
        let index = |offset: usize| {
            if offset < self.node_offset {
                0
            } else if offset - self.node_offset >= self.source_len {
                length
            } else {
                (prefix + offset - self.node_offset).min(length)
            }
        };
        let from = if continued { 0 } else { index(start) };
        let to = index(end);
        (from < to).then_some((from, to))
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedShadow {
    pub offset_x: f32,
    pub offset_y: f32,
    pub blur: f32,
    pub spread: f32,
    pub color: Color,
    pub inset: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LayoutBox {
    // the element that produced the box; generated boxes report their owner
    pub node: NodeId,
    pub tag: String,
    pub rect: Rect,
    pub background: Color,
    // first layer on top
    pub(crate) backgrounds: Vec<crate::css::BackgroundLayer>,
    // the mask-image layers, whose alpha scales everything the box and its subtree paint
    pub(crate) masks: Vec<crate::css::BackgroundLayer>,
    pub(crate) object_fit: crate::css::ObjectFit,
    // in the top layer, as a modal dialog and its backdrop are
    pub(crate) top_layer: bool,
    // the CSS transform about its origin, in coordinates relative to the border box corner
    pub(crate) transform: Option<crate::transform::Matrix>,
    pub(crate) backface_hidden: bool,
    // relative to the border box corner
    pub(crate) clip_path: Option<crate::clip_path::ClipRegion>,
    pub(crate) radii: CornerRadii,
    pub opacity: f32,
    // first on top, as declared
    pub shadows: Vec<ResolvedShadow>,
    pub(crate) filters: crate::filter::Filters,
    pub(crate) border: BoxBorder,
    pub(crate) outline: Option<BoxOutline>,
    pub text: Vec<TextRun>,
    pub(crate) control_value: Option<String>,
    pub(crate) control_selection: Option<(usize, usize, bool)>,
    pub(crate) control_focused: bool,
    pub(crate) control_text_origin: Option<(f32, f32)>,
    pub children: Vec<LayoutBox>,
    pub(crate) position: Position,
    pub(crate) visual_offset: (f32, f32),
    pub(crate) z_index: i32,
    pub(crate) creates_stacking_context: bool,
    pub(crate) sticky_inset: Edges,
    pub(crate) font_size: f32,
    pub(crate) overflow: Overflow,
    // the x and y axes, after a scrolling axis has settled the other
    pub(crate) overflow_axes: [Overflow; 2],
    pub(crate) visible: bool,
    // pointer-events and user-select, both inherited, so a descendant can opt back in
    pub(crate) pointer_events: bool,
    pub(crate) selectable: bool,
    pub(crate) cursor: Cursor,
    // floats paint after their in-flow siblings
    pub(crate) floated: bool,
    // one line's part of an inline element, painted under the text of the block it is in
    pub(crate) inline: bool,
    pub(crate) control: Option<ControlPaint>,
    // the ::selection background behind this box's selected text
    pub(crate) selection: Option<Color>,
    // the ::selection text color, when it differs from the box's own
    pub(crate) selection_color: Option<Color>,
    pub(crate) scroll_content: Size,
    pub(crate) scroll_offset: (f32, f32),
    // the shapes an outer svg element draws in its content box
    pub(crate) svg: Option<Rc<crate::svg::Scene>>,
    pub(crate) scrollbar: Scrollbar,
    pub(crate) margin: ResolvedEdges,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LayoutDocument {
    pub viewport: Size,
    pub root: LayoutBox,
    pub(crate) images: Vec<ImageLayer>,
    // prose ranges to highlight, keyed by the DOM text node that owns the run
    pub(crate) prose_selections: Vec<ProseSpan>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ImageLayer {
    pub(crate) node: NodeId,
    pub(crate) image: std::sync::Arc<Image>,
    // the CSS background url this decodes, rather than the element's own content
    pub(crate) background: Option<crate::image::ImageSource>,
}

// corner radii as horizontal and vertical extents, from the top-left corner clockwise
pub(crate) type CornerRadii = [(f32, f32); 4];

pub(crate) const SQUARE_CORNERS: CornerRadii = [(0.0, 0.0); 4];

// the parts of a form control paint draws over its box
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ControlPaint {
    Checkbox {
        checked: bool,
        accent: Color,
    },
    Radio {
        checked: bool,
        accent: Color,
    },
    // the arrow at the end of a closed select
    Select {
        color: Color,
    },
    Range {
        fraction: f32,
        accent: Color,
        disabled: bool,
        // the page draws the track and thumb as boxes, under appearance: none
        styled: bool,
        // where the thumb's left edge starts from the box's left edge, and how far it travels
        travel: (f32, f32),
        thumb: f32,
    },
    // none is indeterminate
    Progress {
        fraction: Option<f32>,
        accent: Color,
    },
    Meter {
        fraction: f32,
        color: Color,
    },
    FileButton {
        width: f32,
    },
}

// a border as painted: widths, and each side's style and color from the top clockwise
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BoxBorder {
    pub(crate) widths: ResolvedEdges,
    pub(crate) styles: [BorderStyle; 4],
    pub(crate) colors: [Color; 4],
}

impl BoxBorder {
    pub(crate) const NONE: Self = Self {
        widths: ResolvedEdges {
            top: 0.0,
            right: 0.0,
            bottom: 0.0,
            left: 0.0,
        },
        styles: [BorderStyle::None; 4],
        colors: [Color::transparent(); 4],
    };

    pub(crate) fn of(style: &Style) -> Self {
        Self {
            widths: used_border(style),
            styles: style.border.map(|side| side.style),
            colors: style.border.map(|side| side.color.unwrap_or(style.color)),
        }
    }

    pub(crate) fn is_none(&self) -> bool {
        self.widths.top <= 0.0
            && self.widths.right <= 0.0
            && self.widths.bottom <= 0.0
            && self.widths.left <= 0.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BoxOutline {
    pub(crate) width: f32,
    pub(crate) style: BorderStyle,
    pub(crate) color: Color,
    pub(crate) offset: f32,
}

fn box_outline(style: &Style) -> Option<BoxOutline> {
    let side = style.outline.side;
    (side.used_width() > 0.0).then(|| BoxOutline {
        width: side.used_width(),
        style: side.style,
        color: side.color.unwrap_or(style.color),
        offset: style.outline.offset,
    })
}

pub(crate) fn used_border(style: &Style) -> ResolvedEdges {
    let [top, right, bottom, left] = style.border.map(|side| side.used_width());
    ResolvedEdges {
        top,
        right,
        bottom,
        left,
    }
}

// percentages resolve against the border box, and radii that would overlap shrink together
pub(crate) fn resolve_corner_radii(
    radii: CornerLengths,
    width: f32,
    height: f32,
    font_size: f32,
) -> CornerRadii {
    let mut resolved = radii.map(|(horizontal, vertical)| {
        (
            horizontal.resolve(width, font_size).unwrap_or(0.0).max(0.0),
            vertical.resolve(height, font_size).unwrap_or(0.0).max(0.0),
        )
    });
    let [top_left, top_right, bottom_right, bottom_left] = resolved;
    let scale = [
        (width, top_left.0 + top_right.0),
        (width, bottom_left.0 + bottom_right.0),
        (height, top_left.1 + bottom_left.1),
        (height, top_right.1 + bottom_right.1),
    ]
    .into_iter()
    .filter(|(_, sum)| *sum > 0.0)
    .map(|(side, sum)| side.max(0.0) / sum)
    .fold(1.0_f32, f32::min);
    if scale < 1.0 {
        for corner in &mut resolved {
            *corner = (corner.0 * scale, corner.1 * scale);
        }
    }
    resolved
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ResolvedEdges {
    pub(crate) top: f32,
    pub(crate) right: f32,
    pub(crate) bottom: f32,
    pub(crate) left: f32,
}

struct InlineLine {
    y: f32,
    used: f32,
    height: f32,
    // how far floats reach into this line from the left and right of the content box
    left: f32,
    right: f32,
    // the block's line height, the band a fresh line checks floats against
    band: f32,
    // the block's own font above and below the baseline, which every line holds
    strut: (f32, f32),
    floats: Option<Rc<FloatArea>>,
    // inline elements seen in this block, and those still open at the start of the line
    decorations: Vec<InlineDecoration>,
    open_inlines: Vec<usize>,
    // whether the text appended so far ended on whitespace, which decides word joining
    trailing_space: bool,
    // the paragraph runs right to left
    rtl: bool,
    // text-overflow: ellipsis on a block that clips its overflow
    ellipsis: bool,
    fragments: Vec<InlineFragment>,
    // inline-block and replaced boxes already placed by a flushed line
    placed: Vec<LayoutBox>,
}

// an inline element, whose boxes come from the lines it lands on
struct InlineDecoration {
    node: NodeId,
    tag: String,
    style: Style,
    font_size: f32,
    // how far its baseline sits above the line's
    raise: f32,
    padding: ResolvedEdges,
    border: ResolvedEdges,
    margin: ResolvedEdges,
    opacity: f32,
}

impl InlineLine {
    fn base_level(&self) -> u8 {
        u8::from(self.rtl)
    }

    fn has_content(&self) -> bool {
        self.fragments
            .iter()
            .any(|fragment| fragment.edge.is_none())
    }

    // the text just before any element edges, which owns a collapsed space that follows it
    fn text_before_edges(&mut self) -> Option<&mut InlineFragment> {
        self.fragments
            .iter_mut()
            .rev()
            .find(|fragment| fragment.edge.is_none())
            .filter(|fragment| fragment.atomic.is_none())
    }

    // the start or end of an inline element: a fragment as wide as its margin, border and
    // padding on that side
    fn push_edge(&mut self, decoration: usize, opening: bool, width: f32) {
        self.used += width;
        self.fragments.push(InlineFragment {
            text: String::new(),
            width,
            font_size: 0.0,
            letter_spacing: 0.0,
            font_stack: FontStack::default(),
            color: Color::transparent(),
            font: FontStyle::NORMAL,
            node: None,
            node_offset: 0,
            source_len: 0,
            rtl: false,
            decoration: Default::default(),
            line_height: 0.0,
            atomic: None,
            edge: Some((decoration, opening)),
            level: self.base_level(),
            leading: 0.0,
            baseline: 0.0,
            shift: LineShift::Raise(0.0),
        });
    }

    fn available(&self, content_width: f32) -> f32 {
        (content_width - self.left - self.right).max(0.0)
    }

    // a line measured piece by piece lands a rounding error past the width the same text
    // measured whole, so overflow starts one layout unit (1/64 device px) beyond the line
    fn overflows(&self, width: f32, content_width: f32) -> bool {
        width > self.available(content_width) + 1.0 / 64.0 / crate::font::device_scale()
    }

    // a fresh line narrows to the floats beside it
    fn fit_to_floats(&mut self, content_x: f32, content_width: f32) {
        if let Some(floats) = &self.floats {
            (self.left, self.right) = floats.insets(self.y, self.band, content_x, content_width);
        }
    }

    // moves an empty line down past floats until something this wide fits beside them
    fn make_room(&mut self, width: f32, content_x: f32, content_width: f32) {
        while !self.has_content()
            && self.overflows(width, content_width)
            && (self.left > 0.0 || self.right > 0.0)
        {
            let Some(bottom) = self
                .floats
                .as_ref()
                .and_then(|floats| floats.next_bottom(self.y, self.band))
            else {
                return;
            };
            self.y = bottom;
            self.fit_to_floats(content_x, content_width);
        }
    }
}

// the floats of one block formatting context, as margin boxes in layout coordinates
#[derive(Default)]
struct FloatArea {
    placed: std::cell::RefCell<Vec<(Rect, Float)>>,
}

impl FloatArea {
    fn crossing(&self, top: f32, height: f32) -> Vec<(Rect, Float)> {
        let bottom = top + height.max(1.0);
        self.placed
            .borrow()
            .iter()
            .filter(|(rect, _)| rect.y < bottom && rect.bottom() > top)
            .copied()
            .collect()
    }

    // how far floats reach into a band of a content box from its left and right
    fn insets(&self, top: f32, height: f32, content_x: f32, content_width: f32) -> (f32, f32) {
        self.crossing(top, height).into_iter().fold(
            (0.0_f32, 0.0_f32),
            |(left, right), (rect, side)| {
                if side == Float::Left {
                    (left.max(rect.x + rect.width - content_x), right)
                } else {
                    (left, right.max(content_x + content_width - rect.x))
                }
            },
        )
    }

    // the nearest float bottom below a band's top, where the band can widen
    fn next_bottom(&self, top: f32, height: f32) -> Option<f32> {
        self.crossing(top, height)
            .into_iter()
            .map(|(rect, _)| rect.bottom())
            .filter(|bottom| *bottom > top)
            .min_by(f32::total_cmp)
    }

    // the top a box must move to so that it sits below the floats it clears
    fn clearance(&self, clear: Clear) -> Option<f32> {
        self.placed
            .borrow()
            .iter()
            .filter(|(_, side)| match clear {
                Clear::None => false,
                Clear::Left => *side == Float::Left,
                Clear::Right => *side == Float::Right,
                Clear::Both => true,
            })
            .map(|(rect, _)| rect.bottom())
            .max_by(f32::total_cmp)
    }

    fn bottom(&self) -> Option<f32> {
        self.clearance(Clear::Both)
    }

    // a float may not sit higher than one placed before it
    fn lowest_top(&self) -> Option<f32> {
        self.placed
            .borrow()
            .iter()
            .map(|(rect, _)| rect.y)
            .max_by(f32::total_cmp)
    }
}

// boxes that keep their floats to themselves and stay clear of outside floats
fn establishes_formatting_context(style: &Style) -> bool {
    style.float != Float::None
        || style.position.is_out_of_flow()
        || style.overflow.clips()
        || !matches!(
            style.display,
            Display::Block | Display::Inline | Display::ListItem
        )
}

struct InlineFragment {
    text: String,
    width: f32,
    font_size: f32,
    letter_spacing: f32,
    font_stack: FontStack,
    color: Color,
    font: FontStyle,
    node: Option<NodeId>,
    node_offset: usize,
    source_len: usize,
    rtl: bool,
    decoration: crate::css::TextDecoration,
    line_height: f32,
    // an inline-block or replaced element that sits on the line like a word
    atomic: Option<Box<LayoutBox>>,
    // where an inline element opens or closes
    edge: Option<(usize, bool)>,
    // the bidi embedding level; odd levels run right to left
    level: u8,
    // half the leading, between the top of the line and the glyph box
    leading: f32,
    // how far below its top the baseline sits, and where the line puts it
    baseline: f32,
    shift: LineShift,
}

// raised above the line's baseline, or pinned to the line's top or bottom
#[derive(Clone, Copy)]
enum LineShift {
    Raise(f32),
    Top,
    Bottom,
}

#[derive(Clone, Copy)]
struct ParentStyle {
    font_size: f32,
    opacity: f32,
    color: Color,
    weight: FontWeight,
    text: InheritedText,
}

// an element to lay out, or the generated ::before or ::after box of one with its content
#[derive(Clone, Copy)]
struct Subject<'a> {
    document: &'a Document,
    node: NodeId,
    generated: Option<(PseudoElement, &'a str)>,
}

enum Child<'a> {
    Text(Option<NodeId>, &'a str),
    Element(Subject<'a>),
}

impl<'a> Subject<'a> {
    fn new(document: &'a Document, node: NodeId) -> Self {
        Self {
            document,
            node,
            generated: None,
        }
    }

    fn element(self) -> Option<&'a Element> {
        if self.generated.is_some() {
            None
        } else {
            self.document.element(self.node)
        }
    }

    fn name(self) -> &'a str {
        match self.generated {
            Some((pseudo, _)) => pseudo.tag(),
            None => self.element().map_or("", |element| element.name.as_str()),
        }
    }

    // an outer svg element is replaced content, drawn from its subtree instead of laid out
    fn svg_root(self) -> Option<&'a Element> {
        self.element()
            .filter(|element| element.namespace == Namespace::Svg && element.name == "svg")
    }

    fn replaced(self) -> bool {
        matches!(self.name(), "img" | "canvas" | "video") || self.svg_root().is_some()
    }

    // the node whose own state, such as a control value or focus, belongs to the box
    fn own(self) -> Option<NodeId> {
        self.generated.is_none().then_some(self.node)
    }

    fn style_context(self, parent: ParentStyle, state: &'a StyleState) -> StyleContext<'a> {
        StyleContext {
            document: self.document,
            node: self.node,
            pseudo: self.generated.map(|(pseudo, _)| pseudo),
            parent_font_size: parent.font_size,
            parent_color: parent.color,
            parent_weight: parent.weight,
            parent_text: parent.text,
            state,
        }
    }

    // css generated content, which exists when the cascade gives it a content string
    fn generated_content(self, stylesheet: &Stylesheet, state: &StyleState) -> [Option<String>; 2] {
        if self.generated.is_some() {
            return [None, None];
        }
        [PseudoElement::Before, PseudoElement::After].map(|pseudo| {
            stylesheet.generated_content(StyleContext {
                document: self.document,
                node: self.node,
                pseudo: Some(pseudo),
                parent_font_size: 16.0,
                parent_color: Color::BLACK,
                parent_weight: FontWeight::NORMAL,
                parent_text: InheritedText::default(),
                state,
            })
        })
    }

    // text and element children in order, with generated boxes first and last
    fn children(self, generated: &'a [Option<String>; 2]) -> Vec<Child<'a>> {
        let mut items = Vec::new();
        if let Some((_, content)) = self.generated {
            if !content.is_empty() {
                items.push(Child::Text(None, content));
            }
            return items;
        }
        // canvas and video children are fallback content for agents that cannot show them
        if self.svg_root().is_some() || matches!(self.name(), "canvas" | "video") {
            return items;
        }
        let generated_box = |pseudo: PseudoElement, content: &'a Option<String>| {
            content.as_deref().map(|content| {
                Child::Element(Subject {
                    document: self.document,
                    node: self.node,
                    generated: Some((pseudo, content)),
                })
            })
        };
        items.extend(generated_box(PseudoElement::Before, &generated[0]));
        for &child in self.document.flat_children(self.node).iter() {
            match self.document.kind(child) {
                Some(NodeKind::Text(text)) => items.push(Child::Text(Some(child), text)),
                Some(NodeKind::Element(_))
                    if !TOP_LAYER.with(|layer| layer.borrow().contains(&child)) =>
                {
                    items.push(Child::Element(Subject::new(self.document, child)))
                }
                _ => {}
            }
        }
        items.extend(generated_box(PseudoElement::After, &generated[1]));
        items
    }
}

const NO_GENERATED_CONTENT: [Option<String>; 2] = [None, None];

fn resolve_shadows(
    shadows: Option<crate::css::ShadowList>,
    containing_width: f32,
    font_size: f32,
) -> Vec<ResolvedShadow> {
    let Some(shadows) = shadows else {
        return Vec::new();
    };
    let resolve = |length: crate::css::Length| {
        length
            .resolve(containing_width, font_size)
            .unwrap_or(0.0)
            .clamp(-2048.0, 2048.0)
    };
    shadows
        .shadows()
        .iter()
        .filter(|shadow| shadow.color.alpha > 0)
        .map(|shadow| ResolvedShadow {
            offset_x: resolve(shadow.offset_x),
            offset_y: resolve(shadow.offset_y),
            blur: resolve(shadow.blur).max(0.0),
            spread: resolve(shadow.spread),
            color: shadow.color,
            inset: shadow.inset,
        })
        .collect()
}

// a hidden box's own text paints nothing, while its visible descendants still show
fn text_color(style: &Style) -> Color {
    if style.visibility == Visibility::Visible {
        style.color
    } else {
        Color::transparent()
    }
}

#[derive(Clone, Copy)]
struct InlineContext {
    font_size: f32,
    line_height: f32,
    opacity: f32,
    color: Color,
    weight: FontWeight,
    text: InheritedText,
    content_width: f32,
    content_x: f32,
    align: TextAlign,
    // how far this text's baseline sits above the line's
    raise: f32,
}

impl InlineContext {
    fn font(&self) -> FontStyle {
        FontStyle {
            weight: self.weight.0,
            italic: self.text.italic,
            variations: self.text.font_variations,
            optical_size: optical_size(self.text.optical_sizing, self.font_size),
            features: self.text.font_features,
        }
    }
}

// which DOM text node the next appended words belong to, for prose selection
#[derive(Clone)]
struct InlineSource {
    node: Option<NodeId>,
    offset: usize,
}

struct InlineElementInput<'a> {
    node: Subject<'a>,
    parent: ParentStyle,
    context: InlineContext,
    stylesheet: &'a Stylesheet,
    style_state: &'a StyleState,
    viewport: Size,
    containing_block: ContainingBlock,
}

struct FlexLayoutInput<'a> {
    // the container's own content height when it is definite
    content_height: Option<f32>,
    // min-height and max-height of the content box, which bound an auto height
    height_bounds: (f32, f32),
    parent_opacity: f32,
    node: Subject<'a>,
    style: Style,
    stylesheet: &'a Stylesheet,
    style_state: &'a StyleState,
    margin: ResolvedEdges,
    padding: ResolvedEdges,
    border: ResolvedEdges,
    rect: Rect,
    content_x: f32,
    content_y: f32,
    content_width: f32,
    font_size: f32,
    viewport: Size,
    containing_block: ContainingBlock,
    inherited_containing_block: ContainingBlock,
    mode: LayoutMode,
}

struct GridItem<'a> {
    source: FlexSource<'a>,
    style: Style,
    margin: ResolvedEdges,
    padding: ResolvedEdges,
    border: ResolvedEdges,
    // the item's tracks as (start, span), columns then rows
    area: [(usize, usize); 2],
    // a content-box width its alignment decided
    width: Option<f32>,
    layout: Option<LayoutBox>,
}

struct GridLayoutInput<'a> {
    content_height: Option<f32>,
    height_bounds: (f32, f32),
    parent_opacity: f32,
    node: Subject<'a>,
    style: Style,
    stylesheet: &'a Stylesheet,
    style_state: &'a StyleState,
    margin: ResolvedEdges,
    padding: ResolvedEdges,
    border: ResolvedEdges,
    rect: Rect,
    content_x: f32,
    content_y: f32,
    content_width: f32,
    font_size: f32,
    viewport: Size,
    containing_block: ContainingBlock,
    inherited_containing_block: ContainingBlock,
    mode: LayoutMode,
}

#[derive(Clone)]
struct ElementLayoutInput<'node, 'style> {
    node: Subject<'node>,
    containing_x: f32,
    containing_y: f32,
    containing_width: f32,
    // the containing block's height when it is definite, for percentage heights
    containing_height: Option<f32>,
    // a content-box width and height a flex container has already decided
    size_override: (Option<f32>, Option<f32>),
    // the floats of the formatting context a block child shares with its parent
    floats: Option<Rc<FloatArea>>,
    // the number a list item's marker shows
    list_ordinal: i32,
    parent: ParentStyle,
    context: LayoutContext<'style>,
}

pub fn layout(document: &Document, stylesheet: &Stylesheet, viewport: Size) -> LayoutDocument {
    layout_with_state(document, stylesheet, viewport, &StyleState::default())
}

#[derive(Clone, Copy)]
struct ContainingBlock {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

// absolutely positioned descendants place and size themselves against the padding box
fn padding_block(layout: &LayoutBox) -> ContainingBlock {
    let border = layout.border.widths;
    ContainingBlock {
        x: layout.rect.x + border.left,
        y: layout.rect.y + border.top,
        width: (layout.rect.width - border.left - border.right).max(0.0),
        height: (layout.rect.height - border.top - border.bottom).max(0.0),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LayoutMode {
    Flow,
    Absolute,
    Fixed,
}

#[derive(Clone, Copy)]
struct LayoutContext<'a> {
    viewport: Size,
    containing_block: ContainingBlock,
    mode: LayoutMode,
    stylesheet: &'a Stylesheet,
    style_state: &'a StyleState,
}

#[derive(Clone, Copy)]
struct PositionedLayoutContext<'a> {
    containing_block: ContainingBlock,
    viewport: ContainingBlock,
    viewport_size: Size,
    parent: ParentStyle,
    stylesheet: &'a Stylesheet,
    style_state: &'a StyleState,
}

thread_local! {
    // query containers sized by the most recent layout on this thread
    static CONTAINERS: std::cell::RefCell<std::collections::BTreeMap<NodeId, ContainerSize>> =
        std::cell::RefCell::default();
    // the top layer of the layout in progress, whose elements leave their place in the tree
    static TOP_LAYER: std::cell::RefCell<Vec<NodeId>> = std::cell::RefCell::default();
}

// the query containers the last layout sized, for container queries in the next style pass
pub(crate) fn take_layout_containers() -> std::collections::BTreeMap<NodeId, ContainerSize> {
    CONTAINERS.with(|containers| std::mem::take(&mut *containers.borrow_mut()))
}

pub fn layout_with_state(
    document: &Document,
    stylesheet: &Stylesheet,
    viewport: Size,
    style_state: &StyleState,
) -> LayoutDocument {
    let _profile = crate::profile::scope("layout");
    CONTAINERS.with(|containers| containers.borrow_mut().clear());
    TOP_LAYER.with(|layer| layer.borrow_mut().clone_from(&style_state.top_layer));
    let _viewport_environment = crate::css::use_layout_environment(crate::css::LayoutEnvironment {
        viewport,
        root_font_size: 16.0,
    });
    let mut computed_state = style_state.clone();
    // a caller may hand over a state whose custom properties it resolved already
    if computed_state.root.is_none() {
        stylesheet.populate_custom_properties(document, &mut computed_state);
    }
    let style_state = &computed_state;
    let _root_environment = crate::css::use_layout_environment(crate::css::LayoutEnvironment {
        viewport,
        root_font_size: root_font_size(document, stylesheet, viewport, style_state),
    });
    let _styles = crate::css::memoize_styles();
    let _answers = keep_answers(document, stylesheet, style_state);
    let mut root = LayoutBox {
        node: document.root(),
        tag: "#document".to_string(),
        rect: Rect {
            x: 0.0,
            y: 0.0,
            width: viewport.width,
            height: viewport.height,
        },
        background: Color::transparent(),
        backgrounds: Vec::new(),
        masks: Vec::new(),
        object_fit: crate::css::ObjectFit::Fill,
        top_layer: false,
        transform: None,
        backface_hidden: false,
        clip_path: None,
        radii: SQUARE_CORNERS,
        opacity: 1.0,
        shadows: Vec::new(),
        filters: crate::filter::Filters::default(),
        border: BoxBorder::NONE,
        outline: None,
        text: Vec::new(),
        control_value: None,
        control_selection: None,
        control_focused: false,
        control_text_origin: None,
        children: Vec::new(),
        position: Position::Static,
        visual_offset: (0.0, 0.0),
        z_index: 0,
        creates_stacking_context: true,
        sticky_inset: Edges::auto(),
        font_size: 16.0,
        overflow: Overflow::Visible,
        overflow_axes: [Overflow::Visible; 2],
        visible: true,
        pointer_events: true,
        selectable: true,
        cursor: Cursor::Auto,
        floated: false,
        inline: false,
        control: None,
        selection: None,
        selection_color: None,
        scroll_content: Size {
            width: viewport.width,
            height: viewport.height,
        },
        scroll_offset: (0.0, 0.0),
        svg: None,
        scrollbar: Scrollbar::default(),
        margin: ResolvedEdges::default(),
    };
    // the root element's scrollbar styles draw the viewport's bars
    if let Some(html) = document.document_element() {
        let font_size = root_font_size(document, stylesheet, viewport, style_state);
        let style = stylesheet.style_for(Subject::new(document, html).style_context(
            ParentStyle {
                font_size,
                color: Color::BLACK,
                weight: FontWeight::NORMAL,
                text: InheritedText::default(),
                opacity: 1.0,
            },
            style_state,
        ));
        // the viewport scrolls whatever the root's own overflow
        let style = Style {
            overflow: Overflow::Auto,
            ..style
        };
        root.scrollbar =
            scrollbar_for(stylesheet, (document, html), &style, font_size, style_state);
    }
    root.children = layout_document_children(document, stylesheet, viewport, style_state);
    finalize_scroll(&mut root, ResolvedEdges::default(), style_state);
    // a classic vertical scrollbar takes its width from the initial and fixed containing
    // blocks, while vw, resolved from the layout environment, keeps the whole viewport
    if let Some(bar) = scrollbar_metrics(&root).vertical_track {
        let scrollport = Size {
            width: (viewport.width - bar.width).max(0.0),
            ..viewport
        };
        CONTAINERS.with(|containers| containers.borrow_mut().clear());
        root.children = layout_document_children(document, stylesheet, scrollport, style_state);
        finalize_scroll(&mut root, ResolvedEdges::default(), style_state);
    }
    LayoutDocument {
        viewport,
        root,
        images: Vec::new(),
        prose_selections: Vec::new(),
    }
}

fn layout_document_children(
    document: &Document,
    stylesheet: &Stylesheet,
    viewport: Size,
    style_state: &StyleState,
) -> Vec<LayoutBox> {
    let viewport_block = viewport_block(viewport);
    let mut children = Vec::new();
    let mut cursor_y = 0.0_f32;
    let mut positioned = Vec::new();
    let root_parent = ParentStyle {
        font_size: 16.0,
        color: Color::BLACK,
        weight: FontWeight::NORMAL,
        text: InheritedText::default(),
        opacity: 1.0,
    };
    // the document is one formatting context, shared by elements outside any html element
    let root_floats = Rc::new(FloatArea::default());
    for node in document.element_children(document.root()) {
        let node = Subject::new(document, node);
        let style = stylesheet.style_for(node.style_context(root_parent, style_state));
        if style.position.is_out_of_flow() {
            positioned.push(node);
            continue;
        }
        if let Some(clear_to) = root_floats.clearance(style.clear) {
            cursor_y = cursor_y.max(clear_to);
        }
        if style.float != Float::None {
            let mut line = InlineLine {
                y: cursor_y,
                used: 0.0,
                height: 0.0,
                left: 0.0,
                right: 0.0,
                band: 1.0,
                strut: (0.0, 0.0),
                floats: Some(root_floats.clone()),
                decorations: Vec::new(),
                open_inlines: Vec::new(),
                trailing_space: false,
                rtl: false,
                ellipsis: false,
                fragments: Vec::new(),
                placed: Vec::new(),
            };
            place_float(
                InlineElementInput {
                    node,
                    parent: root_parent,
                    context: InlineContext {
                        font_size: 16.0,
                        line_height: 20.0,
                        color: Color::BLACK,
                        weight: FontWeight::NORMAL,
                        text: InheritedText::default(),
                        opacity: 1.0,
                        content_width: viewport.width,
                        content_x: 0.0,
                        align: TextAlign::Start,
                        raise: 0.0,
                    },
                    stylesheet,
                    style_state,
                    viewport,
                    containing_block: viewport_block,
                },
                &style,
                &mut line,
            );
            children.append(&mut line.placed);
            continue;
        }
        if let Some(child_box) = layout_element(ElementLayoutInput {
            node,
            containing_x: 0.0,
            containing_y: cursor_y,
            containing_width: viewport.width,
            containing_height: Some(viewport.height),
            size_override: (None, None),
            floats: Some(root_floats.clone()),
            list_ordinal: 1,
            parent: root_parent,
            context: LayoutContext {
                viewport,
                containing_block: viewport_block,
                mode: LayoutMode::Flow,
                stylesheet,
                style_state,
            },
        }) {
            cursor_y = child_box.rect.bottom() + child_box.margin.bottom;
            children.push(child_box);
        }
    }
    let positioned_context = PositionedLayoutContext {
        containing_block: viewport_block,
        viewport: viewport_block,
        viewport_size: viewport,
        parent: root_parent,
        stylesheet,
        style_state,
    };
    append_positioned_nodes(&mut children, positioned, positioned_context);
    // the top layer paints over everything, each element over its backdrop, inheriting from
    // where it sits in the tree but not its ancestors' opacity
    let inherited = |node: Option<NodeId>| {
        node.and_then(|node| stylesheet.computed_style(document, node, style_state))
            .map_or(root_parent, |(style, font_size)| ParentStyle {
                font_size,
                color: style.color,
                weight: style.font_weight,
                text: inherited_text(&style),
                opacity: 1.0,
            })
    };
    for &node in &style_state.top_layer {
        let backdrop = Subject {
            document,
            node,
            generated: Some((PseudoElement::Backdrop, "")),
        };
        let mut layer = Vec::new();
        for (subject, parent) in [
            (backdrop, inherited(Some(node))),
            (
                Subject::new(document, node),
                inherited(document.parent(node)),
            ),
        ] {
            let context = PositionedLayoutContext {
                parent,
                ..positioned_context
            };
            append_positioned_nodes(&mut layer, vec![subject], context);
        }
        for layout in &mut layer {
            layout.z_index = i32::MAX;
            layout.top_layer = true;
            layout.creates_stacking_context = true;
        }
        children.append(&mut layer);
    }
    children
}

impl LayoutDocument {
    // the body's box, where a page's own content starts
    #[cfg(test)]
    pub(crate) fn body(&self) -> &LayoutBox {
        let mut stack = vec![&self.root];
        while let Some(layout_box) = stack.pop() {
            if layout_box.tag == "body" {
                return layout_box;
            }
            stack.extend(layout_box.children.iter().rev());
        }
        &self.root
    }

    // maps a point inside a text control to a UTF-16 offset in its value
    pub fn control_text_offset_at(&self, x: f32, y: f32) -> Option<(NodeId, usize)> {
        let mut found = None;
        collect_control_at(&self.root, x, y, (0.0, 0.0), &mut found);
        let (node, runs, origin, (x, y)) = found?;
        let mut offset = 0;
        let mut line = None;
        for run in &runs {
            let top = run.y + origin.1;
            let bottom = top + run.font_size.max(1.0);
            if y >= top && y < bottom {
                line = Some((offset, run.clone()));
                break;
            }
            offset += run.text.encode_utf16().count();
        }
        let (base, run) = line.or_else(|| {
            let last = runs.last()?;
            let base = runs[..runs.len() - 1]
                .iter()
                .map(|run| run.text.encode_utf16().count())
                .sum();
            Some((base, last.clone()))
        })?;
        Some((node, base + offset_within_run(&run, x - origin.0)))
    }

    // maps a point over prose to the DOM text node and UTF-16 offset it lands on;
    // a snapping drag resolves points between lines or beside text to the nearest line
    pub fn prose_text_offset_at(&self, x: f32, y: f32, snap: bool) -> Option<(NodeId, usize)> {
        let runs = self
            .prose_runs()
            .into_iter()
            .filter_map(|run| run.local(x, y).map(|point| (run, point)))
            .collect::<Vec<_>>();
        let within_line = |(run, (_, y)): &&(ProseRun, (f32, f32))| {
            *y >= run.run.y && *y < run.run.y + run.line_height()
        };
        let mut line = runs
            .iter()
            .filter(|entry| {
                within_line(entry)
                    && (snap || contains_point(entry.0.box_rect, entry.1.0, entry.1.1))
            })
            .collect::<Vec<_>>();
        if line.is_empty() && snap {
            let nearest = runs
                .iter()
                .min_by(|(left, (_, left_y)), (right, (_, right_y))| {
                    left.vertical_distance(*left_y)
                        .total_cmp(&right.vertical_distance(*right_y))
                })?;
            line = runs
                .iter()
                .filter(|(run, _)| {
                    run.run.y == nearest.0.run.y && run.box_node == nearest.0.box_node
                })
                .collect();
        }
        let (hit, (x, _)) =
            line.into_iter()
                .min_by(|(left, (left_x, _)), (right, (right_x, _))| {
                    left.horizontal_distance(*left_x)
                        .total_cmp(&right.horizontal_distance(*right_x))
                })?;
        let index = offset_within_run(&hit.run, *x);
        Some((hit.node, hit.run.node_offset_at(index)))
    }

    // every painted prose run in tree order, translated to the viewport
    pub(crate) fn prose_runs(&self) -> Vec<ProseRun> {
        let mut runs = Vec::new();
        collect_prose_runs(&self.root, (0.0, 0.0), &mut Vec::new(), &mut runs);
        runs
    }
}

// a selected span of one text node; continued spans began in an earlier node,
// so a run's leading space belongs to the selection
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ProseSpan {
    pub(crate) node: NodeId,
    pub(crate) start: usize,
    pub(crate) end: usize,
    pub(crate) continued: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ProseRun {
    pub(crate) node: NodeId,
    pub(crate) box_node: NodeId,
    pub(crate) box_rect: Rect,
    pub(crate) run: TextRun,
    // full transforms of transformed ancestors, outermost first
    pub(crate) transforms: Vec<crate::transform::Matrix>,
}

impl ProseRun {
    // the viewport point in the run's own untransformed space
    fn local(&self, x: f32, y: f32) -> Option<(f32, f32)> {
        self.transforms
            .iter()
            .try_fold((x, y), |(x, y), transform| transform.unmap_point(x, y))
    }
}

impl ProseRun {
    pub(crate) fn line_height(&self) -> f32 {
        self.run.font_size.max(1.0)
    }

    fn vertical_distance(&self, y: f32) -> f32 {
        if y < self.run.y {
            self.run.y - y
        } else {
            (y - (self.run.y + self.line_height())).max(0.0)
        }
    }

    fn horizontal_distance(&self, x: f32) -> f32 {
        if x < self.run.x {
            self.run.x - x
        } else {
            (x - (self.run.x + self.run.width)).max(0.0)
        }
    }
}

fn offset_within_run(run: &TextRun, x: f32) -> usize {
    // boundaries are measured as the run draws, so kerning and ligatures move them too
    let mut offset = 0;
    let mut previous = 0.0;
    let mut prefix = String::new();
    for character in run.text.chars() {
        prefix.push(character);
        let width = measure_text(
            &prefix,
            run.font_stack,
            run.font_size,
            run.font,
            run.letter_spacing,
        );
        if x < run.x + (previous + width) / 2.0 {
            return offset;
        }
        previous = width;
        offset += character.len_utf16();
    }
    offset
}

// a text control under the point: its runs, visual offset and the point in its own space
type ControlHit = (NodeId, Vec<TextRun>, (f32, f32), (f32, f32));

fn collect_control_at(
    layout_box: &LayoutBox,
    x: f32,
    y: f32,
    ancestor_offset: (f32, f32),
    found: &mut Option<ControlHit>,
) {
    let Some((x, y)) = local_point(layout_box, ancestor_offset, x, y) else {
        return;
    };
    let offset = visual_offset_for(layout_box, ancestor_offset);
    let rect = visual_rect_for(layout_box, ancestor_offset);
    let inside = x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height;
    if inside && layout_box.control_value.is_some() && !layout_box.text.is_empty() {
        *found = Some((layout_box.node, layout_box.text.clone(), offset, (x, y)));
    }
    for child in &layout_box.children {
        collect_control_at(child, x, y, offset, found);
    }
}

fn collect_prose_runs(
    layout_box: &LayoutBox,
    ancestor_offset: (f32, f32),
    transforms: &mut Vec<crate::transform::Matrix>,
    found: &mut Vec<ProseRun>,
) {
    let offset = visual_offset_for(layout_box, ancestor_offset);
    let box_rect = visual_rect_for(layout_box, ancestor_offset);
    if let Some(transform) = layout_box.transform {
        transforms.push(transform.about(box_rect.x, box_rect.y));
    }
    for run in layout_box.text.iter().filter(|_| layout_box.selectable) {
        let Some(node) = run.node else {
            continue;
        };
        if run.text.trim().is_empty() {
            continue;
        }
        let mut run = run.clone();
        run.x += offset.0;
        run.y += offset.1;
        found.push(ProseRun {
            node,
            box_node: layout_box.node,
            box_rect,
            run,
            transforms: transforms.clone(),
        });
    }
    for child in &layout_box.children {
        collect_prose_runs(child, offset, transforms, found);
    }
    if layout_box.transform.is_some() {
        transforms.pop();
    }
}

// where a node's box sits in the viewport, as script geometry reads it
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BoxGeometry {
    // the border box
    pub(crate) rect: Rect,
    pub(crate) border: ResolvedEdges,
    pub(crate) position: Position,
    // the border box less any scrollbar, which client sizes start from
    pub(crate) scrollport: Rect,
    pub(crate) scroll_size: Size,
}

impl LayoutDocument {
    // every node's boxes in viewport coordinates and tree order, leaving out generated content
    pub(crate) fn box_index(&self) -> HashMap<NodeId, Vec<BoxGeometry>> {
        let mut index = HashMap::new();
        index_boxes(&self.root, (0.0, 0.0), &mut index);
        index
    }

    pub fn paint(&self) -> crate::paint::Canvas {
        crate::paint::paint(self)
    }

    pub fn describe(&self) -> String {
        let mut lines = Vec::new();
        describe_box(&self.root, 0, &mut lines);
        lines.join("\n")
    }

    pub fn hit_test(&self, x: f32, y: f32) -> Option<NodeId> {
        hit_test_box(&self.root, x, y, Some(self.root.rect), (0.0, 0.0)).map(|hit| hit.layout.node)
    }

    // the pointer shape over a point; auto shows a text cursor over text and editable fields
    pub(crate) fn cursor_at(&self, x: f32, y: f32) -> Cursor {
        let Some(hit) = hit_test_box(&self.root, x, y, Some(self.root.rect), (0.0, 0.0)) else {
            return Cursor::Default;
        };
        let layout = hit.layout;
        if layout.cursor != Cursor::Auto {
            return layout.cursor;
        }
        let editable = layout.tag == "textarea"
            || (layout.tag == "input" && layout.control_text_origin.is_some());
        let over_text = layout.text.iter().any(|run| {
            contains_point(
                Rect {
                    x: run.x + hit.offset.0,
                    y: run.y + hit.offset.1,
                    width: run.width,
                    height: run.font_size * 1.25,
                },
                hit.point.0,
                hit.point.1,
            )
        });
        if editable || over_text {
            Cursor::Text
        } else {
            Cursor::Default
        }
    }

    pub(crate) fn scroll_target_at(&self, x: f32, y: f32) -> Option<ScrollTarget> {
        scroll_target_box(&self.root, x, y, Some(self.root.rect), (0.0, 0.0))
    }

    // a node's box when it scrolls, with where in the viewport its content shows
    pub(crate) fn scroll_box(&self, node: NodeId) -> Option<(ScrollTarget, Rect)> {
        find_scroll_box(&self.root, node, (0.0, 0.0))
    }

    // maps a pointer over a scrollbar thumb to the drag state that moves it
    pub(crate) fn thumb_drag_at(&self, x: f32, y: f32) -> Option<ThumbDrag> {
        thumb_drag_box(&self.root, x, y, Some(self.root.rect), (0.0, 0.0))
    }

    // an enabled range input under a pointer press, and where its thumb travels
    pub(crate) fn range_drag_at(&self, x: f32, y: f32) -> Option<RangeDrag> {
        let hit = hit_test_box(&self.root, x, y, Some(self.root.rect), (0.0, 0.0))?;
        range_drag_box(&self.root, hit.layout.node, x, (0.0, 0.0))
    }
}

// a range input being dragged, in viewport coordinates; ancestor transforms are not mapped
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RangeDrag {
    pub(crate) node: NodeId,
    // where the thumb's center sits at the minimum, and how far it travels to the maximum
    pub(crate) start: f32,
    pub(crate) length: f32,
    // the pointer's offset from the thumb's center when the press landed on the thumb, so it
    // does not jump; a press on the track centers the thumb under the pointer
    pub(crate) grab: f32,
    // the value at the press, which change compares against on release
    pub(crate) pressed: String,
}

impl RangeDrag {
    pub(crate) fn fraction(&self, x: f32) -> f64 {
        if self.length > 0.0 {
            f64::from(((x - self.grab - self.start) / self.length).clamp(0.0, 1.0))
        } else {
            0.0
        }
    }
}

fn range_drag_box(
    layout_box: &LayoutBox,
    node: NodeId,
    x: f32,
    ancestor_offset: (f32, f32),
) -> Option<RangeDrag> {
    if layout_box.node == node
        && let Some(ControlPaint::Range {
            fraction,
            disabled: false,
            travel: (start, length),
            thumb,
            ..
        }) = layout_box.control
    {
        let start = visual_rect_for(layout_box, ancestor_offset).x + start + thumb / 2.0;
        let center = start + fraction * length;
        return Some(RangeDrag {
            node,
            start,
            length,
            grab: if (x - center).abs() <= thumb / 2.0 {
                x - center
            } else {
                0.0
            },
            pressed: String::new(),
        });
    }
    let offset = visual_offset_for(layout_box, ancestor_offset);
    layout_box
        .children
        .iter()
        .find_map(|child| range_drag_box(child, node, x, offset))
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ScrollTarget {
    pub(crate) node: NodeId,
    pub(crate) max_x: f32,
    pub(crate) max_y: f32,
    pub(crate) offset: (f32, f32),
}

#[derive(Clone, Debug)]
pub struct ThumbDrag {
    pub(crate) node: NodeId,
    // the stretch the thumb slides along and the thumb's length, in viewport coordinates
    pub track: Rect,
    pub vertical: bool,
    pub thumb_extent: f32,
    // pointer position along the track minus the thumb's own start
    pub grab: f32,
    pub(crate) offset: (f32, f32),
    pub(crate) max_offset: f32,
    // a press off the thumb scrolls by this much instead of dragging: a page of 0.875 of the
    // visible length on the track, or a 40px line on an arrow button
    pub(crate) page: f32,
}

// the classic bar edge draws, and its scrollbar-width: thin version
pub(crate) const SCROLLBAR_THICKNESS: f32 = 15.0;
const THIN_SCROLLBAR_THICKNESS: f32 = 10.0;

// how a scroll container draws its bars
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) enum Scrollbar {
    #[default]
    Native,
    // the platform's bars, narrowed by scrollbar-width: thin and recolored by scrollbar-color
    // as (thumb, track)
    Standard {
        thin: bool,
        colors: Option<(Color, Color)>,
    },
    // the ::-webkit-scrollbar pieces some rule styles, with no arrow buttons
    Styled(Box<ScrollbarParts>),
    Hidden,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ScrollbarParts {
    // the vertical bar's width and the horizontal bar's height
    pub(crate) thickness: (f32, f32),
    pub(crate) bar: Style,
    pub(crate) track: Option<Style>,
    pub(crate) thumb: Option<Style>,
    pub(crate) corner: Option<Style>,
}

impl Scrollbar {
    fn thickness(&self) -> (f32, f32) {
        match self {
            Self::Native | Self::Standard { thin: false, .. } => {
                (SCROLLBAR_THICKNESS, SCROLLBAR_THICKNESS)
            }
            Self::Standard { thin: true, .. } => {
                (THIN_SCROLLBAR_THICKNESS, THIN_SCROLLBAR_THICKNESS)
            }
            Self::Styled(parts) => parts.thickness,
            Self::Hidden => (0.0, 0.0),
        }
    }

    // the native thumb is a pill narrower than its bar, centered across it
    pub(crate) fn native_thumb(&self) -> Option<f32> {
        match self {
            Self::Native | Self::Standard { thin: false, .. } => Some(9.0),
            Self::Standard { thin: true, .. } => Some(6.0),
            Self::Styled(_) | Self::Hidden => None,
        }
    }
}

// the standard properties win over ::-webkit-scrollbar, whose parts count only when some rule
// for them matches the element
fn scrollbar_for(
    stylesheet: &Stylesheet,
    (document, node): (&Document, NodeId),
    style: &Style,
    font_size: f32,
    state: &StyleState,
) -> Scrollbar {
    if style.overflow != Overflow::Auto {
        return Scrollbar::Native;
    }
    let matched = |pseudo: PseudoElement| {
        let context = StyleContext {
            document,
            node,
            pseudo: Some(pseudo),
            parent_font_size: font_size,
            parent_color: style.color,
            parent_weight: style.font_weight,
            parent_text: inherited_text(style),
            state,
        };
        (stylesheet.styles_pseudo(pseudo) && !stylesheet.declared_values(context).is_empty())
            .then(|| stylesheet.style_for(context))
    };
    let thin = match style.scrollbar_width {
        ScrollbarWidth::None => return Scrollbar::Hidden,
        ScrollbarWidth::Thin => true,
        ScrollbarWidth::Auto => false,
    };
    if thin || style.scrollbar_color.is_some() {
        return Scrollbar::Standard {
            thin,
            colors: style.scrollbar_color,
        };
    }
    let Some(bar) = matched(PseudoElement::Scrollbar) else {
        return Scrollbar::Native;
    };
    if bar.display == Display::None {
        return Scrollbar::Hidden;
    }
    let size = |length: Length| {
        length
            .resolve(0.0, font_size)
            .map_or(SCROLLBAR_THICKNESS, |size| size.max(0.0))
    };
    Scrollbar::Styled(Box::new(ScrollbarParts {
        thickness: (size(bar.width), size(bar.height)),
        bar,
        track: matched(PseudoElement::ScrollbarTrack),
        thumb: matched(PseudoElement::ScrollbarThumb),
        corner: matched(PseudoElement::ScrollbarCorner),
    }))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ScrollbarMetrics {
    pub(crate) scrollport: Rect,
    pub(crate) vertical_track: Option<Rect>,
    pub(crate) horizontal_track: Option<Rect>,
}

// the bars sit inside the border, and the scrollport is the border box less them
pub(crate) fn scrollbar_metrics(layout: &LayoutBox) -> ScrollbarMetrics {
    let rect = layout.rect;
    let scroll_container = layout.is_document() || layout.overflow == Overflow::Auto;
    let (thickness_x, thickness_y) = layout.scrollbar.thickness();
    if !scroll_container {
        return ScrollbarMetrics {
            scrollport: rect,
            vertical_track: None,
            horizontal_track: None,
        };
    }
    let [user_x, user_y] = user_scroll_axes(layout);
    let user_x = user_x && thickness_y > 0.0;
    let user_y = user_y && thickness_x > 0.0;
    let mut vertical = user_y && layout.scroll_content.height > rect.height;
    let mut horizontal = user_x
        && layout.scroll_content.width
            > (rect.width - if vertical { thickness_x } else { 0.0 }).max(0.0);
    vertical = user_y
        && layout.scroll_content.height
            > (rect.height - if horizontal { thickness_y } else { 0.0 }).max(0.0);
    horizontal = user_x
        && layout.scroll_content.width
            > (rect.width - if vertical { thickness_x } else { 0.0 }).max(0.0);
    let border = layout.border.widths;
    let inner = Rect {
        x: rect.x + border.left,
        y: rect.y + border.top,
        width: (rect.width - border.left - border.right).max(0.0),
        height: (rect.height - border.top - border.bottom).max(0.0),
    };
    let vertical_width = if vertical {
        thickness_x.min(inner.width)
    } else {
        0.0
    };
    let horizontal_height = if horizontal {
        thickness_y.min(inner.height)
    } else {
        0.0
    };
    let scrollport = Rect {
        x: rect.x,
        y: rect.y,
        width: (rect.width - vertical_width).max(0.0),
        height: (rect.height - horizontal_height).max(0.0),
    };
    let vertical_track = vertical.then_some(Rect {
        x: inner.x + inner.width - vertical_width,
        y: inner.y,
        width: vertical_width,
        height: (inner.height - horizontal_height).max(0.0),
    });
    let horizontal_track = horizontal.then_some(Rect {
        x: inner.x,
        y: inner.y + inner.height - horizontal_height,
        width: (inner.width - vertical_width).max(0.0),
        height: horizontal_height,
    });
    ScrollbarMetrics {
        scrollport,
        vertical_track,
        horizontal_track,
    }
}

// where a bar's thumb sits: the stretch it slides along, the thumb, and the scroll range it
// maps onto
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ThumbPlacement {
    pub(crate) region: Rect,
    pub(crate) thumb: Rect,
    pub(crate) max_offset: f32,
    // the padding box's visible length along the bar, less the other bar
    pub(crate) visible: f32,
    // the length of the arrow button at each end of a native bar
    pub(crate) button: f32,
}

pub(crate) fn thumb_placement(
    layout: &LayoutBox,
    metrics: &ScrollbarMetrics,
    vertical: bool,
) -> Option<ThumbPlacement> {
    let track = if vertical {
        metrics.vertical_track
    } else {
        metrics.horizontal_track
    }?;
    let border = layout.border.widths;
    let (borders, client, content, offset) = if vertical {
        (
            border.top + border.bottom,
            metrics.scrollport.height,
            layout.scroll_content.height,
            layout.scroll_offset.1,
        )
    } else {
        (
            border.left + border.right,
            metrics.scrollport.width,
            layout.scroll_content.width,
            layout.scroll_offset.0,
        )
    };
    let max_offset = (content - client).max(0.0);
    // the native bar keeps a square arrow button at each end, and its thumb slides between them
    // inset as far as it is from the bar's sides
    let mut button = 0.0;
    let region = match layout.scrollbar.native_thumb() {
        Some(pill) => {
            let thickness = if vertical { track.width } else { track.height };
            let length = if vertical { track.height } else { track.width };
            let inset = ((thickness - pill) / 2.0).max(0.0);
            button = thickness.min(length / 2.0);
            let along = (length - 2.0 * (button + inset)).max(0.0);
            let across = (thickness - 2.0 * inset).max(0.0);
            if vertical {
                Rect {
                    x: track.x + inset,
                    y: track.y + button + inset,
                    width: across,
                    height: along,
                }
            } else {
                Rect {
                    x: track.x + button + inset,
                    y: track.y + inset,
                    width: along,
                    height: across,
                }
            }
        }
        None => track,
    };
    let length = if vertical {
        region.height
    } else {
        region.width
    };
    let visible = (client - borders).max(0.0);
    let scrolled = content - borders;
    let thumb_length = if scrolled > 0.0 {
        (length * (visible / scrolled).clamp(0.0, 1.0))
            .round()
            .max(8.0_f32.min(length))
            .min(length)
    } else {
        length
    };
    let progress = if max_offset > 0.0 {
        (offset / max_offset).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let start = ((length - thumb_length) * progress).round();
    let thumb = if vertical {
        Rect {
            y: region.y + start,
            height: thumb_length,
            ..region
        }
    } else {
        Rect {
            x: region.x + start,
            width: thumb_length,
            ..region
        }
    };
    Some(ThumbPlacement {
        region,
        thumb,
        max_offset,
        visible,
        button,
    })
}

// the axes a user can scroll with a wheel or scrollbar; hidden ones scroll only from script
fn user_scroll_axes(layout: &LayoutBox) -> [bool; 2] {
    if layout.is_document() {
        return [true, true];
    }
    layout.overflow_axes.map(|axis| axis == Overflow::Auto)
}

// how far a user can scroll a box on each axis, zero for boxes that are not scroll containers
pub(crate) fn user_scroll_range(layout: &LayoutBox) -> (f32, f32) {
    if layout.overflow != Overflow::Auto && !layout.is_document() {
        return (0.0, 0.0);
    }
    let scrollport = scrollbar_metrics(layout).scrollport;
    let [user_x, user_y] = user_scroll_axes(layout);
    let range = |user: bool, content: f32, port: f32| match user {
        true => (content - port).max(0.0),
        false => 0.0,
    };
    (
        range(user_x, layout.scroll_content.width, scrollport.width),
        range(user_y, layout.scroll_content.height, scrollport.height),
    )
}

// what an overflow container clips its content to: the padding box, less any scrollbar, on
// the axes that clip
pub(crate) fn overflow_clip_rect(layout: &LayoutBox) -> Rect {
    // the scrollport is the border box less the bars, which sit inside the border
    const UNCLIPPED: f32 = 1.0e9;
    let scrollport = scrollbar_metrics(layout).scrollport;
    let border = layout.border.widths;
    let rect = layout.rect;
    let [clip_x, clip_y] = if layout.is_document() {
        [true, true]
    } else {
        layout.overflow_axes.map(Overflow::clips)
    };
    let (left, right) = if clip_x {
        (
            rect.x + border.left,
            scrollport.x + scrollport.width - border.right,
        )
    } else {
        (-UNCLIPPED, UNCLIPPED)
    };
    let (top, bottom) = if clip_y {
        (
            rect.y + border.top,
            scrollport.y + scrollport.height - border.bottom,
        )
    } else {
        (-UNCLIPPED, UNCLIPPED)
    };
    Rect {
        x: left,
        y: top,
        width: (right - left).max(0.0),
        height: (bottom - top).max(0.0),
    }
}

fn viewport_block(viewport: Size) -> ContainingBlock {
    ContainingBlock {
        x: 0.0,
        y: 0.0,
        width: viewport.width,
        height: viewport.height,
    }
}

#[derive(Clone, Copy)]
struct ResolvedInsets {
    top: Option<f32>,
    right: Option<f32>,
    bottom: Option<f32>,
    left: Option<f32>,
}

fn resolve_insets(edges: Edges, containing: ContainingBlock, font_size: f32) -> ResolvedInsets {
    ResolvedInsets {
        top: edges.top.resolve(containing.height, font_size),
        right: edges.right.resolve(containing.width, font_size),
        bottom: edges.bottom.resolve(containing.height, font_size),
        left: edges.left.resolve(containing.width, font_size),
    }
}

fn position_layout_box(
    layout: &mut LayoutBox,
    style: Style,
    mode: LayoutMode,
    containing: ContainingBlock,
    font_size: f32,
) {
    let inset = resolve_insets(style.inset, containing, font_size);
    // with both insets set, auto margins share what the box leaves between them, the start
    // margin taking none of a shortfall inline
    if mode == LayoutMode::Fixed || mode == LayoutMode::Absolute {
        let auto = |length: Length| length == Length::Auto;
        if let (Some(left), Some(right)) = (inset.left, inset.right) {
            let free = containing.width
                - left
                - right
                - layout.rect.width
                - layout.margin.left
                - layout.margin.right;
            match (auto(style.margin.left), auto(style.margin.right)) {
                (true, true) if free < 0.0 => layout.margin.right += free,
                (true, true) => {
                    layout.margin.left += free / 2.0;
                    layout.margin.right += free / 2.0;
                }
                (true, false) => layout.margin.left += free,
                (false, true) => layout.margin.right += free,
                (false, false) => {}
            }
        }
        if let (Some(top), Some(bottom)) = (inset.top, inset.bottom) {
            let free = containing.height
                - top
                - bottom
                - layout.rect.height
                - layout.margin.top
                - layout.margin.bottom;
            match (auto(style.margin.top), auto(style.margin.bottom)) {
                (true, true) => {
                    layout.margin.top += free / 2.0;
                    layout.margin.bottom += free / 2.0;
                }
                (true, false) => layout.margin.top += free,
                (false, true) => layout.margin.bottom += free,
                (false, false) => {}
            }
        }
    }
    let x = if let Some(left) = inset.left {
        containing.x + left + layout.margin.left
    } else if let Some(right) = inset.right {
        containing.x + containing.width - right - layout.rect.width - layout.margin.right
    } else {
        containing.x + layout.margin.left
    };
    let y = if let Some(top) = inset.top {
        containing.y + top + layout.margin.top
    } else if let Some(bottom) = inset.bottom {
        containing.y + containing.height - bottom - layout.rect.height - layout.margin.bottom
    } else {
        containing.y + layout.margin.top
    };
    if mode == LayoutMode::Fixed || mode == LayoutMode::Absolute {
        let offset_x = x - layout.rect.x;
        let offset_y = y - layout.rect.y;
        translate_layout_box(layout, offset_x, offset_y);
    }
}

fn append_positioned_nodes(
    children: &mut Vec<LayoutBox>,
    nodes: Vec<Subject<'_>>,
    context: PositionedLayoutContext<'_>,
) {
    let PositionedLayoutContext {
        containing_block,
        viewport,
        viewport_size,
        parent,
        stylesheet,
        style_state,
    } = context;
    for node in nodes {
        let style = stylesheet.style_for(node.style_context(parent, style_state));
        let mode = if style.position == Position::Fixed {
            LayoutMode::Fixed
        } else {
            LayoutMode::Absolute
        };
        let block = if mode == LayoutMode::Fixed {
            viewport
        } else {
            containing_block
        };
        if let Some(layout) = layout_element(ElementLayoutInput {
            node,
            containing_x: 0.0,
            containing_y: 0.0,
            containing_width: block.width,
            containing_height: (block.height > 0.0).then_some(block.height),
            size_override: (None, None),
            floats: None,
            list_ordinal: 1,
            parent,
            context: LayoutContext {
                viewport: viewport_size,
                containing_block: if mode == LayoutMode::Fixed {
                    viewport
                } else {
                    containing_block
                },
                mode,
                stylesheet,
                style_state,
            },
        }) {
            children.push(layout);
        }
    }
}

fn index_boxes(
    layout: &LayoutBox,
    ancestor_offset: (f32, f32),
    index: &mut HashMap<NodeId, Vec<BoxGeometry>>,
) {
    let offset = visual_offset_for(layout, ancestor_offset);
    if !layout.tag.starts_with("::") {
        index.entry(layout.node).or_default().push(BoxGeometry {
            rect: translate_rect(layout.rect, offset),
            border: layout.border.widths,
            position: layout.position,
            scrollport: translate_rect(scrollbar_metrics(layout).scrollport, offset),
            scroll_size: layout.scroll_content,
        });
    }
    // text nodes are measured by the runs they produced
    for run in &layout.text {
        let Some(node) = run.node else {
            continue;
        };
        let rect = Rect {
            x: run.x + offset.0,
            y: run.y + offset.1,
            width: run.width,
            height: run.font_size * 1.25,
        };
        index.entry(node).or_default().push(BoxGeometry {
            rect,
            border: ResolvedEdges::default(),
            position: Position::Static,
            scrollport: rect,
            scroll_size: Size {
                width: rect.width,
                height: rect.height,
            },
        });
    }
    for child in &layout.children {
        index_boxes(child, offset, index);
    }
}

// the box under a point, with its visual offset and the point in its local space
#[derive(Clone, Copy)]
struct Hit<'a> {
    layout: &'a LayoutBox,
    offset: (f32, f32),
    point: (f32, f32),
}

fn hit_test_box(
    layout: &LayoutBox,
    x: f32,
    y: f32,
    clip: Option<Rect>,
    ancestor_offset: (f32, f32),
) -> Option<Hit<'_>> {
    hit_test_context(layout, x, y, clip, ancestor_offset)
}

fn hit_test_context(
    layout: &LayoutBox,
    x: f32,
    y: f32,
    clip: Option<Rect>,
    ancestor_offset: (f32, f32),
) -> Option<Hit<'_>> {
    if let Some(clip) = clip
        && !contains_point(clip, x, y)
    {
        return None;
    }
    // inside a transform the ancestor clip has been applied, and the rest works in local space
    let (x, y) = local_point(layout, ancestor_offset, x, y)?;
    let clip = if layout.transform.is_some() {
        None
    } else {
        clip
    };
    let visual_offset = visual_offset_for(layout, ancestor_offset);
    let rect = visual_rect_for(layout, ancestor_offset);
    if !inside_clip_path(layout, rect, x, y) {
        return None;
    }
    let inside_layout = contains_point(rect, x, y);
    let mut contexts = stacking_context_entries(layout, clip, ancestor_offset);
    contexts.sort_by_key(|entry| (entry.z_index, entry.order));
    for entry in contexts.iter().rev().filter(|entry| entry.z_index >= 0) {
        let hit = if entry.layout.creates_stacking_context {
            hit_test_context(entry.layout, x, y, entry.clip, entry.ancestor_offset)
        } else {
            hit_test_non_context(entry.layout, x, y, entry.clip, entry.ancestor_offset)
        };
        if let Some(path) = hit {
            return Some(path);
        }
    }
    if let Some(path) = hit_test_non_context_children(layout, x, y, clip, visual_offset) {
        return Some(path);
    }
    for entry in contexts.iter().rev().filter(|entry| entry.z_index < 0) {
        if let Some(path) = hit_test_context(entry.layout, x, y, entry.clip, entry.ancestor_offset)
        {
            return Some(path);
        }
    }
    (inside_layout && hit_target(layout, rect, x, y)).then_some(Hit {
        layout,
        offset: visual_offset,
        point: (x, y),
    })
}

// a clip-path hides its box and everything in it from the pointer outside the shape
fn inside_clip_path(layout: &LayoutBox, rect: Rect, x: f32, y: f32) -> bool {
    use crate::clip_path::{ClipRegion, polygon_contains};
    match &layout.clip_path {
        None => true,
        Some(ClipRegion::Rounded(clip, radii)) => {
            let clip = translate_rect(*clip, (rect.x, rect.y));
            contains_point(clip, x, y) && inside_corners(clip, *radii, x, y)
        }
        Some(ClipRegion::Polygon { even_odd, points }) => {
            polygon_contains(points, *even_odd, (x - rect.x, y - rect.y))
        }
    }
}

fn hit_target(layout: &LayoutBox, rect: Rect, x: f32, y: f32) -> bool {
    !layout.is_document()
        && layout.visible
        && layout.pointer_events
        && inside_corners(rect, layout.radii, x, y)
}

// whether a point inside a box's rectangle is also inside its rounded corners
fn inside_corners(rect: Rect, radii: CornerRadii, x: f32, y: f32) -> bool {
    let right = x > rect.x + rect.width / 2.0;
    let bottom = y > rect.y + rect.height / 2.0;
    let corner = match (right, bottom) {
        (false, false) => 0,
        (true, false) => 1,
        (true, true) => 2,
        (false, true) => 3,
    };
    let (radius_x, radius_y) = radii[corner];
    if radius_x <= 0.0 || radius_y <= 0.0 {
        return true;
    }
    let along_x = if right {
        x - (rect.x + rect.width - radius_x)
    } else {
        rect.x + radius_x - x
    };
    let along_y = if bottom {
        y - (rect.y + rect.height - radius_y)
    } else {
        rect.y + radius_y - y
    };
    along_x <= 0.0
        || along_y <= 0.0
        || (along_x / radius_x).powi(2) + (along_y / radius_y).powi(2) <= 1.0
}

fn hit_test_non_context_children(
    layout: &LayoutBox,
    x: f32,
    y: f32,
    clip: Option<Rect>,
    visual_offset: (f32, f32),
) -> Option<Hit<'_>> {
    let child_clip = stacking_child_clip(layout, clip, visual_offset);
    for index in paint_order(&layout.children).into_iter().rev() {
        let child = &layout.children[index];
        if child.creates_stacking_context || child.paints_positioned() {
            continue;
        }
        if let Some(path) = hit_test_non_context(child, x, y, child_clip, visual_offset) {
            return Some(path);
        }
    }
    None
}

fn hit_test_non_context(
    layout: &LayoutBox,
    x: f32,
    y: f32,
    clip: Option<Rect>,
    ancestor_offset: (f32, f32),
) -> Option<Hit<'_>> {
    if let Some(clip) = clip
        && !contains_point(clip, x, y)
    {
        return None;
    }
    let visual_offset = visual_offset_for(layout, ancestor_offset);
    let rect = visual_rect_for(layout, ancestor_offset);
    if let Some(path) = hit_test_non_context_children(layout, x, y, clip, visual_offset) {
        return Some(path);
    }
    (contains_point(rect, x, y) && hit_target(layout, rect, x, y)).then_some(Hit {
        layout,
        offset: visual_offset,
        point: (x, y),
    })
}

pub(crate) struct StackingContextEntry<'a> {
    pub(crate) layout: &'a LayoutBox,
    pub(crate) clip: Option<Rect>,
    // clipping boxes between the context and its stacking ancestor, with their ancestor
    // offsets, so their rounded corners still cut the context
    pub(crate) clippers: Vec<(&'a LayoutBox, (f32, f32))>,
    pub(crate) ancestor_offset: (f32, f32),
    pub(crate) z_index: i32,
    pub(crate) order: usize,
}

pub(crate) fn stacking_context_entries(
    layout: &LayoutBox,
    clip: Option<Rect>,
    ancestor_offset: (f32, f32),
) -> Vec<StackingContextEntry<'_>> {
    let mut entries = Vec::new();
    let mut order = 0;
    let mut clippers = Vec::new();
    collect_stacking_contexts(
        layout,
        clip,
        ancestor_offset,
        &mut order,
        &mut clippers,
        &mut entries,
    );
    entries
}

fn collect_stacking_contexts<'a>(
    layout: &'a LayoutBox,
    clip: Option<Rect>,
    ancestor_offset: (f32, f32),
    order: &mut usize,
    clippers: &mut Vec<(&'a LayoutBox, (f32, f32))>,
    entries: &mut Vec<StackingContextEntry<'a>>,
) {
    let visual_offset = visual_offset_for(layout, ancestor_offset);
    let child_clip = stacking_child_clip(layout, clip, visual_offset);
    for index in paint_order(&layout.children) {
        let child = &layout.children[index];
        let child_order = *order;
        *order += 1;
        // a positioned box that makes no context of its own still paints with the contexts
        // at level zero, in tree order, over the content in flow; the contexts inside it
        // stay entries of this context
        if child.creates_stacking_context || child.paints_positioned() {
            entries.push(StackingContextEntry {
                layout: child,
                clip: child_clip,
                clippers: clippers.clone(),
                ancestor_offset: visual_offset,
                z_index: child.z_index,
                order: child_order,
            });
        }
        if !child.creates_stacking_context {
            let clips = child.overflow.clips();
            if clips {
                clippers.push((child, visual_offset));
            }
            collect_stacking_contexts(child, child_clip, visual_offset, order, clippers, entries);
            if clips {
                clippers.pop();
            }
        }
    }
}

fn stacking_child_clip(
    layout: &LayoutBox,
    clip: Option<Rect>,
    visual_offset: (f32, f32),
) -> Option<Rect> {
    if layout.overflow.clips() || layout.is_document() {
        // a clip that leaves nothing still clips, where none would let the children paint anywhere
        Some(
            intersect_rect(
                clip,
                translate_rect(overflow_clip_rect(layout), visual_offset),
            )
            .unwrap_or_default(),
        )
    } else {
        clip
    }
}

impl LayoutBox {
    // whether the box paints in its stacking context's positioned layer without being a
    // context itself; an inline box paints with its line
    pub(crate) fn paints_positioned(&self) -> bool {
        self.position != Position::Static && !self.creates_stacking_context && !self.inline
    }
}

pub(crate) fn paint_order(children: &[LayoutBox]) -> Vec<usize> {
    let mut order = (0..children.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| (children[*index].z_index, children[*index].floated, *index));
    order
}

// maps a point into a transformed box's untransformed space; None when the point misses
// the projected plane or the box shows a hidden back face
fn local_point(
    layout: &LayoutBox,
    ancestor_offset: (f32, f32),
    x: f32,
    y: f32,
) -> Option<(f32, f32)> {
    let Some(transform) = layout.transform else {
        return Some((x, y));
    };
    let rect = visual_rect_for(layout, ancestor_offset);
    let full = transform.about(rect.x, rect.y);
    if layout.backface_hidden && full.shows_back() {
        return None;
    }
    full.unmap_point(x, y)
}

pub(crate) fn visual_offset_for(layout: &LayoutBox, ancestor_offset: (f32, f32)) -> (f32, f32) {
    if layout.position == Position::Fixed {
        layout.visual_offset
    } else {
        (
            ancestor_offset.0 + layout.visual_offset.0,
            ancestor_offset.1 + layout.visual_offset.1,
        )
    }
}

pub(crate) fn visual_rect_for(layout: &LayoutBox, ancestor_offset: (f32, f32)) -> Rect {
    let offset = visual_offset_for(layout, ancestor_offset);
    translate_rect(layout.rect, offset)
}

pub(crate) fn translate_rect(rect: Rect, offset: (f32, f32)) -> Rect {
    Rect {
        x: rect.x + offset.0,
        y: rect.y + offset.1,
        ..rect
    }
}

fn scroll_target_box(
    layout: &LayoutBox,
    x: f32,
    y: f32,
    clip: Option<Rect>,
    ancestor_offset: (f32, f32),
) -> Option<ScrollTarget> {
    if let Some(clip) = clip
        && !contains_point(clip, x, y)
    {
        return None;
    }
    let (x, y) = local_point(layout, ancestor_offset, x, y)?;
    let clip = if layout.transform.is_some() {
        None
    } else {
        clip
    };
    let visual_offset = visual_offset_for(layout, ancestor_offset);
    let rect = visual_rect_for(layout, ancestor_offset);
    if !inside_clip_path(layout, rect, x, y) {
        return None;
    }
    let inside_layout = contains_point(rect, x, y);
    let child_clip = if layout.overflow.clips() || layout.is_document() {
        intersect_rect(
            clip,
            translate_rect(overflow_clip_rect(layout), visual_offset),
        )
    } else {
        clip
    };
    for index in paint_order(&layout.children).into_iter().rev() {
        if let Some(target) =
            scroll_target_box(&layout.children[index], x, y, child_clip, visual_offset)
        {
            return Some(target);
        }
    }
    if inside_layout {
        let (max_x, max_y) = user_scroll_range(layout);
        if max_x > 0.0 || max_y > 0.0 {
            return Some(ScrollTarget {
                node: layout.node,
                max_x,
                max_y,
                offset: layout.scroll_offset,
            });
        }
    }
    None
}

fn find_scroll_box(
    layout: &LayoutBox,
    node: NodeId,
    ancestor_offset: (f32, f32),
) -> Option<(ScrollTarget, Rect)> {
    let visual_offset = visual_offset_for(layout, ancestor_offset);
    if layout.node == node {
        let (max_x, max_y) = user_scroll_range(layout);
        if max_x > 0.0 || max_y > 0.0 {
            let target = ScrollTarget {
                node,
                max_x,
                max_y,
                offset: layout.scroll_offset,
            };
            return Some((
                target,
                translate_rect(overflow_clip_rect(layout), visual_offset),
            ));
        }
    }
    layout
        .children
        .iter()
        .find_map(|child| find_scroll_box(child, node, visual_offset))
}

fn contains_point(rect: Rect, x: f32, y: f32) -> bool {
    x >= rect.x && y >= rect.y && x < rect.x + rect.width && y < rect.y + rect.height
}

fn thumb_drag_box(
    layout_box: &LayoutBox,
    x: f32,
    y: f32,
    clip: Option<Rect>,
    ancestor_offset: (f32, f32),
) -> Option<ThumbDrag> {
    if let Some(clip) = clip
        && !contains_point(clip, x, y)
    {
        return None;
    }
    let (x, y) = local_point(layout_box, ancestor_offset, x, y)?;
    let clip = if layout_box.transform.is_some() {
        None
    } else {
        clip
    };
    let visual_offset = visual_offset_for(layout_box, ancestor_offset);
    let child_clip = if layout_box.overflow.clips() || layout_box.is_document() {
        intersect_rect(
            clip,
            translate_rect(overflow_clip_rect(layout_box), visual_offset),
        )
    } else {
        clip
    };
    for child in &layout_box.children {
        if let Some(target) = thumb_drag_box(child, x, y, child_clip, visual_offset) {
            return Some(target);
        }
    }
    let metrics = scrollbar_metrics(layout_box);
    for (track_rect, vertical) in [
        (metrics.vertical_track, true),
        (metrics.horizontal_track, false),
    ] {
        let Some(track) = track_rect else { continue };
        let track = translate_rect(track, visual_offset);
        if !contains_point(track, x, y) {
            continue;
        }
        let Some(placement) = thumb_placement(layout_box, &metrics, vertical) else {
            continue;
        };
        let region = translate_rect(placement.region, visual_offset);
        let thumb = translate_rect(placement.thumb, visual_offset);
        let pointer = if vertical { y } else { x };
        let (thumb_start, thumb_extent) = if vertical {
            (thumb.y, thumb.height)
        } else {
            (thumb.x, thumb.width)
        };
        let (track_start, track_end) = if vertical {
            (track.y, track.y + track.height)
        } else {
            (track.x, track.x + track.width)
        };
        let page = if pointer < track_start + placement.button {
            -40.0
        } else if pointer >= track_end - placement.button {
            40.0
        } else if pointer < thumb_start {
            -0.875 * placement.visible
        } else if pointer >= thumb_start + thumb_extent {
            0.875 * placement.visible
        } else {
            0.0
        };
        return Some(ThumbDrag {
            node: layout_box.node,
            track: region,
            vertical,
            thumb_extent,
            // where the pointer sits inside the thumb, kept through the drag
            grab: pointer - thumb_start,
            offset: layout_box.scroll_offset,
            max_offset: placement.max_offset,
            page,
        });
    }
    None
}

fn intersect_rect(parent: Option<Rect>, rect: Rect) -> Option<Rect> {
    if rect.width <= 0.0 || rect.height <= 0.0 {
        return None;
    }
    let Some(parent) = parent else {
        return Some(rect);
    };
    let left = parent.x.max(rect.x);
    let top = parent.y.max(rect.y);
    let right = (parent.x + parent.width).min(rect.x + rect.width);
    let bottom = (parent.y + parent.height).min(rect.y + rect.height);
    (right > left && bottom > top).then_some(Rect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    })
}

// everything an element's layout is decided by besides the document, stylesheet and state,
// compared bit for bit
#[derive(Clone, Copy, PartialEq)]
struct BoxInputs {
    numbers: [u32; 11],
    heights: (Option<u32>, Option<u32>, Option<u32>),
    list_ordinal: i32,
    color: Color,
    weight: FontWeight,
    text: InheritedText,
    mode: LayoutMode,
    zero_font: (crate::font::FontStack, crate::font::FontStyle),
    environment: crate::css::LayoutEnvironment,
}

// an answer worked out for a node under some inputs: a box or an intrinsic width
struct Kept<Inputs, Answer> {
    inputs: Inputs,
    answer: Answer,
    // the pass it was last worked out or asked for in
    pass: u64,
    // how many passes had worked something out by then
    used: u64,
    // the document's change stamp it was worked out at
    stamp: u64,
    // it sampled motion or sized a query container, so it stands in its own pass only
    momentary: bool,
}

type KeptMap<Key, Inputs, Answer> = HashMap<Key, Vec<Kept<Inputs, Answer>>>;

// what every kept answer was worked out under besides its own inputs and the nodes it read
#[derive(PartialEq)]
struct KeptUnder {
    stylesheet: u64,
    document: u64,
    selector_version: u64,
    fonts: (*const crate::font::FontSet, u64),
    scale: u32,
    // without the clock, which only motion reads, the custom properties, which follow from
    // the rest, and the hovered element, which is followed apart
    state: StyleState,
}

// the boxes and intrinsic widths worked out by the layout in progress and, where they can
// stand in a later one, by the layout before it. within a layout a flex or grid container
// lays an item out to measure it and again to place it, and nesting multiplies that. from
// one layout to the next an answer stands while nothing in its node's subtree and no
// ancestor of the node has been written, so a frame that moves one thing lays out the chain
// above it and takes the rest as it was
#[derive(Default)]
struct KeptAnswers {
    // none where no answer outlives its pass; the font set is held so its address stays its
    under: Option<(KeptUnder, Rc<crate::font::FontSet>)>,
    hovered: Option<NodeId>,
    passes: u64,
    // the number of the pass in progress and the document's change stamp at its start
    pass: Option<(u64, u64)>,
    // how many passes have worked an answer out, and whether the one in progress has
    works: u64,
    worked: bool,
    boxes: KeptMap<(NodeId, Option<PseudoElement>), BoxInputs, Option<LayoutBox>>,
    widths: KeptMap<IntrinsicKey, IntrinsicInputs, f32>,
}

thread_local! {
    static KEPT: std::cell::RefCell<KeptAnswers> = std::cell::RefCell::default();
}

// the answer kept for the inputs when it is from this pass or still stands, and whether it
// came from an earlier pass
fn kept_answer<Key: Eq + std::hash::Hash, Inputs: PartialEq, Answer: Clone>(
    map: &mut KeptMap<Key, Inputs, Answer>,
    (key, inputs): (&Key, &Inputs),
    (pass, works): (u64, u64),
    stands: impl FnOnce(u64) -> bool,
) -> Option<(Answer, bool)> {
    let kept = map
        .get_mut(key)?
        .iter_mut()
        .find(|kept| kept.inputs == *inputs)?;
    // an answer for one moment only is dropped when its pass ends, so none comes from earlier
    let earlier = kept.pass != pass;
    if earlier && !stands(kept.stamp) {
        crate::profile::count("kept: answer written since");
        return None;
    }
    if earlier {
        crate::profile::count("kept: answer taken");
    }
    kept.pass = pass;
    kept.used = works;
    Some((kept.answer.clone(), earlier))
}

fn keep_answer<Key: Eq + std::hash::Hash, Inputs: PartialEq, Answer>(
    map: &mut KeptMap<Key, Inputs, Answer>,
    key: Key,
    answer: Kept<Inputs, Answer>,
) {
    let answers = map.entry(key).or_default();
    match answers.iter_mut().find(|kept| kept.inputs == answer.inputs) {
        Some(kept) => *kept = answer,
        None => answers.push(answer),
    }
}

struct KeptPass {
    // the answers of the layout this one runs inside of
    outer: Option<KeptAnswers>,
}

impl Drop for KeptPass {
    fn drop(&mut self) {
        KEPT.with(|kept| {
            let mut kept = kept.borrow_mut();
            if let Some(outer) = self.outer.take() {
                *kept = outer;
                return;
            }
            if kept.pass.take().is_none() {
                return;
            }
            if kept.under.is_none() {
                kept.boxes.clear();
                kept.widths.clear();
                return;
            }
            // each kept box holds its whole subtree, so only what recent work used stays: the
            // boxes beside the chain that changed, and not the ones inside them. a pass that
            // works nothing out takes the root box whole and uses nothing else; a page can
            // make several of those between two changes, as a script measures and a frame
            // lays out, so they age nothing, and an answer goes once two passes that did
            // work something out have not asked for it
            if std::mem::take(&mut kept.worked) {
                kept.works += 1;
            }
            let works = kept.works;
            fn prune<Key, Inputs, Answer>(map: &mut KeptMap<Key, Inputs, Answer>, works: u64) {
                map.retain(|_, answers| {
                    answers.retain(|kept| kept.used + 2 >= works && !kept.momentary);
                    !answers.is_empty()
                });
            }
            prune(&mut kept.boxes, works);
            prune(&mut kept.widths, works);
        });
    }
}

// the document, stylesheet and state must stay unchanged while the guard lives
fn keep_answers(document: &Document, stylesheet: &Stylesheet, state: &StyleState) -> KeptPass {
    KEPT.with(|kept| {
        let mut kept = kept.borrow_mut();
        // a layout inside another has its own document, so the outer answers step aside
        let outer = kept.pass.is_some().then(|| std::mem::take(&mut *kept));
        // a shadow tree is written without its host hearing of it, and a query container's
        // size is an answer the pass itself has to write down
        let lasting = outer.is_none()
            && !document.has_shadow_roots()
            && stylesheet.follows_versions()
            && !stylesheet.has_container_queries();
        let fonts = crate::font::active_fonts();
        let under = lasting.then(|| KeptUnder {
            stylesheet: stylesheet.identity(),
            document: document.identity(),
            selector_version: document.selector_version(),
            fonts: (Rc::as_ptr(&fonts), fonts.web_face_changes()),
            scale: crate::font::device_scale().to_bits(),
            state: StyleState {
                clock: 0,
                custom_properties: HashMap::new(),
                hovered: None,
                ..state.clone()
            },
        });
        if under.is_none() || kept.under.as_ref().map(|(under, _)| under) != under.as_ref() {
            crate::profile::count("kept: all answers dropped");
            kept.boxes.clear();
            kept.widths.clear();
            kept.under = under.map(|under| (under, fonts));
        } else if kept.hovered != state.hovered {
            // a pointer move restyles a few subtrees, and with them the chain above each
            match stylesheet.hover_reach(document, kept.hovered, state.hovered) {
                Some(roots) => {
                    let mut stale = std::collections::HashSet::new();
                    for &root in &roots {
                        stale.extend(document.subtree(root));
                        stale.extend(document.ancestors(root));
                    }
                    kept.boxes.retain(|(node, _), _| !stale.contains(node));
                    kept.widths.retain(|(node, ..), _| !stale.contains(node));
                }
                None => {
                    kept.boxes.clear();
                    kept.widths.clear();
                }
            }
        }
        kept.hovered = state.hovered;
        kept.passes += 1;
        kept.pass = Some((kept.passes, document.change_stamp()));
        KeptPass { outer }
    })
}

// drops the answers kept for the next layout
pub(crate) fn forget_kept_answers() {
    KEPT.with(|kept| {
        let mut kept = kept.borrow_mut();
        if kept.pass.is_none() {
            *kept = KeptAnswers::default();
        }
    });
}

// a layout that takes nothing from the one before it and leaves the kept answers as they were
#[cfg(test)]
fn layout_afresh(
    document: &Document,
    stylesheet: &Stylesheet,
    viewport: Size,
    state: &StyleState,
) -> LayoutDocument {
    let kept = KEPT.with(|kept| std::mem::take(&mut *kept.borrow_mut()));
    let layout = layout_with_state(document, stylesheet, viewport, state);
    KEPT.with(|answers| *answers.borrow_mut() = kept);
    layout
}

fn layout_element(input: ElementLayoutInput<'_, '_>) -> Option<LayoutBox> {
    // floats are shared state the layout writes to, so a box among them is laid out each time
    if input.floats.is_some() {
        return layout_element_fresh(input);
    }
    let block = input.context.containing_block;
    let bits = f32::to_bits;
    let inputs = BoxInputs {
        numbers: [
            input.containing_x,
            input.containing_y,
            input.containing_width,
            input.parent.font_size,
            input.parent.opacity,
            input.context.viewport.width,
            input.context.viewport.height,
            block.x,
            block.y,
            block.width,
            block.height,
        ]
        .map(bits),
        heights: (
            input.containing_height.map(bits),
            input.size_override.0.map(bits),
            input.size_override.1.map(bits),
        ),
        list_ordinal: input.list_ordinal,
        color: input.parent.color,
        weight: input.parent.weight,
        text: input.parent.text,
        mode: input.context.mode,
        zero_font: crate::css::zero_font(),
        environment: crate::css::layout_environment(),
    };
    let key = (
        input.node.node,
        input.node.generated.map(|(pseudo, _)| pseudo),
    );
    let Some((pass, stamp, works)) = KEPT.with(|kept| {
        let kept = kept.borrow();
        kept.pass.map(|(pass, stamp)| (pass, stamp, kept.works))
    }) else {
        return layout_element_fresh(input);
    };
    let (document, node) = (input.node.document, input.node.node);
    let known = KEPT.with(|kept| {
        kept_answer(
            &mut kept.borrow_mut().boxes,
            (&key, &inputs),
            (pass, works),
            |since| document.unwritten_since(node, since),
        )
    });
    if let Some((layout, earlier)) = known {
        if cfg!(feature = "recheck") && earlier {
            let reported = crate::css::moment_reports();
            let fresh = layout_element_fresh(input);
            assert!(fresh == layout, "a kept box went stale: {node:?}");
            assert_eq!(
                reported,
                crate::css::moment_reports(),
                "a kept box stands for one moment only: {node:?}"
            );
        }
        return layout;
    }
    let reported = crate::css::moment_reports();
    let layout = layout_element_fresh(input);
    let momentary = crate::css::moment_reports() != reported;
    KEPT.with(|kept| {
        let mut kept = kept.borrow_mut();
        kept.worked = true;
        keep_answer(
            &mut kept.boxes,
            key,
            Kept {
                inputs,
                answer: layout.clone(),
                pass,
                used: works,
                stamp,
                momentary,
            },
        );
    });
    layout
}

fn layout_element_fresh(input: ElementLayoutInput<'_, '_>) -> Option<LayoutBox> {
    let _profile = crate::profile::scope("layout: element");
    let layout = layout_element_with_gutter(input.clone(), 0.0)?;
    // a vertical scrollbar sits between the border and the padding, so the content box gives
    // up its width and the content lays out again
    if user_scroll_axes(&layout)[1]
        && let Some(bar) = scrollbar_metrics(&layout).vertical_track
        && layout.rect.width > bar.width
    {
        return layout_element_with_gutter(input, bar.width);
    }
    Some(layout)
}

fn layout_element_with_gutter(input: ElementLayoutInput<'_, '_>, gutter: f32) -> Option<LayoutBox> {
    let ElementLayoutInput {
        node,
        containing_x,
        containing_y,
        containing_width,
        containing_height,
        size_override,
        floats,
        list_ordinal,
        parent,
        context:
            LayoutContext {
                viewport,
                containing_block,
                mode,
                stylesheet,
                style_state,
            },
    } = input;
    let style = stylesheet.style_for(node.style_context(parent, style_state));
    if style.display == Display::None {
        return None;
    }
    let name = node.name();
    let position_block = match mode {
        LayoutMode::Flow | LayoutMode::Absolute => containing_block,
        LayoutMode::Fixed => ContainingBlock {
            x: 0.0,
            y: 0.0,
            width: viewport.width,
            height: viewport.height,
        },
    };
    let font_size = style
        .font_size
        .resolve(parent.font_size, parent.font_size)
        .unwrap_or(parent.font_size)
        .max(1.0);
    let _zero_font = crate::css::use_zero_font(style.font_family, font_style(&style, font_size));
    let mut margin = resolve_margins(style.margin, containing_width, font_size);
    let padding = resolve_edges(style.padding, containing_width, font_size);
    let border = used_border(&style);
    let horizontal_inner = padding.left + padding.right + border.left + border.right;
    let vertical_inner = padding.top + padding.bottom + border.top + border.bottom;
    let horizontal_extras = margin.left + margin.right + horizontal_inner;
    // border-box sizes include padding and border, so the content takes what is left
    let border_box = style.box_sizing == crate::css::BoxSizing::BorderBox;
    let inner_width = |size: f32| {
        if border_box {
            (size - horizontal_inner).max(0.0)
        } else {
            size
        }
    };
    let inner_height = |size: f32| {
        if border_box {
            (size - vertical_inner).max(0.0)
        } else {
            size
        }
    };
    let inset = resolve_insets(style.inset, position_block, font_size);
    let control_size = control_content_size(
        node,
        &style,
        font_size,
        used_line_height(&style, font_size),
        style_state,
    );
    // an out-of-flow box with both block insets set stretches between them
    let explicit_height = resolve_height(style.height, containing_height, font_size)
        .map(inner_height)
        .or_else(|| {
            let stretch = mode != LayoutMode::Flow && style.content_size[1] == ContentSize::None;
            (stretch && inset.top.is_some() && inset.bottom.is_some()).then(|| {
                (position_block.height
                    - inset.top.unwrap_or(0.0)
                    - inset.bottom.unwrap_or(0.0)
                    - margin.top
                    - margin.bottom
                    - vertical_inner)
                    .max(0.0)
            })
        })
        .or(control_size.1);
    let content_width = style
        .width
        .resolve(containing_width, font_size)
        .map(inner_width)
        .or_else(|| {
            let stretch = mode != LayoutMode::Flow && style.content_size[0] == ContentSize::None;
            (stretch && inset.left.is_some() && inset.right.is_some()).then(|| {
                (position_block.width
                    - inset.left.unwrap_or(0.0)
                    - inset.right.unwrap_or(0.0)
                    - horizontal_extras)
                    .max(0.0)
            })
        })
        .or_else(|| {
            style
                .aspect_ratio
                .zip(explicit_height)
                .map(|(ratio, height)| height * ratio)
        })
        .or(control_size.0)
        .or_else(|| {
            // an out-of-flow box with an auto width shrinks to fit the room its insets leave,
            // as fit-content does in flow
            (mode != LayoutMode::Flow || style.content_size[0] != ContentSize::None).then(|| {
                let measure = |minimum: bool| {
                    intrinsic_width(
                        MaxContentInput {
                            node,
                            stylesheet,
                            style_state,
                            parent,
                            containing_width,
                            own_width: true,
                        },
                        minimum,
                    )
                };
                let room = if mode == LayoutMode::Flow {
                    containing_width
                } else {
                    position_block.width - inset.left.unwrap_or(0.0) - inset.right.unwrap_or(0.0)
                };
                let width = match style.content_size[0] {
                    ContentSize::Max => measure(false),
                    ContentSize::Min => measure(true),
                    _ => measure(false).min(room.max(measure(true))),
                };
                width - horizontal_extras
            })
        })
        .unwrap_or((containing_width - horizontal_extras).max(0.0))
        .max(0.0);
    let min_width = style
        .min_width
        .resolve(containing_width, font_size)
        .map(inner_width)
        .unwrap_or(0.0);
    let max_width = style
        .max_width
        .resolve(containing_width, font_size)
        .map(inner_width)
        .unwrap_or(f32::INFINITY);
    let content_width = size_override
        .0
        .unwrap_or_else(|| content_width.min(max_width).max(min_width));
    // auto margins share the width a block leaves over, which centers it
    if mode == LayoutMode::Flow {
        let free = containing_width - content_width - horizontal_inner - margin.left - margin.right;
        match (
            style.margin.left == Length::Auto,
            style.margin.right == Length::Auto,
        ) {
            (true, true) => {
                margin.left = (free / 2.0).max(0.0);
                margin.right = margin.left;
            }
            (true, false) => margin.left = free.max(0.0),
            (false, true) => margin.right = free.max(0.0),
            (false, false) => {}
        }
    }
    let min_height = resolve_height(style.min_height, containing_height, font_size)
        .map(inner_height)
        .unwrap_or(0.0);
    let max_height = resolve_height(style.max_height, containing_height, font_size)
        .map(inner_height)
        .unwrap_or(f32::INFINITY);
    let explicit_height = size_override.1.or_else(|| {
        explicit_height
            .or_else(|| style.aspect_ratio.map(|ratio| content_width / ratio))
            .map(|height| height.min(max_height).max(min_height))
    });
    let content_width = (content_width - gutter).max(0.0);
    let border_box_width = content_width + horizontal_inner + gutter;
    // a block child of a block lets the margins of its first and last children through an
    // edge with no border or padding, where they collapse with its own
    let in_block_flow = floats.is_some();
    let leading = in_block_flow
        .then(|| {
            leading_margin(
                node,
                &style,
                font_size,
                content_width,
                (stylesheet, style_state),
            )
        })
        .flatten();
    if let Some((_, inner)) = leading {
        margin.top = collapse_margins(margin.top, inner);
    }
    let rect = Rect {
        x: if mode == LayoutMode::Flow {
            containing_x + margin.left
        } else {
            position_block.x
        },
        y: if mode == LayoutMode::Flow {
            containing_y + margin.top
        } else {
            position_block.y
        },
        width: border_box_width,
        height: 0.0,
    };
    let local_containing_block = ContainingBlock {
        x: rect.x,
        y: rect.y,
        width: border_box_width,
        height: explicit_height
            .map(|height| height.max(0.0) + vertical_inner)
            .unwrap_or(0.0),
    };
    let descendant_block = if style.position == Position::Static {
        containing_block
    } else {
        local_containing_block
    };
    let content_x = rect.x + border.left + padding.left;
    let content_y = rect.y + border.top + padding.top;
    // size containment keeps a container's size from depending on its content, so its
    // width, and a size container's explicit height, are known here
    if let Some(both_axes) = style.container_type
        && node.own().is_some()
    {
        let size = ContainerSize {
            width: content_width,
            height: explicit_height.unwrap_or(0.0),
            both_axes,
            name: style.container_name,
        };
        CONTAINERS.with(|containers| containers.borrow_mut().insert(node.node, size));
        crate::css::report_moment();
    }
    if matches!(style.display, Display::Flex | Display::InlineFlex) {
        return Some(layout_flex_element(FlexLayoutInput {
            content_height: explicit_height,
            height_bounds: (min_height, max_height),
            parent_opacity: parent.opacity,
            node,
            style,
            stylesheet,
            style_state,
            margin,
            padding,
            border,
            rect,
            content_x,
            content_y,
            content_width,
            font_size,
            viewport,
            containing_block: descendant_block,
            inherited_containing_block: containing_block,
            mode,
        }));
    }
    if matches!(style.display, Display::Table | Display::InlineTable) {
        return Some(layout_table_element(GridLayoutInput {
            content_height: explicit_height,
            height_bounds: (min_height, max_height),
            parent_opacity: parent.opacity,
            node,
            style,
            stylesheet,
            style_state,
            margin,
            padding,
            border,
            rect,
            content_x,
            content_y,
            content_width,
            font_size,
            viewport,
            containing_block: descendant_block,
            inherited_containing_block: containing_block,
            mode,
        }));
    }
    if matches!(style.display, Display::Grid | Display::InlineGrid) {
        return Some(layout_grid_element(GridLayoutInput {
            content_height: explicit_height,
            height_bounds: (min_height, max_height),
            parent_opacity: parent.opacity,
            node,
            style,
            stylesheet,
            style_state,
            margin,
            padding,
            border,
            rect,
            content_x,
            content_y,
            content_width,
            font_size,
            viewport,
            containing_block: descendant_block,
            inherited_containing_block: containing_block,
            mode,
        }));
    }
    let line_height = used_line_height(&style, font_size);
    // a box that starts a formatting context keeps a float area of its own
    let float_area = match floats {
        Some(area) if !establishes_formatting_context(&style) => area,
        _ => Rc::new(FloatArea::default()),
    };
    let owns_floats = Rc::strong_count(&float_area) == 1;
    let mut line = InlineLine {
        trailing_space: false,
        rtl: style.rtl,
        ellipsis: style.text_overflow_ellipsis && style.overflow_axes()[0].clips(),
        y: content_y,
        used: 0.0,
        height: line_height,
        left: 0.0,
        right: 0.0,
        band: line_height,
        strut: line_extent(line_height, style.font_family, font_size),
        floats: Some(float_area.clone()),
        decorations: Vec::new(),
        open_inlines: Vec::new(),
        fragments: Vec::new(),
        placed: Vec::new(),
    };
    line.fit_to_floats(content_x, content_width);
    let mut text = Vec::new();
    // ::marker, ::placeholder and ::selection style parts of this box; they are cascaded only
    // when some rule names them
    let part_style = |pseudo: PseudoElement| {
        stylesheet.style_for(StyleContext {
            document: node.document,
            node: node.node,
            pseudo: Some(pseudo),
            parent_font_size: font_size,
            parent_color: style.color,
            parent_weight: style.font_weight,
            parent_text: inherited_text(&style),
            state: style_state,
        })
    };
    let pseudo_style =
        |pseudo: PseudoElement| stylesheet.styles_pseudo(pseudo).then(|| part_style(pseudo));
    let control_value = text_control_value(node, style_state);
    let control_placeholder = control_value
        .as_deref()
        .filter(|value| value.is_empty())
        .and_then(|_| node.element().and_then(text_control_placeholder));
    let control_display_value = control_value.as_ref().map(|value| {
        node.element().map_or_else(
            || value.clone(),
            |element| text_control_display_value(element, value),
        )
    });
    let mut control_text_origin = control_value.as_ref().map(|_| (content_x, content_y));
    let letter_spacing = style
        .letter_spacing
        .resolve(font_size, font_size)
        .unwrap_or(0.0);
    let line_height = if name == "input" && control_value.is_some() {
        editor_line_height(&style, font_size, line_height)
    } else {
        line_height
    };
    if let Some(value) = control_placeholder
        .as_ref()
        .or(control_display_value.as_ref())
    {
        let value_lines = control_text_lines(name, node.element(), value, content_width, font_size);
        let color = if control_placeholder.is_some() {
            pseudo_style(PseudoElement::Placeholder)
                .map_or(Color::opaque(117, 117, 117), |placeholder| {
                    placeholder.color
                })
        } else {
            style.color
        };
        for (line_index, value_line) in value_lines.iter().enumerate() {
            text.push(TextRun {
                text: value_line.clone(),
                x: content_x,
                y: content_y
                    + line_index as f32 * line_height
                    + half_leading(line_height, style.font_family, font_size),
                width: measure_text(
                    value_line,
                    style.font_family,
                    font_size,
                    font_style(&style, font_size),
                    letter_spacing,
                ),
                font_size,
                letter_spacing,
                font_stack: style.font_family,
                color,
                font: font_style(&style, font_size),
                node: None,
                node_offset: 0,
                source_len: 0,
                rtl: false,
                decoration: Default::default(),
            });
        }
        line.y = content_y + value_lines.len() as f32 * line_height;
    }
    let mut children = Vec::new();
    let mut positioned = Vec::new();
    // the bottom margin of the last block child, which collapses with the next block's top
    let mut pending_margin: Option<f32> = None;
    let inline_context = InlineContext {
        font_size,
        line_height,
        color: text_color(&style),
        weight: style.font_weight,
        text: inherited_text(&style),
        opacity: 1.0,
        content_width,
        content_x,
        align: style.text_align,
        raise: 0.0,
    };
    let child_parent = ParentStyle {
        font_size,
        color: style.color,
        weight: style.font_weight,
        text: inherited_text(&style),
        opacity: 1.0,
    };
    let generated = node.generated_content(stylesheet, style_state);
    // an ordered list counts its items from start, or down from the item count when reversed
    let list_attribute =
        |attribute: &str| node.element().and_then(|element| element.attr(attribute));
    let reversed_list = name == "ol" && list_attribute("reversed").is_some();
    let mut list_counter = list_attribute("start")
        .and_then(|value| value.trim().parse::<i32>().ok())
        .unwrap_or_else(|| {
            if reversed_list {
                node.document
                    .element_children(node.node)
                    .filter(|child| {
                        node.document
                            .element(*child)
                            .is_some_and(|element| element.name == "li")
                    })
                    .count() as i32
            } else {
                1
            }
        });
    let mut marker_style = style;
    if style.display == Display::ListItem
        && let Some(marker) = pseudo_style(PseudoElement::Marker)
    {
        marker_style.color = marker.color;
    }
    if style.display == Display::ListItem && style.list_inside {
        match style.list_style_type.marker_text(list_ordinal) {
            Some(label) => append_inline_text(
                &format!("{label} "),
                InlineContext {
                    color: text_color(&marker_style),
                    ..inline_context
                },
                InlineSource {
                    node: None,
                    offset: 0,
                },
                &mut text,
                &mut line,
            ),
            None => {
                let size = bullet_size(font_size);
                let rect = Rect {
                    x: 0.0,
                    y: (line_height - size) / 2.0,
                    width: size,
                    height: size,
                };
                if let Some(mut bullet) = bullet_marker(&marker_style, node.node, rect, font_size) {
                    // the bullet sits on the line like a word, with a space after it
                    bullet.margin.top = rect.y;
                    bullet.margin.right = font_size * 0.5;
                    line.used += size + bullet.margin.right;
                    line.height = line.height.max(line_height);
                    line.fragments.push(InlineFragment {
                        text: String::new(),
                        width: size + bullet.margin.right,
                        font_size,
                        letter_spacing: 0.0,
                        font_stack: style.font_family,
                        color: Color::transparent(),
                        font: FontStyle::NORMAL,
                        node: None,
                        node_offset: 0,
                        source_len: 0,
                        rtl: false,
                        decoration: Default::default(),
                        line_height,
                        atomic: Some(Box::new(bullet)),
                        edge: None,
                        level: line.base_level(),
                        leading: 0.0,
                        baseline: 0.0,
                        shift: LineShift::Top,
                    });
                }
            }
        }
    }
    // a push button, select or file field shows a label of its own instead of its children
    let control = control_type(node, style_state);
    if let Some(label) = control.as_ref().and_then(control_label) {
        append_inline_text(
            &label,
            inline_context,
            InlineSource {
                node: None,
                offset: 0,
            },
            &mut text,
            &mut line,
        );
    }
    let own_children = !matches!(name, "textarea" | "select");
    for child in node.children(&generated) {
        match child {
            Child::Text(text_node, value) if own_children => {
                if !value.trim().is_empty()
                    && let Some(pending) = pending_margin.take()
                {
                    line.y += pending;
                }
                let source = InlineSource {
                    node: text_node,
                    offset: 0,
                };
                append_inline_text(value, inline_context, source, &mut text, &mut line)
            }
            Child::Text(..) => {}
            Child::Element(_) if !own_children => {}
            Child::Element(child) => {
                let child_style =
                    stylesheet.style_for(child.style_context(child_parent, style_state));
                if child_style.display == Display::None {
                    continue;
                }
                if child_style.position.is_out_of_flow() {
                    positioned.push(child);
                    continue;
                }
                if child_style.float != Float::None {
                    if let Some(pending) = pending_margin.take() {
                        line.y += pending;
                        line.fit_to_floats(content_x, content_width);
                    }
                    place_float(
                        InlineElementInput {
                            node: child,
                            parent: child_parent,
                            context: inline_context,
                            stylesheet,
                            style_state,
                            viewport,
                            containing_block: descendant_block,
                        },
                        &child_style,
                        &mut line,
                    );
                    continue;
                }
                if !child_style.display.is_inline_level() {
                    let list_ordinal = if child_style.display == Display::ListItem {
                        if let Some(value) = child
                            .element()
                            .and_then(|element| element.attr("value"))
                            .and_then(|value| value.trim().parse::<i32>().ok())
                        {
                            list_counter = value;
                        }
                        let ordinal = list_counter;
                        list_counter += if reversed_list { -1 } else { 1 };
                        ordinal
                    } else {
                        1
                    };
                    flush_line(
                        &mut line,
                        &mut text,
                        content_x,
                        content_width,
                        style.text_align,
                    );
                    children.append(&mut line.placed);
                    // adjacent vertical margins collapse into the larger one
                    let child_font_size = child_style
                        .font_size
                        .resolve(font_size, font_size)
                        .unwrap_or(font_size)
                        .max(1.0);
                    let child_top = flow_top_margin(
                        child,
                        &child_style,
                        child_font_size,
                        content_width,
                        (stylesheet, style_state),
                    );
                    let mut child_y = match pending_margin.take() {
                        Some(pending) => line.y + collapse_margins(pending, child_top) - child_top,
                        // the margin that went through this box's top edge is its own now
                        None if leading.is_some_and(|(first, _)| first == child.node) => {
                            line.y - child_top
                        }
                        None => line.y,
                    };
                    // clearance drops the border box below the floats it clears
                    if let Some(clear_to) = float_area.clearance(child_style.clear) {
                        child_y = child_y.max(clear_to - child_top);
                    }
                    // a box with its own formatting context sits beside floats, not under them
                    let (child_x, child_width) = if establishes_formatting_context(&child_style) {
                        let (left, right) =
                            float_area.insets(child_y + child_top, 1.0, content_x, content_width);
                        (content_x + left, (content_width - left - right).max(0.0))
                    } else {
                        (content_x, content_width)
                    };
                    if let Some(child_box) = layout_element(ElementLayoutInput {
                        node: child,
                        containing_x: child_x,
                        containing_y: child_y,
                        containing_width: child_width,
                        containing_height: explicit_height,
                        size_override: (None, None),
                        floats: Some(float_area.clone()),
                        list_ordinal,
                        parent: child_parent,
                        context: LayoutContext {
                            viewport,
                            containing_block: descendant_block,
                            mode: LayoutMode::Flow,
                            stylesheet,
                            style_state,
                        },
                    }) {
                        line.y = child_box.rect.bottom();
                        pending_margin = Some(child_box.margin.bottom);
                        line.used = 0.0;
                        line.height = line_height;
                        line.fragments.clear();
                        line.fit_to_floats(content_x, content_width);
                        children.push(child_box);
                    }
                } else {
                    if let Some(pending) = pending_margin.take() {
                        line.y += pending;
                    }
                    append_inline_element(
                        InlineElementInput {
                            node: child,
                            parent: child_parent,
                            context: inline_context,
                            stylesheet,
                            style_state,
                            viewport,
                            containing_block: descendant_block,
                        },
                        &mut text,
                        &mut line,
                    );
                }
            }
        }
    }
    flush_line(
        &mut line,
        &mut text,
        content_x,
        content_width,
        style.text_align,
    );
    children.append(&mut line.placed);
    if let Some(pending) = pending_margin.take() {
        // the last child's margin goes through a bottom edge that nothing holds in place
        if in_block_flow
            && margins_pass_through(node, &style)
            && border.bottom == 0.0
            && padding.bottom == 0.0
            && explicit_height.is_none()
            && min_height == 0.0
        {
            margin.bottom = collapse_margins(margin.bottom, pending);
        } else {
            line.y += pending;
        }
    }
    // a formatting context grows to hold its floats
    if owns_floats && let Some(bottom) = float_area.bottom() {
        line.y = line.y.max(bottom);
    }
    let content_height = explicit_height
        .unwrap_or_else(|| {
            (line.y - content_y)
                .max(0.0)
                .min(max_height)
                .max(min_height)
        })
        .max(0.0);
    // blink's align-content-block centres a control's content in its content box, halving the
    // layout-unit difference toward zero: buttons and a select's label never rise above the top,
    // a single-line field's editor overflows both ways
    let centring = match name {
        "button" | "select" => Some(true),
        "input" if control_value.is_some() || control.is_some() => Some(false),
        _ => None,
    };
    if let Some(safe) = centring
        && !matches!(
            control,
            Some(ControlType::Checkbox | ControlType::Radio | ControlType::File)
        )
    {
        let scale = crate::font::device_scale();
        let free = content_height - (line.y - content_y);
        let free = if safe { free.max(0.0) } else { free };
        let offset = (free * scale * 32.0).trunc() / 64.0 / scale;
        for run in &mut text {
            run.y += offset;
        }
        for child in &mut children {
            translate_layout_box(child, 0.0, offset);
        }
        if let Some(origin) = &mut control_text_origin {
            origin.1 += offset;
        }
    }
    let border_box_height = content_height + vertical_inner;
    let rect = Rect {
        height: border_box_height,
        ..rect
    };
    let selection_style = pseudo_style(PseudoElement::Selection);
    if style.display == Display::ListItem && !style.list_inside {
        children.extend(outside_marker(
            &marker_style,
            list_ordinal,
            node.node,
            (content_x, content_y),
            (font_size, line_height),
        ));
    }
    let svg = node.svg_root().map(|_| {
        let style_of = |child, parent: &Style, parent_font_size| {
            stylesheet.child_style(node.document, child, parent, parent_font_size, style_state)
        };
        let viewport = (
            border.left + padding.left,
            border.top + padding.top,
            content_width,
            content_height,
        );
        Rc::new(crate::svg::build(
            node.document,
            node.node,
            (&style, font_size),
            viewport,
            &style_of,
        ))
    });
    let mut control = control_paint(node, &style, style_state, font_size);
    if let Some(ControlPaint::Range {
        fraction,
        accent,
        styled,
        travel,
        thumb,
        ..
    }) = &mut control
    {
        if *styled {
            let (track, start, (length, width)) = slider_parts(
                node.node,
                [PseudoElement::SliderTrack, PseudoElement::SliderThumb].map(part_style),
                Rect {
                    x: content_x,
                    y: content_y,
                    width: content_width,
                    height: content_height,
                },
                (*fraction, *accent),
                (font_size, style.cursor),
            );
            children.push(track);
            *travel = (start - rect.x, length);
            *thumb = width;
        } else {
            *travel = (content_x - rect.x, (content_width - 16.0).max(0.0));
            *thumb = 16.0;
        }
    }
    let mut layout = LayoutBox {
        node: node.node,
        tag: name.to_string(),
        rect,
        background: style.background,
        backgrounds: style.layers(),
        masks: style.mask_layers(),
        object_fit: style.object_fit,
        top_layer: false,
        transform: None,
        backface_hidden: style.backface_hidden,
        clip_path: None,
        radii: resolve_corner_radii(style.border_radius, rect.width, rect.height, font_size),
        opacity: style.opacity * parent.opacity,
        shadows: resolve_shadows(style.shadows, content_width, font_size),
        filters: crate::filter::Filters::of(&style, font_size),
        border: BoxBorder::of(&style),
        outline: box_outline(&style),
        text,
        control,
        selection: selection_style
            .map(|selection| selection.background)
            .filter(|background| background.alpha > 0),
        selection_color: selection_style
            .map(|selection| selection.color)
            .filter(|color| *color != style.color),
        control_selection: node
            .own()
            .and_then(|own| style_state.text_selections.get(&own).copied()),
        control_focused: node.own().is_some() && style_state.focused == node.own(),
        control_text_origin,
        control_value,
        children,
        position: style.position,
        visual_offset: (0.0, 0.0),
        z_index: positioned_z_index(style),
        creates_stacking_context: creates_stacking_context(style)
            || style.opacity * parent.opacity < 1.0,
        sticky_inset: style.inset,
        font_size,
        overflow: style.overflow,
        overflow_axes: style.overflow_axes(),
        visible: style.visibility == Visibility::Visible,
        pointer_events: style.pointer_events,
        selectable: style.selectable,
        cursor: style.cursor,
        floated: false,
        inline: false,
        scroll_content: Size::default(),
        scroll_offset: (0.0, 0.0),
        svg,
        scrollbar: scrollbar_for(
            stylesheet,
            (node.document, node.node),
            &style,
            font_size,
            style_state,
        ),
        margin,
    };
    // a checked box fills with the accent color, and a checked radio rings with it, unless
    // the page painted the box itself
    let unstyled = layout.background == Color::WHITE;
    match layout.control {
        Some(ControlPaint::Checkbox {
            checked: true,
            accent,
        }) if unstyled => {
            layout.background = accent;
            layout.border.colors = [accent; 4];
        }
        Some(ControlPaint::Radio {
            checked: true,
            accent,
        }) if unstyled => layout.border.colors = [accent; 4],
        _ => {}
    }
    if mode != LayoutMode::Flow {
        position_layout_box(&mut layout, style, mode, position_block, font_size);
    }
    apply_relative_offset(&mut layout, style, containing_block, font_size);
    apply_transform(&mut layout, &style, font_size);
    let positioned_block = padding_block(&layout);
    append_positioned_nodes(
        &mut layout.children,
        positioned,
        PositionedLayoutContext {
            containing_block: if style.position == Position::Static {
                containing_block
            } else {
                positioned_block
            },
            viewport: viewport_block(viewport),
            viewport_size: viewport,
            parent: child_parent,
            stylesheet,
            style_state,
        },
    );
    finalize_scroll(&mut layout, padding, style_state);
    Some(layout)
}

// a flex or grid item: an element child, or a run of text the container wraps in an anonymous box
enum FlexSource<'a> {
    Element(Subject<'a>),
    Text(Vec<(Option<NodeId>, &'a str)>),
}

struct FlexItem<'a> {
    source: FlexSource<'a>,
    style: Style,
    font_size: f32,
    margin: ResolvedEdges,
    padding: ResolvedEdges,
    border: ResolvedEdges,
    // auto margins in main-start, main-end, cross-start and cross-end order
    auto_margins: [bool; 4],
    // sizes along the main axis, all content-box
    base: f32,
    hypothetical: f32,
    min_main: f32,
    max_main: f32,
    target: f32,
    frozen: bool,
    layout: Option<LayoutBox>,
}

impl FlexItem<'_> {
    fn main_inner(&self, row: bool) -> f32 {
        if row {
            self.padding.left + self.padding.right + self.border.left + self.border.right
        } else {
            self.padding.top + self.padding.bottom + self.border.top + self.border.bottom
        }
    }

    fn cross_inner(&self, row: bool) -> f32 {
        self.main_inner(!row)
    }

    fn main_margins(&self, row: bool) -> f32 {
        if row {
            self.margin.left + self.margin.right
        } else {
            self.margin.top + self.margin.bottom
        }
    }

    fn cross_margins(&self, row: bool) -> f32 {
        self.main_margins(!row)
    }

    fn outer_hypothetical(&self, row: bool) -> f32 {
        self.hypothetical + self.main_inner(row) + self.main_margins(row)
    }

    fn outer_target(&self, row: bool) -> f32 {
        self.target + self.main_inner(row) + self.main_margins(row)
    }

    // the border-box cross size of its current layout
    fn laid_cross(&self, row: bool) -> f32 {
        self.layout.as_ref().map_or(0.0, |layout| {
            if row {
                layout.rect.height
            } else {
                layout.rect.width
            }
        })
    }

    fn align(&self, container: AlignItems) -> AlignItems {
        self.style.align_self.unwrap_or(container)
    }
}

// a box with no style of its own, carrying text an anonymous item or line holds
fn anonymous_box(
    node: NodeId,
    rect: Rect,
    text: Vec<TextRun>,
    (font_size, cursor): (f32, Cursor),
    opacity: f32,
) -> LayoutBox {
    LayoutBox {
        node,
        tag: "::text".to_string(),
        rect,
        background: Color::transparent(),
        backgrounds: Vec::new(),
        masks: Vec::new(),
        object_fit: crate::css::ObjectFit::Fill,
        top_layer: false,
        transform: None,
        backface_hidden: false,
        clip_path: None,
        radii: SQUARE_CORNERS,
        opacity,
        shadows: Vec::new(),
        filters: crate::filter::Filters::default(),
        border: BoxBorder::NONE,
        outline: None,
        text,
        control_value: None,
        control_selection: None,
        control_focused: false,
        control_text_origin: None,
        children: Vec::new(),
        position: Position::Static,
        visual_offset: (0.0, 0.0),
        z_index: 0,
        creates_stacking_context: false,
        sticky_inset: Edges::auto(),
        font_size,
        overflow: Overflow::Visible,
        overflow_axes: [Overflow::Visible; 2],
        visible: true,
        pointer_events: true,
        selectable: true,
        cursor,
        floated: false,
        inline: false,
        control: None,
        selection: None,
        selection_color: None,
        scroll_content: Size::default(),
        scroll_offset: (0.0, 0.0),
        svg: None,
        scrollbar: Scrollbar::default(),
        margin: ResolvedEdges::default(),
    }
}

// a slider or scrollbar part, drawn as a box of its own style that belongs to the element
pub(crate) fn part_box(
    node: NodeId,
    pseudo: PseudoElement,
    style: &Style,
    rect: Rect,
    (font_size, cursor): (f32, Cursor),
) -> LayoutBox {
    LayoutBox {
        tag: pseudo.tag().to_string(),
        background: style.background,
        backgrounds: style.layers(),
        radii: resolve_corner_radii(style.border_radius, rect.width, rect.height, font_size),
        shadows: resolve_shadows(style.shadows, rect.width, font_size),
        filters: crate::filter::Filters::of(style, font_size),
        border: BoxBorder::of(style),
        // a filtered part paints through a layer of its own
        creates_stacking_context: style.filter.is_some(),
        visible: style.visibility == Visibility::Visible,
        ..anonymous_box(node, rect, Vec::new(), (font_size, cursor), style.opacity)
    }
}

// the track and thumb of a range input under appearance: none, placed as chromium does: the
// track centered in the content box, the thumb a child of it that travels its content width;
// returns the track, where the thumb's travel starts, and the travel and thumb widths
fn slider_parts(
    node: NodeId,
    [track_style, thumb_style]: [Style; 2],
    content: Rect,
    (fraction, accent): (f32, Color),
    (font_size, cursor): (f32, Cursor),
) -> (LayoutBox, f32, (f32, f32)) {
    let inset = |style: &Style| {
        let padding = resolve_edges(style.padding, content.width, font_size);
        let border = used_border(style);
        ResolvedEdges {
            top: padding.top + border.top,
            right: padding.right + border.right,
            bottom: padding.bottom + border.bottom,
            left: padding.left + border.left,
        }
    };
    let border_box = |style: &Style, length: Length, containing: f32, inner: f32| {
        length
            .resolve(containing, font_size)
            .map(|size| match style.box_sizing {
                crate::css::BoxSizing::BorderBox => size.max(inner),
                crate::css::BoxSizing::ContentBox => size.max(0.0) + inner,
            })
    };
    let track_inset = inset(&track_style);
    let track_inner = (content.width - track_inset.left - track_inset.right).max(0.0);
    let thumb_inset = inset(&thumb_style);
    // a thumb left at appearance: auto keeps the platform's circle
    let native_thumb = !thumb_style.appearance_none;
    let thumb_size = |length: Length, containing: f32, inner: f32| {
        if native_thumb {
            16.0
        } else {
            border_box(&thumb_style, length, containing, inner).unwrap_or(inner)
        }
    };
    let thumb_width = thumb_size(
        thumb_style.width,
        track_inner,
        thumb_inset.left + thumb_inset.right,
    );
    let thumb_height = thumb_size(
        thumb_style.height,
        0.0,
        thumb_inset.top + thumb_inset.bottom,
    );
    let track_vertical = track_inset.top + track_inset.bottom;
    let track_height = border_box(
        &track_style,
        track_style.height,
        content.height,
        track_vertical,
    )
    .unwrap_or(thumb_height + track_vertical);
    let track = Rect {
        y: content.y + (content.height - track_height) / 2.0,
        height: track_height,
        ..content
    };
    let margin = resolve_margins(thumb_style.margin, track_inner, font_size);
    let start = track.x + track_inset.left + margin.left;
    let travel = (track_inner - margin.left - margin.right - thumb_width).max(0.0);
    let mut thumb = part_box(
        node,
        PseudoElement::SliderThumb,
        &thumb_style,
        Rect {
            x: start + fraction * travel,
            y: track.y + track_inset.top + margin.top,
            width: thumb_width,
            height: thumb_height,
        },
        (font_size, cursor),
    );
    if native_thumb {
        thumb.background = accent;
        thumb.radii = [(8.0, 8.0); 4];
    }
    let mut track = part_box(
        node,
        PseudoElement::SliderTrack,
        &track_style,
        track,
        (font_size, cursor),
    );
    track.children.push(thumb);
    (track, start, (travel, thumb_width))
}

// lays out a run of text at a width, as an anonymous block
fn layout_text_item(
    pieces: &[(Option<NodeId>, &str)],
    node: NodeId,
    width: f32,
    context: InlineContext,
    opacity: f32,
) -> LayoutBox {
    let context = InlineContext {
        content_x: 0.0,
        content_width: width,
        ..context
    };
    let mut line = InlineLine {
        y: 0.0,
        used: 0.0,
        height: context.line_height,
        left: 0.0,
        right: 0.0,
        band: context.line_height,
        strut: line_extent(context.line_height, context.text.family, context.font_size),
        floats: None,
        decorations: Vec::new(),
        open_inlines: Vec::new(),
        trailing_space: false,
        rtl: context.text.rtl,
        ellipsis: false,
        fragments: Vec::new(),
        placed: Vec::new(),
    };
    let mut text = Vec::new();
    for &(text_node, value) in pieces {
        append_inline_text(
            value,
            context,
            InlineSource {
                node: text_node,
                offset: 0,
            },
            &mut text,
            &mut line,
        );
    }
    flush_line(&mut line, &mut text, 0.0, width, context.align);
    let rect = Rect {
        x: 0.0,
        y: 0.0,
        width,
        height: line.y.max(0.0),
    };
    anonymous_box(
        node,
        rect,
        text,
        (context.font_size, context.text.cursor),
        opacity,
    )
}

// the widest and narrowest a run of text can be laid out
fn text_intrinsic_widths(pieces: &[(Option<NodeId>, &str)], context: InlineContext) -> (f32, f32) {
    let letter_spacing = context
        .text
        .letter_spacing
        .resolve(context.font_size, context.font_size)
        .unwrap_or(0.0);
    let font = context.font();
    let joined = pieces
        .iter()
        .map(|(_, value)| *value)
        .collect::<Vec<_>>()
        .join("");
    let joined = context.text.transform.apply(&joined);
    let measure = |value: &str| {
        measure_text(
            value,
            context.text.family,
            context.font_size,
            font,
            letter_spacing,
        )
    };
    text_extents(&joined, &context.text, &measure)
}

// a text's min-content width, its widest unbreakable piece, and its max-content width, its
// widest line, as its white-space and word breaking settings shape them
fn text_extents(value: &str, text: &InheritedText, measure: &dyn Fn(&str) -> f32) -> (f32, f32) {
    let preserve = matches!(
        text.space_collapse,
        SpaceCollapse::Preserve | SpaceCollapse::BreakSpaces
    );
    let lines = if preserve || text.space_collapse == SpaceCollapse::PreserveBreaks {
        value.split('\n').collect::<Vec<_>>()
    } else {
        vec![value]
    };
    let shown = |line: &str| {
        if preserve {
            measure(line)
        } else {
            measure(&line.split_ascii_whitespace().collect::<Vec<_>>().join(" "))
        }
    };
    let widest = lines.iter().map(|line| shown(line)).fold(0.0, f32::max);
    if !text.wrap {
        return (widest, widest);
    }
    if text.overflow_wrap == OverflowWrap::Anywhere {
        let widest_character = value
            .chars()
            .filter(|character| !character.is_whitespace())
            .map(|character| measure(character.encode_utf8(&mut [0; 4])))
            .fold(0.0, f32::max);
        return (widest_character, widest);
    }
    let mut narrowest = 0.0_f32;
    for word in value.split_ascii_whitespace() {
        let simple = !may_break_inside(word) && text.word_break != WordBreak::BreakAll;
        if simple {
            narrowest = narrowest.max(measure(word));
            continue;
        }
        let mut start = 0;
        for (offset, _) in break_opportunities(word, text.word_break) {
            narrowest = narrowest.max(measure(&word[start..offset]));
            start = offset;
        }
        narrowest = narrowest.max(measure(&word[start..]));
    }
    (narrowest, widest)
}

// the free space a line leaves, shared out as justify-content asks: an offset before the
// first item and extra space between each pair
fn justify_offsets(justify: JustifyContent, free: f32, count: usize) -> (f32, f32) {
    if count == 0 {
        return (0.0, 0.0);
    }
    let count_f = count as f32;
    match justify {
        JustifyContent::Center => (free / 2.0, 0.0),
        JustifyContent::FlexEnd => (free, 0.0),
        JustifyContent::SpaceBetween if count > 1 && free > 0.0 => (0.0, free / (count_f - 1.0)),
        JustifyContent::SpaceAround if free > 0.0 => (free / count_f / 2.0, free / count_f),
        JustifyContent::SpaceEvenly if free > 0.0 => {
            (free / (count_f + 1.0), free / (count_f + 1.0))
        }
        // with no room to share, the distributed values fall back to start or center
        JustifyContent::SpaceAround | JustifyContent::SpaceEvenly => (free / 2.0, 0.0),
        _ => (0.0, 0.0),
    }
}

fn align_offset(align: AlignItems, available: f32, item: f32) -> f32 {
    match align {
        AlignItems::Center => (available - item) / 2.0,
        AlignItems::FlexEnd => available - item,
        AlignItems::FlexStart | AlignItems::Stretch | AlignItems::Baseline => 0.0,
    }
}

// how far below its border-box top a box's first baseline sits; a box with no line of text
// takes its bottom edge
fn baseline_offset(layout: &LayoutBox) -> f32 {
    first_baseline(layout).map_or(layout.rect.height, |baseline| baseline - layout.rect.y)
}

fn first_baseline(layout: &LayoutBox) -> Option<f32> {
    baselines(layout).reduce(f32::min)
}

fn last_baseline(layout: &LayoutBox) -> Option<f32> {
    baselines(layout).reduce(f32::max)
}

fn baselines(layout: &LayoutBox) -> Box<dyn Iterator<Item = f32> + '_> {
    let fonts = crate::font::active_fonts();
    let own = layout
        .text
        .iter()
        .map(move |run| run.y + fonts.ascent(run.font_stack, run.font_size));
    let nested = layout
        .children
        .iter()
        .filter(|child| !child.position.is_out_of_flow())
        .flat_map(baselines);
    Box::new(own.chain(nested))
}

// the part of a line box a font takes above and below its baseline
fn line_extent(line_height: f32, stack: FontStack, font_size: f32) -> (f32, f32) {
    let above = half_leading(line_height, stack, font_size)
        + crate::font::active_fonts().ascent(stack, font_size);
    (above, line_height - above)
}

// where vertical-align puts a box of this height and baseline against its parent's text,
// with percentages of the box's own line height
fn vertical_shift(
    align: VerticalAlign,
    parent: &InlineContext,
    (height, baseline): (f32, f32),
    (font_size, line_height): (f32, f32),
) -> LineShift {
    let fonts = crate::font::active_fonts();
    let (family, size) = (parent.text.family, parent.font_size);
    let ascent = fonts.ascent(family, size);
    let raise = match align {
        VerticalAlign::Top => return LineShift::Top,
        VerticalAlign::Bottom => return LineShift::Bottom,
        VerticalAlign::Baseline => 0.0,
        VerticalAlign::Sub => -(size / 5.0 + 1.0),
        VerticalAlign::Super => size / 3.0 + 1.0,
        VerticalAlign::TextTop => ascent - baseline,
        VerticalAlign::TextBottom => {
            height - baseline - (fonts.content_height(family, size) - ascent)
        }
        VerticalAlign::Middle => height / 2.0 - baseline + fonts.x_height(family, size) / 2.0,
        VerticalAlign::Raise(length) => length.resolve(line_height, font_size).unwrap_or(0.0),
    };
    LineShift::Raise(parent.raise + raise)
}

// resolves flexible lengths for one line, as CSS Flexbox section 9.7 describes
fn resolve_flexible_lengths(items: &mut [&mut FlexItem<'_>], available: f32, gaps: f32, row: bool) {
    let used: f32 = items
        .iter()
        .map(|item| item.outer_hypothetical(row))
        .sum::<f32>()
        + gaps;
    let growing = used < available;
    for item in items.iter_mut() {
        let factor = if growing {
            item.style.flex_grow
        } else {
            item.style.flex_shrink
        };
        item.target = item.hypothetical;
        item.frozen = factor == 0.0
            || (growing && item.base > item.hypothetical)
            || (!growing && item.base < item.hypothetical);
    }
    let initial_free = available
        - gaps
        - items
            .iter()
            .map(|item| {
                let size = if item.frozen { item.target } else { item.base };
                size + item.main_inner(row) + item.main_margins(row)
            })
            .sum::<f32>();
    loop {
        if items.iter().all(|item| item.frozen) {
            break;
        }
        let mut free = available
            - gaps
            - items
                .iter()
                .map(|item| {
                    let size = if item.frozen { item.target } else { item.base };
                    size + item.main_inner(row) + item.main_margins(row)
                })
                .sum::<f32>();
        let factor_sum: f32 = items
            .iter()
            .filter(|item| !item.frozen)
            .map(|item| {
                if growing {
                    item.style.flex_grow
                } else {
                    item.style.flex_shrink
                }
            })
            .sum();
        // factors under one only take that share of the space
        if factor_sum < 1.0 {
            let scaled = initial_free * factor_sum;
            if scaled.abs() < free.abs() {
                free = scaled;
            }
        }
        if growing {
            for item in items.iter_mut().filter(|item| !item.frozen) {
                item.target =
                    item.base + free * item.style.flex_grow / factor_sum.max(f32::EPSILON);
            }
        } else {
            let scaled_sum: f32 = items
                .iter()
                .filter(|item| !item.frozen)
                .map(|item| item.style.flex_shrink * item.base)
                .sum();
            for item in items.iter_mut().filter(|item| !item.frozen) {
                let share = if scaled_sum > 0.0 {
                    item.style.flex_shrink * item.base / scaled_sum
                } else {
                    0.0
                };
                item.target = item.base + free * share;
            }
        }
        // clamp to min and max, then freeze the side that violated most
        let mut total_violation = 0.0;
        let mut violations = Vec::with_capacity(items.len());
        for item in items.iter_mut() {
            if item.frozen {
                violations.push(0.0);
                continue;
            }
            let clamped = item
                .target
                .min(item.max_main)
                .max(item.min_main.min(item.max_main));
            let clamped = clamped.max(0.0);
            violations.push(clamped - item.target);
            total_violation += clamped - item.target;
            item.target = clamped;
        }
        for (item, violation) in items.iter_mut().zip(violations) {
            if item.frozen {
                continue;
            }
            let freeze = if total_violation == 0.0 {
                true
            } else if total_violation > 0.0 {
                violation > 0.0
            } else {
                violation < 0.0
            };
            if freeze {
                item.frozen = true;
            }
        }
    }
}

fn layout_flex_element(input: FlexLayoutInput<'_>) -> LayoutBox {
    let _profile = crate::profile::scope("layout: flex");
    let FlexLayoutInput {
        content_height,
        height_bounds,
        parent_opacity,
        node,
        style,
        stylesheet,
        style_state,
        margin,
        padding,
        border,
        rect,
        content_x,
        content_y,
        content_width,
        font_size,
        viewport,
        containing_block,
        inherited_containing_block,
        mode,
    } = input;
    let row = style.flex_direction.is_row();
    let child_parent = ParentStyle {
        font_size,
        color: style.color,
        weight: style.font_weight,
        text: inherited_text(&style),
        opacity: 1.0,
    };
    let line_height = used_line_height(&style, font_size);
    let text_context = InlineContext {
        font_size,
        line_height,
        color: text_color(&style),
        weight: style.font_weight,
        text: inherited_text(&style),
        opacity: 1.0,
        content_width,
        content_x: 0.0,
        align: style.text_align,
        raise: 0.0,
    };
    let box_opacity = style.opacity * parent_opacity;
    let item_block = if style.position == Position::Static {
        inherited_containing_block
    } else {
        containing_block
    };
    let main_gap = if row { style.gap } else { style.row_gap }
        .resolve(content_width, font_size)
        .unwrap_or(0.0)
        .max(0.0);
    let cross_gap = if row { style.row_gap } else { style.gap }
        .resolve(content_width, font_size)
        .unwrap_or(0.0)
        .max(0.0);
    let available_main = if row {
        Some(content_width)
    } else {
        content_height
    };
    let available_cross = if row {
        content_height
    } else {
        Some(content_width)
    };

    // the items, element children and runs of text, in order-modified document order
    let generated = node.generated_content(stylesheet, style_state);
    let mut sources = Vec::new();
    let mut positioned = Vec::new();
    let mut run: Vec<(Option<NodeId>, &str)> = Vec::new();
    for child in node.children(&generated) {
        match child {
            Child::Text(text_node, value) => run.push((text_node, value)),
            Child::Element(child) => {
                let child_style =
                    stylesheet.style_for(child.style_context(child_parent, style_state));
                if child_style.display == Display::None {
                    continue;
                }
                if run.iter().any(|(_, value)| !value.trim().is_empty()) {
                    sources.push((FlexSource::Text(std::mem::take(&mut run)), None));
                }
                run.clear();
                if child_style.position.is_out_of_flow() {
                    positioned.push(child);
                    continue;
                }
                sources.push((FlexSource::Element(child), Some(child_style)));
            }
        }
    }
    if run.iter().any(|(_, value)| !value.trim().is_empty()) {
        sources.push((FlexSource::Text(run), None));
    }
    // an anonymous item inherits text styles and none of the container's box
    let mut anonymous_style = style;
    anonymous_style.margin = Edges::zero();
    anonymous_style.padding = Edges::zero();
    anonymous_style.width = Length::Auto;
    anonymous_style.height = Length::Auto;
    anonymous_style.min_width = Length::Auto;
    anonymous_style.max_width = Length::Auto;
    anonymous_style.min_height = Length::Auto;
    anonymous_style.max_height = Length::Auto;
    anonymous_style.box_sizing = crate::css::BoxSizing::ContentBox;
    anonymous_style.border = [BorderSide::INITIAL; 4];
    anonymous_style.overflow = Overflow::Visible;
    anonymous_style.flex_grow = 0.0;
    anonymous_style.flex_shrink = 1.0;
    anonymous_style.flex_basis = Length::Auto;
    anonymous_style.order = 0;
    anonymous_style.align_self = None;
    let mut items = Vec::with_capacity(sources.len());
    for (source, child_style) in sources {
        let child_style = child_style.unwrap_or(anonymous_style);
        let child_font_size = child_style
            .font_size
            .resolve(font_size, font_size)
            .unwrap_or(font_size)
            .max(1.0);
        let child_margin = resolve_margins(child_style.margin, content_width, child_font_size);
        let child_padding = resolve_edges(child_style.padding, content_width, child_font_size);
        let child_border = used_border(&child_style);
        let auto = |length: Length| length == Length::Auto;
        let edges = child_style.margin;
        let auto_margins = if row {
            [
                auto(edges.left),
                auto(edges.right),
                auto(edges.top),
                auto(edges.bottom),
            ]
        } else {
            [
                auto(edges.top),
                auto(edges.bottom),
                auto(edges.left),
                auto(edges.right),
            ]
        };
        items.push(FlexItem {
            source,
            style: child_style,
            font_size: child_font_size,
            margin: child_margin,
            padding: child_padding,
            border: child_border,
            auto_margins,
            base: 0.0,
            hypothetical: 0.0,
            min_main: 0.0,
            max_main: f32::INFINITY,
            target: 0.0,
            frozen: false,
            layout: None,
        });
    }
    items.sort_by_key(|item| item.style.order);

    // lays an item out with a content-box main size and, when known, a cross size
    let lay_out =
        |item: &FlexItem<'_>, main: Option<f32>, cross: Option<f32>| -> Option<LayoutBox> {
            let (width, height) = if row { (main, cross) } else { (cross, main) };
            match &item.source {
                FlexSource::Text(pieces) => {
                    let width =
                        width.unwrap_or_else(|| text_intrinsic_widths(pieces, text_context).1);
                    let mut layout =
                        layout_text_item(pieces, node.node, width, text_context, box_opacity);
                    if let Some(height) = height {
                        layout.rect.height = height;
                    }
                    Some(layout)
                }
                FlexSource::Element(child) => layout_element(ElementLayoutInput {
                    node: *child,
                    containing_x: 0.0,
                    containing_y: 0.0,
                    containing_width: width.map_or(content_width, |width| {
                        width
                            + item.padding.left
                            + item.padding.right
                            + item.border.left
                            + item.border.right
                            + item.margin.left
                            + item.margin.right
                    }),
                    containing_height: if row { available_cross } else { available_main },
                    size_override: (width, height),
                    floats: None,
                    list_ordinal: 1,
                    parent: child_parent,
                    context: LayoutContext {
                        viewport,
                        containing_block: item_block,
                        mode: LayoutMode::Flow,
                        stylesheet,
                        style_state,
                    },
                }),
            }
        };
    // the content widths an item takes at its widest and narrowest
    let intrinsic_widths = |item: &FlexItem<'_>| -> (f32, f32) {
        match &item.source {
            FlexSource::Text(pieces) => text_intrinsic_widths(pieces, text_context),
            FlexSource::Element(child) => {
                let extras = item.padding.left
                    + item.padding.right
                    + item.border.left
                    + item.border.right
                    + item.margin.left
                    + item.margin.right;
                let measure = |minimum: bool| {
                    (intrinsic_width(
                        MaxContentInput {
                            node: *child,
                            stylesheet,
                            style_state,
                            parent: child_parent,
                            containing_width: content_width,
                            own_width: false,
                        },
                        minimum,
                    ) - extras)
                        .max(0.0)
                };
                (measure(true), measure(false))
            }
        }
    };

    // flex base and hypothetical main sizes
    for item in &mut items {
        let inner = item.main_inner(row);
        let border_box = item.style.box_sizing == crate::css::BoxSizing::BorderBox;
        let to_content = |size: f32| {
            if border_box {
                (size - inner).max(0.0)
            } else {
                size
            }
        };
        let resolve_main = |length: Length| {
            if length.has_percent() {
                available_main.and_then(|basis| length.resolve(basis, item.font_size))
            } else {
                length.resolve(0.0, item.font_size)
            }
        };
        let main_property = if row {
            item.style.width
        } else {
            item.style.height
        };
        let (min_content, max_content) = intrinsic_widths(item);
        item.base =
            match resolve_main(item.style.flex_basis).or_else(|| resolve_main(main_property)) {
                Some(size) => to_content(size),
                None if row && item.style.content_size[0] == ContentSize::Min => min_content,
                None if row => max_content,
                None => {
                    let cross =
                        (content_width - item.cross_margins(row) - item.cross_inner(row)).max(0.0);
                    lay_out(item, None, Some(cross))
                        .map_or(0.0, |layout| (layout.rect.height - inner).max(0.0))
                }
            };
        let min_property = if row {
            item.style.min_width
        } else {
            item.style.min_height
        };
        let max_property = if row {
            item.style.max_width
        } else {
            item.style.max_height
        };
        // an auto minimum keeps an item from shrinking below its longest word
        item.min_main = match resolve_main(min_property) {
            Some(size) => to_content(size),
            None if row && item.style.overflow == Overflow::Visible => {
                let specified = resolve_main(main_property).map(to_content);
                specified.map_or(min_content, |specified| specified.min(min_content))
            }
            None => 0.0,
        };
        item.max_main = resolve_main(max_property).map_or(f32::INFINITY, to_content);
        item.hypothetical = item.base.min(item.max_main).max(item.min_main);
        item.target = item.hypothetical;
    }

    // collect items into lines
    let wrap = style.flex_wrap != crate::css::FlexWrap::NoWrap;
    let mut lines: Vec<Vec<usize>> = Vec::new();
    match available_main.filter(|_| wrap) {
        Some(limit) => {
            let mut current = Vec::new();
            let mut used = 0.0;
            for (index, item) in items.iter().enumerate() {
                let outer = item.outer_hypothetical(row);
                let gap = if current.is_empty() { 0.0 } else { main_gap };
                if !current.is_empty() && used + gap + outer > limit {
                    lines.push(std::mem::take(&mut current));
                    used = 0.0;
                    current.push(index);
                    used += outer;
                } else {
                    current.push(index);
                    used += gap + outer;
                }
            }
            if !current.is_empty() {
                lines.push(current);
            }
        }
        None => lines.push((0..items.len()).collect()),
    }

    // resolve flexible lengths line by line
    let line_main_size = |items: &[FlexItem<'_>], line: &[usize]| {
        line.iter()
            .map(|&index| items[index].outer_hypothetical(row))
            .sum::<f32>()
            + main_gap * line.len().saturating_sub(1) as f32
    };
    let content_main = lines
        .iter()
        .map(|line| line_main_size(&items, line))
        .fold(0.0, f32::max);
    let container_main = available_main.unwrap_or(if row {
        content_main
    } else {
        content_main.min(height_bounds.1).max(height_bounds.0)
    });
    // a column whose min-height or max-height moved its height flexes its items into it
    let main_definite = available_main.is_some() || container_main != content_main;
    for line in &lines {
        let gaps = main_gap * line.len().saturating_sub(1) as f32;
        let mut line_items = items
            .iter_mut()
            .enumerate()
            .filter(|(index, _)| line.contains(index))
            .map(|(_, item)| item)
            .collect::<Vec<_>>();
        if main_definite {
            resolve_flexible_lengths(&mut line_items, container_main, gaps, row);
        }
    }

    // hypothetical cross sizes: lay out at the resolved main size
    let single_line = lines.len() == 1 && !wrap;
    for item in &mut items {
        let explicit_cross = if row {
            resolve_height(item.style.height, available_cross, item.font_size)
        } else {
            item.style.width.resolve(content_width, item.font_size)
        };
        let cross = match explicit_cross {
            Some(_) => None,
            None if !row => {
                // a column item without a width is stretched or shrinks to its content
                let stretch = item.align(style.align_items) == AlignItems::Stretch
                    && !item.auto_margins[2]
                    && !item.auto_margins[3];
                let available =
                    (content_width - item.cross_margins(row) - item.cross_inner(row)).max(0.0);
                Some(if stretch {
                    available
                } else {
                    intrinsic_widths(item).1.min(available)
                })
            }
            None => None,
        };
        item.layout = lay_out(item, Some(item.target), cross);
    }

    // line cross sizes
    let mut line_cross = lines
        .iter()
        .map(|line| {
            line.iter()
                .map(|&index| items[index].laid_cross(row) + items[index].cross_margins(row))
                .fold(0.0, f32::max)
        })
        .collect::<Vec<_>>();
    let bound_cross = |cross: f32| {
        if row {
            cross.min(height_bounds.1).max(height_bounds.0)
        } else {
            cross
        }
    };
    if single_line {
        line_cross[0] = available_cross.unwrap_or_else(|| bound_cross(line_cross[0]));
    }
    let cross_used =
        line_cross.iter().sum::<f32>() + cross_gap * lines.len().saturating_sub(1) as f32;
    let container_cross = available_cross.unwrap_or_else(|| bound_cross(cross_used));
    // align-content shares leftover cross space among lines
    let free_cross = container_cross - cross_used;
    let (mut cross_cursor, line_extra) = if lines.len() > 1 || wrap {
        match style.align_content {
            crate::css::AlignContent::Stretch if free_cross > 0.0 => {
                let extra = free_cross / lines.len() as f32;
                for size in &mut line_cross {
                    *size += extra;
                }
                (0.0, 0.0)
            }
            crate::css::AlignContent::Stretch | crate::css::AlignContent::FlexStart => (0.0, 0.0),
            crate::css::AlignContent::FlexEnd => (free_cross, 0.0),
            crate::css::AlignContent::Center => (free_cross / 2.0, 0.0),
            crate::css::AlignContent::SpaceBetween => {
                justify_offsets(JustifyContent::SpaceBetween, free_cross, lines.len())
            }
            crate::css::AlignContent::SpaceAround => {
                justify_offsets(JustifyContent::SpaceAround, free_cross, lines.len())
            }
            crate::css::AlignContent::SpaceEvenly => {
                justify_offsets(JustifyContent::SpaceEvenly, free_cross, lines.len())
            }
        }
    } else {
        (0.0, 0.0)
    };
    let wrap_reverse = style.flex_wrap == crate::css::FlexWrap::WrapReverse;
    let line_order = if wrap_reverse {
        (0..lines.len()).rev().collect::<Vec<_>>()
    } else {
        (0..lines.len()).collect::<Vec<_>>()
    };

    let mut children = Vec::with_capacity(items.len());
    for line_index in line_order {
        let line = &lines[line_index];
        let cross_size = line_cross[line_index];
        // stretched items take the line's cross size
        for &index in line {
            let item = &mut items[index];
            let explicit_cross = if row {
                item.style.height != Length::Auto
            } else {
                item.style.width != Length::Auto
            };
            let stretch = item.align(style.align_items) == AlignItems::Stretch
                && !explicit_cross
                && !item.auto_margins[2]
                && !item.auto_margins[3];
            if stretch && row {
                let cross = (cross_size - item.cross_margins(row) - item.cross_inner(row)).max(0.0);
                item.layout = lay_out(item, Some(item.target), Some(cross));
            }
        }
        let used = line
            .iter()
            .map(|&index| items[index].outer_target(row))
            .sum::<f32>()
            + main_gap * line.len().saturating_sub(1) as f32;
        let mut free = container_main - used;
        let auto_count = line
            .iter()
            .map(|&index| {
                let margins = items[index].auto_margins;
                usize::from(margins[0]) + usize::from(margins[1])
            })
            .sum::<usize>();
        // auto margins take positive free space before justify-content sees it
        let auto_share = if auto_count > 0 && free > 0.0 {
            let share = free / auto_count as f32;
            free = 0.0;
            share
        } else {
            0.0
        };
        let (start, between) = justify_offsets(style.justify_content, free, line.len());
        // in a row, baseline-aligned items share the lowest of their baselines
        let line_baseline = line
            .iter()
            .map(|&index| &items[index])
            .filter(|item| row && item.align(style.align_items) == AlignItems::Baseline)
            .filter_map(|item| Some(item.margin.top + baseline_offset(item.layout.as_ref()?)))
            .fold(0.0_f32, f32::max);
        let mut cursor = start;
        for &index in line {
            let item = &mut items[index];
            let Some(mut child) = item.layout.take() else {
                continue;
            };
            let (main_start_margin, main_end_margin) = if row {
                (item.margin.left, item.margin.right)
            } else {
                (item.margin.top, item.margin.bottom)
            };
            let (cross_start_margin, cross_end_margin) = if row {
                (item.margin.top, item.margin.bottom)
            } else {
                (item.margin.left, item.margin.right)
            };
            let start_auto = if item.auto_margins[0] {
                auto_share
            } else {
                0.0
            };
            let end_auto = if item.auto_margins[1] {
                auto_share
            } else {
                0.0
            };
            let main_size = if row {
                child.rect.width
            } else {
                child.rect.height
            };
            let cross_box = if row {
                child.rect.height
            } else {
                child.rect.width
            };
            let outer_cross = cross_box + cross_start_margin + cross_end_margin;
            let cross_offset = match (item.auto_margins[2], item.auto_margins[3]) {
                (true, true) => ((cross_size - outer_cross) / 2.0).max(0.0),
                (true, false) => (cross_size - outer_cross).max(0.0),
                (false, true) => 0.0,
                (false, false) if row && item.align(style.align_items) == AlignItems::Baseline => {
                    line_baseline - cross_start_margin - baseline_offset(&child)
                }
                (false, false) => {
                    align_offset(item.align(style.align_items), cross_size, outer_cross)
                }
            };
            let main_position = cursor + start_auto + main_start_margin;
            // a reversed axis runs from the main end
            let main_position = if style.flex_direction.is_reverse() {
                container_main - main_position - main_size
            } else {
                main_position
            };
            let cross_position = cross_cursor + cross_offset + cross_start_margin;
            let (desired_x, desired_y) = if row {
                (content_x + main_position, content_y + cross_position)
            } else {
                (content_x + cross_position, content_y + main_position)
            };
            let (offset_x, offset_y) = (desired_x - child.rect.x, desired_y - child.rect.y);
            translate_layout_box(&mut child, offset_x, offset_y);
            cursor += main_start_margin
                + main_size
                + main_end_margin
                + start_auto
                + end_auto
                + main_gap
                + between;
            children.push(child);
        }
        cross_cursor += cross_size + cross_gap + line_extra;
    }
    let content_height = if row { container_cross } else { container_main };
    let rect = Rect {
        height: content_height + padding.top + padding.bottom + border.top + border.bottom,
        ..rect
    };
    let mut layout = LayoutBox {
        node: node.node,
        tag: node.name().to_string(),
        rect,
        background: style.background,
        backgrounds: style.layers(),
        masks: style.mask_layers(),
        object_fit: style.object_fit,
        top_layer: false,
        transform: None,
        backface_hidden: style.backface_hidden,
        clip_path: None,
        radii: resolve_corner_radii(style.border_radius, rect.width, rect.height, font_size),
        opacity: box_opacity,
        shadows: resolve_shadows(style.shadows, content_width, font_size),
        filters: crate::filter::Filters::of(&style, font_size),
        border: BoxBorder::of(&style),
        outline: box_outline(&style),
        text: Vec::new(),
        control_value: None,
        control_selection: None,
        control_focused: false,
        control_text_origin: None,
        children,
        position: style.position,
        visual_offset: (0.0, 0.0),
        z_index: positioned_z_index(style),
        creates_stacking_context: creates_stacking_context(style) || box_opacity < 1.0,
        sticky_inset: style.inset,
        font_size,
        overflow: style.overflow,
        overflow_axes: style.overflow_axes(),
        visible: style.visibility == Visibility::Visible,
        pointer_events: style.pointer_events,
        selectable: style.selectable,
        cursor: style.cursor,
        floated: false,
        inline: false,
        control: None,
        selection: None,
        selection_color: None,
        scroll_content: Size::default(),
        scroll_offset: (0.0, 0.0),
        svg: None,
        scrollbar: scrollbar_for(
            stylesheet,
            (node.document, node.node),
            &style,
            font_size,
            style_state,
        ),
        margin,
    };
    if mode != LayoutMode::Flow {
        position_layout_box(
            &mut layout,
            style,
            mode,
            if mode == LayoutMode::Fixed {
                viewport_block(viewport)
            } else {
                inherited_containing_block
            },
            font_size,
        );
    }
    apply_relative_offset(&mut layout, style, inherited_containing_block, font_size);
    apply_transform(&mut layout, &style, font_size);
    let positioned_block = padding_block(&layout);
    append_positioned_nodes(
        &mut layout.children,
        positioned,
        PositionedLayoutContext {
            containing_block: if style.position == Position::Static {
                inherited_containing_block
            } else {
                positioned_block
            },
            viewport: viewport_block(viewport),
            viewport_size: viewport,
            parent: child_parent,
            stylesheet,
            style_state,
        },
    );
    finalize_scroll(&mut layout, padding, style_state);
    layout
}

fn layout_grid_element(input: GridLayoutInput<'_>) -> LayoutBox {
    let _profile = crate::profile::scope("layout: grid");
    let GridLayoutInput {
        content_height,
        height_bounds,
        parent_opacity,
        node,
        style,
        stylesheet,
        style_state,
        margin,
        padding,
        border,
        rect,
        content_x,
        content_y,
        content_width,
        font_size,
        viewport,
        containing_block,
        inherited_containing_block,
        mode,
    } = input;
    let child_parent = ParentStyle {
        font_size,
        color: style.color,
        weight: style.font_weight,
        text: inherited_text(&style),
        opacity: 1.0,
    };
    let line_height = used_line_height(&style, font_size);
    let text_context = InlineContext {
        font_size,
        line_height,
        color: text_color(&style),
        weight: style.font_weight,
        text: inherited_text(&style),
        opacity: 1.0,
        content_width,
        content_x: 0.0,
        align: style.text_align,
        raise: 0.0,
    };
    let box_opacity = style.opacity * parent_opacity;
    let item_block = if style.position == Position::Static {
        inherited_containing_block
    } else {
        containing_block
    };
    let column_gap = style
        .gap
        .resolve(content_width, font_size)
        .unwrap_or(0.0)
        .max(0.0);
    let row_gap = style
        .row_gap
        .resolve(content_height.unwrap_or(0.0), font_size)
        .unwrap_or(0.0)
        .max(0.0);

    // the items, element children and runs of text, in order-modified document order
    let generated = node.generated_content(stylesheet, style_state);
    let mut sources = Vec::new();
    let mut positioned = Vec::new();
    let mut run: Vec<(Option<NodeId>, &str)> = Vec::new();
    for child in node.children(&generated) {
        match child {
            Child::Text(text_node, value) => run.push((text_node, value)),
            Child::Element(child) => {
                let child_style =
                    stylesheet.style_for(child.style_context(child_parent, style_state));
                if child_style.display == Display::None {
                    continue;
                }
                if run.iter().any(|(_, value)| !value.trim().is_empty()) {
                    sources.push((FlexSource::Text(std::mem::take(&mut run)), None));
                }
                run.clear();
                if child_style.position.is_out_of_flow() {
                    positioned.push(child);
                    continue;
                }
                sources.push((FlexSource::Element(child), Some(child_style)));
            }
        }
    }
    if run.iter().any(|(_, value)| !value.trim().is_empty()) {
        sources.push((FlexSource::Text(run), None));
    }
    // an anonymous item inherits text styles and none of the container's box or placement
    let mut anonymous_style = style;
    anonymous_style.margin = Edges::zero();
    anonymous_style.padding = Edges::zero();
    anonymous_style.width = Length::Auto;
    anonymous_style.height = Length::Auto;
    anonymous_style.border = [BorderSide::INITIAL; 4];
    anonymous_style.order = 0;
    anonymous_style.align_self = None;
    anonymous_style.justify_self = None;
    anonymous_style.grid_column = GridPlacement::AUTO;
    anonymous_style.grid_row = GridPlacement::AUTO;
    let mut items = sources
        .into_iter()
        .map(|(source, child_style)| {
            let child_style = child_style.unwrap_or(anonymous_style);
            let child_font_size = child_style
                .font_size
                .resolve(font_size, font_size)
                .unwrap_or(font_size)
                .max(1.0);
            let border = used_border(&child_style);
            GridItem {
                source,
                style: child_style,
                margin: resolve_margins(child_style.margin, content_width, child_font_size),
                padding: resolve_edges(child_style.padding, content_width, child_font_size),
                border,
                area: [(0, 1); 2],
                width: None,
                layout: None,
            }
        })
        .collect::<Vec<_>>();
    items.sort_by_key(|item| item.style.order);

    // the explicit grid, then every item's area in it
    let areas = style.grid_template_areas.map();
    let mut columns = explicit_grid_tracks(
        &style.grid_template_columns.list(),
        Some(content_width),
        column_gap,
        font_size,
    );
    let mut rows = explicit_grid_tracks(
        &style.grid_template_rows.list(),
        content_height,
        row_gap,
        font_size,
    );
    let auto_columns = implicit_track_sizes(style.grid_auto_columns);
    let auto_rows = implicit_track_sizes(style.grid_auto_rows);
    columns.pad_to(areas.columns, &auto_columns);
    rows.pad_to(areas.rows, &auto_rows);
    for area in &areas.areas {
        rows.names
            .push((area.rows.0, format!("{}-start", area.name)));
        rows.names.push((area.rows.1, format!("{}-end", area.name)));
        columns
            .names
            .push((area.columns.0, format!("{}-start", area.name)));
        columns
            .names
            .push((area.columns.1, format!("{}-end", area.name)));
    }
    let placements = items
        .iter()
        .map(|item| {
            [
                resolve_grid_placement(item.style.grid_column, &columns),
                resolve_grid_placement(item.style.grid_row, &rows),
            ]
        })
        .collect::<Vec<_>>();
    let (placed, counts, shift) = place_grid_items(
        &placements,
        [columns.tracks.len(), rows.tracks.len()],
        style.grid_auto_flow_column,
        style.grid_auto_flow_dense,
    );
    for (item, area) in items.iter_mut().zip(placed) {
        item.area = area;
    }
    let column_tracks = columns.all_tracks(counts[0], shift[0], &auto_columns);
    let row_tracks = rows.all_tracks(counts[1], shift[1], &auto_rows);
    let column_collapsed = collapsed_tracks(&columns, counts[0], shift[0], &items, 0);
    let row_collapsed = collapsed_tracks(&rows, counts[1], shift[1], &items, 1);

    let justify = |item: &GridItem<'_>| {
        auto_margin_alignment(item.style.margin.left, item.style.margin.right)
            .unwrap_or(item.style.justify_self.unwrap_or(style.justify_items))
    };
    let align = |item: &GridItem<'_>| {
        auto_margin_alignment(item.style.margin.top, item.style.margin.bottom)
            .unwrap_or(item.style.align_self.unwrap_or(style.align_items))
    };
    // an item's outer min-content and max-content widths
    let intrinsic = |item: &GridItem<'_>| -> (f32, f32) {
        match &item.source {
            FlexSource::Text(pieces) => text_intrinsic_widths(pieces, text_context),
            FlexSource::Element(child) => {
                let measure = |minimum: bool| {
                    intrinsic_width(
                        MaxContentInput {
                            node: *child,
                            stylesheet,
                            style_state,
                            parent: child_parent,
                            containing_width: content_width,
                            own_width: true,
                        },
                        minimum,
                    )
                };
                (measure(true), measure(false))
            }
        }
    };

    let column_items = items
        .iter()
        .map(|item| (item.area[0].0, item.area[0].1))
        .collect::<Vec<_>>();
    let column_sizes = size_grid_tracks(
        GridAxis {
            tracks: &column_tracks,
            collapsed: &column_collapsed,
            available: Some(content_width),
            gap: column_gap,
            font_size,
            stretch: style.justify_content == JustifyContent::FlexStart,
        },
        &column_items,
        &mut |index| {
            let item = &items[index];
            let (minimum, maximum) = intrinsic(item);
            // a scroll container, or an item with a min-width of its own, has that for its
            // automatic minimum, where any other item has its narrowest content
            let scrolls = item.style.overflow_axes()[0] != Overflow::Visible;
            let own = item.style.min_width.resolve(content_width, font_size);
            let floor = match own {
                None if !scrolls => minimum,
                own => {
                    let inner = item.padding.left
                        + item.padding.right
                        + item.border.left
                        + item.border.right;
                    let own = own.unwrap_or(0.0);
                    item.margin.left
                        + item.margin.right
                        + match item.style.box_sizing {
                            crate::css::BoxSizing::BorderBox => own.max(inner),
                            crate::css::BoxSizing::ContentBox => own + inner,
                        }
                }
            };
            [floor, minimum, maximum]
        },
    );
    let column_offsets = grid_track_offsets(
        &column_sizes,
        &column_collapsed,
        column_gap,
        content_width,
        style.justify_content,
    );

    // lays an item out in its column span, at a content-box width and height when decided
    let lay_out = |item: &GridItem<'_>,
                   cell_width: f32,
                   cell_height: Option<f32>,
                   height: Option<f32>|
     -> Option<LayoutBox> {
        match &item.source {
            FlexSource::Text(pieces) => {
                let mut layout = layout_text_item(
                    pieces,
                    node.node,
                    item.width.unwrap_or(cell_width),
                    text_context,
                    box_opacity,
                );
                if let Some(height) = height {
                    layout.rect.height = height;
                }
                Some(layout)
            }
            FlexSource::Element(child) => layout_element(ElementLayoutInput {
                node: *child,
                containing_x: 0.0,
                containing_y: 0.0,
                containing_width: cell_width,
                containing_height: cell_height,
                size_override: (item.width, height),
                floats: None,
                list_ordinal: 1,
                parent: child_parent,
                context: LayoutContext {
                    viewport,
                    containing_block: item_block,
                    mode: LayoutMode::Flow,
                    stylesheet,
                    style_state,
                },
            }),
        }
    };
    let span_size = |offsets: &[f32], sizes: &[f32], (start, span): (usize, usize)| {
        let end = start + span - 1;
        offsets[end] + sizes[end] - offsets[start]
    };
    for item in &mut items {
        let cell_width = span_size(&column_offsets, &column_sizes, item.area[0]);
        // a stretched element fills its area as a block fills its container
        let width = match (&item.source, justify(item)) {
            (FlexSource::Element(_), AlignItems::Stretch) => None,
            _ if item.style.width != Length::Auto => None,
            (FlexSource::Text(_), AlignItems::Stretch) => Some(cell_width),
            (_, _) => {
                let extras = item.margin.left
                    + item.margin.right
                    + item.padding.left
                    + item.padding.right
                    + item.border.left
                    + item.border.right;
                let (minimum, maximum) = intrinsic(item);
                let extras = if matches!(item.source, FlexSource::Text(_)) {
                    0.0
                } else {
                    extras
                };
                Some((maximum.min(cell_width).max(minimum) - extras).max(0.0))
            }
        };
        item.width = width;
        item.layout = lay_out(item, cell_width, None, None);
    }

    let row_items = items
        .iter()
        .map(|item| (item.area[1].0, item.area[1].1))
        .collect::<Vec<_>>();
    let size_rows = |available: Option<f32>| {
        size_grid_tracks(
            GridAxis {
                tracks: &row_tracks,
                collapsed: &row_collapsed,
                available,
                gap: row_gap,
                font_size,
                stretch: style.align_content == AlignContent::Stretch,
            },
            &row_items,
            &mut |index| {
                let item = &items[index];
                let margins = item.margin.top + item.margin.bottom;
                let height = item
                    .layout
                    .as_ref()
                    .map_or(0.0, |layout| layout.rect.height)
                    + margins;
                // an item given a min-height and no height of its own, or one in percent
                // that waits on the row, may be left as little as that min-height
                let own = item.style.height;
                let minimum = if item.style.min_height != Length::Auto
                    && (own == Length::Auto || own.has_percent())
                {
                    let frame = item.padding.top
                        + item.padding.bottom
                        + item.border.top
                        + item.border.bottom;
                    let least =
                        resolve_height(item.style.min_height, None, font_size).unwrap_or(0.0);
                    let least = match item.style.box_sizing {
                        crate::css::BoxSizing::BorderBox => least.max(frame),
                        crate::css::BoxSizing::ContentBox => least + frame,
                    };
                    (least + margins).min(height)
                } else {
                    height
                };
                [minimum, height, height]
            },
        )
    };
    let mut row_sizes = size_rows(content_height);
    let rows_total = grid_tracks_total(&row_sizes, &row_collapsed, row_gap);
    let content_height = content_height.unwrap_or_else(|| {
        // an auto height kept within min-height and max-height sizes the rows again in it
        let bounded = rows_total.min(height_bounds.1).max(height_bounds.0);
        if bounded != rows_total {
            row_sizes = size_rows(Some(bounded));
        }
        bounded
    });
    let row_offsets = grid_track_offsets(
        &row_sizes,
        &row_collapsed,
        row_gap,
        content_height,
        justify_for_content(style.align_content),
    );

    // baseline-aligned items in a row share the lowest of their baselines
    let mut row_baselines = vec![0.0_f32; row_sizes.len()];
    for item in &items {
        if let Some(child) = &item.layout
            && align(item) == AlignItems::Baseline
        {
            let row = &mut row_baselines[item.area[1].0];
            *row = row.max(item.margin.top + baseline_offset(child));
        }
    }
    let mut children = Vec::with_capacity(items.len());
    for item in &mut items {
        let Some(mut child) = item.layout.take() else {
            continue;
        };
        let cell_x = column_offsets[item.area[0].0];
        let cell_y = row_offsets[item.area[1].0];
        let cell_width = span_size(&column_offsets, &column_sizes, item.area[0]);
        let cell_height = span_size(&row_offsets, &row_sizes, item.area[1]);
        let item_align = align(item);
        if item_align == AlignItems::Stretch && item.style.height == Length::Auto {
            let inner =
                item.padding.top + item.padding.bottom + item.border.top + item.border.bottom;
            let target = (cell_height - item.margin.top - item.margin.bottom - inner).max(0.0);
            if (child.rect.height - inner - target).abs() > 0.01 {
                child = lay_out(item, cell_width, Some(cell_height), Some(target)).unwrap_or(child);
            }
        } else if [
            item.style.height,
            item.style.min_height,
            item.style.max_height,
        ]
        .iter()
        .any(|length| length.has_percent())
        {
            // rows were sized with percentage heights as auto; now they resolve against the area
            child = lay_out(item, cell_width, Some(cell_height), None).unwrap_or(child);
        }
        let free_x = cell_width - child.rect.width - item.margin.left - item.margin.right;
        let free_y = cell_height - child.rect.height - item.margin.top - item.margin.bottom;
        let desired_x =
            content_x + cell_x + item.margin.left + align_offset(justify(item), free_x, 0.0);
        let desired_y = if item_align == AlignItems::Baseline {
            content_y + cell_y + row_baselines[item.area[1].0] - baseline_offset(&child)
        } else {
            content_y + cell_y + item.margin.top + align_offset(item_align, free_y, 0.0)
        };
        let offset = (desired_x - child.rect.x, desired_y - child.rect.y);
        translate_layout_box(&mut child, offset.0, offset.1);
        children.push(child);
    }
    let rect = Rect {
        height: content_height + padding.top + padding.bottom + border.top + border.bottom,
        ..rect
    };
    let mut layout = LayoutBox {
        node: node.node,
        tag: node.name().to_string(),
        rect,
        background: style.background,
        backgrounds: style.layers(),
        masks: style.mask_layers(),
        object_fit: style.object_fit,
        top_layer: false,
        transform: None,
        backface_hidden: style.backface_hidden,
        clip_path: None,
        radii: resolve_corner_radii(style.border_radius, rect.width, rect.height, font_size),
        opacity: box_opacity,
        shadows: resolve_shadows(style.shadows, content_width, font_size),
        filters: crate::filter::Filters::of(&style, font_size),
        border: BoxBorder::of(&style),
        outline: box_outline(&style),
        text: Vec::new(),
        control_value: None,
        control_selection: None,
        control_focused: false,
        control_text_origin: None,
        children,
        position: style.position,
        visual_offset: (0.0, 0.0),
        z_index: positioned_z_index(style),
        creates_stacking_context: creates_stacking_context(style) || box_opacity < 1.0,
        sticky_inset: style.inset,
        font_size,
        overflow: style.overflow,
        overflow_axes: style.overflow_axes(),
        visible: style.visibility == Visibility::Visible,
        pointer_events: style.pointer_events,
        selectable: style.selectable,
        cursor: style.cursor,
        floated: false,
        inline: false,
        control: None,
        selection: None,
        selection_color: None,
        scroll_content: Size::default(),
        scroll_offset: (0.0, 0.0),
        svg: None,
        scrollbar: scrollbar_for(
            stylesheet,
            (node.document, node.node),
            &style,
            font_size,
            style_state,
        ),
        margin,
    };
    if mode != LayoutMode::Flow {
        position_layout_box(
            &mut layout,
            style,
            mode,
            if mode == LayoutMode::Fixed {
                viewport_block(viewport)
            } else {
                inherited_containing_block
            },
            font_size,
        );
    }
    apply_relative_offset(&mut layout, style, inherited_containing_block, font_size);
    apply_transform(&mut layout, &style, font_size);
    let positioned_block = padding_block(&layout);
    append_positioned_nodes(
        &mut layout.children,
        positioned,
        PositionedLayoutContext {
            containing_block: if style.position == Position::Static {
                inherited_containing_block
            } else {
                positioned_block
            },
            viewport: viewport_block(viewport),
            viewport_size: viewport,
            parent: child_parent,
            stylesheet,
            style_state,
        },
    );
    finalize_scroll(&mut layout, padding, style_state);
    layout
}

// a row of a table: its element, or none for cells placed straight into a group or table
struct TableRow<'a> {
    node: Option<(Subject<'a>, Style)>,
    group: usize,
    cells: Vec<(Subject<'a>, Style)>,
}

struct TableCellSlot<'a> {
    node: Subject<'a>,
    style: Style,
    row: usize,
    column: usize,
    rows: usize,
    columns: usize,
    layout: Option<LayoutBox>,
}

fn table_span(cell: Subject<'_>, attribute: &str, limit: usize) -> usize {
    cell.element()
        .and_then(|element| element.attr(attribute))
        .and_then(|value| value.trim().parse::<usize>().ok())
        .map_or(1, |span| {
            if span == 0 {
                limit
            } else {
                span.clamp(1, limit)
            }
        })
}

// a table, laid out by the automatic table layout algorithm (or the fixed one) of CSS 2.1
// with separated borders, or collapsed borders that overlap by the widest cell border
fn layout_table_element(input: GridLayoutInput<'_>) -> LayoutBox {
    let GridLayoutInput {
        content_height,
        height_bounds: _,
        parent_opacity,
        node,
        style,
        stylesheet,
        style_state,
        margin,
        padding,
        border,
        rect,
        content_x,
        content_y,
        content_width,
        font_size,
        viewport,
        containing_block,
        inherited_containing_block,
        mode,
    } = input;
    let child_parent = ParentStyle {
        font_size,
        color: style.color,
        weight: style.font_weight,
        text: inherited_text(&style),
        opacity: 1.0,
    };
    let box_opacity = style.opacity * parent_opacity;
    let item_block = if style.position == Position::Static {
        inherited_containing_block
    } else {
        containing_block
    };
    let style_of = |child: Subject<'_>, parent: ParentStyle| {
        stylesheet.style_for(child.style_context(parent, style_state))
    };
    let parent_of = |child_style: &Style| ParentStyle {
        font_size: child_style
            .font_size
            .resolve(font_size, font_size)
            .unwrap_or(font_size)
            .max(1.0),
        color: child_style.color,
        weight: child_style.font_weight,
        text: inherited_text(child_style),
        opacity: 1.0,
    };

    // captions, then row groups in header, body, footer order, each with its rows and cells
    let mut captions = Vec::new();
    let mut positioned = Vec::new();
    let mut groups: Vec<(Option<(Subject<'_>, Style)>, u8)> = Vec::new();
    let mut rows: Vec<TableRow<'_>> = Vec::new();
    let anonymous_group = |groups: &mut Vec<(Option<(Subject<'_>, Style)>, u8)>| {
        if !matches!(groups.last(), Some((None, _))) {
            groups.push((None, 1));
        }
        groups.len() - 1
    };
    for child in node.children(&NO_GENERATED_CONTENT) {
        let Child::Element(child) = child else {
            continue;
        };
        let child_style = style_of(child, child_parent);
        if child_style.display == Display::None {
            continue;
        }
        if child_style.position.is_out_of_flow() {
            positioned.push(child);
            continue;
        }
        match child_style.display {
            Display::TableCaption => captions.push((child, child_style)),
            Display::TableColumn | Display::TableColumnGroup => {}
            Display::TableHeaderGroup | Display::TableRowGroup | Display::TableFooterGroup => {
                let kind = match child_style.display {
                    Display::TableHeaderGroup => 0,
                    Display::TableFooterGroup => 2,
                    _ => 1,
                };
                groups.push((Some((child, child_style)), kind));
                let group = groups.len() - 1;
                let group_parent = parent_of(&child_style);
                let mut loose: Option<usize> = None;
                for grandchild in child.children(&NO_GENERATED_CONTENT) {
                    let Child::Element(grandchild) = grandchild else {
                        continue;
                    };
                    let grandchild_style = style_of(grandchild, group_parent);
                    if grandchild_style.display == Display::None {
                        continue;
                    }
                    if grandchild_style.display == Display::TableRow {
                        loose = None;
                        rows.push(TableRow {
                            node: Some((grandchild, grandchild_style)),
                            group,
                            cells: Vec::new(),
                        });
                    } else {
                        let row = *loose.get_or_insert_with(|| {
                            rows.push(TableRow {
                                node: None,
                                group,
                                cells: Vec::new(),
                            });
                            rows.len() - 1
                        });
                        rows[row].cells.push((grandchild, grandchild_style));
                    }
                }
            }
            Display::TableRow => {
                let group = anonymous_group(&mut groups);
                rows.push(TableRow {
                    node: Some((child, child_style)),
                    group,
                    cells: Vec::new(),
                });
            }
            _ => {
                // anything else becomes a cell of an anonymous row
                let group = anonymous_group(&mut groups);
                let reuse = rows
                    .last()
                    .is_some_and(|row| row.node.is_none() && row.group == group);
                if !reuse {
                    rows.push(TableRow {
                        node: None,
                        group,
                        cells: Vec::new(),
                    });
                }
                if let Some(row) = rows.last_mut() {
                    row.cells.push((child, child_style));
                }
            }
        }
    }
    // the cells of each real row
    for row in &mut rows {
        let Some((row_node, row_style)) = row.node else {
            continue;
        };
        let row_parent = parent_of(&row_style);
        for cell in row_node.children(&NO_GENERATED_CONTENT) {
            let Child::Element(cell) = cell else {
                continue;
            };
            let cell_style = style_of(cell, row_parent);
            if cell_style.display != Display::None && !cell_style.position.is_out_of_flow() {
                row.cells.push((cell, cell_style));
            }
        }
    }
    // headers first and footers last, keeping document order otherwise
    let mut group_order = (0..groups.len()).collect::<Vec<_>>();
    group_order.sort_by_key(|group| groups[*group].1);
    rows.sort_by_key(|row| {
        group_order
            .iter()
            .position(|group| *group == row.group)
            .unwrap_or(0)
    });

    // cells take the first free slots of their row, below any cell spanning down into it
    let mut occupied: Vec<Vec<bool>> = vec![Vec::new(); rows.len()];
    let mut cells: Vec<TableCellSlot<'_>> = Vec::new();
    for (row_index, row) in rows.iter().enumerate() {
        let group_end = rows
            .iter()
            .enumerate()
            .skip(row_index)
            .take_while(|(_, other)| other.group == row.group)
            .count();
        let mut column = 0;
        for &(cell, cell_style) in &row.cells {
            while occupied[row_index].get(column).copied().unwrap_or(false) {
                column += 1;
            }
            let columns = table_span(cell, "colspan", 1000);
            let spanned_rows = table_span(cell, "rowspan", group_end).min(group_end);
            for slots in &mut occupied[row_index..row_index + spanned_rows] {
                if slots.len() < column + columns {
                    slots.resize(column + columns, false);
                }
                slots[column..column + columns].fill(true);
            }
            cells.push(TableCellSlot {
                node: cell,
                style: cell_style,
                row: row_index,
                column,
                rows: spanned_rows,
                columns,
                layout: None,
            });
            column += columns;
        }
    }
    let column_count = occupied.iter().map(Vec::len).max().unwrap_or(0);
    let row_count = rows.len();

    // collapsed borders share their width between neighbours; separated ones keep spacing
    let collapse_overlap = |side: fn(&ResolvedEdges) -> f32| {
        cells
            .iter()
            .map(|cell| side(&used_border(&cell.style)))
            .fold(0.0_f32, f32::max)
    };
    let (spacing_x, spacing_y) = if style.border_collapse {
        (
            -collapse_overlap(|edges| edges.left.max(edges.right)),
            -collapse_overlap(|edges| edges.top.max(edges.bottom)),
        )
    } else {
        style.border_spacing
    };
    let edge_x = spacing_x.max(0.0);
    let edge_y = spacing_y.max(0.0);

    // column widths from the cells' minimum and maximum content widths
    let intrinsic = |cell: &TableCellSlot<'_>, minimum: bool| {
        intrinsic_width(
            MaxContentInput {
                node: cell.node,
                stylesheet,
                style_state,
                parent: child_parent,
                containing_width: content_width,
                own_width: true,
            },
            minimum,
        )
    };
    let mut column_min = vec![0.0_f32; column_count];
    let mut column_max = vec![0.0_f32; column_count];
    let mut by_span = (0..cells.len()).collect::<Vec<_>>();
    by_span.sort_by_key(|index| cells[*index].columns);
    for index in by_span {
        let cell = &cells[index];
        let (minimum, maximum) = (intrinsic(cell, true), intrinsic(cell, false));
        let maximum = maximum.max(minimum);
        let range = cell.column..cell.column + cell.columns;
        let gaps = spacing_x * (cell.columns - 1) as f32;
        let have_min = column_min[range.clone()].iter().sum::<f32>() + gaps;
        let have_max = column_max[range.clone()].iter().sum::<f32>() + gaps;
        let share = |extra: f32| (extra / cell.columns as f32).max(0.0);
        let (min_share, max_share) = (share(minimum - have_min), share(maximum - have_max));
        for column in range {
            column_min[column] += min_share;
            column_max[column] += max_share;
            column_max[column] = column_max[column].max(column_min[column]);
        }
    }
    let spacing_total = edge_x * 2.0 + spacing_x * column_count.saturating_sub(1) as f32;
    let sum_min = column_min.iter().sum::<f32>();
    let sum_max = column_max.iter().sum::<f32>();
    let table_width = if style.width == Length::Auto {
        (sum_max + spacing_total)
            .min(content_width)
            .max(sum_min + spacing_total)
    } else {
        content_width.max(sum_min + spacing_total)
    };
    let available = (table_width - spacing_total).max(0.0);
    let mut column_widths = if style.table_fixed && style.width != Length::Auto {
        // fixed layout reads widths from the first row and shares out the rest
        let mut widths = vec![None; column_count];
        for cell in cells
            .iter()
            .filter(|cell| cell.row == 0 && cell.columns == 1)
        {
            widths[cell.column] = cell.style.width.resolve(available, font_size).map(|width| {
                if cell.style.box_sizing == crate::css::BoxSizing::BorderBox {
                    width
                } else {
                    let inner = resolve_edges(cell.style.padding, available, font_size);
                    let edges = used_border(&cell.style);
                    width + inner.left + inner.right + edges.left + edges.right
                }
            });
        }
        let fixed = widths.iter().flatten().sum::<f32>();
        let open = widths.iter().filter(|width| width.is_none()).count();
        let share = if open > 0 {
            ((available - fixed) / open as f32).max(0.0)
        } else {
            0.0
        };
        widths
            .into_iter()
            .map(|width| width.unwrap_or(share))
            .collect()
    } else if available >= sum_max {
        let extra = available - sum_max;
        column_max
            .iter()
            .map(|maximum| {
                if sum_max > 0.0 {
                    maximum + extra * maximum / sum_max
                } else {
                    extra / column_count.max(1) as f32
                }
            })
            .collect()
    } else {
        let ratio = if sum_max > sum_min {
            ((available - sum_min) / (sum_max - sum_min)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        column_min
            .iter()
            .zip(&column_max)
            .map(|(minimum, maximum)| minimum + (maximum - minimum) * ratio)
            .collect::<Vec<_>>()
    };
    if column_widths.is_empty() {
        column_widths.push(0.0);
    }
    let mut column_x = Vec::with_capacity(column_count);
    let mut x = edge_x;
    for width in &column_widths {
        column_x.push(x);
        x += width + spacing_x;
    }
    let span_width = |cell: &TableCellSlot<'_>| {
        column_widths[cell.column..cell.column + cell.columns]
            .iter()
            .sum::<f32>()
            + spacing_x * (cell.columns - 1) as f32
    };

    // cells laid out at their widths decide the row heights
    let lay_out_cell = |cell: &TableCellSlot<'_>| {
        let width = span_width(cell);
        let inner_padding = resolve_edges(cell.style.padding, width, font_size);
        let edges = used_border(&cell.style);
        layout_element(ElementLayoutInput {
            node: cell.node,
            containing_x: 0.0,
            containing_y: 0.0,
            containing_width: width,
            containing_height: None,
            size_override: (
                Some(
                    (width - inner_padding.left - inner_padding.right - edges.left - edges.right)
                        .max(0.0),
                ),
                None,
            ),
            floats: None,
            list_ordinal: 1,
            parent: child_parent,
            context: LayoutContext {
                viewport,
                containing_block: item_block,
                mode: LayoutMode::Flow,
                stylesheet,
                style_state,
            },
        })
    };
    for cell in &mut cells {
        cell.layout = lay_out_cell(cell);
    }
    let mut row_heights = rows
        .iter()
        .map(|row| {
            row.node
                .and_then(|(_, row_style)| resolve_height(row_style.height, None, font_size))
                .unwrap_or(0.0)
        })
        .collect::<Vec<_>>();
    let mut by_rows = (0..cells.len()).collect::<Vec<_>>();
    by_rows.sort_by_key(|index| cells[*index].rows);
    for index in by_rows {
        let cell = &cells[index];
        let height = cell
            .layout
            .as_ref()
            .map_or(0.0, |layout| layout.rect.height);
        let range = cell.row..cell.row + cell.rows;
        let have =
            row_heights[range.clone()].iter().sum::<f32>() + spacing_y * (cell.rows - 1) as f32;
        if height > have {
            row_heights[range.end - 1] += height - have;
        }
    }
    let rows_total = row_heights.iter().sum::<f32>()
        + edge_y * 2.0
        + spacing_y * row_count.saturating_sub(1) as f32;
    // a taller specified table height grows its rows evenly
    if let Some(height) = content_height
        && height > rows_total
        && row_count > 0
    {
        let extra = (height - rows_total) / row_count as f32;
        for row_height in &mut row_heights {
            *row_height += extra;
        }
    }
    let table_x = content_x;
    let mut cursor_y = content_y;
    let mut children = Vec::new();
    let caption_box = |caption: &(Subject<'_>, Style), y: f32| {
        layout_element(ElementLayoutInput {
            node: caption.0,
            containing_x: table_x,
            containing_y: y,
            containing_width: table_width,
            containing_height: None,
            size_override: (None, None),
            floats: None,
            list_ordinal: 1,
            parent: child_parent,
            context: LayoutContext {
                viewport,
                containing_block: item_block,
                mode: LayoutMode::Flow,
                stylesheet,
                style_state,
            },
        })
    };
    if !style.caption_bottom {
        for caption in &captions {
            if let Some(laid_out) = caption_box(caption, cursor_y) {
                cursor_y = laid_out.rect.bottom() + laid_out.margin.bottom;
                children.push(laid_out);
            }
        }
    }
    let grid_top = cursor_y;
    let mut row_y = Vec::with_capacity(row_count);
    let mut y = grid_top + edge_y;
    for height in &row_heights {
        row_y.push(y);
        y += height + spacing_y;
    }
    let grid_height = if row_count > 0 {
        y - spacing_y + edge_y - grid_top
    } else {
        0.0
    };

    // cells take their row span's height and place their content by vertical-align
    let mut cell_boxes: Vec<Vec<LayoutBox>> = vec![Vec::new(); row_count];
    for cell in &mut cells {
        let Some(mut laid_out) = cell.layout.take() else {
            continue;
        };
        let height = row_heights[cell.row..cell.row + cell.rows]
            .iter()
            .sum::<f32>()
            + spacing_y * (cell.rows - 1) as f32;
        // the free space is measured from the content, which an explicit cell height leaves
        // at the top of a taller box
        let content_top = laid_out.rect.y + laid_out.border.widths.top + {
            let padding = resolve_edges(cell.style.padding, span_width(cell), font_size);
            padding.top
        };
        let content_bottom = laid_out
            .children
            .iter()
            .map(|child| child.rect.bottom() + child.margin.bottom)
            .chain(laid_out.text.iter().map(|run| run.y + run.font_size * 1.25))
            .fold(content_top, f32::max);
        let inner_bottom = laid_out.rect.y + height.max(laid_out.rect.height)
            - laid_out.border.widths.bottom
            - resolve_edges(cell.style.padding, span_width(cell), font_size).bottom;
        let free = inner_bottom - content_bottom;
        laid_out.rect.height = laid_out.rect.height.max(height);
        if free > 0.01 {
            let shift = match cell.style.vertical_align {
                VerticalAlign::Middle => free / 2.0,
                VerticalAlign::Bottom => free,
                _ => 0.0,
            };
            for child in &mut laid_out.children {
                translate_layout_box(child, 0.0, shift);
            }
            for run in &mut laid_out.text {
                run.y += shift;
            }
        }
        let target = (table_x + column_x[cell.column], row_y[cell.row]);
        let offset = (target.0 - laid_out.rect.x, target.1 - laid_out.rect.y);
        translate_layout_box(&mut laid_out, offset.0, offset.1);
        cell_boxes[cell.row].push(laid_out);
    }

    // rows and groups are boxes of their own, so their backgrounds paint under the cells
    let inner_left = table_x + edge_x;
    let inner_width = (table_width - edge_x * 2.0).max(0.0);
    let section_box = |subject: Subject<'_>, section_style: &Style, top: f32, height: f32| {
        let rect = Rect {
            x: inner_left,
            y: top,
            width: inner_width,
            height,
        };
        let mut section = anonymous_box(
            subject.node,
            rect,
            Vec::new(),
            (font_size, section_style.cursor),
            box_opacity * section_style.opacity,
        );
        section.tag = subject.name().to_string();
        section.background = section_style.background;
        section.backgrounds = section_style.layers();
        section.masks = section_style.mask_layers();
        section.visible = section_style.visibility == Visibility::Visible;
        section.pointer_events = section_style.pointer_events;
        section.selectable = section_style.selectable;
        section
    };
    let mut row_boxes: Vec<(usize, LayoutBox)> = Vec::new();
    for (index, (row, cells_here)) in rows.iter().zip(cell_boxes).enumerate() {
        match row.node {
            Some((row_node, row_style)) => {
                let mut row_box =
                    section_box(row_node, &row_style, row_y[index], row_heights[index]);
                row_box.children = cells_here;
                row_boxes.push((row.group, row_box));
            }
            None => {
                for cell_box in cells_here {
                    row_boxes.push((row.group, cell_box));
                }
            }
        }
    }
    for group in group_order {
        let members = row_boxes
            .iter()
            .filter(|(owner, _)| *owner == group)
            .count();
        if members == 0 {
            continue;
        }
        let mut contents = Vec::with_capacity(members);
        let mut index = 0;
        while index < row_boxes.len() {
            if row_boxes[index].0 == group {
                contents.push(row_boxes.remove(index).1);
            } else {
                index += 1;
            }
        }
        match groups[group].0 {
            Some((group_node, group_style)) => {
                let top = contents
                    .iter()
                    .map(|row| row.rect.y)
                    .fold(f32::INFINITY, f32::min);
                let bottom = contents
                    .iter()
                    .map(|row| row.rect.bottom())
                    .fold(f32::NEG_INFINITY, f32::max);
                let mut group_box = section_box(group_node, &group_style, top, bottom - top);
                group_box.children = contents;
                children.push(group_box);
            }
            None => children.extend(contents),
        }
    }
    cursor_y = grid_top + grid_height;
    if style.caption_bottom {
        for caption in &captions {
            if let Some(laid_out) = caption_box(caption, cursor_y) {
                cursor_y = laid_out.rect.bottom() + laid_out.margin.bottom;
                children.push(laid_out);
            }
        }
    }

    // an auto-width table shrinks to its columns, and auto margins center what is left
    let content_height = content_height.unwrap_or(0.0).max(cursor_y - content_y);
    let mut rect = Rect {
        width: table_width + padding.left + padding.right + border.left + border.right,
        height: content_height + padding.top + padding.bottom + border.top + border.bottom,
        ..rect
    };
    let free = content_width - table_width;
    if mode == LayoutMode::Flow && free > 0.0 {
        let shift = match (
            style.margin.left == Length::Auto,
            style.margin.right == Length::Auto,
        ) {
            (true, true) => free / 2.0,
            (true, false) => free,
            _ => 0.0,
        };
        if shift > 0.0 {
            rect.x += shift;
            for child in &mut children {
                translate_layout_box(child, shift, 0.0);
            }
        }
    }
    let mut layout = LayoutBox {
        node: node.node,
        tag: node.name().to_string(),
        rect,
        background: style.background,
        backgrounds: style.layers(),
        masks: style.mask_layers(),
        object_fit: style.object_fit,
        top_layer: false,
        transform: None,
        backface_hidden: style.backface_hidden,
        clip_path: None,
        radii: resolve_corner_radii(style.border_radius, rect.width, rect.height, font_size),
        opacity: box_opacity,
        shadows: resolve_shadows(style.shadows, content_width, font_size),
        filters: crate::filter::Filters::of(&style, font_size),
        border: BoxBorder::of(&style),
        outline: box_outline(&style),
        text: Vec::new(),
        control_value: None,
        control_selection: None,
        control_focused: false,
        control_text_origin: None,
        children,
        position: style.position,
        visual_offset: (0.0, 0.0),
        z_index: positioned_z_index(style),
        creates_stacking_context: creates_stacking_context(style) || box_opacity < 1.0,
        sticky_inset: style.inset,
        font_size,
        overflow: style.overflow,
        overflow_axes: style.overflow_axes(),
        visible: style.visibility == Visibility::Visible,
        pointer_events: style.pointer_events,
        selectable: style.selectable,
        cursor: style.cursor,
        floated: false,
        inline: false,
        control: None,
        selection: None,
        selection_color: None,
        scroll_content: Size::default(),
        scroll_offset: (0.0, 0.0),
        svg: None,
        scrollbar: scrollbar_for(
            stylesheet,
            (node.document, node.node),
            &style,
            font_size,
            style_state,
        ),
        margin,
    };
    if mode != LayoutMode::Flow {
        position_layout_box(
            &mut layout,
            style,
            mode,
            if mode == LayoutMode::Fixed {
                viewport_block(viewport)
            } else {
                inherited_containing_block
            },
            font_size,
        );
    }
    apply_relative_offset(&mut layout, style, inherited_containing_block, font_size);
    apply_transform(&mut layout, &style, font_size);
    let positioned_block = padding_block(&layout);
    append_positioned_nodes(
        &mut layout.children,
        positioned,
        PositionedLayoutContext {
            containing_block: if style.position == Position::Static {
                inherited_containing_block
            } else {
                positioned_block
            },
            viewport: viewport_block(viewport),
            viewport_size: viewport,
            parent: child_parent,
            stylesheet,
            style_state,
        },
    );
    finalize_scroll(&mut layout, padding, style_state);
    layout
}

// what an element draws as a form control
#[derive(Clone, Debug, PartialEq)]
enum ControlType {
    Text,
    Textarea,
    // a push button input, with its label
    Button(String),
    Checkbox,
    Radio,
    Range,
    File,
    Select(String),
    Progress,
    Meter,
}

fn input_type(element: &Element) -> String {
    element.attr("type").map_or_else(
        || "text".to_string(),
        |kind| kind.trim().to_ascii_lowercase(),
    )
}

fn control_type(node: Subject<'_>, style_state: &StyleState) -> Option<ControlType> {
    let element = node.element()?;
    Some(match element.name.as_str() {
        "textarea" => ControlType::Textarea,
        "progress" => ControlType::Progress,
        "meter" => ControlType::Meter,
        "select" => ControlType::Select(select_label(node, style_state)),
        "input" => match input_type(element).as_str() {
            "checkbox" => ControlType::Checkbox,
            "radio" => ControlType::Radio,
            "range" => ControlType::Range,
            "file" => ControlType::File,
            "color" | "image" | "hidden" => return None,
            kind @ ("button" | "submit" | "reset") => {
                ControlType::Button(element.attr("value").map_or_else(
                    || match kind {
                        "submit" => "Submit".to_string(),
                        "reset" => "Reset".to_string(),
                        _ => String::new(),
                    },
                    str::to_string,
                ))
            }
            _ => ControlType::Text,
        },
        _ => return None,
    })
}

// a closed select shows its selected option, or its first when none is
fn select_label(node: Subject<'_>, style_state: &StyleState) -> String {
    let document = node.document;
    let options = document
        .descendants(node.node)
        .filter(|descendant| {
            document
                .element(*descendant)
                .is_some_and(|element| element.name == "option")
        })
        .collect::<Vec<_>>();
    let chosen = options
        .iter()
        .find(|option| {
            document
                .element(**option)
                .is_some_and(|element| style_state.is_checked(element, **option))
        })
        .or(options.first());
    chosen.map_or_else(String::new, |option| {
        let element = document.element(*option);
        element
            .and_then(|element| element.attr("label"))
            .map_or_else(
                || {
                    document
                        .text_content(*option)
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                },
                str::to_string,
            )
    })
}

const FILE_BUTTON_LABEL: &str = "Choose File";
const FILE_EMPTY_LABEL: &str = "No file chosen";

// the content size a control takes when its width or height is auto: text fields from their
// size, cols and rows attributes, buttons and selects from their labels
fn control_content_size(
    node: Subject<'_>,
    style: &Style,
    font_size: f32,
    line_height: f32,
    style_state: &StyleState,
) -> (Option<f32>, Option<f32>) {
    let Some(control) = control_type(node, style_state) else {
        return (None, None);
    };
    let element = node.element();
    let measure = |text: &str| {
        measure_text(
            text,
            style.font_family,
            font_size,
            font_style(style, font_size),
            0.0,
        )
    };
    let attribute = |name: &str, default: f32| {
        element
            .and_then(|element| element.attr(name))
            .and_then(|value| value.trim().parse::<f32>().ok())
            .filter(|value| *value > 0.0)
            .unwrap_or(default)
    };
    let character = measure("0").max(1.0);
    match control {
        ControlType::Text => (Some(attribute("size", 20.0) * character), Some(line_height)),
        ControlType::Textarea => (
            Some(attribute("cols", 20.0) * character),
            Some(attribute("rows", 2.0) * line_height),
        ),
        ControlType::Button(label) => (Some(measure(&label)), Some(line_height)),
        ControlType::Select(_) => {
            let document = node.document;
            let widest = document
                .descendants(node.node)
                .filter(|descendant| {
                    document
                        .element(*descendant)
                        .is_some_and(|element| element.name == "option")
                })
                .map(|option| measure(document.text_content(option).trim()))
                .fold(0.0, f32::max);
            // chromium rounds the widest option up to a whole device pixel
            let scale = crate::font::device_scale();
            (Some((widest * scale).ceil() / scale), Some(line_height))
        }
        ControlType::File => (
            Some(file_button_width(&measure) + font_size * 0.4 + measure(FILE_EMPTY_LABEL)),
            Some(line_height + 4.0),
        ),
        _ => (None, None),
    }
}

fn file_button_width(measure: &dyn Fn(&str) -> f32) -> f32 {
    measure(FILE_BUTTON_LABEL) + 14.0
}

// the text a button-like control shows inside its box
fn control_label(control: &ControlType) -> Option<String> {
    match control {
        ControlType::Button(label) | ControlType::Select(label) if !label.is_empty() => {
            Some(label.clone())
        }
        ControlType::File => Some(format!("{FILE_BUTTON_LABEL}  {FILE_EMPTY_LABEL}")),
        _ => None,
    }
}

fn number_attribute(element: &Element, name: &str) -> Option<f32> {
    element
        .attr(name)
        .and_then(|value| value.trim().parse::<f32>().ok())
        .filter(|value| value.is_finite())
}

// what paint draws on top of a control's box
fn control_paint(
    node: Subject<'_>,
    style: &Style,
    style_state: &StyleState,
    font_size: f32,
) -> Option<ControlPaint> {
    let control = control_type(node, style_state)?;
    if style.appearance_none && !matches!(control, ControlType::Range) {
        return None;
    }
    let element = node.element()?;
    let accent = style.accent_color.unwrap_or(Color::opaque(0, 117, 255));
    let mark = text_color(style);
    Some(match control {
        ControlType::Checkbox => ControlPaint::Checkbox {
            checked: style_state.is_checked(element, node.node),
            accent,
        },
        ControlType::Radio => ControlPaint::Radio {
            checked: style_state.is_checked(element, node.node),
            accent,
        },
        ControlType::Select(_) => ControlPaint::Select { color: mark },
        ControlType::Range => {
            let bounds = RangeBounds::of(element);
            let value = bounds.sanitize(
                style_state
                    .control_values
                    .get(&node.node)
                    .map(String::as_str)
                    .or_else(|| element.attr("value")),
            );
            ControlPaint::Range {
                fraction: bounds.fraction(value) as f32,
                accent,
                disabled: style_state.is_disabled(element, node.node),
                styled: style.appearance_none,
                travel: (0.0, 0.0),
                thumb: 0.0,
            }
        }
        ControlType::Progress => {
            let maximum = number_attribute(element, "max")
                .filter(|max| *max > 0.0)
                .unwrap_or(1.0);
            let fraction =
                number_attribute(element, "value").map(|value| (value / maximum).clamp(0.0, 1.0));
            ControlPaint::Progress { fraction, accent }
        }
        ControlType::Meter => {
            let minimum = number_attribute(element, "min").unwrap_or(0.0);
            let maximum = number_attribute(element, "max").unwrap_or(1.0).max(minimum);
            let value = number_attribute(element, "value")
                .unwrap_or(minimum)
                .clamp(minimum, maximum);
            let low = number_attribute(element, "low").unwrap_or(minimum);
            let high = number_attribute(element, "high").unwrap_or(maximum);
            // values past the low or high marks read as a warning
            let color = if value < low || value > high {
                Color::opaque(255, 185, 0)
            } else {
                Color::opaque(16, 124, 16)
            };
            let fraction = if maximum > minimum {
                (value - minimum) / (maximum - minimum)
            } else {
                0.0
            };
            ControlPaint::Meter { fraction, color }
        }
        ControlType::File => {
            let width = file_button_width(&|text: &str| {
                measure_text(text, style.font_family, font_size, FontStyle::NORMAL, 0.0)
            });
            ControlPaint::FileButton { width }
        }
        _ => return None,
    })
}

struct MaxContentInput<'a> {
    node: Subject<'a>,
    stylesheet: &'a Stylesheet,
    style_state: &'a StyleState,
    parent: ParentStyle,
    containing_width: f32,
    // false measures what the content needs, ignoring the element's own width
    own_width: bool,
}

// the widest the element can be before wrapping, used for auto grid tracks
fn max_content_width(input: MaxContentInput<'_>) -> f32 {
    intrinsic_width(input, false)
}

// what an intrinsic width was measured under, beyond the element and which width it was
#[derive(Clone, Copy, PartialEq)]
struct IntrinsicInputs {
    containing_width: u32,
    font_size: u32,
    weight: FontWeight,
    text: InheritedText,
    zero_font: (crate::font::FontStack, crate::font::FontStyle),
    environment: crate::css::LayoutEnvironment,
}

type IntrinsicKey = (NodeId, Option<PseudoElement>, bool, bool);

// an element's outer width at its widest (max-content) or its narrowest (min-content); nested
// flex and grid containers ask for the same widths many times over
fn intrinsic_width(input: MaxContentInput<'_>, minimum: bool) -> f32 {
    let key = (
        input.node.node,
        input.node.generated.map(|(pseudo, _)| pseudo),
        minimum,
        input.own_width,
    );
    let inputs = IntrinsicInputs {
        containing_width: input.containing_width.to_bits(),
        font_size: input.parent.font_size.to_bits(),
        weight: input.parent.weight,
        text: input.parent.text,
        zero_font: crate::css::zero_font(),
        environment: crate::css::layout_environment(),
    };
    let Some((pass, stamp, works)) = KEPT.with(|kept| {
        let kept = kept.borrow();
        kept.pass.map(|(pass, stamp)| (pass, stamp, kept.works))
    }) else {
        return measure_intrinsic_width(input, minimum);
    };
    let (document, node) = (input.node.document, input.node.node);
    let known = KEPT.with(|kept| {
        kept_answer(
            &mut kept.borrow_mut().widths,
            (&key, &inputs),
            (pass, works),
            |since| document.unwritten_since(node, since),
        )
    });
    if let Some((width, earlier)) = known {
        if cfg!(feature = "recheck") && earlier {
            let reported = crate::css::moment_reports();
            let fresh = measure_intrinsic_width(input, minimum);
            assert!(fresh == width, "a kept width went stale: {node:?}");
            assert_eq!(
                reported,
                crate::css::moment_reports(),
                "a kept width stands for one moment only: {node:?}"
            );
        }
        return width;
    }
    let reported = crate::css::moment_reports();
    let width = measure_intrinsic_width(input, minimum);
    let momentary = crate::css::moment_reports() != reported;
    KEPT.with(|kept| {
        let mut kept = kept.borrow_mut();
        kept.worked = true;
        keep_answer(
            &mut kept.widths,
            key,
            Kept {
                inputs,
                answer: width,
                pass,
                used: works,
                stamp,
                momentary,
            },
        );
    });
    width
}

fn measure_intrinsic_width(input: MaxContentInput<'_>, minimum: bool) -> f32 {
    let _profile = crate::profile::scope("layout: intrinsic width");
    let MaxContentInput {
        node,
        stylesheet,
        style_state,
        parent,
        containing_width,
        own_width,
    } = input;
    let style = stylesheet.style_for(node.style_context(parent, style_state));
    if style.display == Display::None {
        return 0.0;
    }
    // a box sized to its widest or its narrowest content is that wide whichever is asked for
    let minimum = match style.content_size[0] {
        ContentSize::Max if own_width => false,
        ContentSize::Min if own_width => true,
        _ => minimum,
    };
    let font_size = style
        .font_size
        .resolve(parent.font_size, parent.font_size)
        .unwrap_or(parent.font_size)
        .max(1.0);
    let _zero_font = crate::css::use_zero_font(style.font_family, font_style(&style, font_size));
    let margin = resolve_edges(style.margin, containing_width, font_size);
    let padding = resolve_edges(style.padding, containing_width, font_size);
    let extras = margin.left
        + margin.right
        + padding.left
        + padding.right
        + style.border[1].used_width()
        + style.border[3].used_width();
    // a border-box width already holds the padding and border
    let inner =
        padding.left + padding.right + style.border[1].used_width() + style.border[3].used_width();
    let content_box = |width: f32| {
        if style.box_sizing == crate::css::BoxSizing::BorderBox {
            (width - inner).max(0.0)
        } else {
            width
        }
    };
    let bounds = (
        style
            .min_width
            .resolve(containing_width, font_size)
            .map_or(0.0, content_box),
        style
            .max_width
            .resolve(containing_width, font_size)
            .map_or(f32::INFINITY, content_box),
    );
    let bounded = |width: f32| {
        if own_width {
            width.min(bounds.1).max(bounds.0)
        } else {
            width
        }
    };
    if own_width && let Some(width) = style.width.resolve(containing_width, font_size) {
        return extras + bounded(content_box(width).max(0.0));
    }
    if let (Some(width), _) = control_content_size(
        node,
        &style,
        font_size,
        used_line_height(&style, font_size),
        style_state,
    ) {
        return extras + width;
    }
    // a row of flex items, or of table cells, sits side by side as inline content does
    let row_flex = (matches!(style.display, Display::Flex | Display::InlineFlex)
        && style.flex_direction.is_row())
        || style.display == Display::TableRow;
    let nowrap_row = (row_flex && style.flex_wrap == crate::css::FlexWrap::NoWrap)
        || style.display == Display::TableRow;
    let letter_spacing = style
        .letter_spacing
        .resolve(font_size, font_size)
        .unwrap_or(0.0);
    let font = font_style(&style, font_size);
    let child_parent = ParentStyle {
        font_size,
        color: style.color,
        weight: style.font_weight,
        text: inherited_text(&style),
        opacity: 1.0,
    };
    let mut inline = 0.0_f32;
    let mut widest_block = 0.0_f32;
    let text = inherited_text(&style);
    // flex and grid containers drop the spaces around their items
    let keeps_spaces = !matches!(
        style.display,
        Display::Flex | Display::InlineFlex | Display::Grid | Display::InlineGrid
    ) && !row_flex
        && !matches!(
            text.space_collapse,
            SpaceCollapse::Preserve | SpaceCollapse::BreakSpaces
        );
    let children = node.children(&NO_GENERATED_CONTENT);
    let mut joined = String::new();
    let mut after_element = false;
    let mut flex_items = 0_usize;
    for (index, child) in children.iter().enumerate() {
        match *child {
            Child::Text(_, value) => {
                // adjacent text nodes read as one, so spaces at their seams survive collapsing
                joined.push_str(value);
                if matches!(children.get(index + 1), Some(Child::Text(..))) {
                    continue;
                }
                let value = style.text_transform.apply(&std::mem::take(&mut joined));
                let measure = |text: &str| {
                    measure_text(text, style.font_family, font_size, font, letter_spacing)
                };
                let (narrowest, widest) = text_extents(&value, &text, &measure);
                // a run of text in a flex row is an item of its own, with gaps beside it
                let item =
                    row_flex && style.display != Display::TableRow && !value.trim().is_empty();
                if item && (nowrap_row || !minimum) {
                    flex_items += 1;
                }
                if minimum && item && nowrap_row {
                    inline += narrowest;
                } else if minimum {
                    widest_block = widest_block.max(narrowest);
                } else {
                    // a collapsed space beside an element still takes its width on the line
                    let edges = [
                        after_element && value.starts_with(|c: char| c.is_ascii_whitespace()),
                        index + 1 < children.len()
                            && value.ends_with(|c: char| c.is_ascii_whitespace()),
                    ];
                    let spaces = match edges.iter().filter(|edge| **edge).count() {
                        _ if !keeps_spaces => 0,
                        2 if value.trim_ascii().is_empty() => 1,
                        count => count,
                    };
                    inline += widest + spaces as f32 * measure(" ");
                }
            }
            Child::Element(child) => {
                after_element = true;
                let width = intrinsic_width(
                    MaxContentInput {
                        node: child,
                        stylesheet,
                        style_state,
                        parent: child_parent,
                        containing_width,
                        own_width: true,
                    },
                    minimum,
                );
                let child_display = stylesheet
                    .style_for(child.style_context(child_parent, style_state))
                    .display;
                let side_by_side = if minimum {
                    nowrap_row
                } else {
                    row_flex
                        || matches!(
                            child_display,
                            Display::Inline
                                | Display::InlineBlock
                                | Display::InlineFlex
                                | Display::InlineGrid
                        )
                };
                if side_by_side {
                    inline += width;
                } else {
                    widest_block = widest_block.max(width);
                }
                if side_by_side && row_flex && child_display != Display::None {
                    flex_items += 1;
                }
            }
        }
    }
    if style.display != Display::TableRow && flex_items > 1 {
        let gap = style.gap.resolve(0.0, font_size).unwrap_or(0.0).max(0.0);
        inline += gap * (flex_items - 1) as f32;
    }
    extras + bounded(inline.max(widest_block))
}

// one axis of the explicit grid, with auto repeats expanded
struct ExplicitTracks {
    tracks: Vec<TrackSize>,
    // tracks an auto-fit repeat made, which collapse when no item lands in them
    fit: Vec<bool>,
    names: Vec<(usize, String)>,
}

impl ExplicitTracks {
    // grid-template-areas can make the explicit grid larger than its track list
    fn pad_to(&mut self, count: usize, implicit: &[TrackSize]) {
        while self.tracks.len() < count {
            let index = self.tracks.len();
            self.tracks.push(implicit[index % implicit.len()]);
            self.fit.push(false);
        }
    }

    // every track of the grid, with implicit ones before and after the explicit grid
    fn all_tracks(&self, count: usize, shift: usize, implicit: &[TrackSize]) -> Vec<TrackSize> {
        let cycle = implicit.len();
        (0..count)
            .map(|track| {
                if track < shift {
                    implicit[(cycle - (shift - track) % cycle) % cycle]
                } else if let Some(size) = self.tracks.get(track - shift) {
                    *size
                } else {
                    implicit[(track - shift - self.tracks.len()) % cycle]
                }
            })
            .collect()
    }
}

fn fixed_breadth(length: Length, basis: Option<f32>, font_size: f32) -> Option<f32> {
    if length.has_percent() && basis.is_none() {
        return None;
    }
    length
        .resolve(basis.unwrap_or(0.0), font_size)
        .map(|size| size.max(0.0))
}

fn explicit_grid_tracks(
    list: &TrackList,
    available: Option<f32>,
    gap: f32,
    font_size: f32,
) -> ExplicitTracks {
    let Some(repeat) = &list.repeat else {
        return ExplicitTracks {
            tracks: list.tracks.clone(),
            fit: vec![false; list.tracks.len()],
            names: list.names.clone(),
        };
    };
    // a repeated track counts at its fixed maximum, or else its fixed minimum
    let fixed = |size: &TrackSize| {
        let pick = |breadth: Breadth| match breadth {
            Breadth::Length(length) => fixed_breadth(length, available, font_size),
            _ => None,
        };
        pick(size.max).or_else(|| pick(size.min))
    };
    let repeated = repeat.tracks.iter().map(fixed).collect::<Option<Vec<_>>>();
    let count = match (available, repeated) {
        (Some(available), Some(repeated)) => {
            let others = list
                .tracks
                .iter()
                .map(|size| fixed(size).unwrap_or(0.0))
                .sum::<f32>()
                + gap * list.tracks.len() as f32
                - gap;
            let per = repeated.iter().sum::<f32>() + gap * repeated.len() as f32;
            if per > 0.0 {
                ((available - others) / per).floor().clamp(1.0, 10_000.0) as usize
            } else {
                1
            }
        }
        _ => 1,
    };
    let added = repeat.tracks.len() * count;
    let mut tracks = Vec::with_capacity(list.tracks.len() + added);
    tracks.extend_from_slice(&list.tracks[..repeat.at]);
    for _ in 0..count {
        tracks.extend_from_slice(&repeat.tracks);
    }
    tracks.extend_from_slice(&list.tracks[repeat.at..]);
    let mut fit = vec![false; tracks.len()];
    fit[repeat.at..repeat.at + added].fill(repeat.fit);
    let names = list
        .names
        .iter()
        .map(|(line, name)| {
            let line = if *line > repeat.at {
                line + added - repeat.tracks.len()
            } else {
                *line
            };
            (line, name.clone())
        })
        .collect();
    ExplicitTracks { tracks, fit, names }
}

fn implicit_track_sizes(tracks: Tracks) -> Vec<TrackSize> {
    let list = tracks.list();
    if list.tracks.is_empty() {
        vec![TrackSize::AUTO]
    } else {
        list.tracks.clone()
    }
}

// the line an item names: the nth line called name, or name-start or name-end from an area,
// counting into the implicit grid past the named lines that exist
fn find_named_line(lines: &ExplicitTracks, name: &str, suffix: &str, nth: i32) -> i32 {
    let matching = |candidate: &str| {
        let mut found = lines
            .names
            .iter()
            .filter(|(_, line_name)| line_name == candidate)
            .map(|(line, _)| *line as i32)
            .collect::<Vec<_>>();
        found.sort_unstable();
        found.dedup();
        found
    };
    let mut found = matching(name);
    if found.is_empty() {
        found = matching(&format!("{name}-{suffix}"));
    }
    let count = found.len() as i32;
    if nth > 0 {
        if nth <= count {
            found[nth as usize - 1]
        } else {
            lines.tracks.len() as i32 + nth - count
        }
    } else if -nth <= count {
        found[(count + nth) as usize]
    } else {
        nth + count
    }
}

// one axis of an item's placement as a start line, or none for auto placement, and a span
fn resolve_grid_placement(
    placement: GridPlacement,
    lines: &ExplicitTracks,
) -> (Option<i32>, usize) {
    const LIMIT: i32 = 1_000;
    let line = |side: GridLine, suffix: &str| {
        match side {
            GridLine::Line(number) if number > 0 => Some(number - 1),
            GridLine::Line(number) => Some(lines.tracks.len() as i32 + 1 + number),
            GridLine::Named(name, nth) => Some(find_named_line(
                lines,
                &crate::grid::name(name),
                suffix,
                nth,
            )),
            _ => None,
        }
        .map(|line| line.clamp(-LIMIT, LIMIT))
    };
    let span = |side: GridLine| match side {
        GridLine::Span(span) => span.min(LIMIT as u32) as usize,
        _ => 1,
    };
    match (line(placement.start, "start"), line(placement.end, "end")) {
        (Some(start), Some(end)) => {
            let (start, end) = (start.min(end), start.max(end));
            (Some(start), (end - start).max(1) as usize)
        }
        (Some(start), None) => (Some(start), span(placement.end)),
        (None, Some(end)) => {
            let span = span(placement.start);
            (Some(end - span as i32), span)
        }
        (None, None) => match placement.start {
            GridLine::Span(_) => (None, span(placement.start)),
            _ => (None, span(placement.end)),
        },
    }
}

fn grid_cells_free(
    occupied: &[Vec<bool>],
    (major, major_span): (usize, usize),
    (minor, minor_span): (usize, usize),
) -> bool {
    occupied.iter().skip(major).take(major_span).all(|cells| {
        cells
            .iter()
            .skip(minor)
            .take(minor_span)
            .all(|taken| !taken)
    })
}

fn occupy_grid_cells(
    occupied: &mut Vec<Vec<bool>>,
    (major, major_span): (usize, usize),
    (minor, minor_span): (usize, usize),
) {
    if occupied.len() < major + major_span {
        occupied.resize(major + major_span, Vec::new());
    }
    for cells in &mut occupied[major..major + major_span] {
        if cells.len() < minor + minor_span {
            cells.resize(minor + minor_span, false);
        }
        cells[minor..minor + minor_span].fill(true);
    }
}

// places items in grid order: definite areas, then items locked to a row (or column), then the
// rest by the auto-placement cursor; returns each area as (start, span) for columns then rows,
// the track count on each axis, and how many implicit tracks come before the explicit grid
type GridArea = [(usize, usize); 2];

fn place_grid_items(
    placements: &[[(Option<i32>, usize); 2]],
    explicit: [usize; 2],
    column_flow: bool,
    dense: bool,
) -> (Vec<GridArea>, [usize; 2], [usize; 2]) {
    let shift = [0, 1].map(|axis| {
        placements
            .iter()
            .filter_map(|placement| placement[axis].0)
            .min()
            .unwrap_or(0)
            .min(0)
            .unsigned_abs() as usize
    });
    let at = |placement: &[(Option<i32>, usize); 2], axis: usize| {
        placement[axis]
            .0
            .map(|start| (start + shift[axis] as i32) as usize)
    };
    let (major, minor) = if column_flow { (0, 1) } else { (1, 0) };
    let mut minor_count = explicit[minor] + shift[minor];
    for placement in placements {
        let span = placement[minor].1;
        minor_count = minor_count.max(at(placement, minor).map_or(span, |start| start + span));
    }
    let mut areas = vec![[(0, 1); 2]; placements.len()];
    let mut occupied: Vec<Vec<bool>> = Vec::new();
    let mut place =
        |index: usize, major_start: usize, minor_start: usize, occupied: &mut Vec<Vec<bool>>| {
            let placement = &placements[index];
            areas[index][major] = (major_start, placement[major].1);
            areas[index][minor] = (minor_start, placement[minor].1);
            occupy_grid_cells(
                occupied,
                (major_start, placement[major].1),
                (minor_start, placement[minor].1),
            );
        };
    for (index, placement) in placements.iter().enumerate() {
        if let (Some(major_start), Some(minor_start)) = (at(placement, major), at(placement, minor))
        {
            place(index, major_start, minor_start, &mut occupied);
        }
    }
    let mut line_cursors: HashMap<usize, usize> = HashMap::new();
    for (index, placement) in placements.iter().enumerate() {
        let (Some(major_start), None) = (at(placement, major), at(placement, minor)) else {
            continue;
        };
        let mut minor_start = if dense {
            0
        } else {
            line_cursors.get(&major_start).copied().unwrap_or(0)
        };
        while !grid_cells_free(
            &occupied,
            (major_start, placement[major].1),
            (minor_start, placement[minor].1),
        ) {
            minor_start += 1;
        }
        minor_count = minor_count.max(minor_start + placement[minor].1);
        line_cursors.insert(major_start, minor_start + placement[minor].1);
        place(index, major_start, minor_start, &mut occupied);
    }
    let mut cursor = (0, 0);
    for (index, placement) in placements.iter().enumerate() {
        if at(placement, major).is_some() {
            continue;
        }
        if dense {
            cursor = (0, 0);
        }
        let (major_span, minor_span) = (placement[major].1, placement[minor].1);
        match at(placement, minor) {
            Some(minor_start) => {
                if !dense && minor_start < cursor.1 {
                    cursor.0 += 1;
                }
                cursor.1 = minor_start;
                while !grid_cells_free(&occupied, (cursor.0, major_span), (minor_start, minor_span))
                {
                    cursor.0 += 1;
                }
            }
            None => loop {
                if cursor.1 + minor_span > minor_count {
                    cursor = (cursor.0 + 1, 0);
                } else if grid_cells_free(&occupied, (cursor.0, major_span), (cursor.1, minor_span))
                {
                    break;
                } else {
                    cursor.1 += 1;
                }
            },
        }
        place(index, cursor.0, cursor.1, &mut occupied);
        cursor.1 += minor_span;
    }
    let major_count = areas
        .iter()
        .map(|area| area[major].0 + area[major].1)
        .fold(explicit[major] + shift[major], usize::max);
    let mut counts = [0; 2];
    counts[major] = major_count;
    counts[minor] = minor_count;
    (areas, counts, shift)
}

// empty tracks an auto-fit repeat made, which take no space and no gaps
fn collapsed_tracks(
    explicit: &ExplicitTracks,
    count: usize,
    shift: usize,
    items: &[GridItem<'_>],
    axis: usize,
) -> Vec<bool> {
    let mut used = vec![false; count];
    for item in items {
        let (start, span) = item.area[axis];
        used[start..start + span].fill(true);
    }
    (0..count)
        .map(|track| {
            !used[track]
                && track >= shift
                && explicit.fit.get(track - shift).copied().unwrap_or(false)
        })
        .collect()
}

struct GridAxis<'a> {
    tracks: &'a [TrackSize],
    collapsed: &'a [bool],
    available: Option<f32>,
    gap: f32,
    font_size: f32,
    // whether free space goes to auto tracks, as normal content distribution does
    stretch: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Sizing {
    Fixed(f32),
    Fr(f32),
    Auto,
    MinContent,
    MaxContent,
    FitContent(f32),
}

fn grid_tracks_total(sizes: &[f32], collapsed: &[bool], gap: f32) -> f32 {
    let visible = collapsed.iter().filter(|collapsed| !**collapsed).count();
    sizes.iter().sum::<f32>() + gap * visible.saturating_sub(1) as f32
}

fn grow_tracks(sizes: &mut [f32], targets: &[usize], extra: f32) {
    if extra > 0.0 && !targets.is_empty() {
        let share = extra / targets.len() as f32;
        for &track in targets {
            sizes[track] += share;
        }
    }
}

// the grid track sizing algorithm (css grid 1, section 11), given each item's span and a way
// to measure its minimum, min-content and max-content contributions; the minimum is what an
// auto track must leave the item, which a min size of the item's own can set below its
// content
fn size_grid_tracks(
    axis: GridAxis<'_>,
    items: &[(usize, usize)],
    contribution: &mut dyn FnMut(usize) -> [f32; 3],
) -> Vec<f32> {
    let GridAxis {
        tracks,
        collapsed,
        available,
        gap,
        font_size,
        stretch,
    } = axis;
    let sizing = |breadth: Breadth| match breadth {
        Breadth::Length(length) => {
            fixed_breadth(length, available, font_size).map_or(Sizing::Auto, Sizing::Fixed)
        }
        Breadth::Fr(factor) => Sizing::Fr(factor),
        Breadth::Auto => Sizing::Auto,
        Breadth::MinContent => Sizing::MinContent,
        Breadth::MaxContent => Sizing::MaxContent,
        Breadth::FitContent(limit) => {
            Sizing::FitContent(fixed_breadth(limit, available, font_size).unwrap_or(f32::INFINITY))
        }
    };
    let mins = tracks
        .iter()
        .map(|size| sizing(size.min))
        .collect::<Vec<_>>();
    let maxs = tracks
        .iter()
        .map(|size| sizing(size.max))
        .collect::<Vec<_>>();
    let flexible = |track: usize| matches!(maxs[track], Sizing::Fr(_)) && !collapsed[track];
    let factor = |track: usize| match maxs[track] {
        Sizing::Fr(factor) => factor,
        _ => 0.0,
    };
    let intrinsic_min = |track: usize| {
        matches!(
            mins[track],
            Sizing::Auto | Sizing::MinContent | Sizing::MaxContent
        ) && !collapsed[track]
    };
    let intrinsic_max = |track: usize| {
        matches!(
            maxs[track],
            Sizing::Auto | Sizing::MinContent | Sizing::MaxContent | Sizing::FitContent(_)
        ) && !collapsed[track]
    };
    let mut base = (0..tracks.len())
        .map(|track| match mins[track] {
            Sizing::Fixed(size) if !collapsed[track] => size,
            _ => 0.0,
        })
        .collect::<Vec<_>>();
    // what content asks of each intrinsic maximum
    let mut grown = vec![0.0_f32; tracks.len()];
    let fixed_limit = |track: usize| match maxs[track] {
        Sizing::Fixed(size) => Some(size),
        _ => None,
    };
    let mut flex_items = Vec::new();
    let mut order = (0..items.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| items[*index].1);
    for index in order {
        let (start, span) = items[index];
        let range = start..start + span;
        let crosses_flex = range.clone().any(flexible);
        let needs_min = range.clone().any(intrinsic_min);
        let needs_max = range
            .clone()
            .any(|track| intrinsic_max(track) || mins[track] == Sizing::MaxContent)
            || (crosses_flex && available.is_none());
        if !needs_min && !needs_max {
            continue;
        }
        let [minimum, min_content, max_content] = contribution(index);
        let floor = |targets: &[usize]| {
            if targets.iter().all(|track| mins[*track] == Sizing::Auto) {
                minimum
            } else {
                min_content
            }
        };
        let gaps = gap * (span - 1) as f32;
        let spanned_base = range.clone().map(|track| base[track]).sum::<f32>();
        if crosses_flex {
            let targets = range
                .clone()
                .filter(|track| flexible(*track) && intrinsic_min(*track))
                .collect::<Vec<_>>();
            grow_tracks(&mut base, &targets, floor(&targets) - gaps - spanned_base);
            if available.is_none() {
                flex_items.push((range, max_content));
            }
            continue;
        }
        let targets = range
            .clone()
            .filter(|track| intrinsic_min(*track))
            .collect::<Vec<_>>();
        let needed = if targets
            .iter()
            .any(|track| mins[*track] == Sizing::MaxContent)
        {
            max_content
        } else {
            floor(&targets)
        };
        grow_tracks(&mut base, &targets, needed - gaps - spanned_base);
        let targets = range
            .clone()
            .filter(|track| intrinsic_max(*track))
            .collect::<Vec<_>>();
        let needed = if targets
            .iter()
            .all(|track| maxs[*track] == Sizing::MinContent)
        {
            min_content
        } else {
            max_content
        };
        let spanned_limit = range
            .map(|track| fixed_limit(track).unwrap_or(grown[track].max(base[track])))
            .sum::<f32>();
        let extra = needed - gaps - spanned_limit;
        if extra > 0.0 && !targets.is_empty() {
            let share = extra / targets.len() as f32;
            for &track in &targets {
                grown[track] = grown[track].max(base[track]) + share;
            }
        }
    }
    let limit = (0..tracks.len())
        .map(|track| match maxs[track] {
            _ if collapsed[track] => 0.0,
            Sizing::Fixed(size) => size.max(base[track]),
            Sizing::Fr(_) => f32::INFINITY,
            Sizing::FitContent(cap) => grown[track].min(cap).max(base[track]),
            _ => grown[track].max(base[track]),
        })
        .collect::<Vec<_>>();

    // give free space to tracks below their limits
    if let Some(available) = available {
        let mut free = available - grid_tracks_total(&base, collapsed, gap);
        for _ in 0..tracks.len() {
            let growable = (0..tracks.len())
                .filter(|track| !flexible(*track) && limit[*track] > base[*track] + 0.01)
                .collect::<Vec<_>>();
            if free <= 0.01 || growable.is_empty() {
                break;
            }
            let share = free / growable.len() as f32;
            for track in growable {
                let added = share.min(limit[track] - base[track]);
                base[track] += added;
                free -= added;
            }
        }
    } else {
        for track in 0..tracks.len() {
            if !flexible(track) {
                base[track] = limit[track];
            }
        }
    }

    let flex_tracks = (0..tracks.len())
        .filter(|track| flexible(*track))
        .collect::<Vec<_>>();
    if !flex_tracks.is_empty() {
        let fr_size = match available {
            Some(available) => {
                let mut inflexible = vec![false; tracks.len()];
                loop {
                    let fixed_space = (0..tracks.len())
                        .filter(|track| !flexible(*track) || inflexible[*track])
                        .map(|track| base[track])
                        .sum::<f32>();
                    let gaps = grid_tracks_total(&vec![0.0; tracks.len()], collapsed, gap);
                    let factors = flex_tracks
                        .iter()
                        .filter(|track| !inflexible[**track])
                        .map(|track| factor(*track))
                        .sum::<f32>()
                        .max(1.0);
                    let size = (available - fixed_space - gaps).max(0.0) / factors;
                    let newly = flex_tracks
                        .iter()
                        .filter(|track| {
                            !inflexible[**track] && size * factor(**track) < base[**track]
                        })
                        .copied()
                        .collect::<Vec<_>>();
                    if newly.is_empty() {
                        break size;
                    }
                    for track in newly {
                        inflexible[track] = true;
                    }
                }
            }
            None => {
                let mut size = flex_tracks
                    .iter()
                    .map(|track| {
                        let factor = factor(*track);
                        if factor > 1.0 {
                            base[*track] / factor
                        } else {
                            base[*track]
                        }
                    })
                    .fold(0.0, f32::max);
                for (range, max_content) in flex_items {
                    let span = range.len();
                    let rest = max_content
                        - gap * (span - 1) as f32
                        - range
                            .clone()
                            .filter(|track| !flexible(*track))
                            .map(|track| base[track])
                            .sum::<f32>();
                    let factors = range.map(factor).sum::<f32>().max(1.0);
                    size = size.max(rest / factors);
                }
                size
            }
        };
        for track in flex_tracks {
            base[track] = base[track].max(fr_size * factor(track));
        }
    }

    if stretch && let Some(available) = available {
        let free = available - grid_tracks_total(&base, collapsed, gap);
        let targets = (0..tracks.len())
            .filter(|track| maxs[*track] == Sizing::Auto && !collapsed[*track])
            .collect::<Vec<_>>();
        grow_tracks(&mut base, &targets, free);
    }
    base
}

// where each track starts, after content distribution places the tracks in the space
fn grid_track_offsets(
    sizes: &[f32],
    collapsed: &[bool],
    gap: f32,
    available: f32,
    distribution: JustifyContent,
) -> Vec<f32> {
    let visible = collapsed.iter().filter(|collapsed| !**collapsed).count();
    let free = available - grid_tracks_total(sizes, collapsed, gap);
    let (mut position, between) = justify_offsets(distribution, free, visible);
    let mut offsets = Vec::with_capacity(sizes.len());
    let mut first = true;
    for (size, collapsed) in sizes.iter().zip(collapsed) {
        if !collapsed && !first {
            position += gap + between;
        }
        offsets.push(position);
        if !collapsed {
            position += size;
            first = false;
        }
    }
    offsets
}

fn justify_for_content(align: AlignContent) -> JustifyContent {
    match align {
        AlignContent::Stretch | AlignContent::FlexStart => JustifyContent::FlexStart,
        AlignContent::FlexEnd => JustifyContent::FlexEnd,
        AlignContent::Center => JustifyContent::Center,
        AlignContent::SpaceBetween => JustifyContent::SpaceBetween,
        AlignContent::SpaceAround => JustifyContent::SpaceAround,
        AlignContent::SpaceEvenly => JustifyContent::SpaceEvenly,
    }
}

// auto margins on an axis take the free space, which aligns the item
fn auto_margin_alignment(start: Length, end: Length) -> Option<AlignItems> {
    match (start == Length::Auto, end == Length::Auto) {
        (true, true) => Some(AlignItems::Center),
        (true, false) => Some(AlignItems::FlexEnd),
        (false, true) => Some(AlignItems::FlexStart),
        (false, false) => None,
    }
}

fn positioned_z_index(style: Style) -> i32 {
    if style.position.is_out_of_flow()
        || matches!(style.position, Position::Relative | Position::Sticky)
    {
        style.z_index.unwrap_or(0)
    } else {
        0
    }
}

fn creates_stacking_context(style: Style) -> bool {
    style.has_transform()
        || style.perspective.is_some()
        || style.clip_path.is_some()
        || style.masks.is_some_and(|masks| !masks.layers().is_empty())
        || style.filter.is_some()
        || style.backdrop_filter.is_some()
        || matches!(style.position, Position::Fixed | Position::Sticky)
        || (style.position != Position::Static && style.z_index.is_some())
}

// resolves the transform properties and clip-path against the border box, then lets the
// box's perspective act on the transformed children laid out inside it
fn apply_transform(layout: &mut LayoutBox, style: &Style, font_size: f32) {
    use crate::transform::Matrix;
    let (width, height) = (layout.rect.width, layout.rect.height);
    layout.clip_path = style
        .clip_path
        .and_then(|path| path.resolve(width, height, font_size));
    if style.has_transform() {
        let matrix = [
            style.translate_property,
            style.rotate_property,
            style.scale_property,
            style.transform,
        ]
        .into_iter()
        .flatten()
        .fold(Matrix::IDENTITY, |matrix, list| {
            matrix.multiply(&list.matrix(width, height, font_size))
        });
        if !matrix.is_identity() {
            let origin_x = style
                .transform_origin
                .0
                .resolve(width, font_size)
                .unwrap_or(width / 2.0);
            let origin_y = style
                .transform_origin
                .1
                .resolve(height, font_size)
                .unwrap_or(height / 2.0);
            layout.transform = Some(matrix.about(origin_x, origin_y));
        }
    }
    if let Some(distance) = style.perspective {
        let origin_x = style
            .perspective_origin
            .0
            .resolve(width, font_size)
            .unwrap_or(width / 2.0);
        let origin_y = style
            .perspective_origin
            .1
            .resolve(height, font_size)
            .unwrap_or(height / 2.0);
        let (corner_x, corner_y) = (layout.rect.x, layout.rect.y);
        for child in &mut layout.children {
            let Some(transform) = child.transform else {
                continue;
            };
            let relative_x = corner_x + origin_x - child.rect.x;
            let relative_y = corner_y + origin_y - child.rect.y;
            child.transform = Some(
                Matrix::perspective(distance)
                    .about(relative_x, relative_y)
                    .multiply(&transform),
            );
        }
    }
}

fn apply_relative_offset(
    layout: &mut LayoutBox,
    style: Style,
    containing: ContainingBlock,
    font_size: f32,
) {
    // a translation moves the visual subtree the same way a relative inset does
    let translate = (
        style
            .translate
            .0
            .resolve(layout.rect.width, font_size)
            .unwrap_or(0.0),
        style
            .translate
            .1
            .resolve(layout.rect.height, font_size)
            .unwrap_or(0.0),
    );
    if style.position != Position::Relative {
        layout.visual_offset = translate;
        return;
    }
    let inset = resolve_insets(style.inset, containing, font_size);
    layout.visual_offset = (
        translate.0
            + inset
                .left
                .or_else(|| inset.right.map(|value| -value))
                .unwrap_or(0.0),
        translate.1
            + inset
                .top
                .or_else(|| inset.bottom.map(|value| -value))
                .unwrap_or(0.0),
    );
}

fn finalize_scroll(layout: &mut LayoutBox, padding: ResolvedEdges, style_state: &StyleState) {
    let size = Size {
        width: layout.rect.width,
        height: layout.rect.height,
    };
    layout.scroll_content = measure_scroll_content(layout, padding, size);
    let is_document = layout.is_document();
    // clip never scrolls; hidden scrolls only when script asks
    if !matches!(layout.overflow, Overflow::Auto | Overflow::Hidden) && !is_document {
        return;
    }
    // measured from the scrollport, the gutter one scrollbar takes cannot read as overflow
    // on the other axis
    let gutter = scrollbar_metrics(layout).scrollport;
    let floor = Size {
        width: gutter.width,
        height: gutter.height,
    };
    layout.scroll_content = measure_scroll_content(layout, padding, floor);
    let scrollport = scrollbar_metrics(layout).scrollport;
    layout.scroll_content.width = layout.scroll_content.width.max(scrollport.width);
    layout.scroll_content.height = layout.scroll_content.height.max(scrollport.height);
    let max_x = (layout.scroll_content.width - scrollport.width).max(0.0);
    let max_y = (layout.scroll_content.height - scrollport.height).max(0.0);
    let requested = style_state
        .scroll_offsets
        .get(&layout.node)
        .copied()
        .unwrap_or_default();
    let offset = (requested.0.clamp(0.0, max_x), requested.1.clamp(0.0, max_y));
    layout.scroll_offset = offset;
    let scrollport = scrollbar_metrics(layout).scrollport;
    if offset == (0.0, 0.0) {
        apply_sticky_descendants(
            &mut layout.children,
            scrollport,
            layout.rect,
            layout.visual_offset,
        );
        return;
    }
    for child in &mut layout.children {
        translate_scroll_box(child, -offset.0, -offset.1);
    }
    apply_sticky_descendants(
        &mut layout.children,
        scrollport,
        layout.rect,
        layout.visual_offset,
    );
}

// what laying out again with only the scroll offsets changed would give, by moving each
// scroller's content as finalize_scroll does; none when a sticky box would have to settle again
pub(crate) fn rescroll(
    document: &LayoutDocument,
    offsets: &std::collections::BTreeMap<NodeId, (f32, f32)>,
) -> Option<LayoutDocument> {
    fn has_sticky(layout: &LayoutBox) -> bool {
        layout.position == Position::Sticky || layout.children.iter().any(has_sticky)
    }
    fn scroll(
        layout: &mut LayoutBox,
        offsets: &std::collections::BTreeMap<NodeId, (f32, f32)>,
    ) -> bool {
        if matches!(layout.overflow, Overflow::Auto | Overflow::Hidden) || layout.is_document() {
            let scrollport = scrollbar_metrics(layout).scrollport;
            let max_x = (layout.scroll_content.width - scrollport.width).max(0.0);
            let max_y = (layout.scroll_content.height - scrollport.height).max(0.0);
            let requested = offsets.get(&layout.node).copied().unwrap_or_default();
            let offset = (requested.0.clamp(0.0, max_x), requested.1.clamp(0.0, max_y));
            if offset != layout.scroll_offset {
                if layout.children.iter().any(has_sticky) {
                    return false;
                }
                let shift = (
                    layout.scroll_offset.0 - offset.0,
                    layout.scroll_offset.1 - offset.1,
                );
                for child in &mut layout.children {
                    translate_scroll_box(child, shift.0, shift.1);
                }
                layout.scroll_offset = offset;
            }
        }
        layout
            .children
            .iter_mut()
            .all(|child| scroll(child, offsets))
    }
    let mut moved = document.clone();
    scroll(&mut moved.root, offsets).then_some(moved)
}

fn apply_sticky_descendants(
    children: &mut [LayoutBox],
    scrollport: Rect,
    containing_block: Rect,
    ancestor_offset: (f32, f32),
) {
    for child in children {
        let child_rect = visual_rect_for(child, ancestor_offset);
        if child.position == Position::Sticky {
            let top = child
                .sticky_inset
                .top
                .resolve(scrollport.height, child.font_size)
                .map(|value| value.max(0.0));
            let bottom = child
                .sticky_inset
                .bottom
                .resolve(scrollport.height, child.font_size)
                .map(|value| value.max(0.0));
            let mut shift: f32 = 0.0;
            if let Some(top) = top {
                shift = shift.max(scrollport.y + top - child_rect.y);
            }
            if let Some(bottom) = bottom {
                shift = shift.min(scrollport.bottom() - bottom - child_rect.bottom());
            }
            let max_down = (containing_block.bottom() - child_rect.bottom()).max(0.0);
            let max_up = (containing_block.y - child_rect.y).min(0.0);
            child.visual_offset.1 += shift.clamp(max_up, max_down);
        }
        let child_offset = visual_offset_for(child, ancestor_offset);
        let child_rect = visual_rect_for(child, ancestor_offset);
        if child.overflow == Overflow::Auto {
            continue;
        }
        apply_sticky_descendants(&mut child.children, scrollport, child_rect, child_offset);
    }
}

// in border box coordinates, so the extent counts both borders as the scrollport does
fn measure_scroll_content(layout: &LayoutBox, padding: ResolvedEdges, floor: Size) -> Size {
    let mut right = layout.rect.x + floor.width;
    let mut bottom = layout.rect.y + floor.height;
    let border = layout.border.widths;
    // rounding in the sums below must not read as overflow
    const SLACK: f32 = 0.01;
    for child in &layout.children {
        if child.position == Position::Fixed {
            continue;
        }
        // as in chromium, in-flow margin boxes carry the end padding and border with them,
        // while an absolutely positioned box reaches only as far as its border box
        let (end_x, end_y) = if child.position == Position::Absolute {
            (border.right, border.bottom)
        } else {
            (
                child.margin.right + padding.right + border.right,
                child.margin.bottom + padding.bottom + border.bottom,
            )
        };
        right = right.max(child.rect.x + child.visual_offset.0 + child.rect.width + end_x);
        bottom = bottom.max(child.rect.y + child.visual_offset.1 + child.rect.height + end_y);
    }
    let extent = |far: f32, start: f32, size: f32| {
        let measured = far - start;
        if measured > size + SLACK {
            measured
        } else {
            size
        }
    };
    Size {
        width: extent(right, layout.rect.x, floor.width),
        height: extent(bottom, layout.rect.y, floor.height),
    }
}

fn translate_scroll_box(layout: &mut LayoutBox, offset_x: f32, offset_y: f32) {
    if layout.position == Position::Fixed {
        return;
    }
    layout.rect.x += offset_x;
    layout.rect.y += offset_y;
    for text in &mut layout.text {
        text.x += offset_x;
        text.y += offset_y;
    }
    if let Some(origin) = &mut layout.control_text_origin {
        origin.0 += offset_x;
        origin.1 += offset_y;
    }
    for child in &mut layout.children {
        translate_scroll_box(child, offset_x, offset_y);
    }
}

fn translate_layout_box(layout: &mut LayoutBox, offset_x: f32, offset_y: f32) {
    layout.rect.x += offset_x;
    layout.rect.y += offset_y;
    for text in &mut layout.text {
        text.x += offset_x;
        text.y += offset_y;
    }
    if let Some(origin) = &mut layout.control_text_origin {
        origin.0 += offset_x;
        origin.1 += offset_y;
    }
    for child in &mut layout.children {
        translate_layout_box(child, offset_x, offset_y);
    }
}

fn append_inline_element(
    input: InlineElementInput<'_>,
    text: &mut Vec<TextRun>,
    line: &mut InlineLine,
) {
    let InlineElementInput {
        node,
        parent,
        context,
        stylesheet,
        style_state,
        viewport,
        containing_block,
    } = input;
    if node.name() == "br" {
        if !line.has_content() {
            line.y += context.line_height;
            line.height = 0.0;
            line.fit_to_floats(context.content_x, context.content_width);
        } else {
            flush_line(
                line,
                text,
                context.content_x,
                context.content_width,
                context.align,
            );
        }
        return;
    }
    let style = stylesheet.style_for(node.style_context(parent, style_state));
    if style.display == Display::None {
        return;
    }
    if style.float != Float::None && !style.position.is_out_of_flow() {
        place_float(input, &style, line);
        return;
    }
    if matches!(
        style.display,
        Display::InlineBlock | Display::InlineFlex | Display::InlineGrid | Display::InlineTable
    ) || node.replaced()
    {
        append_atomic(input, &style, text, line);
        return;
    }
    let font_size = style
        .font_size
        .resolve(parent.font_size, parent.font_size)
        .unwrap_or(parent.font_size)
        .max(1.0);
    let line_height = used_line_height(&style, font_size);
    let (above, _) = line_extent(line_height, style.font_family, font_size);
    let raise = match vertical_shift(
        style.vertical_align,
        &context,
        (line_height, above),
        (font_size, line_height),
    ) {
        LineShift::Raise(raise) => raise,
        LineShift::Top | LineShift::Bottom => context.raise,
    };
    let child_context = InlineContext {
        font_size,
        line_height,
        color: text_color(&style),
        weight: style.font_weight,
        text: inherited_text(&style),
        opacity: style.opacity * parent.opacity,
        content_width: context.content_width,
        content_x: context.content_x,
        align: context.align,
        raise,
    };
    let child_parent = ParentStyle {
        font_size,
        color: style.color,
        weight: style.font_weight,
        text: inherited_text(&style),
        opacity: style.opacity * parent.opacity,
    };
    let margin = resolve_margins(style.margin, context.content_width, font_size);
    let padding = resolve_edges(style.padding, context.content_width, font_size);
    let border = used_border(&style);
    let decoration = line.decorations.len();
    line.decorations.push(InlineDecoration {
        node: node.node,
        tag: node.name().to_string(),
        style,
        font_size,
        raise,
        padding,
        border,
        margin,
        opacity: style.opacity * parent.opacity,
    });
    line.push_edge(decoration, true, margin.left + border.left + padding.left);
    let generated = node.generated_content(stylesheet, style_state);
    for child in node.children(&generated) {
        match child {
            Child::Text(text_node, value) => {
                let source = InlineSource {
                    node: text_node,
                    offset: 0,
                };
                append_inline_text(value, child_context, source, text, line)
            }
            Child::Element(child) => append_inline_element(
                InlineElementInput {
                    node: child,
                    parent: child_parent,
                    context: child_context,
                    stylesheet,
                    style_state,
                    viewport,
                    containing_block,
                },
                text,
                line,
            ),
        }
    }
    line.push_edge(
        decoration,
        false,
        padding.right + border.right + margin.right,
    );
}

fn append_inline_text(
    value: &str,
    context: InlineContext,
    source: InlineSource,
    text: &mut Vec<TextRun>,
    line: &mut InlineLine,
) {
    let mut placer = TextPlacer {
        context,
        source,
        letter_spacing: context
            .text
            .letter_spacing
            .resolve(context.font_size, context.font_size)
            .unwrap_or(0.0),
        font: context.font(),
        levels: None,
        space: 0.0,
        leading: half_leading(context.line_height, context.text.family, context.font_size),
    };
    placer.space = placer.measure(" ");
    // right-to-left text, or any text in a right-to-left paragraph, resolves its levels
    if line.rtl || value.chars().any(|character| u32::from(character) >= 0x590) {
        let (_, levels) = resolve_levels(value, Some(u8::from(line.rtl)));
        if levels.iter().any(|level| *level > 0) {
            placer.levels = Some(levels_by_utf16(value, &levels));
        }
    }
    match context.text.space_collapse {
        SpaceCollapse::Collapse | SpaceCollapse::PreserveBreaks => {
            placer.collapsed(value, text, line);
        }
        SpaceCollapse::Preserve | SpaceCollapse::BreakSpaces => placer.preserved(value, text, line),
    }
}

// puts the words and spaces of one text node on lines
struct TextPlacer {
    context: InlineContext,
    source: InlineSource,
    letter_spacing: f32,
    font: FontStyle,
    // each UTF-16 unit's bidi level, when some text is right to left
    levels: Option<Vec<u8>>,
    // measured once for the node: a space, and half the leading
    space: f32,
    leading: f32,
}

// whether the line breaking algorithm could find a break inside a word: ASCII letters, digits
// and most punctuation hold together, and the rest is left to the full algorithm
fn may_break_inside(word: &str) -> bool {
    !word.is_ascii()
        || word
            .bytes()
            .any(|byte| matches!(byte, b'-' | b'/' | b'!' | b'?' | b']' | b'}' | b'|'))
}

// spreads per-character levels over the UTF-16 units each character takes
fn levels_by_utf16(value: &str, levels: &[u8]) -> Vec<u8> {
    value
        .chars()
        .zip(levels)
        .flat_map(|(character, level)| std::iter::repeat_n(*level, character.len_utf16()))
        .collect()
}

// a piece of text that sits on a line whole
struct TextUnit<'a> {
    text: &'a str,
    // where it starts in the text node, in UTF-16 units
    offset: usize,
    // a collapsed space drawn before it, when it is not the first thing on its line
    space_before: bool,
    // whether the line may end just before it
    breakable: bool,
}

impl TextPlacer {
    fn measure(&self, text: &str) -> f32 {
        measure_text(
            text,
            self.context.text.family,
            self.context.font_size,
            self.font,
            self.letter_spacing,
        )
    }

    // where a word may break inside, for scripts without spaces, hyphens and the like
    fn pieces<'a>(&self, word: &'a str) -> Vec<&'a str> {
        if !self.context.text.wrap
            || (!may_break_inside(word) && self.context.text.word_break != WordBreak::BreakAll)
        {
            return vec![word];
        }
        let mut pieces = Vec::new();
        let mut start = 0;
        for (offset, _) in break_opportunities(word, self.context.text.word_break) {
            pieces.push(&word[start..offset]);
            start = offset;
        }
        pieces.push(&word[start..]);
        pieces
    }

    // a line feed kept by white-space ends the line, or makes an empty one
    fn forced_break(&self, text: &mut Vec<TextRun>, line: &mut InlineLine) {
        if line.has_content() {
            flush_line(
                line,
                text,
                self.context.content_x,
                self.context.content_width,
                self.context.align,
            );
        } else {
            line.y += self.context.line_height;
            line.height = 0.0;
            line.fit_to_floats(self.context.content_x, self.context.content_width);
        }
    }

    // white space collapses to single spaces between words; under pre-line, line feeds stay
    fn collapsed(&mut self, value: &str, text: &mut Vec<TextRun>, line: &mut InlineLine) {
        let keep_breaks = self.context.text.space_collapse == SpaceCollapse::PreserveBreaks;
        let mut pending_space = line.trailing_space;
        let mut units = self.source.offset;
        let mut rest = value;
        while !rest.is_empty() {
            let space_end = rest
                .find(|character: char| !character.is_ascii_whitespace())
                .unwrap_or(rest.len());
            if space_end > 0 {
                let spaces = &rest[..space_end];
                if keep_breaks && spaces.contains('\n') {
                    for _ in 0..spaces.matches('\n').count() {
                        self.forced_break(text, line);
                    }
                    pending_space = false;
                } else {
                    pending_space = true;
                }
                units += spaces.len();
                rest = &rest[space_end..];
                continue;
            }
            let word_end = rest
                .find(|character: char| character.is_ascii_whitespace())
                .unwrap_or(rest.len());
            let word = &rest[..word_end];
            for (index, piece) in self.pieces(word).into_iter().enumerate() {
                let first = index == 0;
                self.place(
                    TextUnit {
                        text: piece,
                        offset: units,
                        space_before: first && pending_space,
                        breakable: !first || pending_space,
                    },
                    text,
                    line,
                );
                units += piece.encode_utf16().count();
            }
            pending_space = false;
            rest = &rest[word_end..];
        }
        if !value.is_empty() {
            line.trailing_space = pending_space;
        }
    }

    // spaces and line feeds stay as written; lines still wrap at spaces under pre-wrap
    fn preserved(&mut self, value: &str, text: &mut Vec<TextRun>, line: &mut InlineLine) {
        let spaces_break = self.context.text.space_collapse == SpaceCollapse::BreakSpaces;
        let wrap = self.context.text.wrap;
        let mut units = self.source.offset;
        for (index, segment) in value.split('\n').enumerate() {
            if index > 0 {
                self.forced_break(text, line);
                units += 1;
            }
            let segment = segment.strip_suffix('\r').unwrap_or(segment);
            let mut rest = segment;
            let mut after_space = false;
            while !rest.is_empty() {
                let space_end = rest
                    .find(|character: char| character != ' ' && character != '\t')
                    .unwrap_or(rest.len());
                if space_end > 0 {
                    // a tab advances to the next stop eight spaces apart
                    let space = self.space.max(0.1);
                    let mut shown = String::new();
                    let mut used = line.used;
                    for character in rest[..space_end].chars() {
                        let count = if character == '\t' {
                            let stop = space * 8.0;
                            (((used / stop).floor() + 1.0) * stop - used) / space
                        } else {
                            1.0
                        };
                        let count = count.round().max(1.0) as usize;
                        shown.push_str(&" ".repeat(count));
                        used += space * count as f32;
                    }
                    self.place_preserved(&shown, units, spaces_break && wrap, text, line);
                    units += rest[..space_end].encode_utf16().count();
                    rest = &rest[space_end..];
                    after_space = true;
                    continue;
                }
                let word_end = rest.find([' ', '\t']).unwrap_or(rest.len());
                for (piece_index, piece) in self.pieces(&rest[..word_end]).into_iter().enumerate() {
                    self.place(
                        TextUnit {
                            text: piece,
                            offset: units,
                            space_before: false,
                            breakable: wrap && (piece_index > 0 || after_space),
                        },
                        text,
                        line,
                    );
                    units += piece.encode_utf16().count();
                }
                after_space = false;
                rest = &rest[word_end..];
            }
        }
        line.trailing_space = false;
    }

    // kept spaces hang at the end of a line rather than wrapping it, unless break-spaces
    fn place_preserved(
        &mut self,
        spaces: &str,
        offset: usize,
        breakable: bool,
        text: &mut Vec<TextRun>,
        line: &mut InlineLine,
    ) {
        let width = self.measure(spaces);
        if breakable
            && line.has_content()
            && line.overflows(line.used + width, self.context.content_width)
        {
            flush_line(
                line,
                text,
                self.context.content_x,
                self.context.content_width,
                self.context.align,
            );
        }
        self.push(
            spaces.to_string(),
            width,
            offset,
            spaces.encode_utf16().count(),
            line,
        );
    }

    fn place(&mut self, unit: TextUnit<'_>, text: &mut Vec<TextRun>, line: &mut InlineLine) {
        let TextUnit {
            text: source_text,
            offset,
            space_before,
            breakable,
        } = unit;
        let transformed = match self.context.text.transform {
            crate::css::TextTransform::None => std::borrow::Cow::Borrowed(source_text),
            transform => std::borrow::Cow::Owned(transform.apply(source_text)),
        };
        let word = transformed.as_ref();
        let word_width = self.measure(word);
        let (content_x, content_width) = (self.context.content_x, self.context.content_width);
        if !line.has_content() {
            line.fit_to_floats(content_x, content_width);
            line.make_room(word_width, content_x, content_width);
        }
        let after_opening = line
            .fragments
            .last()
            .is_some_and(|fragment| matches!(fragment.edge, Some((_, true))));
        // a space before an element's first word belongs to the text before the element, in
        // that text's font
        let spaced = space_before && line.has_content();
        let mut space_width = match line.text_before_edges() {
            _ if !spaced => 0.0,
            Some(previous) if after_opening => measure_text(
                " ",
                previous.font_stack,
                previous.font_size,
                previous.font,
                previous.letter_spacing,
            ),
            _ => self.space,
        };
        if self.context.text.wrap
            && breakable
            && line.has_content()
            && line.overflows(line.used + space_width + word_width, content_width)
        {
            flush_line(line, text, content_x, content_width, self.context.align);
            // the space that broke the line is not drawn at the start of the next
            space_width = 0.0;
        }
        if space_width > 0.0
            && after_opening
            && let Some(previous) = line.text_before_edges()
        {
            previous.text.push(' ');
            previous.width += space_width;
            line.used += space_width;
            space_width = 0.0;
        }
        let source_len = source_text.encode_utf16().count();
        // a word too wide for any line breaks between its characters when overflow-wrap allows
        if self.context.text.wrap
            && self.context.text.overflow_wrap != OverflowWrap::Normal
            && line.overflows(word_width, content_width)
            && word.chars().nth(1).is_some()
        {
            if space_width > 0.0 {
                self.push(" ".to_string(), space_width, offset, 0, line);
            }
            let mut piece = String::new();
            let mut piece_offset = offset;
            for character in word.chars() {
                let mut candidate = piece.clone();
                candidate.push(character);
                if !piece.is_empty()
                    && line.overflows(line.used + self.measure(&candidate), content_width)
                {
                    let width = self.measure(&piece);
                    let length = piece.encode_utf16().count();
                    self.push(
                        std::mem::take(&mut piece),
                        width,
                        piece_offset,
                        length,
                        line,
                    );
                    piece_offset += length;
                    flush_line(line, text, content_x, content_width, self.context.align);
                }
                piece.push(character);
            }
            if !piece.is_empty() {
                let width = self.measure(&piece);
                let length = piece.encode_utf16().count();
                self.push(piece, width, piece_offset, length, line);
            }
            return;
        }
        let shown = if space_width > 0.0 {
            format!(" {word}")
        } else {
            word.to_string()
        };
        let width = if space_width > 0.0 {
            self.measure(&shown)
        } else {
            word_width
        };
        self.push(shown, width, offset, source_len, line);
    }

    fn push(
        &self,
        shown: String,
        width: f32,
        offset: usize,
        source_len: usize,
        line: &mut InlineLine,
    ) {
        let level = self.levels.as_ref().map_or(line.base_level(), |levels| {
            levels
                .get(offset.saturating_sub(self.source.offset))
                .copied()
                .unwrap_or(line.base_level())
        });
        line.used += width;
        line.height = line.height.max(self.context.line_height);
        line.fragments.push(InlineFragment {
            text: shown,
            width,
            font_size: self.context.font_size,
            letter_spacing: self.letter_spacing,
            font_stack: self.context.text.family,
            color: self.context.color.scale_alpha(self.context.opacity),
            font: self.font,
            node: self.source.node,
            node_offset: offset,
            source_len,
            rtl: false,
            decoration: self.context.text.decoration,
            line_height: self.context.line_height,
            atomic: None,
            edge: None,
            level,
            leading: self.leading,
            baseline: line_extent(
                self.context.line_height,
                self.context.text.family,
                self.context.font_size,
            )
            .0,
            shift: LineShift::Raise(self.context.raise),
        });
    }
}

// lays out an inline-block or replaced element as a block, then places it on the line
fn append_atomic(
    input: InlineElementInput<'_>,
    style: &Style,
    text: &mut Vec<TextRun>,
    line: &mut InlineLine,
) {
    let InlineElementInput {
        node,
        parent,
        context,
        stylesheet,
        style_state,
        viewport,
        containing_block,
    } = input;
    let font_size = style
        .font_size
        .resolve(parent.font_size, parent.font_size)
        .unwrap_or(parent.font_size)
        .max(1.0);
    // an auto width shrinks to the content, as inline-block sizing requires
    let containing_width =
        if style.width == crate::css::Length::Auto && style.aspect_ratio.is_none() {
            max_content_width(MaxContentInput {
                node,
                stylesheet,
                style_state,
                parent,
                containing_width: context.content_width,
                own_width: true,
            })
            .min(context.content_width)
        } else {
            context.content_width
        };
    let Some(laid_out) = layout_element(ElementLayoutInput {
        node,
        containing_x: context.content_x + line.left + line.used,
        containing_y: line.y,
        containing_width,
        containing_height: None,
        size_override: (None, None),
        floats: None,
        list_ordinal: 1,
        parent,
        context: LayoutContext {
            viewport,
            containing_block,
            mode: LayoutMode::Flow,
            stylesheet,
            style_state,
        },
    }) else {
        return;
    };
    let outer_width = laid_out.rect.width + laid_out.margin.left + laid_out.margin.right;
    let outer_height = laid_out.rect.height + laid_out.margin.top + laid_out.margin.bottom;
    if !line.has_content() {
        line.fit_to_floats(context.content_x, context.content_width);
        line.make_room(outer_width, context.content_x, context.content_width);
    }
    // a collapsed space before the box is drawn unless the line breaks there
    if line.trailing_space && line.has_content() {
        let font = context.font();
        let space = match line.text_before_edges() {
            Some(previous) => measure_text(
                " ",
                previous.font_stack,
                previous.font_size,
                previous.font,
                previous.letter_spacing,
            ),
            None => measure_text(" ", context.text.family, context.font_size, font, 0.0),
        };
        if !line.overflows(line.used + space + outer_width, context.content_width) {
            if let Some(previous) = line.text_before_edges() {
                previous.text.push(' ');
                previous.width += space;
            } else {
                line.fragments.push(InlineFragment {
                    text: " ".to_string(),
                    width: space,
                    font_size: context.font_size,
                    letter_spacing: 0.0,
                    font_stack: context.text.family,
                    color: Color::transparent(),
                    font,
                    node: None,
                    node_offset: 0,
                    source_len: 0,
                    rtl: false,
                    decoration: Default::default(),
                    line_height: context.line_height,
                    atomic: None,
                    edge: None,
                    level: line.base_level(),
                    leading: half_leading(
                        context.line_height,
                        context.text.family,
                        context.font_size,
                    ),
                    baseline: line_extent(
                        context.line_height,
                        context.text.family,
                        context.font_size,
                    )
                    .0,
                    shift: LineShift::Raise(context.raise),
                });
            }
            line.used += space;
        }
    }
    if line.has_content() && line.overflows(line.used + outer_width, context.content_width) {
        flush_line(
            line,
            text,
            context.content_x,
            context.content_width,
            context.align,
        );
    }
    // a replaced box, a box that clips or one with no line of text sits on its bottom margin
    // edge; inline-block takes its last line, the other inline boxes their first
    let baseline = (!style.overflow.clips() && !node.replaced())
        .then(|| {
            if style.display == Display::InlineBlock {
                last_baseline(&laid_out)
            } else {
                first_baseline(&laid_out)
            }
        })
        .flatten()
        .map_or(outer_height, |baseline| {
            laid_out.margin.top + baseline - laid_out.rect.y
        });
    let line_height = used_line_height(style, font_size);
    let shift = vertical_shift(
        style.vertical_align,
        &context,
        (outer_height, baseline),
        (font_size, line_height),
    );
    line.used += outer_width;
    line.height = line.height.max(outer_height);
    line.trailing_space = false;
    line.fragments.push(InlineFragment {
        text: String::new(),
        width: outer_width,
        font_size,
        letter_spacing: 0.0,
        font_stack: context.text.family,
        color: Color::transparent(),
        font: FontStyle::NORMAL,
        node: None,
        node_offset: 0,
        source_len: 0,
        rtl: false,
        decoration: Default::default(),
        line_height: outer_height,
        atomic: Some(Box::new(laid_out)),
        edge: None,
        level: line.base_level(),
        leading: 0.0,
        baseline,
        shift,
    });
}

fn flush_line(
    line: &mut InlineLine,
    text: &mut Vec<TextRun>,
    content_x: f32,
    content_width: f32,
    align: TextAlign,
) {
    if !line.has_content() {
        return;
    }
    let available = line.available(content_width);
    if line.ellipsis && line.used > available + 0.5 {
        truncate_with_ellipsis(line, available);
    }
    let end = (available - line.used).max(0.0);
    let offset = match (align, line.rtl) {
        (TextAlign::Left, _) | (TextAlign::Start, false) | (TextAlign::End, true) => 0.0,
        (TextAlign::Center, _) => end / 2.0,
        _ => end,
    };
    let mut x = content_x + line.left + offset;
    let line_start = x;
    let (mut above, mut below) = line.strut;
    let mut pinned = 0.0_f32;
    for fragment in line
        .fragments
        .iter()
        .filter(|fragment| fragment.edge.is_none())
    {
        match fragment.shift {
            LineShift::Raise(raise) => {
                above = above.max(fragment.baseline + raise);
                below = below.max(fragment.line_height - fragment.baseline - raise);
            }
            LineShift::Top | LineShift::Bottom => pinned = pinned.max(fragment.line_height),
        }
    }
    let height = (above + below).max(pinned);
    let baseline_y = line.y + above;
    let mut active = line.open_inlines.clone();
    let mut starts = HashMap::new();
    let mut ends = HashMap::new();
    let mut fragments = std::mem::take(&mut line.fragments);
    if line.rtl || fragments.iter().any(|fragment| fragment.level > 0) {
        fragments = visual_fragments(fragments, line.base_level());
    }
    for fragment in fragments {
        if let Some((decoration, opening)) = fragment.edge {
            let margin = line.decorations[decoration].margin;
            if opening {
                starts.insert(decoration, x + margin.left);
                active.push(decoration);
            } else {
                ends.insert(decoration, x + fragment.width - margin.right);
            }
            x += fragment.width;
            continue;
        }
        let top = match fragment.shift {
            LineShift::Raise(raise) => baseline_y - raise - fragment.baseline,
            LineShift::Top => line.y,
            LineShift::Bottom => line.y + height - fragment.line_height,
        };
        if let Some(mut atomic) = fragment.atomic {
            let outer_x = atomic.rect.x - atomic.margin.left;
            let outer_y = atomic.rect.y - atomic.margin.top;
            translate_layout_box(&mut atomic, x - outer_x, top - outer_y);
            line.placed.push(*atomic);
            x += fragment.width;
            continue;
        }
        text.push(TextRun {
            y: top + fragment.leading,
            text: fragment.text,
            x,
            width: fragment.width,
            font_size: fragment.font_size,
            letter_spacing: fragment.letter_spacing,
            font_stack: fragment.font_stack,
            color: fragment.color,
            font: fragment.font,
            node: fragment.node,
            node_offset: fragment.node_offset,
            source_len: fragment.source_len,
            rtl: fragment.rtl,
            decoration: fragment.decoration,
        });
        x += fragment.width;
    }
    for &decoration in &active {
        let start = starts.get(&decoration).copied().unwrap_or(line_start);
        let end = ends.get(&decoration).copied().unwrap_or(x);
        let fragment = inline_fragment_box(
            &line.decorations[decoration],
            (start, end),
            (
                starts.contains_key(&decoration),
                ends.contains_key(&decoration),
            ),
            {
                let decoration = &line.decorations[decoration];
                baseline_y
                    - decoration.raise
                    - crate::font::active_fonts()
                        .ascent(decoration.style.font_family, decoration.font_size)
            },
        );
        line.placed.push(fragment);
    }
    line.open_inlines = active
        .into_iter()
        .filter(|decoration| !ends.contains_key(decoration))
        .collect();
    line.y += height.max(1.0);
    line.used = 0.0;
    line.height = 0.0;
    line.fit_to_floats(content_x, content_width);
}

// one line's part of an inline element: its content area with padding and border around it,
// and the left and right edges only where the element starts and ends
fn inline_fragment_box(
    decoration: &InlineDecoration,
    (start, end): (f32, f32),
    (first, last): (bool, bool),
    text_top: f32,
) -> LayoutBox {
    let InlineDecoration {
        node,
        tag,
        style,
        font_size,
        padding,
        border,
        ..
    } = decoration;
    let border = ResolvedEdges {
        left: if first { border.left } else { 0.0 },
        right: if last { border.right } else { 0.0 },
        ..*border
    };
    let content_height = crate::font::active_fonts().content_height(style.font_family, *font_size);
    let rect = Rect {
        x: start,
        y: text_top - padding.top - border.top,
        width: (end - start).max(0.0),
        height: content_height + padding.top + padding.bottom + border.top + border.bottom,
    };
    let mut fragment = anonymous_box(
        *node,
        rect,
        Vec::new(),
        (*font_size, style.cursor),
        decoration.opacity,
    );
    fragment.tag = tag.clone();
    fragment.inline = true;
    fragment.visible = style.visibility == Visibility::Visible;
    fragment.pointer_events = style.pointer_events;
    fragment.selectable = style.selectable;
    fragment.background = style.background;
    fragment.backgrounds = style.layers();
    fragment.border = BoxBorder {
        widths: border,
        ..BoxBorder::of(style)
    };
    let radii = resolve_corner_radii(style.border_radius, rect.width, rect.height, *font_size);
    fragment.radii = [
        if first { radii[0] } else { (0.0, 0.0) },
        if last { radii[1] } else { (0.0, 0.0) },
        if last { radii[2] } else { (0.0, 0.0) },
        if first { radii[3] } else { (0.0, 0.0) },
    ];
    fragment.outline = box_outline(style);
    fragment
}

// a marker outside the content box, level with the first line: a drawn bullet, or the item's
// number in the list's style
fn outside_marker(
    style: &Style,
    ordinal: i32,
    node: NodeId,
    (content_x, content_y): (f32, f32),
    (font_size, line_height): (f32, f32),
) -> Option<LayoutBox> {
    let color = text_color(style);
    let gap = font_size * 0.5;
    if let Some(label) = style.list_style_type.marker_text(ordinal) {
        let font = font_style(style, font_size);
        let letter_spacing = style
            .letter_spacing
            .resolve(font_size, font_size)
            .unwrap_or(0.0);
        let width = measure_text(&label, style.font_family, font_size, font, letter_spacing);
        let rect = Rect {
            x: content_x - gap - width,
            y: content_y + half_leading(line_height, style.font_family, font_size),
            width,
            height: line_height,
        };
        let run = TextRun {
            text: label,
            x: rect.x,
            y: rect.y,
            width,
            font_size,
            letter_spacing,
            font_stack: style.font_family,
            color,
            font,
            node: None,
            node_offset: 0,
            source_len: 0,
            rtl: false,
            decoration: Default::default(),
        };
        let mut marker = anonymous_box(node, rect, vec![run], (font_size, style.cursor), 1.0);
        marker.tag = "::marker".to_string();
        return Some(marker);
    }
    let size = bullet_size(font_size);
    bullet_marker(
        style,
        node,
        Rect {
            x: content_x - gap - size,
            y: content_y + (line_height - size) / 2.0,
            width: size,
            height: size,
        },
        font_size,
    )
}

fn bullet_size(font_size: f32) -> f32 {
    (font_size * 0.35).max(3.0)
}

// a disc, circle or square drawn as a box, so it needs no glyph from the font
fn bullet_marker(style: &Style, node: NodeId, rect: Rect, font_size: f32) -> Option<LayoutBox> {
    let color = text_color(style);
    let size = rect.width;
    let mut marker = anonymous_box(node, rect, Vec::new(), (font_size, style.cursor), 1.0);
    marker.tag = "::marker".to_string();
    let round = [(size / 2.0, size / 2.0); 4];
    match style.list_style_type {
        ListStyleType::Disc => {
            marker.background = color;
            marker.radii = round;
        }
        ListStyleType::Circle => {
            let width = (size / 5.0).max(1.0);
            marker.radii = round;
            marker.border = BoxBorder {
                widths: ResolvedEdges {
                    top: width,
                    right: width,
                    bottom: width,
                    left: width,
                },
                styles: [BorderStyle::Solid; 4],
                colors: [color; 4],
            };
        }
        ListStyleType::Square => marker.background = color,
        _ => return None,
    }
    Some(marker)
}

// L2 of the bidi algorithm over a line's fragments: runs reverse by level, and right-to-left
// text reads backwards, with mirrored brackets
fn visual_fragments(fragments: Vec<InlineFragment>, base: u8) -> Vec<InlineFragment> {
    // trailing spaces go back to the paragraph level, as L1 asks
    let mut levels = fragments
        .iter()
        .map(|fragment| fragment.level)
        .collect::<Vec<_>>();
    for (index, fragment) in fragments.iter().enumerate().rev() {
        if fragment.atomic.is_none() && fragment.text.trim().is_empty() {
            levels[index] = base;
        } else {
            break;
        }
    }
    let order = visual_order(&levels);
    let mut slots = fragments.into_iter().map(Some).collect::<Vec<_>>();
    order
        .into_iter()
        .filter_map(|index| {
            let mut fragment = slots[index].take()?;
            if levels[index] % 2 == 1 && fragment.atomic.is_none() {
                if crate::font::active_fonts().is_builtin() {
                    fragment.text = reversed_for_display(&fragment.text);
                } else {
                    // shaping needs logical order, so the run is drawn backwards instead; in a
                    // left-to-right paragraph the space joining it to the text before stays on
                    // the left
                    if base.is_multiple_of(2)
                        && let Some(rest) = fragment.text.strip_prefix(' ')
                    {
                        fragment.text = format!("{rest} ");
                    }
                    fragment.rtl = true;
                }
            }
            Some(fragment)
        })
        .collect()
}

// characters in reverse with each base kept before its marks, brackets mirrored; a space
// that joins the piece to what comes before stays in front
fn reversed_for_display(text: &str) -> String {
    let (lead, text) = match text.strip_prefix(' ') {
        Some(rest) => (" ", rest),
        None => ("", text),
    };
    let mut clusters: Vec<String> = Vec::new();
    for character in text.chars() {
        match clusters.last_mut() {
            Some(cluster) if bidi_class(character) == BidiClass::Nsm => cluster.push(character),
            _ => clusters.push(mirrored(character).to_string()),
        }
    }
    lead.to_string() + &clusters.into_iter().rev().collect::<String>()
}

// cuts a line that overflows its clipping block where an ellipsis still fits, and ends it
// with one in the style of the last text kept
fn truncate_with_ellipsis(line: &mut InlineLine, available: f32) {
    let measure = |text: &str, fragment: &InlineFragment| {
        measure_text(
            text,
            fragment.font_stack,
            fragment.font_size,
            fragment.font,
            fragment.letter_spacing,
        )
    };
    let Some(template) = line
        .fragments
        .iter()
        .rev()
        .find(|fragment| fragment.edge.is_none() && fragment.atomic.is_none())
        .map(clone_text_fragment)
    else {
        return;
    };
    let ellipsis = "\u{2026}";
    let ellipsis_width = measure(ellipsis, &template);
    let limit = (available - ellipsis_width).max(0.0);
    let mut used = 0.0;
    let mut kept = Vec::new();
    for mut fragment in std::mem::take(&mut line.fragments) {
        if used + fragment.width <= limit {
            used += fragment.width;
            kept.push(fragment);
            continue;
        }
        if fragment.edge.is_none() && fragment.atomic.is_none() {
            // as many whole characters of the crossing piece as fit
            let mut cut = String::new();
            for character in fragment.text.chars() {
                cut.push(character);
                if used + measure(&cut, &fragment) > limit {
                    cut.pop();
                    break;
                }
            }
            if !cut.is_empty() {
                fragment.width = measure(&cut, &fragment);
                fragment.source_len = cut.encode_utf16().count().min(fragment.source_len);
                fragment.text = cut;
                used += fragment.width;
                kept.push(fragment);
            }
        }
        break;
    }
    kept.push(InlineFragment {
        text: ellipsis.to_string(),
        width: ellipsis_width,
        node: None,
        source_len: 0,
        ..template
    });
    line.fragments = kept;
    line.used = used + ellipsis_width;
}

fn clone_text_fragment(fragment: &InlineFragment) -> InlineFragment {
    InlineFragment {
        text: fragment.text.clone(),
        width: fragment.width,
        font_size: fragment.font_size,
        letter_spacing: fragment.letter_spacing,
        font_stack: fragment.font_stack,
        color: fragment.color,
        font: fragment.font,
        node: fragment.node,
        node_offset: fragment.node_offset,
        source_len: fragment.source_len,
        rtl: fragment.rtl,
        decoration: fragment.decoration,
        line_height: fragment.line_height,
        atomic: None,
        edge: None,
        level: fragment.level,
        leading: fragment.leading,
        baseline: fragment.baseline,
        shift: fragment.shift,
    }
}

// takes a float out of the line and puts it as high and as far to its side as the floats
// already there allow, beside the current line when it fits
fn place_float(input: InlineElementInput<'_>, style: &Style, line: &mut InlineLine) {
    let InlineElementInput {
        node,
        parent,
        context,
        stylesheet,
        style_state,
        viewport,
        containing_block,
    } = input;
    let Some(floats) = line.floats.clone() else {
        return;
    };
    let (content_x, content_width) = (context.content_x, context.content_width);
    // an auto width shrinks to fit, as for inline-blocks
    let containing_width = if style.width == Length::Auto && style.aspect_ratio.is_none() {
        let measure = |minimum: bool| {
            intrinsic_width(
                MaxContentInput {
                    node,
                    stylesheet,
                    style_state,
                    parent,
                    containing_width: content_width,
                    own_width: true,
                },
                minimum,
            )
        };
        measure(false).min(content_width.max(measure(true)))
    } else {
        content_width
    };
    let Some(mut laid_out) = layout_element(ElementLayoutInput {
        node,
        containing_x: 0.0,
        containing_y: 0.0,
        containing_width,
        containing_height: None,
        size_override: (None, None),
        floats: None,
        list_ordinal: 1,
        parent,
        context: LayoutContext {
            viewport,
            containing_block,
            mode: LayoutMode::Flow,
            stylesheet,
            style_state,
        },
    }) else {
        return;
    };
    let margin = laid_out.margin;
    let outer = (
        laid_out.rect.width + margin.left + margin.right,
        laid_out.rect.height + margin.top + margin.bottom,
    );
    let mut y = line.y;
    if let Some(top) = floats.lowest_top() {
        y = y.max(top);
    }
    if let Some(clear_to) = floats.clearance(style.clear) {
        y = y.max(clear_to);
    }
    loop {
        let (left, right) = floats.insets(y, outer.1, content_x, content_width);
        let beside_line = y < line.y + line.band && line.has_content();
        let used = if beside_line { line.used } else { 0.0 };
        if outer.0 <= content_width - left - right - used + 0.01 {
            break;
        }
        if let Some(bottom) = floats.next_bottom(y, outer.1) {
            y = bottom;
        } else if beside_line {
            y = line.y + line.height.max(line.band);
        } else {
            break;
        }
    }
    let (left, right) = floats.insets(y, outer.1, content_x, content_width);
    let x = if style.float == Float::Left {
        content_x + left
    } else {
        content_x + content_width - right - outer.0
    };
    let shift = (
        x + margin.left - laid_out.rect.x,
        y + margin.top - laid_out.rect.y,
    );
    translate_layout_box(&mut laid_out, shift.0, shift.1);
    laid_out.floated = true;
    floats.placed.borrow_mut().push((
        Rect {
            x,
            y,
            width: outer.0,
            height: outer.1,
        },
        style.float,
    ));
    line.placed.push(laid_out);
    let (left, right) = floats.insets(line.y, line.band, content_x, content_width);
    if !line.has_content() {
        (line.left, line.right) = (left, right);
    } else {
        // content already on the line keeps its place, and the rest of the line narrows
        line.left = line.left.max(left);
        line.right = line.right.max(right);
    }
}

// a height, where a percentage resolves only against a definite containing height
fn resolve_height(length: Length, containing_height: Option<f32>, font_size: f32) -> Option<f32> {
    if length.has_percent() {
        length.resolve(containing_height?, font_size)
    } else {
        length.resolve(0.0, font_size)
    }
}

// margins may be negative, and auto reads as zero until a layout mode gives it a size
fn resolve_margins(edges: Edges, containing_width: f32, font_size: f32) -> ResolvedEdges {
    let side = |length: Length| length.resolve(containing_width, font_size).unwrap_or(0.0);
    ResolvedEdges {
        top: side(edges.top),
        right: side(edges.right),
        bottom: side(edges.bottom),
        left: side(edges.left),
    }
}

// two adjoining vertical margins: the largest positive plus the most negative
// whether a block is of a kind that lets its children's margins through its edges; its
// border, padding and height decide the rest
fn margins_pass_through(node: Subject<'_>, style: &Style) -> bool {
    node.own().is_some()
        && !establishes_formatting_context(style)
        && !(style.display == Display::ListItem && style.list_inside)
        && node.document.document_element() != Some(node.node)
        && node.document.shadow_root(node.node).is_none()
        && node.svg_root().is_none()
        && !matches!(node.name(), "textarea" | "select" | "canvas" | "video")
}

// the first child whose top margin reaches a block's top edge, and that margin as it shows
// there: collapsed with whatever reaches the child's own top edge in turn
fn leading_margin(
    node: Subject<'_>,
    style: &Style,
    font_size: f32,
    width: f32,
    styles: (&Stylesheet, &StyleState),
) -> Option<(NodeId, f32)> {
    let (stylesheet, style_state) = styles;
    if !margins_pass_through(node, style)
        || used_border(style).top != 0.0
        || resolve_edges(style.padding, width, font_size).top != 0.0
        || node.generated_content(stylesheet, style_state)[0].is_some()
    {
        return None;
    }
    let parent = ParentStyle {
        font_size,
        color: style.color,
        weight: style.font_weight,
        text: inherited_text(style),
        opacity: 1.0,
    };
    for &child in node.document.children(node.node) {
        match node.document.kind(child) {
            Some(crate::dom::NodeKind::Text(text)) if text.trim().is_empty() => continue,
            Some(crate::dom::NodeKind::Element(_)) => {}
            Some(crate::dom::NodeKind::Text(_)) => return None,
            _ => continue,
        }
        let child = Subject::new(node.document, child);
        let child_style = stylesheet.style_for(child.style_context(parent, style_state));
        if child_style.display == Display::None
            || child_style.position.is_out_of_flow()
            || child_style.float != Float::None
        {
            continue;
        }
        // text on a line of its own, or a child moved clear of floats, keeps the margins apart
        if child_style.display.is_inline_level() || child_style.clear != Clear::None {
            return None;
        }
        let child_font_size = child_style
            .font_size
            .resolve(font_size, font_size)
            .unwrap_or(font_size)
            .max(1.0);
        return Some((
            child.node,
            flow_top_margin(child, &child_style, child_font_size, width, styles),
        ));
    }
    None
}

// the top margin a block child shows to what lies above it: its own, collapsed with the
// margins that reach its top edge from inside
fn flow_top_margin(
    node: Subject<'_>,
    style: &Style,
    font_size: f32,
    width: f32,
    styles: (&Stylesheet, &StyleState),
) -> f32 {
    let own = resolve_margins(style.margin, width, font_size).top;
    match leading_margin(node, style, font_size, width, styles) {
        Some((_, inner)) => collapse_margins(own, inner),
        None => own,
    }
}

fn collapse_margins(first: f32, second: f32) -> f32 {
    first.max(second).max(0.0) + first.min(second).min(0.0)
}

pub(crate) fn resolve_edges(edges: Edges, containing_width: f32, font_size: f32) -> ResolvedEdges {
    ResolvedEdges {
        top: edges
            .top
            .resolve(containing_width, font_size)
            .unwrap_or(0.0)
            .max(0.0),
        right: edges
            .right
            .resolve(containing_width, font_size)
            .unwrap_or(0.0)
            .max(0.0),
        bottom: edges
            .bottom
            .resolve(containing_width, font_size)
            .unwrap_or(0.0)
            .max(0.0),
        left: edges
            .left
            .resolve(containing_width, font_size)
            .unwrap_or(0.0)
            .max(0.0),
    }
}

// a frame that only moves the clock keeps the boxes of the last layout when every element in
// motion animates paint-only properties; it gives up when a box did not take those values from
// its own style, which leaves a full layout to get it right
pub(crate) fn repaint_motion(
    document: &Document,
    stylesheet: &Stylesheet,
    viewport: Size,
    [last_state, next_state]: [&StyleState; 2],
    (nodes, last): (&std::collections::BTreeSet<NodeId>, &LayoutDocument),
) -> Option<LayoutDocument> {
    let _viewport_environment = crate::css::use_layout_environment(crate::css::LayoutEnvironment {
        viewport,
        root_font_size: 16.0,
    });
    let _root_environment = crate::css::use_layout_environment(crate::css::LayoutEnvironment {
        viewport,
        root_font_size: root_font_size(document, stylesheet, viewport, next_state),
    });
    let mut styles = HashMap::new();
    for &node in nodes {
        // their backgrounds reach the canvas, which a box patch does not
        if document
            .element(node)
            .is_none_or(|element| matches!(element.name.as_str(), "html" | "body"))
        {
            return None;
        }
        let (last_style, _) = stylesheet.computed_style(document, node, last_state)?;
        styles.insert(node, last_style);
    }
    // the wake belongs to the frame drawn now, not the last one
    crate::css::take_animation_wake();
    let mut moved = HashMap::new();
    for (node, last_style) in styles {
        let (next_style, font_size) = stylesheet.computed_style(document, node, next_state)?;
        moved.insert(node, (last_style, next_style, font_size));
    }
    let mut next = last.clone();
    patch_motion(&mut next.root, document, &moved).then_some(next)
}

fn patch_motion(
    layout: &mut LayoutBox,
    document: &Document,
    moved: &HashMap<NodeId, (Style, Style, f32)>,
) -> bool {
    // a field moves only when it holds what the last style gives it
    fn swap<T: PartialEq>(field: &mut T, [last, next]: [T; 2]) -> bool {
        if *field == last {
            *field = next;
            true
        } else {
            last == next
        }
    }
    if let Some((last, next, font_size)) = moved.get(&layout.node)
        && document
            .element(layout.node)
            .is_some_and(|element| element.name == layout.tag)
    {
        let styles = [last, next];
        let width = layout.rect.width;
        let widths = layout.border.widths;
        let shadows = styles.map(|style| resolve_shadows(style.shadows, width, *font_size));
        // inline fragments draw no shadows
        let shadows_moved = if layout.inline {
            shadows[0] == shadows[1]
        } else {
            swap(&mut layout.shadows, shadows)
        };
        let moved = layout.control.is_none()
            && shadows_moved
            && swap(&mut layout.background, styles.map(|style| style.background))
            && swap(&mut layout.backgrounds, styles.map(Style::layers))
            && swap(
                &mut layout.border,
                styles.map(|style| BoxBorder {
                    widths,
                    ..BoxBorder::of(style)
                }),
            )
            && swap(&mut layout.outline, styles.map(box_outline));
        if !moved {
            return false;
        }
    }
    layout
        .children
        .iter_mut()
        .all(|child| patch_motion(child, document, moved))
}

// rem resolves against the root element font size, which itself may only use absolute units
fn root_font_size(
    document: &Document,
    stylesheet: &Stylesheet,
    viewport: Size,
    style_state: &StyleState,
) -> f32 {
    const INITIAL_FONT_SIZE: f32 = 16.0;
    let Some(root) = document.document_element() else {
        return INITIAL_FONT_SIZE;
    };
    stylesheet
        .style_for(Subject::new(document, root).style_context(
            ParentStyle {
                font_size: INITIAL_FONT_SIZE,
                color: Color::BLACK,
                weight: FontWeight::NORMAL,
                text: InheritedText::default(),
                opacity: 1.0,
            },
            style_state,
        ))
        .font_size
        .resolve(viewport.width, INITIAL_FONT_SIZE)
        .filter(|size| size.is_finite() && *size > 0.0)
        .unwrap_or(INITIAL_FONT_SIZE)
}

// chromium keeps line heights in 1/64 px units: a number scales the snapped font
// size and truncates, a length or normal rounds
fn used_line_height(style: &Style, font_size: f32) -> f32 {
    let scale = crate::font::device_scale();
    let units = match style.line_height {
        Length::Terms(terms)
            if terms
                == (LengthTerms {
                    em: terms.em,
                    ..LengthTerms::default()
                }) =>
        {
            let font_size = (font_size * scale * 64.0).round() / 64.0;
            (font_size * (terms.em * 100.0) / 100.0 * 64.0).trunc()
        }
        length => (length
            .resolve(font_size, font_size)
            .unwrap_or_else(|| normal_line_height(style.font_family, font_size))
            * scale
            * 64.0)
            .round(),
    };
    (units / 64.0 / scale).max(1.0)
}

// chromium's inner editor drops a single-line field's line-height for normal when the font is
// at least the whole-pixel line height, or the field's height is a percentage or a fixed length
// taller than it
fn editor_line_height(style: &Style, font_size: f32, line_height: f32) -> f32 {
    let scale = crate::font::device_scale();
    let line_pixels = (line_height * scale).floor();
    let resets = font_size * scale >= line_pixels
        || matches!(style.height, Length::Terms(terms) if terms.percent != 0.0
            || style.height.resolve(0.0, font_size).is_some_and(|height| height * scale > line_pixels));
    if resets {
        normal_line_height(style.font_family, font_size)
    } else {
        line_height
    }
}

// line-height normal takes the primary face's ascent, descent and line gap
pub(crate) fn normal_line_height(stack: FontStack, font_size: f32) -> f32 {
    crate::font::active_fonts().line_height(stack, font_size)
}

// the space above a glyph box that centres it in its line: half the leading floored to a
// whole pixel, the rest going below, as blink's CalculateLeadingSpace splits it
fn half_leading(line_height: f32, stack: FontStack, font_size: f32) -> f32 {
    let scale = crate::font::device_scale();
    let content_height = crate::font::active_fonts().content_height(stack, font_size);
    let leading_units = ((line_height - content_height) * scale * 64.0).round();
    ((leading_units / 2.0).trunc() / 64.0).floor() / scale
}

// the weight, slant, axis settings, optical size and features a style asks its font for
fn font_style(style: &Style, font_size: f32) -> FontStyle {
    FontStyle {
        weight: style.font_weight.0,
        italic: style.italic,
        variations: style.font_variations,
        optical_size: optical_size(style.optical_sizing, font_size),
        features: style.font_features,
    }
}

// font-optical-sizing auto sets opsz to the used font size in pixels
fn optical_size(auto: bool, font_size: f32) -> u32 {
    if auto { font_size.to_bits() } else { 0 }
}

fn measure_text(
    text: &str,
    stack: FontStack,
    font_size: f32,
    font: FontStyle,
    letter_spacing: f32,
) -> f32 {
    let _profile = crate::profile::scope("layout: measure text");
    let fonts = crate::font::active_fonts();
    if fonts.is_builtin() {
        return fonts.measure(text, stack, font_size, font)
            + letter_spacing * text.chars().count() as f32;
    }
    // spaced letters draw without ligatures, one glyph each
    let shaped = fonts.shape(text, stack, font_size, font, letter_spacing == 0.0);
    shaped.width + letter_spacing * shaped.glyphs.len() as f32
}

fn text_control_value(node: Subject<'_>, style_state: &StyleState) -> Option<String> {
    let element = node.element()?;
    if element.name == "textarea" {
        return Some(
            style_state
                .control_values
                .get(&node.node)
                .cloned()
                .unwrap_or_else(|| node.document.text_content(node.node)),
        );
    }
    if element.name != "input"
        || element.attr("type").is_some_and(|kind| {
            matches!(
                kind.to_ascii_lowercase().as_str(),
                "button"
                    | "checkbox"
                    | "color"
                    | "date"
                    | "datetime-local"
                    | "file"
                    | "hidden"
                    | "image"
                    | "month"
                    | "number"
                    | "radio"
                    | "range"
                    | "reset"
                    | "submit"
                    | "time"
                    | "week"
            )
        })
    {
        return None;
    }
    Some(
        style_state
            .control_values
            .get(&node.node)
            .cloned()
            .or_else(|| element.attr("value").map(str::to_string))
            .unwrap_or_default(),
    )
}

fn text_control_placeholder(element: &Element) -> Option<String> {
    if !matches!(element.name.as_str(), "input" | "textarea") {
        return None;
    }
    element
        .attr("placeholder")
        .filter(|value| !value.is_empty())
        .map(|value| value.replace("\r\n", "\n").replace('\r', "\n"))
}

fn text_control_display_value(element: &Element, value: &str) -> String {
    if element.name == "input"
        && element
            .attr("type")
            .is_some_and(|kind| kind.eq_ignore_ascii_case("password"))
    {
        return "*".repeat(value.encode_utf16().count());
    }
    value.to_string()
}

fn control_text_lines(
    name: &str,
    element: Option<&Element>,
    value: &str,
    content_width: f32,
    font_size: f32,
) -> Vec<String> {
    let max_characters = (content_width
        / measure_text("X", FontStack::default(), font_size, FontStyle::NORMAL, 0.0).max(1.0))
    .floor()
    .max(1.0) as usize;
    let mut lines = Vec::new();
    for value_line in value.split('\n') {
        if name != "textarea"
            || element
                .and_then(|element| element.attr("wrap"))
                .is_some_and(|wrap| wrap.eq_ignore_ascii_case("off"))
        {
            lines.push(value_line.to_string());
            continue;
        }
        let characters = value_line.chars().collect::<Vec<_>>();
        if characters.is_empty() {
            lines.push(String::new());
            continue;
        }
        for chunk in characters.chunks(max_characters) {
            lines.push(chunk.iter().collect());
        }
    }
    lines
}

impl LayoutBox {
    // every field but the children, which damage tracking walks one by one; the pattern names
    // them all so a new field cannot be left out
    pub(crate) fn same_own(&self, other: &Self) -> bool {
        let Self {
            children: _,
            node,
            tag,
            rect,
            background,
            backgrounds,
            masks,
            object_fit,
            top_layer,
            transform,
            backface_hidden,
            clip_path,
            radii,
            opacity,
            shadows,
            filters,
            border,
            outline,
            text,
            control_value,
            control_selection,
            control_focused,
            control_text_origin,
            position,
            visual_offset,
            z_index,
            creates_stacking_context,
            sticky_inset,
            font_size,
            overflow,
            overflow_axes,
            visible,
            pointer_events,
            selectable,
            cursor,
            floated,
            inline,
            control,
            selection,
            selection_color,
            scroll_content,
            scroll_offset,
            svg,
            scrollbar,
            margin,
        } = self;
        *node == other.node
            && *tag == other.tag
            && *rect == other.rect
            && *background == other.background
            && *backgrounds == other.backgrounds
            && *masks == other.masks
            && *object_fit == other.object_fit
            && *top_layer == other.top_layer
            && *transform == other.transform
            && *backface_hidden == other.backface_hidden
            && *clip_path == other.clip_path
            && *radii == other.radii
            && *opacity == other.opacity
            && *shadows == other.shadows
            && *filters == other.filters
            && *border == other.border
            && *outline == other.outline
            && *text == other.text
            && *control_value == other.control_value
            && *control_selection == other.control_selection
            && *control_focused == other.control_focused
            && *control_text_origin == other.control_text_origin
            && *position == other.position
            && *visual_offset == other.visual_offset
            && *z_index == other.z_index
            && *creates_stacking_context == other.creates_stacking_context
            && *sticky_inset == other.sticky_inset
            && *font_size == other.font_size
            && *overflow == other.overflow
            && *overflow_axes == other.overflow_axes
            && *visible == other.visible
            && *pointer_events == other.pointer_events
            && *selectable == other.selectable
            && *cursor == other.cursor
            && *floated == other.floated
            && *inline == other.inline
            && *control == other.control
            && *selection == other.selection
            && *selection_color == other.selection_color
            && *scroll_content == other.scroll_content
            && *scroll_offset == other.scroll_offset
            && *svg == other.svg
            && *scrollbar == other.scrollbar
            && *margin == other.margin
    }

    pub(crate) fn is_document(&self) -> bool {
        self.tag == "#document"
    }
}

fn describe_box(layout_box: &LayoutBox, depth: usize, lines: &mut Vec<String>) {
    let indent = "  ".repeat(depth);
    lines.push(format!(
        "{indent}{} x={:.1} y={:.1} w={:.1} h={:.1} text={}",
        layout_box.tag,
        layout_box.rect.x,
        layout_box.rect.y,
        layout_box.rect.width,
        layout_box.rect.height,
        layout_box.text.len()
    ));
    for child in &layout_box.children {
        describe_box(child, depth + 1, lines);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // a few hundred seeded writes of every kind, each followed by a layout that takes what it
    // can from the one before and by one that takes nothing; the two must not differ
    #[test]
    fn a_layout_that_keeps_answers_matches_one_from_nothing_after_any_write() {
        let mut document = Document::parse_markup(
            r#"<style>
            #root { width: 300px; --gap: 4px; font-size: 14px; position: relative }
            .row { display: flex; gap: var(--gap) }
            .grid { display: grid; grid-template-columns: 1fr 2fr; gap: var(--gap) }
            .wide { width: 80% } .tall { height: 40px } .hot { color: red; font-size: 18px }
            .wide > .tall { margin-left: 10px }
            p + div { padding: 3px }
            div:hover { border: 2px solid black }
            .row:hover > span { margin-left: 6px }
            p:hover + div { height: 9px }
            .abs { position: absolute; left: 10px; top: 5px; width: 30px; height: 10px }
            .spin { animation: grow 1s infinite }
            @keyframes grow { from { width: 10px } to { width: 90px } }
            li::before { content: "-" }
            b, span { padding: inherit; cursor: inherit }
            </style>
            <div id="root"><div class="row"><span>one</span><div class="tall"><p>two words</p></div><div class="abs"></div></div><div class="grid"><p>three</p><div class="spin"></div><div><span>four five six</span></div></div><ol><li>a</li><li>b</li></ol><p>tail <b>bold</b> text</p></div>"#,
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let root = document.find_element_by_id("root").expect("root");
        let viewport = Size {
            width: 400.0,
            height: 300.0,
        };
        let mut state = StyleState::default();
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        let mut pick = |below: usize| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (seed >> 33) as usize % below
        };
        const PROPERTIES: [(&str, [&str; 4]); 13] = [
            ("visibility", ["visible", "hidden", "visible", "hidden"]),
            ("cursor", ["pointer", "default", "text", "pointer"]),
            ("pointer-events", ["none", "auto", "none", "auto"]),
            ("width", ["10px", "50%", "auto", "120px"]),
            ("height", ["12px", "30px", "auto", "50%"]),
            ("margin-left", ["0", "7px", "auto", "20px"]),
            ("padding", ["0", "2px", "9px", "1em"]),
            ("display", ["block", "flex", "none", "grid"]),
            ("font-size", ["10px", "20px", "1.5em", "80%"]),
            ("--gap", ["0px", "2px", "12px", "1em"]),
            ("position", ["static", "absolute", "relative", "static"]),
            ("left", ["auto", "5px", "30px", "10%"]),
            ("flex-grow", ["0", "1", "2", "0"]),
        ];
        const CLASSES: [&str; 8] = ["row", "grid", "wide", "tall", "hot", "abs", "spin", ""];
        const TEXTS: [&str; 4] = [
            "a",
            "longer words here to wrap around the line",
            "",
            "x y z",
        ];
        let mut last = root;
        for step in 0..600 {
            let nodes = document.subtree(root);
            let elements = nodes
                .iter()
                .copied()
                .filter(|&node| document.is_element(node))
                .collect::<Vec<_>>();
            let texts = nodes
                .iter()
                .copied()
                .filter(|&node| matches!(document.kind(node), Some(crate::dom::NodeKind::Text(_))))
                .collect::<Vec<_>>();
            // half the writes land beside or above the one before, as a page's own do
            let near = document
                .parent(last)
                .filter(|&parent| elements.contains(&parent));
            let element = match near {
                Some(parent) if pick(2) == 0 => {
                    let siblings = document.element_children(parent).collect::<Vec<_>>();
                    [parent, siblings[pick(siblings.len())]][pick(2)]
                }
                _ => elements[pick(elements.len())],
            };
            last = element;
            let write = pick(8);
            match write {
                0 | 1 => {
                    let (property, values) = PROPERTIES[pick(PROPERTIES.len())];
                    document.set_style_property(element, property, values[pick(4)].to_string());
                }
                2 => {
                    document.set_attribute(element, "class", CLASSES[pick(8)].to_string());
                }
                3 if !texts.is_empty() => {
                    let text = texts[pick(texts.len())];
                    document.set_character_data(text, TEXTS[pick(4)].to_string());
                }
                4 => {
                    let added = document.create_element(["div", "span", "p"][pick(3)]);
                    let words = document.create_text(TEXTS[pick(4)]);
                    document
                        .append_child(added, words)
                        .expect("text should append");
                    let before = document.children(element).first().copied();
                    document
                        .insert_before(element, added, before.filter(|_| pick(2) == 0))
                        .expect("element should insert");
                }
                5 if element != root && elements.len() > 6 => {
                    document.remove(element);
                }
                6 => {
                    state.hovered = (pick(3) > 0).then_some(element);
                }
                7 if pick(2) == 0 => {
                    document.set_attribute(element, "hidden", String::new());
                }
                _ => {
                    document.remove_attribute(element, "hidden");
                }
            }
            state.clock += 130;
            let kept = layout_with_state(&document, &stylesheet, viewport, &state);
            let fresh = layout_afresh(&document, &stylesheet, viewport, &state);
            assert!(
                kept.root == fresh.root,
                "after write {write} of step {step}"
            );
            // more layouts of the same document, as a script measures and a frame lays out
            for _ in 0..step % 4 {
                let again = layout_with_state(&document, &stylesheet, viewport, &state);
                assert!(again.root == fresh.root, "again after step {step}");
            }
        }
    }

    // a page makes several layouts between two changes, as a script measures and a frame
    // lays out; the second change must still find what the first left beside it
    #[test]
    fn layouts_that_work_nothing_out_do_not_age_what_is_kept() {
        let mut document = Document::parse_markup(
            r#"<style>#wrap, #row { display: flex } #label { width: 50px; flex: none }</style>
            <div id="wrap"><div id="row"><span id="label">a</span><div id="side"><p>one</p><p>two</p></div></div><div id="other">x</div></div>"#,
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let label = document.find_element_by_id("label").expect("label");
        let side = document.find_element_by_id("side").expect("side");
        let text = document.children(label)[0];
        let viewport = Size {
            width: 400.0,
            height: 300.0,
        };
        let state = StyleState::default();
        let lay_out =
            |document: &Document| layout_with_state(document, &stylesheet, viewport, &state);
        let kept = || KEPT.with(|kept| kept.borrow().boxes.contains_key(&(side, None)));
        lay_out(&document);
        document.set_character_data(text, "b".to_string());
        lay_out(&document);
        assert!(kept());
        // these take the row whole and never ask for the box beside the label
        for _ in 0..5 {
            lay_out(&document);
        }
        assert!(kept());
        // two layouts that work something else out take the row whole again, and with that
        // the box inside it has gone unasked for long enough
        let other = document.find_element_by_id("other").expect("other");
        let other = document.children(other)[0];
        for words in ["y", "z"] {
            document.set_character_data(other, words.to_string());
            lay_out(&document);
        }
        // unless every answer taken is worked out again, which asks for the box too
        assert!(cfg!(feature = "recheck") || !kept());
    }

    #[test]
    fn an_auto_grid_row_shrinks_to_an_items_own_min_height_and_fills_the_grid() {
        let height_of = |item: &str| {
            let document = Document::parse_markup(&format!(
                r#"<div id="grid" style="display: grid; height: 100px; width: 200px"><div id="item" style="{item}"><div style="height: 300px"></div></div></div>"#
            ))
            .expect("document should parse");
            let stylesheet = Stylesheet::parse("").expect("stylesheet should parse");
            let result = layout(
                &document,
                &stylesheet,
                Size {
                    width: 400.0,
                    height: 400.0,
                },
            );
            let item = document.find_element_by_id("item").expect("item");
            fn find(layout: &LayoutBox, node: NodeId) -> Option<&LayoutBox> {
                if layout.node == node {
                    return Some(layout);
                }
                layout.children.iter().find_map(|child| find(child, node))
            }
            find(&result.root, item).expect("item box").rect.height
        };
        // the row may leave the item nothing, then grows to the grid, and the item fills it
        assert_eq!(height_of("height: 100%; min-height: 0"), 100.0);
        assert_eq!(height_of("min-height: 20px; overflow: hidden"), 100.0);
        // with no min-height of its own the item keeps its content's height
        assert_eq!(height_of("height: 100%"), 300.0);
        assert_eq!(height_of(""), 300.0);
    }

    #[test]
    fn margins_collapse_through_the_edges_of_a_block_that_nothing_holds_apart() {
        let tops = |wrapper: &str| {
            let document = Document::parse_markup(&format!(
                r#"<body style="margin: 0"><div id="above" style="height: 10px; margin-bottom: 4px"></div><div id="wrapper" style="{wrapper}"><div id="inner"><h3 id="head" style="margin: 14px 0 6px; height: 10px"></h3></div></div><div id="below" style="height: 5px"></div></body>"#
            ))
            .expect("document should parse");
            let stylesheet = Stylesheet::parse("").expect("stylesheet should parse");
            let result = layout(
                &document,
                &stylesheet,
                Size {
                    width: 200.0,
                    height: 200.0,
                },
            );
            fn find(layout: &LayoutBox, node: NodeId) -> Option<&LayoutBox> {
                if layout.node == node {
                    return Some(layout);
                }
                layout.children.iter().find_map(|child| find(child, node))
            }
            ["wrapper", "inner", "head", "below"].map(|id| {
                let node = document.find_element_by_id(id).expect(id);
                let found = find(&result.root, node).expect(id);
                (found.rect.y, found.rect.height)
            })
        };
        // the heading's margins go through both boxes around it and meet the ones outside
        assert_eq!(
            tops(""),
            [(24.0, 10.0), (24.0, 10.0), (24.0, 10.0), (40.0, 5.0)]
        );
        // padding holds the top edge in place, and the bottom still lets the margin through
        assert_eq!(
            tops("padding-top: 1px"),
            [(14.0, 25.0), (29.0, 10.0), (29.0, 10.0), (45.0, 5.0)]
        );
        // a box that starts its own formatting context keeps the margins inside
        assert_eq!(
            tops("overflow: hidden"),
            [(14.0, 30.0), (28.0, 10.0), (28.0, 10.0), (44.0, 5.0)]
        );
        // a height of its own keeps the bottom margin inside
        assert_eq!(
            tops("height: 40px"),
            [(24.0, 40.0), (24.0, 10.0), (24.0, 10.0), (64.0, 5.0)]
        );
    }

    #[test]
    fn inherited_text_transform_and_tracking_reach_layout_runs() {
        let document = Document::parse_markup(
            "<main style=\"text-transform: uppercase; letter-spacing: 2px\"><p>ab cd</p></main>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse("").expect("stylesheet should parse");
        let layout = layout(
            &document,
            &stylesheet,
            Size {
                width: 400.0,
                height: 200.0,
            },
        );
        let runs = collect_text_runs(&layout.root);
        assert_eq!(
            runs.iter().map(|run| run.text.as_str()).collect::<String>(),
            "AB CD"
        );
        assert!(runs.iter().all(|run| run.letter_spacing == 2.0));
        // the builtin cell is 12px at a 16px font, and tracking adds 2px per character
        assert_eq!(runs[0].width, 2.0 * 12.0 + 2.0 * 2.0);
        assert_eq!(runs[1].width, 3.0 * 12.0 + 3.0 * 2.0);
    }

    #[test]
    fn grid_auto_tracks_take_content_width_before_fr_tracks_expand() {
        let document = Document::parse_markup(
            "<div style=\"display: grid; grid-template-columns: auto 1fr auto\"><p>ab</p><p>middle</p><p>cd</p></div>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse("p { margin: 0 }").expect("stylesheet should parse");
        let layout = layout(
            &document,
            &stylesheet,
            Size {
                width: 300.0,
                height: 200.0,
            },
        );
        let grid = &layout.root.children[0];
        let columns: Vec<f32> = grid.children.iter().map(|child| child.rect.width).collect();
        assert_eq!(columns.len(), 3);
        assert_eq!(columns[0], 24.0);
        assert_eq!(columns[2], 24.0);
        assert!(
            columns[1] > 200.0,
            "the fr track should absorb the remaining width, got {}",
            columns[1]
        );
        assert_eq!(columns.iter().sum::<f32>(), 300.0);
    }

    #[test]
    fn a_translate_transform_moves_the_visual_subtree_without_moving_layout() {
        let document =
            Document::parse_markup("<html><body><div id='card'><p>inside</p></div></body></html>")
                .expect("document should parse");
        let stylesheet = Stylesheet::parse(
            "html, body, p { margin: 0 } #card { width: 100px; height: 40px; transform: translate(12px, -6px) }",
        )
        .expect("stylesheet should parse");
        let layout = layout(
            &document,
            &stylesheet,
            Size {
                width: 200.0,
                height: 120.0,
            },
        );
        let card = find_box(&layout.root, "div").expect("the card should lay out");
        assert_eq!(
            (card.rect.x, card.rect.y),
            (0.0, 0.0),
            "flow geometry stays where layout put it"
        );
        assert_eq!(card.visual_offset, (12.0, -6.0));
        // the hit lands inside the translated card, on its paragraph
        assert_eq!(
            layout.hit_test(20.0, 10.0),
            Some(document.node_at(&[0, 0, 0, 0]))
        );
        assert_eq!(
            layout.hit_test(5.0, 30.0),
            Some(document.node_at(&[0, 0])),
            "the vacated area falls through to the parent box"
        );

        let percentage = Stylesheet::parse(
            "html, body, p { margin: 0 } #card { width: 100px; height: 40px; transform: translateX(50%) }",
        )
        .expect("stylesheet should parse");
        let shifted = crate::layout::layout(
            &document,
            &percentage,
            Size {
                width: 200.0,
                height: 120.0,
            },
        );
        let card = find_box(&shifted.root, "div").expect("the card should lay out");
        assert_eq!(card.visual_offset, (50.0, 0.0));
    }

    #[test]
    fn generated_content_becomes_boxes_and_joins_inline_text_without_extra_spaces() {
        let document = Document::parse_markup(
            "<html><body><p class=\"tag\">bracketed</p><div class=\"rule\">bar</div><p>no<span>space</span>here</p></body></html>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(
            "html, body, p, div { margin: 0 } .tag::before { content: \"[\" } .tag::after { content: \"]\" } .rule { position: relative; height: 40px } .rule::before { content: \"\"; position: absolute; inset: 0 auto 0 0; width: 4px; background: #00ff00 }",
        )
        .expect("stylesheet should parse");
        let layout = layout(
            &document,
            &stylesheet,
            Size {
                width: 400.0,
                height: 200.0,
            },
        );
        let runs = collect_text_runs(&layout.root);
        let text = runs
            .iter()
            .map(|run| run.text.as_str())
            .collect::<Vec<_>>()
            .join("");
        assert_eq!(text, "[bracketed]barnospacehere");

        fn positioned_before(layout: &LayoutBox) -> Option<&LayoutBox> {
            if layout.tag == "::before" && !layout.inline {
                return Some(layout);
            }
            layout.children.iter().find_map(positioned_before)
        }
        let generated = positioned_before(&layout.root).expect("a generated box should exist");
        assert_eq!(
            (generated.rect.width, generated.rect.height),
            (4.0, 40.0),
            "an absolute generated box stretches between its insets"
        );
        assert_eq!(generated.background, Color::opaque(0, 255, 0));
    }

    #[test]
    fn a_styled_slider_lays_out_its_track_and_thumb_from_their_rules() {
        let document = Document::parse_markup(
            r#"<html><body><input type="range" value="25"><input type="range" style="appearance: auto" value="0"></body></html>"#,
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(
            "body { margin: 0 } input { margin: 0; vertical-align: top }
             input[type=range] { appearance: none; width: 180px; height: 16px }
             input::-webkit-slider-runnable-track { height: 4px; border-radius: 2px }
             input::-webkit-slider-thumb { appearance: none; width: 14px; height: 14px;
                 margin-top: -5px; border: 1px solid #111; background: #404040;
                 box-shadow: 0 1px 2px #20202080; filter: brightness(2) }",
        )
        .expect("stylesheet should parse");
        let layout = layout(
            &document,
            &stylesheet,
            Size {
                width: 400.0,
                height: 100.0,
            },
        );
        let mut inputs = Vec::new();
        fn collect<'a>(layout_box: &'a LayoutBox, found: &mut Vec<&'a LayoutBox>) {
            if layout_box.tag == "input" {
                found.push(layout_box);
            }
            for child in &layout_box.children {
                collect(child, found);
            }
        }
        collect(&layout.root, &mut inputs);
        let [styled, native] = inputs[..] else {
            panic!("two sliders should lay out, found {}", inputs.len());
        };
        assert_eq!(
            styled.control,
            Some(ControlPaint::Range {
                fraction: 0.25,
                accent: Color::opaque(0, 117, 255),
                disabled: false,
                styled: true,
                travel: (0.0, 166.0),
                thumb: 14.0,
            })
        );
        let [track] = &styled.children[..] else {
            panic!("the slider should hold its track");
        };
        assert_eq!(track.tag, "::-webkit-slider-runnable-track");
        assert_eq!(
            track.rect,
            Rect {
                x: 0.0,
                y: 6.0,
                width: 180.0,
                height: 4.0
            }
        );
        let [thumb] = &track.children[..] else {
            panic!("the track should hold its thumb");
        };
        // the thumb travels the track less its own width, and its margin lifts it over the track
        assert_eq!(
            thumb.rect,
            Rect {
                x: 41.5,
                y: 1.0,
                width: 14.0,
                height: 14.0
            }
        );
        // the thumb keeps its own colors and paints through its filter as a layer of its own
        assert_eq!(thumb.background, Color::opaque(64, 64, 64));
        assert!(matches!(
            thumb.filters.own[..],
            [crate::filter::Filter::Matrix(_)]
        ));
        assert!(thumb.creates_stacking_context && !track.creates_stacking_context);

        // appearance: auto keeps the platform slider, whose thumb starts at the content edge
        assert!(native.children.is_empty());
        assert!(matches!(
            native.control,
            Some(ControlPaint::Range {
                styled: false,
                travel: (0.0, 164.0),
                thumb: 16.0,
                ..
            })
        ));
    }

    #[test]
    fn form_controls_take_their_default_sizes_labels_and_drawn_parts() {
        let document = Document::parse_markup(
            r#"<html><body><input id="text"><input id="short" size="5"><input type="submit"><select><option>First</option><option selected>Second</option></select><input type="checkbox" checked><input type="range" min="0" max="10" value="5"><input type="checkbox" style="appearance: none"><textarea rows="3"></textarea></body></html>"#,
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse("").expect("stylesheet should parse");
        let layout = layout(
            &document,
            &stylesheet,
            Size {
                width: 900.0,
                height: 400.0,
            },
        );
        let mut boxes = Vec::new();
        fn collect<'a>(layout_box: &'a LayoutBox, found: &mut Vec<&'a LayoutBox>) {
            if matches!(layout_box.tag.as_str(), "input" | "select" | "textarea") {
                found.push(layout_box);
            }
            for child in &layout_box.children {
                collect(child, found);
            }
        }
        collect(&layout.root, &mut boxes);
        let [
            text,
            short,
            submit,
            select,
            checkbox,
            range,
            plain,
            textarea,
        ] = boxes[..]
        else {
            panic!("eight controls should lay out, found {}", boxes.len());
        };
        assert!(
            text.rect.width > 100.0,
            "a text field is twenty characters wide"
        );
        assert!(short.rect.width < text.rect.width / 2.0);
        assert!(
            text.rect.height > 15.0,
            "an empty text field is still a line tall"
        );
        assert_eq!(submit.text[0].text, "Submit");
        assert_eq!(select.text[0].text, "Second");
        assert!(matches!(select.control, Some(ControlPaint::Select { .. })));
        assert_eq!(
            checkbox.control,
            Some(ControlPaint::Checkbox {
                checked: true,
                accent: Color::opaque(0, 117, 255)
            })
        );
        assert_eq!(checkbox.background, Color::opaque(0, 117, 255));
        assert_eq!(checkbox.rect.width, 15.0, "13px with a 1px border");
        assert!(matches!(
            range.control,
            Some(ControlPaint::Range { fraction, .. }) if (fraction - 0.5).abs() < 1e-6
        ));
        assert_eq!(plain.control, None);
        assert!(textarea.rect.height > text.rect.height * 2.0, "three rows");

        let canvas = crate::paint::paint(&layout);
        let pixel = |x: f32, y: f32| {
            let index = (y as usize * canvas.width() as usize + x as usize) * 4;
            canvas.rgba_pixels()[index..index + 3].to_vec()
        };
        let corner = (checkbox.rect.x + 2.5, checkbox.rect.y + 2.5);
        assert_eq!(
            pixel(corner.0, corner.1),
            vec![0, 117, 255],
            "a checked box fills"
        );
        let mark = (
            checkbox.rect.x + 1.0 + 13.0 * 0.42,
            checkbox.rect.y + 1.0 + 13.0 * 0.72,
        );
        assert_eq!(
            pixel(mark.0, mark.1),
            vec![255, 255, 255],
            "with a white check"
        );
    }

    #[test]
    fn placeholder_marker_and_selection_pseudo_elements_style_their_parts() {
        let document = Document::parse_markup(
            r#"<html><body><input placeholder="hint"><ul><li>item</li></ul><p>text</p></body></html>"#,
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(
            "input::placeholder { color: #ff0000 } li::marker { color: #00ff00 } p::selection { background: #0000ff }",
        )
        .expect("stylesheet should parse");
        let layout = layout(
            &document,
            &stylesheet,
            Size {
                width: 400.0,
                height: 300.0,
            },
        );
        let input = find_box(&layout.root, "input").expect("input");
        assert_eq!(input.text[0].color, Color::opaque(255, 0, 0));
        let marker = find_box(&layout.root, "::marker").expect("marker");
        assert_eq!(marker.background, Color::opaque(0, 255, 0));
        let paragraph = find_box(&layout.root, "p").expect("paragraph");
        assert_eq!(paragraph.selection, Some(Color::opaque(0, 0, 255)));
        assert_eq!(
            find_box(&layout.root, "li").and_then(|li| li.selection),
            None
        );
    }

    #[test]
    fn text_align_and_line_height_inherit_and_lines_centre_their_glyphs() {
        let document = Document::parse_markup(
            r#"<html><body><main style="text-align: right; line-height: 2"><p>a</p></main><section style="line-height: 30px; font-size: 10px"><p style="font-size: 20px">b</p></section></body></html>"#,
        )
        .expect("document should parse");
        let stylesheet =
            Stylesheet::parse("body, p { margin: 0 }").expect("stylesheet should parse");
        let layout = layout(
            &document,
            &stylesheet,
            Size {
                width: 200.0,
                height: 200.0,
            },
        );
        let paragraphs = {
            fn collect<'a>(layout_box: &'a LayoutBox, found: &mut Vec<&'a LayoutBox>) {
                if layout_box.tag == "p" {
                    found.push(layout_box);
                }
                for child in &layout_box.children {
                    collect(child, found);
                }
            }
            let mut found = Vec::new();
            collect(&layout.root, &mut found);
            found
        };
        let factor = paragraphs[0];
        assert!(factor.text[0].x > 150.0, "text-align inherits from main");
        assert_eq!(factor.rect.height, 32.0, "line-height 2 of a 16px font");
        // a 32px line around a 16px glyph box leaves 8px above it
        assert_eq!(factor.text[0].y - factor.rect.y, 8.0);
        let pixels = paragraphs[1];
        assert_eq!(
            pixels.rect.height, 30.0,
            "a pixel line-height inherits as pixels"
        );
    }

    #[test]
    fn line_heights_snap_to_sixty_fourths_as_chromium_stores_them() {
        let document = Document::parse_markup(
            r#"<html><body><p style="font-size: 11px; line-height: 1.4">a</p><p style="font-size: 11px; line-height: 15.4px">b</p></body></html>"#,
        )
        .expect("document should parse");
        let stylesheet =
            Stylesheet::parse("body, p { margin: 0 }").expect("stylesheet should parse");
        let layout = layout(
            &document,
            &stylesheet,
            Size {
                width: 200.0,
                height: 200.0,
            },
        );
        fn collect(layout_box: &LayoutBox, found: &mut Vec<f32>) {
            if layout_box.tag == "p" {
                found.push(layout_box.rect.height);
            }
            for child in &layout_box.children {
                collect(child, found);
            }
        }
        let mut heights = Vec::new();
        collect(&layout.root, &mut heights);
        assert_eq!(
            heights,
            [985.0 / 64.0, 986.0 / 64.0],
            "a number truncates and a length rounds"
        );
    }

    #[test]
    fn white_space_modes_cjk_breaks_and_overflow_wrap_shape_lines() {
        let html = concat!(
            "<html><body>",
            "<p id='pre' style='white-space: pre'>a  b\nc</p>",
            "<p id='nowrap' style='white-space: nowrap'>one two three four five six</p>",
            "<p id='prewrap' style='white-space: pre-wrap'>one  two  three  four  five</p>",
            "<p id='preline' style='white-space: pre-line'>a     b\nc</p>",
            "<p id='cjk'>\u{4E00}\u{4E8C}\u{4E09}\u{56DB}\u{4E94}\u{516D}\u{4E03}\u{516B}\u{4E5D}\u{5341}\u{4E00}\u{4E8C}\u{4E09}\u{56DB}</p>",
            "<p id='long'>aaaaaaaaaaaaaaaaaaaaaaaa</p>",
            "<p id='anywhere' style='overflow-wrap: anywhere'>aaaaaaaaaaaaaaaaaaaaaaaa</p>",
            "</body></html>"
        );
        let document = Document::parse_markup(html).expect("document should parse");
        let stylesheet = Stylesheet::parse(
            "body, p { margin: 0; width: 60px; font-size: 10px; line-height: 10px }",
        )
        .expect("stylesheet should parse");
        let layout = layout(
            &document,
            &stylesheet,
            Size {
                width: 400.0,
                height: 400.0,
            },
        );
        let paragraphs = {
            fn collect<'a>(layout_box: &'a LayoutBox, found: &mut Vec<&'a LayoutBox>) {
                if layout_box.tag == "p" {
                    found.push(layout_box);
                }
                for child in &layout_box.children {
                    collect(child, found);
                }
            }
            let mut found = Vec::new();
            collect(&layout.root, &mut found);
            found
        };
        let lines = |paragraph: &LayoutBox| {
            let mut tops = paragraph.text.iter().map(|run| run.y).collect::<Vec<_>>();
            tops.dedup();
            tops.len()
        };
        let joined = |paragraph: &LayoutBox| {
            paragraph
                .text
                .iter()
                .map(|run| run.text.as_str())
                .collect::<String>()
        };
        let [pre, nowrap, prewrap, preline, cjk, long, anywhere] = paragraphs[..] else {
            panic!("seven paragraphs should lay out");
        };
        assert_eq!(lines(pre), 2);
        assert_eq!(joined(pre), "a  bc", "pre keeps both spaces");
        assert_eq!(lines(nowrap), 1);
        assert!(lines(prewrap) > 1);
        assert!(
            joined(prewrap).contains("one  two"),
            "pre-wrap keeps spaces"
        );
        assert_eq!(lines(preline), 2);
        assert_eq!(
            joined(preline),
            "a bc",
            "pre-line collapses spaces but keeps the line feed"
        );
        assert!(lines(cjk) > 1, "ideographs break between each other");
        assert_eq!(lines(long), 1, "a long word overflows by default");
        assert!(
            lines(anywhere) > 1,
            "overflow-wrap anywhere breaks inside it"
        );
    }

    #[test]
    fn right_to_left_text_aligns_to_its_start_and_reorders_by_level() {
        let document = Document::parse_markup(
            "<html><body><p dir='rtl'>abc</p><p>x \u{5D0}\u{5D1} \u{5D2}\u{5D3} y</p></body></html>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse("body, p { margin: 0; width: 200px }")
            .expect("stylesheet should parse");
        let layout = layout(
            &document,
            &stylesheet,
            Size {
                width: 400.0,
                height: 200.0,
            },
        );
        let runs = collect_text_runs(&layout.root);
        let abc = runs.iter().find(|run| run.text == "abc").expect("abc");
        assert!(abc.x > 150.0, "an rtl paragraph starts at its right edge");
        let x_of = |needle: char| {
            runs.iter()
                .find(|run| run.text.contains(needle))
                .map(|run| run.x)
                .expect("run")
        };
        // the hebrew words read right to left, so the second word sits to the left of the first
        assert!(x_of('\u{5D2}') < x_of('\u{5D0}'));
        assert!(x_of('x') < x_of('\u{5D2}') && x_of('\u{5D0}') < x_of('y'));
        let first = runs
            .iter()
            .find(|run| run.text.contains('\u{5D0}'))
            .expect("first word");
        assert!(
            first.text.trim_end().ends_with('\u{5D0}'),
            "its letters are drawn in reverse"
        );
    }

    fn find_box<'a>(layout_box: &'a LayoutBox, tag: &str) -> Option<&'a LayoutBox> {
        if layout_box.tag == tag {
            return Some(layout_box);
        }
        layout_box
            .children
            .iter()
            .find_map(|child| find_box(child, tag))
    }

    fn collect_text_runs(box_: &LayoutBox) -> Vec<TextRun> {
        let mut runs = box_.text.clone();
        for child in &box_.children {
            runs.extend(collect_text_runs(child));
        }
        runs
    }

    #[test]
    fn block_flow_respects_margin_padding_and_border() {
        let document = Document::parse_markup(
            "<style>main { margin: 10px; padding: 4px; border: 2px solid #000; } p { height: 20px; }</style><main><p>hello</p><p>world</p></main>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let result = layout(
            &document,
            &stylesheet,
            Size {
                width: 200.0,
                height: 200.0,
            },
        );
        let main = &result.root.children[0];
        assert_eq!(main.tag, "main");
        assert_eq!(main.rect.x, 10.0);
        assert_eq!(main.rect.width, 180.0);
        assert_eq!(main.children.len(), 2);
        assert!(main.children[1].rect.y > main.children[0].rect.bottom());
    }

    #[test]
    fn inline_boxes_share_the_line_baseline_and_keep_the_space_before_them() {
        let document = Document::parse_markup(concat!(
            "<style>* { margin: 0 } p { font-size: 16px } kbd { display: inline-block; ",
            "font-size: 12px; padding: 0 4px } i { display: inline-block; vertical-align: 10px }",
            "</style><p>press <kbd>Alt</kbd> x<sup>2</sup> <i>up</i></p>",
        ))
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let result = layout(
            &document,
            &stylesheet,
            Size {
                width: 400.0,
                height: 200.0,
            },
        );
        let fonts = crate::font::active_fonts();
        let baseline = |run: &TextRun| run.y + fonts.ascent(run.font_stack, run.font_size);
        let runs = collect_text_runs(&result.root);
        let run = |text: &str| {
            runs.iter()
                .find(|run| run.text.trim() == text)
                .unwrap_or_else(|| panic!("a run for {text}"))
        };
        let press = run("press");
        assert!(
            press.text.ends_with(' '),
            "the space before the box is drawn"
        );
        let kbd = find_box(&result.root, "kbd").expect("kbd box");
        assert!(kbd.rect.x >= press.x + press.width - 0.01);
        assert!((baseline(run("Alt")) - baseline(press)).abs() < 0.01);
        assert!((baseline(press) - baseline(run("2")) - (16.0 / 3.0 + 1.0)).abs() < 0.01);
        assert!((baseline(press) - baseline(run("up")) - 10.0).abs() < 0.01);
    }

    #[test]
    fn a_shrunk_box_fits_text_split_across_nodes_on_one_line() {
        let mut document = Document::parse_markup(concat!(
            "<style>* { margin: 0 } main { display: flex } span { margin-left: auto } ",
            "i { display: inline-block }</style>",
            "<main><b>A</b><span id='split'>9 clips in 3 sessions, </span></main>",
            "<main><b>A</b><span id='inline'>9 clips in <i>3</i> sessions</span></main>",
            "<main><b>A</b><span>9 clips, <em style='font-size: 32px'>684</em></span></main>",
        ))
        .expect("document should parse");
        let split = document.find_element_by_id("split").expect("split span");
        let tail = document.create_text("684.0 MB");
        document.append_child(split, tail).expect("text appends");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let result = layout(
            &document,
            &stylesheet,
            Size {
                width: 400.0,
                height: 200.0,
            },
        );
        fn rows<'a>(layout: &'a LayoutBox, found: &mut Vec<&'a LayoutBox>) {
            if layout.tag == "main" {
                found.push(layout);
            }
            for child in &layout.children {
                rows(child, found);
            }
        }
        let mut found = Vec::new();
        rows(&result.root, &mut found);
        let rows = found;
        assert_eq!(rows.len(), 3);
        for row in rows {
            let runs = collect_text_runs(find_box(row, "span").expect("span box"));
            assert!(runs.len() > 1, "{runs:?}");
            let fonts = crate::font::active_fonts();
            let baseline = |run: &TextRun| run.y + fonts.ascent(run.font_stack, run.font_size);
            assert!(
                runs.iter()
                    .all(|run| (baseline(run) - baseline(&runs[0])).abs() < 0.01),
                "one line: {runs:?}"
            );
        }
    }

    #[test]
    fn wraps_text_when_the_line_is_full() {
        let document = Document::parse_markup("<main>one two three four five six</main>")
            .expect("document should parse");
        let stylesheet = Stylesheet::parse("").expect("empty css should parse");
        let result = layout(
            &document,
            &stylesheet,
            Size {
                width: 100.0,
                height: 100.0,
            },
        );
        let main = &result.root.children[0];
        assert!(main.text.len() > 1);
        assert!(main.rect.height > 20.0);
        let rendered = main
            .text
            .iter()
            .map(|run| run.text.as_str())
            .collect::<String>();
        assert!(rendered.contains("one"));
        assert!(rendered.contains("six"));
    }

    #[test]
    fn focused_input_layout_carries_live_value_and_selection_metadata() {
        let document = Document::parse_markup(
            r#"<input id="name" value="initial" style="display: block; width: 120px; height: 28px">"#,
        )
        .expect("input document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let mut state = StyleState {
            focused: Some(document.node_at(&[0])),
            ..Default::default()
        };
        state
            .control_values
            .insert(document.node_at(&[0]), "live".to_string());
        state
            .text_selections
            .insert(document.node_at(&[0]), (1, 3, true));
        let result = layout_with_state(
            &document,
            &stylesheet,
            Size {
                width: 200.0,
                height: 80.0,
            },
            &state,
        );
        let input = &result.root.children[0];
        assert_eq!(input.control_value.as_deref(), Some("live"));
        assert_eq!(input.control_selection, Some((1, 3, true)));
        assert!(input.control_focused);
        assert_eq!(input.text[0].text, "live");
    }

    #[test]
    fn empty_text_controls_paint_placeholder_without_changing_live_value() {
        let document = Document::parse_markup(
            r#"<input id="name" placeholder="your name"><textarea id="note" placeholder="line one&#13;&#10;line two"></textarea>"#,
        )
        .expect("placeholder document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let result = layout_with_state(
            &document,
            &stylesheet,
            Size {
                width: 240.0,
                height: 120.0,
            },
            &StyleState::default(),
        );
        let input = &result.root.children[0];
        let textarea = &result.root.children[1];
        assert_eq!(input.control_value.as_deref(), Some(""));
        assert_eq!(input.text[0].text, "your name");
        assert_eq!(input.text[0].color, Color::opaque(117, 117, 117));
        assert_eq!(textarea.control_value.as_deref(), Some(""));
        assert_eq!(
            textarea
                .text
                .iter()
                .map(|run| run.text.as_str())
                .collect::<Vec<_>>(),
            ["line one", "line two"]
        );
        let mut state = StyleState::default();
        state
            .control_values
            .insert(document.node_at(&[0]), "live".to_string());
        let live = layout_with_state(
            &document,
            &stylesheet,
            Size {
                width: 240.0,
                height: 120.0,
            },
            &state,
        );
        assert_eq!(live.root.children[0].text[0].text, "live");
    }

    #[test]
    fn controls_centre_their_text_as_blink_aligns_block_content() {
        let document = Document::parse_markup(
            r#"<style>* { margin: 0; padding: 0; border: 0; font-size: 10px; line-height: 12px } input, select, button { display: block; height: 30px } .short { height: 4px }</style><input placeholder="p"><select><option>s</option></select><button>b</button><button class="short">t</button>"#,
        )
        .expect("control document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let result = layout_with_state(
            &document,
            &stylesheet,
            Size {
                width: 240.0,
                height: 200.0,
            },
            &StyleState::default(),
        );
        let text_offsets = result
            .root
            .children
            .iter()
            .map(|control| control.text[0].y - control.rect.y)
            .collect::<Vec<_>>();
        // the field's editor drops its 12px line for normal, 12.5px, under a taller fixed height,
        // and a short button keeps its text at the top instead of overflowing upward
        assert_eq!(text_offsets, [9.75, 10.0, 10.0, 1.0]);
    }

    #[test]
    fn password_controls_mask_display_text_without_changing_live_value() {
        let document = Document::parse_markup(
            r#"<input id="secret" type="password" value="s😀cret" style="display: block; width: 120px; height: 28px">"#,
        )
        .expect("password document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let mut state = StyleState {
            focused: Some(document.node_at(&[0])),
            ..Default::default()
        };
        state
            .text_selections
            .insert(document.node_at(&[0]), (1, 3, false));
        let result = layout_with_state(
            &document,
            &stylesheet,
            Size {
                width: 200.0,
                height: 80.0,
            },
            &state,
        );
        let input = &result.root.children[0];
        assert_eq!(input.control_value.as_deref(), Some("s😀cret"));
        assert_eq!(input.text[0].text, "*******");
        assert_eq!(input.control_selection, Some((1, 3, false)));
        assert!(input.control_focused);
    }

    #[test]
    fn textarea_layout_carries_multiline_live_value_without_child_text_duplication() {
        let document = Document::parse_markup(
            "<textarea id='note' style='display: block; width: 120px; height: 54px'>initial\ntext</textarea>",
        )
        .expect("textarea document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let mut state = StyleState {
            focused: Some(document.node_at(&[0])),
            ..Default::default()
        };
        state
            .control_values
            .insert(document.node_at(&[0]), "live\nvalue".to_string());
        state
            .text_selections
            .insert(document.node_at(&[0]), (1, 7, false));
        let result = layout_with_state(
            &document,
            &stylesheet,
            Size {
                width: 200.0,
                height: 100.0,
            },
            &state,
        );
        let textarea = &result.root.children[0];
        assert_eq!(textarea.control_value.as_deref(), Some("live\nvalue"));
        assert_eq!(textarea.control_selection, Some((1, 7, false)));
        assert!(textarea.control_focused);
        assert_eq!(textarea.text.len(), 2);
        assert_eq!(textarea.text[0].text, "live");
        assert_eq!(textarea.text[1].text, "value");
        assert!(textarea.text[1].y > textarea.text[0].y);
    }

    #[test]
    fn textarea_layout_wraps_to_content_width_and_honors_wrap_off() {
        let document = Document::parse_markup(
            "<textarea id='wrapped' style='display: block; width: 24px; height: 80px; padding: 0; border: 0; font-size: 16px'>old</textarea><textarea id='off' wrap='off' style='display: block; width: 24px; height: 80px; padding: 0; border: 0; font-size: 16px'>old</textarea>",
        )
        .expect("textarea document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let mut state = StyleState::default();
        state
            .control_values
            .insert(document.node_at(&[0]), "abcdef".to_string());
        state
            .control_values
            .insert(document.node_at(&[1]), "abcdef".to_string());
        let result = layout_with_state(
            &document,
            &stylesheet,
            Size {
                width: 120.0,
                height: 200.0,
            },
            &state,
        );
        let wrapped = &result.root.children[0];
        let no_wrap = &result.root.children[1];
        assert_eq!(
            wrapped
                .text
                .iter()
                .map(|run| run.text.as_str())
                .collect::<Vec<_>>(),
            ["ab", "cd", "ef"]
        );
        assert_eq!(no_wrap.text.len(), 1);
        assert_eq!(no_wrap.text[0].text, "abcdef");
    }

    #[test]
    fn inherited_text_color_reaches_nested_blocks() {
        let document = Document::parse_markup(
            "<style>body { color: #42d3b4; }</style><body><main>hello</main></body>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let result = layout(
            &document,
            &stylesheet,
            Size {
                width: 120.0,
                height: 80.0,
            },
        );
        let body = &result.root.children[0];
        assert_eq!(body.children[0].text[0].color, Color::opaque(66, 211, 180));
    }

    #[test]
    fn flex_row_distributes_growing_items_and_gap() {
        let document = Document::parse_markup(
            "<style>#row { display: flex; width: 180px; gap: 10px; } #left { flex-grow: 1; } #right { flex-grow: 2; }</style><div id='row'><p id='left'>left</p><p id='right'>right</p></div>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let result = layout(
            &document,
            &stylesheet,
            Size {
                width: 240.0,
                height: 160.0,
            },
        );
        let row = &result.root.children[0];
        assert_eq!(row.rect.width, 180.0);
        assert_eq!(row.children.len(), 2);
        assert!(row.children[1].rect.x > row.children[0].rect.x);
        assert!(row.children[1].rect.width > row.children[0].rect.width);
        assert!(
            row.children[1].rect.x - (row.children[0].rect.x + row.children[0].rect.width) >= 9.9
        );
    }

    #[test]
    fn flex_column_centers_items_in_an_explicit_height() {
        let document = Document::parse_markup(
            "<style>#column { display: flex; flex-direction: column; width: 100px; height: 100px; gap: 10px; align-items: center; } p { width: 20px; height: 20px; margin: 0; }</style><div id='column'><p>one</p><p>two</p></div>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let result = layout(
            &document,
            &stylesheet,
            Size {
                width: 160.0,
                height: 160.0,
            },
        );
        let column = &result.root.children[0];
        assert_eq!(column.rect.width, 100.0);
        assert_eq!(column.rect.height, 100.0);
        assert_eq!(column.children[0].rect.x, 40.0);
        assert_eq!(
            column.children[1].rect.y,
            column.children[0].rect.bottom() + 10.0
        );
    }

    #[test]
    fn flex_containers_remain_in_block_parent_flow() {
        let document = Document::parse_markup(
            "<body><main style='display: flex;'><p>inside</p></main><p>after</p></body>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse("").expect("empty css should parse");
        let result = layout(
            &document,
            &stylesheet,
            Size {
                width: 240.0,
                height: 160.0,
            },
        );
        let body = &result.root.children[0];
        assert_eq!(body.children.len(), 2);
        assert_eq!(body.children[0].tag, "main");
        assert_eq!(body.children[0].children[0].tag, "p");
        assert!(body.children[1].rect.y >= body.children[0].rect.bottom());
    }

    #[test]
    fn grid_places_children_in_tracks_with_gap() {
        let document = Document::parse_markup(
            "<style>#grid { display: grid; width: 200px; grid-template-columns: 1fr 2fr; grid-template-rows: 30px 40px; gap: 10px; } #grid > p { height: 20px; margin: 0; }</style><div id='grid'><p>one</p><p>two</p><p>three</p><p>four</p></div>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let result = layout(
            &document,
            &stylesheet,
            Size {
                width: 240.0,
                height: 160.0,
            },
        );
        let grid = &result.root.children[0];
        assert_eq!(grid.rect.width, 200.0);
        assert_eq!(grid.rect.height, 80.0);
        assert_eq!(grid.children.len(), 4);
        assert!((grid.children[0].rect.width - 63.333).abs() < 0.01);
        assert!((grid.children[1].rect.x - 73.333).abs() < 0.01);
        assert_eq!(grid.children[2].rect.y, 40.0);
        assert_eq!(grid.children[3].rect.y, 40.0);
    }

    #[test]
    fn grid_item_percentage_heights_resolve_against_the_area() {
        // clip's editor: a fixed height grid whose flexible row holds a 100% tall child, which
        // must take the row's height rather than its own content's
        let document = Document::parse_markup(
            "<style>#grid { display: grid; width: 200px; height: 120px; grid-template-rows: minmax(0, 1fr) 20px; } #grid > div { margin: 0; } #fill { height: 100%; } #fill > i { display: block; height: 500px; } #half { min-height: 50%; }</style><div id='grid'><div id='fill'><i></i></div><div id='half'></div></div>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let result = layout(
            &document,
            &stylesheet,
            Size {
                width: 240.0,
                height: 160.0,
            },
        );
        let grid = &result.root.children[0];
        assert_eq!(grid.rect.height, 120.0);
        assert_eq!(grid.children[0].rect.height, 100.0);
        assert_eq!(grid.children[1].rect.y, 100.0);
        assert_eq!(grid.children[1].rect.height, 20.0);
    }

    #[test]
    fn flex_and_grid_children_include_generated_content_boxes() {
        let document = Document::parse_markup(
            "<style>#row { display: flex; margin: 0; } #row::before { content: 'badge'; } #grid { display: grid; grid-template-columns: 60px; margin: 0; } #grid::after { content: 'tail'; }</style><div id='row'><p>one</p></div><div id='grid'><p>cell</p></div>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let result = layout(
            &document,
            &stylesheet,
            Size {
                width: 200.0,
                height: 120.0,
            },
        );
        let row = &result.root.children[0];
        let badge = find_box(row, "::before").expect("flex before box should exist");
        assert!(!badge.text.is_empty(), "the generated text joins the item");
        let grid = &result.root.children[1];
        let tail = find_box(grid, "::after").expect("grid after box should exist");
        assert_eq!(
            tail.rect.x, 0.0,
            "the generated item takes the first free track"
        );

        // the generated text runs join paint through the normal box path
        let runs = collect_text_runs(&result.root);
        let text = runs
            .iter()
            .map(|run| run.text.as_str())
            .collect::<Vec<_>>()
            .join("");
        assert!(
            text.contains("badge") && text.contains("tail"),
            "generated flex and grid content paints, got {text}"
        );
    }

    #[test]
    fn positioned_children_leave_flow_and_use_relative_containing_blocks() {
        let document = Document::parse_markup(
            "<style>#stage { position: relative; width: 200px; height: 100px; margin: 0; } #overlay { position: absolute; left: 30px; top: 20px; width: 40px; height: 20px; margin: 0; } #flow { width: 20px; height: 10px; margin: 0; }</style><div id='stage'><div id='overlay'></div><div id='flow'></div></div>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let result = layout(
            &document,
            &stylesheet,
            Size {
                width: 240.0,
                height: 160.0,
            },
        );
        let stage = &result.root.children[0];
        assert_eq!(stage.children.len(), 2);
        let flow = &stage.children[0];
        let overlay = &stage.children[1];
        assert_eq!(flow.node, document.node_at(&[1, 1]));
        assert_eq!(flow.rect.y, 0.0);
        assert_eq!(overlay.node, document.node_at(&[1, 0]));
        assert_eq!(overlay.rect.x, 30.0);
        assert_eq!(overlay.rect.y, 20.0);
        assert_eq!(overlay.rect.width, 40.0);
        assert_eq!(overlay.rect.height, 20.0);
    }

    #[test]
    fn fixed_children_anchor_to_the_viewport() {
        let document = Document::parse_markup(
            "<style>#fixed { position: fixed; right: 10px; bottom: 5px; width: 20px; height: 10px; margin: 0; }</style><div id='fixed'></div>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let result = layout(
            &document,
            &stylesheet,
            Size {
                width: 120.0,
                height: 80.0,
            },
        );
        let fixed = &result.root.children[0];
        assert_eq!(fixed.rect.x, 90.0);
        assert_eq!(fixed.rect.y, 65.0);
    }

    #[test]
    fn nested_absolute_children_use_the_nearest_positioned_ancestor() {
        let document = Document::parse_markup(
            "<style>#stage { position: relative; width: 200px; height: 100px; margin: 0; } #wrapper { margin: 0; } #overlay { position: absolute; bottom: 10px; width: 40px; height: 10px; margin: 0; }</style><div id='stage'><div id='wrapper'><div id='overlay'></div></div></div>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let result = layout(
            &document,
            &stylesheet,
            Size {
                width: 240.0,
                height: 160.0,
            },
        );
        let stage = &result.root.children[0];
        let wrapper = &stage.children[0];
        let overlay = &wrapper.children[0];
        assert_eq!(overlay.node, document.node_at(&[1, 0, 0]));
        assert_eq!(overlay.rect.y, 80.0);
    }

    #[test]
    fn absolute_children_place_against_the_padding_box_of_a_bordered_ancestor() {
        let document = Document::parse_markup(
            "<style>body { margin: 0 } .stage { position: relative; width: 100px; height: 50px; border-style: solid; border-width: 3px 7px 2px 5px; padding: 4px } .fill { position: absolute; left: 0; top: 0; width: 100%; height: 100% } .corner { position: absolute; right: 0; bottom: 0; width: 10px; height: 10px }</style><div class='stage'><div class='fill'></div><div class='corner'></div></div><div class='stage' style='display: flex'><div class='fill'></div><div class='corner'></div></div>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let result = layout(
            &document,
            &stylesheet,
            Size {
                width: 240.0,
                height: 200.0,
            },
        );
        for stage in &result.root.children[..2] {
            let top = stage.rect.y;
            let fill = stage.children[0].rect;
            let corner = stage.children[1].rect;
            assert_eq!((fill.x, fill.y - top), (5.0, 3.0));
            assert_eq!((fill.width, fill.height), (108.0, 58.0));
            assert_eq!((corner.x, corner.y - top), (103.0, 51.0));
        }
    }

    #[test]
    fn hidden_overflow_clips_paint_and_hit_testing() {
        let document = Document::parse_markup(
            "<style>#clip { width: 20px; height: 20px; overflow: hidden; margin: 0; } #child { width: 40px; height: 40px; margin: 0; background: #ff0000; }</style><div id='clip'><div id='child'></div></div>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let result = layout(
            &document,
            &stylesheet,
            Size {
                width: 80.0,
                height: 80.0,
            },
        );
        assert_eq!(result.hit_test(10.0, 10.0), Some(document.node_at(&[1, 0])));
        assert_eq!(result.hit_test(30.0, 10.0), None);
        let canvas = result.paint();
        let inside = ((10 * canvas.width() + 10) * 4) as usize;
        let outside = ((10 * canvas.width() + 30) * 4) as usize;
        assert_eq!(&canvas.rgba_pixels()[inside..inside + 4], [255, 0, 0, 255]);
        assert_ne!(
            &canvas.rgba_pixels()[outside..outside + 4],
            [255, 0, 0, 255]
        );
    }

    #[test]
    fn clip_paths_cut_paint_and_hit_testing_for_the_whole_subtree() {
        let document = Document::parse_markup(
            "<style>#cut { width: 40px; height: 40px; margin: 0; background: #ff0000; clip-path: polygon(0 0, calc(100% - 14px) 0, 100% 14px, 100% 100%, 0 100%); } #child { width: 40px; height: 10px; margin: 0; background: #0000ff; } #hidden { position: absolute; left: 50px; top: 0; width: 20px; height: 20px; background: #00ff00; clip-path: inset(50%); }</style><div id='cut'><div id='child'></div></div><div id='hidden'></div>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let result = layout(
            &document,
            &stylesheet,
            Size {
                width: 80.0,
                height: 80.0,
            },
        );
        assert_eq!(result.hit_test(5.0, 2.0), Some(document.node_at(&[1, 0])));
        assert_eq!(result.hit_test(5.0, 20.0), Some(document.node_at(&[1])));
        assert_eq!(result.hit_test(38.0, 2.0), None);
        assert_eq!(result.hit_test(60.0, 10.0), None);
        let canvas = result.paint();
        let pixel = |x: u32, y: u32| {
            let index = ((y * canvas.width() + x) * 4) as usize;
            canvas.rgba_pixels()[index..index + 4].to_vec()
        };
        assert_eq!(pixel(5, 2), [0, 0, 255, 255]);
        assert_eq!(pixel(5, 20), [255, 0, 0, 255]);
        assert_eq!(pixel(38, 2), pixel(78, 78));
        assert_eq!(pixel(60, 10), pixel(78, 78));
    }

    #[test]
    fn shadow_lists_stack_outside_the_border_box_and_inset_inside_the_padding_box() {
        let document = Document::parse_markup(
            "<style>div { position: absolute; top: 20px; width: 40px; height: 40px; } #card { left: 20px; background: rgba(0, 0, 255, 0.5); box-shadow: inset 0 0 0 4px #00ff00, 10px 0 0 #ff0000, 20px 0 0 #000000; } #soft { left: 110px; box-shadow: 0 0 10px #000000; }</style><div id='card'></div><div id='soft'></div>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let canvas = layout(
            &document,
            &stylesheet,
            Size {
                width: 180.0,
                height: 80.0,
            },
        )
        .paint();
        let pixel = |x: u32, y: u32| {
            let index = ((y * canvas.width() + x) * 4) as usize;
            canvas.rgba_pixels()[index..index + 4].to_vec()
        };
        assert_eq!(pixel(22, 40), [0, 255, 0, 255]);
        let page = pixel(178, 78);
        let middle = pixel(40, 40);
        assert!(
            (0..2).all(|channel| middle[channel].abs_diff(page[channel] / 2) <= 1),
            "no outer shadow under a translucent box: {middle:?}"
        );
        assert_eq!(
            pixel(65, 40),
            [255, 0, 0, 255],
            "the first shadow is on top"
        );
        assert_eq!(pixel(75, 40), [0, 0, 0, 255]);
        assert_eq!(pixel(85, 40), page);
        let edge = pixel(109, 40)[0];
        assert!(
            (120..150).contains(&edge),
            "a blurred edge is half covered: {edge}"
        );
        assert!(
            page[0] - pixel(100, 40)[0] <= 10,
            "the blur fades out by its radius"
        );
        assert_eq!(pixel(130, 40), page, "nothing paints under the box");
    }

    #[test]
    fn pointer_events_and_user_select_none_pass_through_until_a_descendant_opts_back_in() {
        let document = Document::parse_markup(
            "<style>#app { user-select: none; } #base { width: 60px; height: 40px; margin: 0; } #veil { position: absolute; left: 0; top: 0; width: 60px; height: 40px; pointer-events: none; } #back { position: absolute; left: 40px; top: 20px; width: 10px; height: 10px; pointer-events: auto; } p { margin: 0; } .field { user-select: text; }</style><div id='base'></div><div id='veil'><div id='back'></div></div><div id='app'><p>locked</p><p class='field'>free</p></div>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let result = layout(
            &document,
            &stylesheet,
            Size {
                width: 200.0,
                height: 200.0,
            },
        );
        assert_eq!(result.hit_test(10.0, 10.0), Some(document.node_at(&[1])));
        assert_eq!(result.hit_test(45.0, 25.0), Some(document.node_at(&[2, 0])));
        let selectable = result
            .prose_runs()
            .into_iter()
            .map(|prose| prose.run.text.trim().to_string())
            .collect::<Vec<_>>();
        assert_eq!(selectable, ["free"]);
    }

    #[test]
    fn scroll_extent_ends_with_the_padding_after_the_children_margin_boxes() {
        let document = Document::parse_markup(
            "<style>body { margin: 0 } #scroller { width: 100px; height: 100px; padding: 10px; border: 4px solid red; overflow: auto } #content { width: 300px; height: 300px; margin: 0 7px 5px 0 }</style><div id='scroller'><div id='content'></div></div>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let mut state = StyleState::default();
        state
            .scroll_offsets
            .insert(document.node_at(&[1]), (1000.0, 1000.0));
        let result = layout_with_state(
            &document,
            &stylesheet,
            Size {
                width: 400.0,
                height: 400.0,
            },
            &state,
        );
        let scroller = &result.root.children[0];
        // chromium: scrollWidth 327 and scrollHeight 325 inside the 4px borders, 105px client
        assert_eq!(
            scroller.scroll_content,
            Size {
                width: 335.0,
                height: 333.0
            }
        );
        assert_eq!(scroller.scroll_offset, (222.0, 220.0));
    }

    #[test]
    fn auto_overflow_scrolls_paint_and_hit_testing_with_bounded_offsets() {
        let document = Document::parse_markup(
            "<style>#viewport { width: 40px; height: 20px; overflow: auto; margin: 0; } #content { width: 80px; height: 60px; margin: 0; background: #ff0000; }</style><div id='viewport'><div id='content'></div></div>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let mut state = StyleState::default();
        state
            .scroll_offsets
            .insert(document.node_at(&[1]), (20.0, 30.0));
        let result = layout_with_state(
            &document,
            &stylesheet,
            Size {
                width: 80.0,
                height: 80.0,
            },
            &state,
        );
        let viewport = &result.root.children[0];
        let content = &viewport.children[0];
        assert_eq!(
            viewport.scroll_content,
            Size {
                width: 80.0,
                height: 60.0
            }
        );
        assert_eq!(viewport.scroll_offset, (20.0, 30.0));
        assert_eq!(content.rect.x, -20.0);
        assert_eq!(content.rect.y, -30.0);
        assert_eq!(result.hit_test(10.0, 4.0), Some(document.node_at(&[1, 0])));
        assert_eq!(result.hit_test(50.0, 4.0), None);
        assert_eq!(result.hit_test(10.0, 15.0), Some(document.node_at(&[1])));
        let target = result
            .scroll_target_at(10.0, 10.0)
            .expect("point should resolve to the scroll container");
        assert_eq!(target.node, document.node_at(&[1]));
        assert_eq!(target.max_x, 55.0);
        assert_eq!(target.max_y, 55.0);
        let canvas = result.paint();
        let inside = ((4 * canvas.width() + 10) * 4) as usize;
        let outside = ((4 * canvas.width() + 50) * 4) as usize;
        assert_eq!(&canvas.rgba_pixels()[inside..inside + 4], [255, 0, 0, 255]);
        assert_ne!(
            &canvas.rgba_pixels()[outside..outside + 4],
            [255, 0, 0, 255]
        );
    }

    #[test]
    fn scrollbars_sit_inside_the_border_in_the_look_their_styles_pick() {
        let markup = "<style>body { margin: 0; } .box { width: 80px; height: 80px; overflow: auto; border: 2px solid #000; } .box div { width: 160px; height: 120px; background: #f00; } #thin { scrollbar-width: thin; } #color { scrollbar-color: #0f0 #00f; } .wk::-webkit-scrollbar { width: 12px; height: 8px; } .wk::-webkit-scrollbar-thumb { background: #ff0; } .wk::-webkit-scrollbar-thumb:hover { background: #f0f; } .wk::-webkit-scrollbar-track { background: #0ff; } #both { scrollbar-width: thin; } #hidden::-webkit-scrollbar { display: none; } #none { scrollbar-width: none; } #zero::-webkit-scrollbar { width: 0; height: 0; }</style><div class='box' id='native'><div></div></div><div class='box' id='thin'><div></div></div><div class='box' id='color'><div></div></div><div class='box wk' id='wk'><div></div></div><div class='box wk' id='both'><div></div></div><div class='box' id='hidden'><div></div></div><div class='box' id='none'><div></div></div><div class='box' id='zero'><div></div></div>";
        let document = Document::parse_markup(markup).expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let viewport = Size {
            width: 200.0,
            height: 700.0,
        };
        let paint = |state: &StyleState| {
            let result = layout_with_state(&document, &stylesheet, viewport, state);
            let canvas = result.paint();
            (result, canvas)
        };
        let (result, canvas) = paint(&StyleState::default());
        let pixel = |canvas: &crate::paint::Canvas, x: u32, y: u32| {
            let at = ((y * canvas.width() + x) * 4) as usize;
            <[u8; 4]>::try_from(&canvas.rgba_pixels()[at..at + 4]).unwrap()
        };
        let metrics = |index: usize| scrollbar_metrics(&result.body().children[index]);
        let tracks = |index: usize| {
            let metrics = metrics(index);
            (metrics.vertical_track, metrics.horizontal_track)
        };
        assert_eq!(
            tracks(0),
            (
                Some(Rect {
                    x: 67.0,
                    y: 2.0,
                    width: 15.0,
                    height: 65.0
                }),
                Some(Rect {
                    x: 2.0,
                    y: 67.0,
                    width: 65.0,
                    height: 15.0
                })
            )
        );
        assert_eq!(metrics(0).scrollport.width, 69.0);
        // the native thumb is a 9px pill between the arrow buttons, 3px in from each side
        let placement = thumb_placement(&result.body().children[0], &metrics(0), true).unwrap();
        assert_eq!(
            placement.thumb,
            Rect {
                x: 70.0,
                y: 20.0,
                width: 9.0,
                height: 16.0
            }
        );
        assert_eq!(pixel(&canvas, 10, 10), [255, 0, 0, 255]);
        assert_eq!(pixel(&canvas, 74, 28), [139, 139, 139, 255]);
        assert_eq!(pixel(&canvas, 68, 40), [252, 252, 252, 255]);
        assert_eq!(pixel(&canvas, 74, 74), [252, 252, 252, 255]);
        assert_eq!(pixel(&canvas, 83, 40), [0, 0, 0, 255]);
        assert_eq!(tracks(1).0.map(|track| track.width), Some(10.0));
        assert_eq!(pixel(&canvas, 74, 2 * 84 + 28), [0, 255, 0, 255]);
        assert_eq!(pixel(&canvas, 68, 2 * 84 + 40), [0, 0, 255, 255]);
        // webkit parts fill the bar with no buttons, and a corner no rule styles is white
        let wk = 3 * 84;
        assert_eq!(
            tracks(3).0.map(|track| (track.width, track.height)),
            Some((12.0, 72.0))
        );
        assert_eq!(pixel(&canvas, 76, wk + 20), [255, 255, 0, 255]);
        assert_eq!(pixel(&canvas, 76, wk + 60), [0, 255, 255, 255]);
        assert_eq!(pixel(&canvas, 76, wk + 78), [255, 255, 255, 255]);
        assert_eq!(tracks(4).0.map(|track| track.width), Some(10.0));
        for index in 5..8 {
            assert_eq!(tracks(index), (None, None));
        }
        let hovered = StyleState {
            hovered_part: Some((
                document.find_element_by_id("wk").unwrap(),
                PseudoElement::ScrollbarThumb,
            )),
            ..StyleState::default()
        };
        assert_eq!(pixel(&paint(&hovered).1, 76, wk + 20), [255, 0, 255, 255]);
    }

    #[test]
    fn min_and_max_sizes_bound_auto_heights_and_shrink_to_fit_widths() {
        for display in ["flex; flex-direction: column", "grid"] {
            let markup = format!(
                "<style>* {{ margin: 0; }} #tall {{ display: {display}; max-height: 30px; }} #short {{ display: {display}; min-height: 50px; }} #tall div {{ height: 100px; flex-shrink: 0; }} span {{ display: inline-block; max-width: 40px; min-width: 0; }} em {{ display: inline-block; min-width: 90px; }}</style><div id='tall'><div></div></div><div id='short'></div><p><span>a b c d e f g h i j k</span><em>x</em></p>"
            );
            let document = Document::parse_markup(&markup).expect("document should parse");
            let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
            let result = layout(
                &document,
                &stylesheet,
                Size {
                    width: 400.0,
                    height: 400.0,
                },
            );
            let children = &result.root.children;
            assert_eq!(children[0].rect.height, 30.0, "{display}");
            assert_eq!(children[1].rect.height, 50.0, "{display}");
            assert_eq!(
                find_box(&result.root, "span").expect("span").rect.width,
                40.0
            );
            assert_eq!(find_box(&result.root, "em").expect("em").rect.width, 90.0);
        }
    }

    #[test]
    fn a_flex_row_counts_its_gaps_in_its_max_content_width() {
        let markup = "<style>* { margin: 0; } section { display: flex; } nav { display: flex; gap: 8px; } i { width: 30px; } b { width: 20px; } u { width: 10px; }</style><section><nav><i></i><b></b><u></u></nav></section>";
        let document = Document::parse_markup(markup).expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let result = layout(
            &document,
            &stylesheet,
            Size {
                width: 400.0,
                height: 100.0,
            },
        );
        assert_eq!(find_box(&result.root, "nav").expect("nav").rect.width, 76.0);
        assert_eq!(find_box(&result.root, "u").expect("u").rect.width, 10.0);
    }

    #[test]
    fn a_scrolling_grid_item_does_not_widen_its_flexible_column() {
        let width = |overflow: &str| {
            let markup = format!(
                "<style>* {{ margin: 0; }} section {{ display: grid; width: 200px; grid-template-columns: 50px 1fr; }} nav {{ overflow: {overflow}; }} i {{ display: block; width: 400px; height: 10px; }}</style><section><b></b><nav><i></i></nav></section>"
            );
            let document = Document::parse_markup(&markup).expect("document should parse");
            let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
            let result = layout(
                &document,
                &stylesheet,
                Size {
                    width: 800.0,
                    height: 100.0,
                },
            );
            find_box(&result.root, "nav").expect("nav").rect.width
        };
        // the column holds the item's content, unless the item scrolls it
        assert_eq!(width("visible"), 400.0);
        assert_eq!(width("auto"), 150.0);
        assert_eq!(width("hidden"), 150.0);
    }

    #[test]
    fn width_keywords_size_a_block_by_its_widest_narrowest_or_fitted_content() {
        let markup = "<style>* { margin: 0; } section { width: 50px; } i, b, u, s { display: block; } i { width: max-content; } b { width: min-content; } u, s { width: fit-content; } nav { display: flex; }</style><section><i>aaaa bbbb cccc</i><b>aaaa bbbb cccc</b><u>aaaa bbbb cccc</u><s>aaaa</s><nav><b>aaaa bbbb cccc</b></nav></section>";
        let document = Document::parse_markup(markup).expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let result = layout(
            &document,
            &stylesheet,
            Size {
                width: 400.0,
                height: 300.0,
            },
        );
        let width = |tag: &str| find_box(&result.root, tag).expect("box").rect.width;
        // one word alone fits the section, so fitted it is as wide as the word
        let word = width("s");
        assert!(word > 0.0 && word < 50.0, "{word}");
        // one line of three words overflows the section, the narrowest is one word wide, and
        // fitted content takes the room there is
        assert!(width("i") > 100.0, "{}", width("i"));
        assert_eq!(width("b"), word);
        assert_eq!(width("u"), 50.0);
        // a flex item sized to its narrowest content stays one word wide
        let nav = find_box(&result.root, "nav").expect("nav");
        assert_eq!(find_box(nav, "b").expect("item").rect.width, word);
    }

    #[test]
    fn a_run_of_text_in_a_flex_row_has_a_gap_beside_it_in_the_intrinsic_width() {
        let width = |gap: &str, inner: &str| {
            let markup = format!(
                "<style>* {{ margin: 0; }} section {{ display: flex; }} nav {{ display: flex; gap: {gap}; }} i {{ width: 30px; }}</style><section><nav>{inner}</nav></section>"
            );
            let document = Document::parse_markup(&markup).expect("document should parse");
            let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
            let result = layout(
                &document,
                &stylesheet,
                Size {
                    width: 400.0,
                    height: 100.0,
                },
            );
            find_box(&result.root, "nav").expect("nav").rect.width
        };
        assert_eq!(width("8px", "<i></i>Save"), width("0", "<i></i>Save") + 8.0);
        assert_eq!(
            width("8px", "<i></i>Save<i></i>"),
            width("0", "<i></i>Save<i></i>") + 16.0
        );
        // spaces between items are no item
        assert_eq!(width("8px", "<i></i> <i></i>"), 68.0);
    }

    #[test]
    fn a_vertical_scrollbar_takes_its_width_from_the_content_box() {
        let widths = |display: &str| {
            let markup = format!(
                "<style>#port {{ display: {display}; flex-direction: column; width: 50px; height: 20px; padding: 0 4px; border: 1px solid #000; overflow: auto; margin: 0; }} #port > div {{ height: 80px; flex-shrink: 0; margin: 0; }}</style><div id='port'><div></div></div>"
            );
            let document = Document::parse_markup(&markup).expect("document should parse");
            let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
            let result = layout(
                &document,
                &stylesheet,
                Size {
                    width: 200.0,
                    height: 100.0,
                },
            );
            let port = &result.root.children[0];
            let metrics = scrollbar_metrics(port);
            (
                port.rect.width,
                port.children[0].rect.width,
                metrics.vertical_track.is_some(),
                metrics.horizontal_track.is_some(),
            )
        };
        for display in ["block", "flex", "grid"] {
            assert_eq!(
                widths(display),
                (60.0, 50.0 - SCROLLBAR_THICKNESS, true, false),
                "{display}"
            );
        }
    }

    #[test]
    fn a_line_overflows_only_past_a_layout_unit_beyond_its_width() {
        let line = InlineLine {
            y: 0.0,
            used: 0.0,
            height: 0.0,
            left: 0.0,
            right: 0.0,
            band: 1.0,
            strut: (0.0, 0.0),
            floats: None,
            decorations: Vec::new(),
            open_inlines: Vec::new(),
            trailing_space: false,
            rtl: false,
            ellipsis: false,
            fragments: Vec::new(),
            placed: Vec::new(),
        };
        // three pieces of a line that measured 68.49325 whole
        assert!(!line.overflows(22.831085 + 7.610362 + 38.05181, 68.49325));
        assert!(line.overflows(68.6, 68.49325));
    }

    #[test]
    fn an_out_of_flow_box_with_an_auto_width_shrinks_to_fit_its_insets() {
        let document = Document::parse_markup(
            "<style>* { margin: 0; } p { position: absolute; left: 10px; } section { position: absolute; left: 0; right: 0; } aside { position: fixed; left: 150px; }</style><p>hi</p><section>x</section><aside>a b c d e f g h i j k l m n</aside>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let result = layout(
            &document,
            &stylesheet,
            Size {
                width: 200.0,
                height: 100.0,
            },
        );
        let width = |tag: &str| find_box(&result.root, tag).expect(tag).rect.width;
        assert!(width("p") > 0.0 && width("p") < 40.0, "{}", width("p"));
        assert_eq!(width("section"), 200.0);
        assert!(width("aside") <= 50.0, "{}", width("aside"));
        let aside = find_box(&result.root, "aside").expect("aside");
        let runs = collect_text_runs(aside);
        assert!(
            runs.iter().any(|run| run.y > runs[0].y),
            "the fixed box wraps in the room it has"
        );
    }

    #[test]
    fn baseline_alignment_lines_up_first_baselines_in_grid_and_flex_rows() {
        for display in ["grid; grid-template-columns: auto auto", "flex"] {
            let markup = format!(
                "<style>#row {{ display: {display}; align-items: baseline; margin: 0; }} p {{ margin: 0; }} #big {{ font-size: 40px; }} #small {{ font-size: 10px; margin-top: 3px; }}</style><div id='row'><p id='big'>Big</p><p id='small'>small</p></div>"
            );
            let document = Document::parse_markup(&markup).expect("document should parse");
            let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
            let result = layout(
                &document,
                &stylesheet,
                Size {
                    width: 300.0,
                    height: 200.0,
                },
            );
            let row = &result.root.children[0];
            let baselines: Vec<f32> = row
                .children
                .iter()
                .map(|child| first_baseline(child).expect("items should hold text"))
                .collect();
            assert!(
                (baselines[0] - baselines[1]).abs() < 0.01,
                "{display}: {baselines:?}"
            );
            assert!(
                row.children[1].rect.y > row.children[0].rect.y + 3.0,
                "{display}"
            );
        }
    }

    #[test]
    fn auto_overflow_keeps_fixed_descendants_at_the_viewport() {
        let document = Document::parse_markup(
            "<style>#viewport { width: 40px; height: 20px; overflow: auto; margin: 0; } #content { width: 40px; height: 80px; margin: 0; } #fixed { position: fixed; left: 70px; top: 12px; width: 8px; height: 8px; margin: 0; }</style><div id='viewport'><div id='content'></div><div id='fixed'></div></div>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let mut state = StyleState::default();
        state
            .scroll_offsets
            .insert(document.node_at(&[1]), (0.0, 30.0));
        let result = layout_with_state(
            &document,
            &stylesheet,
            Size {
                width: 120.0,
                height: 100.0,
            },
            &state,
        );
        let viewport = &result.root.children[0];
        assert_eq!(viewport.children[0].rect.y, -30.0);
        assert_eq!(viewport.children[1].rect.x, 70.0);
        assert_eq!(viewport.children[1].rect.y, 12.0);
    }

    #[test]
    fn sticky_children_hold_to_the_scrollport_during_element_scroll() {
        let document = Document::parse_markup(
            "<style>#viewport { width: 80px; height: 40px; overflow: auto; margin: 0; } #sticky { position: sticky; top: 0; width: 80px; height: 10px; margin: 0; background: #00ff00; } #spacer { width: 80px; height: 80px; margin: 0; background: #ff0000; }</style><div id='viewport'><div id='sticky'></div><div id='spacer'></div></div>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let mut state = StyleState::default();
        state
            .scroll_offsets
            .insert(document.node_at(&[1]), (0.0, 20.0));
        let result = layout_with_state(
            &document,
            &stylesheet,
            Size {
                width: 120.0,
                height: 80.0,
            },
            &state,
        );
        let viewport = &result.root.children[0];
        let sticky = &viewport.children[0];
        assert_eq!(sticky.position, Position::Sticky);
        assert_eq!(sticky.rect.y, -20.0);
        assert_eq!(sticky.visual_offset, (0.0, 20.0));
        assert_eq!(result.hit_test(10.0, 4.0), Some(sticky.node));
        let canvas = result.paint();
        let pixel = &canvas.rgba_pixels()[(4 * canvas.width() as usize + 10) * 4..];
        assert_eq!(&pixel[..4], &[0, 255, 0, 255]);
    }

    #[test]
    fn layout_resolves_inherited_custom_properties_before_paint() {
        let document = Document::parse_markup(
            "<style>:root { --accent: #00ff00; --card-width: calc(50% - 10px); margin: 0; } #card { width: var(--card-width); height: calc(10px + 10px); margin: 0; background: var(--accent); }</style><html><div id='card'></div></html>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let result = layout(
            &document,
            &stylesheet,
            Size {
                width: 100.0,
                height: 60.0,
            },
        );
        let card = &result.root.children[0].children[0];
        assert_eq!(card.rect.width, 40.0);
        assert_eq!(card.rect.height, 20.0);
        let canvas = result.paint();
        let pixel = &canvas.rgba_pixels()[(4 * canvas.width() as usize + 4) * 4..];
        assert_eq!(&pixel[..4], &[0, 255, 0, 255]);
    }

    #[test]
    fn a_vertical_document_scrollbar_narrows_the_initial_containing_block() {
        let viewport = Size {
            width: 100.0,
            height: 80.0,
        };
        let widths = |markup: &str| {
            let document = Document::parse_markup(markup).expect("document should parse");
            let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
            let result = layout(&document, &stylesheet, viewport);
            let widths: Vec<f32> = result
                .root
                .children
                .iter()
                .map(|child| child.rect.width)
                .collect();
            (
                widths,
                scrollbar_metrics(&result.root).horizontal_track.is_some(),
            )
        };
        let style = "<style>div { margin: 0; } #wide { width: 100vw; height: 1px; }</style>";
        assert_eq!(
            widths(&format!(
                "{style}<div style='height: 300px'></div><div id='wide'></div>"
            )),
            (vec![100.0 - SCROLLBAR_THICKNESS, 100.0], true)
        );
        assert_eq!(
            widths(&format!(
                "{style}<div style='height: 300px'></div><div></div>"
            )),
            (vec![100.0 - SCROLLBAR_THICKNESS; 2], false)
        );
        assert_eq!(
            widths(&format!("{style}<div style='height: 10px'></div>")),
            (vec![100.0], false)
        );
    }

    #[test]
    fn rescrolling_matches_laying_out_again() {
        let markup = concat!(
            "<style>body { margin: 0 } #list { height: 40px; overflow: auto; margin: 5px }",
            "#list div { height: 17px } #bar { position: fixed; top: 3px; width: 9px; height: 9px }",
            "#held { position: relative } #tag { position: absolute; right: 2px }",
            "#tall { height: 300px }</style><div id='bar'></div><div id='list'><div>a</div>",
            "<div id='held'>b <i id='tag'>c</i></div><div>d</div><div>e</div></div>",
            "<p>words that wrap over the width</p><div id='tall'></div>",
        );
        let document = Document::parse_markup(markup).expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let list = document
            .find_element_by_id("list")
            .expect("the list exists");
        let offsets = |list_offset: f32, root: f32| {
            let mut state = StyleState::default();
            state.scroll_offsets.insert(list, (0.0, list_offset));
            state.scroll_offsets.insert(document.root(), (0.0, root));
            state
        };
        let size = Size {
            width: 120.0,
            height: 100.0,
        };
        for ((list_from, root_from), (list_to, root_to)) in [
            ((0.0, 0.0), (9.5, 0.0)),
            ((9.5, 0.0), (0.0, 40.0)),
            ((0.0, 40.0), (500.0, 500.0)),
        ] {
            let old =
                layout_with_state(&document, &stylesheet, size, &offsets(list_from, root_from));
            let state = offsets(list_to, root_to);
            let new = layout_with_state(&document, &stylesheet, size, &state);
            assert_eq!(
                rescroll(&old, &state.scroll_offsets).as_ref(),
                Some(&new),
                "{list_from},{root_from} to {list_to},{root_to}"
            );
        }
        let sticky = Document::parse_markup(
            "<style>p { position: sticky; top: 0 } div { height: 300px }</style><p>s</p><div></div>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&sticky.style_text()).expect("css should parse");
        let old = layout_with_state(&sticky, &stylesheet, size, &StyleState::default());
        let moved = std::collections::BTreeMap::from([(sticky.root(), (0.0, 20.0))]);
        assert!(rescroll(&old, &moved).is_none());
    }

    #[test]
    fn sticky_children_hold_to_the_viewport_during_document_scroll() {
        let document = Document::parse_markup(
            "<style>#sticky { position: sticky; top: 0; width: 120px; height: 10px; margin: 0; background: #00ff00; } #spacer { width: 120px; height: 160px; margin: 0; background: #ff0000; }</style><div id='sticky'></div><div id='spacer'></div>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let mut state = StyleState::default();
        state.scroll_offsets.insert(document.root(), (0.0, 20.0));
        let result = layout_with_state(
            &document,
            &stylesheet,
            Size {
                width: 120.0,
                height: 80.0,
            },
            &state,
        );
        let sticky = &result.root.children[0];
        assert_eq!(sticky.position, Position::Sticky);
        assert_eq!(sticky.rect.y, -20.0);
        assert_eq!(sticky.visual_offset, (0.0, 20.0));
        assert_eq!(result.hit_test(10.0, 4.0), Some(sticky.node));
        let canvas = result.paint();
        let pixel = &canvas.rgba_pixels()[(4 * canvas.width() as usize + 10) * 4..];
        assert_eq!(&pixel[..4], &[0, 255, 0, 255]);
    }

    #[test]
    fn document_scroll_moves_flow_content_and_keeps_fixed_chrome_in_place() {
        let document = Document::parse_markup(
            "<style>#content { width: 60px; height: 160px; margin: 0; background: #ff0000; } #fixed { position: fixed; left: 70px; top: 12px; width: 8px; height: 8px; margin: 0; background: #0000ff; }</style><div id='content'></div><div id='fixed'></div>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let mut state = StyleState::default();
        state.scroll_offsets.insert(document.root(), (0.0, 30.0));
        let result = layout_with_state(
            &document,
            &stylesheet,
            Size {
                width: 120.0,
                height: 100.0,
            },
            &state,
        );
        assert_eq!(result.root.scroll_content.height, 160.0);
        assert_eq!(result.root.scroll_offset, (0.0, 30.0));
        assert_eq!(result.root.children[0].rect.y, -30.0);
        assert_eq!(result.root.children[1].rect.x, 70.0);
        assert_eq!(result.root.children[1].rect.y, 12.0);
        let metrics = scrollbar_metrics(&result.root);
        assert_eq!(metrics.scrollport.height, 100.0);
        assert!(metrics.horizontal_track.is_none());
        assert_eq!(metrics.vertical_track.unwrap().width, SCROLLBAR_THICKNESS);
        let target = result
            .scroll_target_at(10.0, 10.0)
            .expect("viewport should expose the document scroll target");
        assert_eq!(target.node, document.root());
        assert_eq!(target.max_y, 60.0);
    }

    #[test]
    fn relative_offsets_move_visual_boxes_without_changing_flow() {
        let document = Document::parse_markup(
            "<style>#first { position: relative; left: 10px; top: 5px; width: 20px; height: 20px; margin: 0; background: #ff0000; } #second { width: 20px; height: 20px; margin: 0; background: #0000ff; }</style><div id='first'></div><div id='second'></div>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let result = layout(
            &document,
            &stylesheet,
            Size {
                width: 120.0,
                height: 100.0,
            },
        );
        let first = &result.root.children[0];
        let second = &result.root.children[1];
        assert_eq!(first.rect.x, 0.0);
        assert_eq!(first.rect.y, 0.0);
        assert_eq!(first.visual_offset, (10.0, 5.0));
        assert_eq!(second.rect.y, 20.0);
        assert_eq!(result.hit_test(10.0, 5.0), Some(first.node));
        assert_eq!(result.hit_test(0.0, 0.0), None);
    }

    #[test]
    fn z_index_orders_overlapping_paint_and_hit_testing() {
        let document = Document::parse_markup(
            "<style>#low { position: absolute; left: 0; top: 0; width: 30px; height: 30px; margin: 0; z-index: 1; background: #ff0000; } #high { position: absolute; left: 0; top: 0; width: 30px; height: 30px; margin: 0; z-index: 2; background: #0000ff; }</style><div id='low'></div><div id='high'></div>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let result = layout(
            &document,
            &stylesheet,
            Size {
                width: 40.0,
                height: 40.0,
            },
        );
        let low = &result.root.children[0];
        let high = &result.root.children[1];
        assert_eq!(low.z_index, 1);
        assert_eq!(high.z_index, 2);
        assert_eq!(result.hit_test(10.0, 10.0), Some(high.node));
        let canvas = result.paint();
        let pixel = &canvas.rgba_pixels()[(10 * canvas.width() as usize + 10) * 4..];
        assert_eq!(&pixel[..4], &[0, 0, 255, 255]);
    }

    #[test]
    fn nested_stacking_contexts_paint_and_hit_test_as_atomic_layers() {
        let document = Document::parse_markup(
            "<style>#low { position: relative; z-index: 1; width: 30px; height: 30px; margin: 0; background: #ff0000; } #low-child { position: absolute; left: 0; top: 0; width: 30px; height: 30px; margin: 0; z-index: 100; background: #0000ff; } #high { position: relative; z-index: 2; top: -30px; width: 30px; height: 30px; margin: 0; background: #00ff00; }</style><div id='low'><div id='low-child'></div></div><div id='high'></div>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let result = layout(
            &document,
            &stylesheet,
            Size {
                width: 40.0,
                height: 40.0,
            },
        );
        let high = &result.root.children[1];
        assert_eq!(result.hit_test(10.0, 10.0), Some(high.node));
        let canvas = result.paint();
        let pixel = &canvas.rgba_pixels()[(10 * canvas.width() as usize + 10) * 4..];
        assert_eq!(&pixel[..4], &[0, 255, 0, 255]);
    }

    #[test]
    fn negative_stacking_layers_stay_behind_in_flow_content() {
        let document = Document::parse_markup(
            "<style>#negative { position: absolute; z-index: -1; left: 0; top: 0; width: 30px; height: 30px; margin: 0; background: #0000ff; } #flow { width: 30px; height: 30px; margin: 0; background: #ff0000; }</style><div id='negative'></div><div id='flow'></div>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let result = layout(
            &document,
            &stylesheet,
            Size {
                width: 40.0,
                height: 40.0,
            },
        );
        let flow = &result.root.children[0];
        assert_eq!(result.hit_test(10.0, 10.0), Some(flow.node));
        let canvas = result.paint();
        let pixel = &canvas.rgba_pixels()[(10 * canvas.width() as usize + 10) * 4..];
        assert_eq!(&pixel[..4], &[255, 0, 0, 255]);
    }

    #[test]
    fn modal_dialogs_center_over_a_backdrop_and_fit_their_content() {
        let document = Document::parse_markup(
            r#"<html><body><p>page</p><dialog id="box" open><div style="width: 200px; height: 50px"></div></dialog></body></html>"#,
        )
        .expect("document should parse");
        let dialog = document.find_element_by_id("box").expect("dialog");
        let stylesheet = Stylesheet::parse("").expect("stylesheet should parse");
        let state = StyleState {
            top_layer: vec![dialog],
            ..StyleState::default()
        };
        let viewport = Size {
            width: 800.0,
            height: 600.0,
        };
        let layout = layout_with_state(&document, &stylesheet, viewport, &state);
        // 200 by 50 of content, 1em of padding and a medium border on each side
        let card = find_box(&layout.root, "dialog").expect("dialog box");
        assert_eq!(
            (card.rect.x, card.rect.y, card.rect.width, card.rect.height),
            (281.0, 256.0, 238.0, 88.0)
        );
        let backdrop = find_box(&layout.root, "::backdrop").expect("backdrop box");
        assert_eq!(
            (
                backdrop.rect.x,
                backdrop.rect.y,
                backdrop.rect.width,
                backdrop.rect.height
            ),
            (0.0, 0.0, 800.0, 600.0)
        );
    }
}
