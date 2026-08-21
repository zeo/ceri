use crate::css::{
    AlignItems, Color, Display, Edges, FlexDirection, FontWeight, Gradient, GridTemplate,
    GridTrack, InheritedText, JustifyContent, Overflow, Position, PseudoElement, Style,
    StyleContext, StyleState, Stylesheet, TextAlign,
};
use crate::dom::{Document, Element, Node, NodePath};
use crate::font::FontStack;
use crate::image::Image;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    pub width: f32,
    pub height: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
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
    pub bold: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedShadow {
    pub offset_x: f32,
    pub offset_y: f32,
    pub blur: f32,
    pub spread: f32,
    pub color: Color,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LayoutBox {
    pub path: NodePath,
    pub tag: String,
    pub rect: Rect,
    pub background: Color,
    pub gradient: Option<Gradient>,
    pub border_radius: f32,
    pub opacity: f32,
    pub shadow: Option<ResolvedShadow>,
    pub border_width: f32,
    pub border_color: Color,
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
    pub(crate) scroll_content: Size,
    pub(crate) scroll_offset: (f32, f32),
    pub(crate) margin: ResolvedEdges,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LayoutDocument {
    pub viewport: Size,
    pub root: LayoutBox,
    pub(crate) images: Vec<ImageLayer>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ImageLayer {
    pub(crate) path: NodePath,
    pub(crate) image: Image,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ResolvedEdges {
    top: f32,
    right: f32,
    bottom: f32,
    left: f32,
}

struct InlineLine {
    y: f32,
    used: f32,
    height: f32,
    // whether the text appended so far ended on whitespace, which decides word joining
    trailing_space: bool,
    fragments: Vec<InlineFragment>,
}

struct InlineFragment {
    text: String,
    width: f32,
    font_size: f32,
    letter_spacing: f32,
    font_stack: FontStack,
    color: Color,
    bold: bool,
}

#[derive(Clone, Copy)]
struct ParentStyle {
    font_size: f32,
    opacity: f32,
    color: Color,
    weight: FontWeight,
    text: InheritedText,
}

// css generated content becomes a synthetic child that the normal cascade styles
fn generated_child(
    element: &Element,
    path: &[usize],
    ancestors: &[&Element],
    stylesheet: &Stylesheet,
    style_state: &StyleState,
    pseudo: PseudoElement,
) -> Option<Node> {
    let mut candidate = Element::new(pseudo.tag());
    let mut candidate_path = path.to_vec();
    candidate_path.push(element.children.len());
    let content = stylesheet.generated_content(StyleContext {
        element: &candidate,
        path: &candidate_path,
        ancestors,
        parent_font_size: 16.0,
        parent_color: Color::BLACK,
        parent_weight: FontWeight::Normal,
        parent_text: InheritedText::default(),
        state: style_state,
    })?;
    if !content.is_empty() {
        candidate.children.push(Node::Text(content));
    }
    Some(Node::Element(candidate))
}

fn resolve_shadow(
    shadow: Option<crate::css::BoxShadow>,
    containing_width: f32,
    font_size: f32,
) -> Option<ResolvedShadow> {
    let shadow = shadow?;
    let resolve = |length: crate::css::Length| {
        length
            .resolve(containing_width, font_size)
            .unwrap_or(0.0)
            .clamp(-2048.0, 2048.0)
    };
    Some(ResolvedShadow {
        offset_x: resolve(shadow.offset_x),
        offset_y: resolve(shadow.offset_y),
        blur: resolve(shadow.blur).max(0.0),
        spread: resolve(shadow.spread),
        color: shadow.color,
    })
}

fn inherited_text(style: &Style) -> InheritedText {
    InheritedText {
        transform: style.text_transform,
        letter_spacing: style.letter_spacing,
        family: style.font_family,
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
}

struct InlineElementInput<'a> {
    element: &'a Element,
    path: &'a [usize],
    parent: ParentStyle,
    context: InlineContext,
    stylesheet: &'a Stylesheet,
    style_state: &'a StyleState,
    ancestors: &'a [&'a Element],
}

struct FlexItem<'a> {
    element: &'a Element,
    path: NodePath,
    style: Style,
    margin: ResolvedEdges,
    main_extras: f32,
    explicit_main: Option<f32>,
    content_main: f32,
    layout: Option<LayoutBox>,
}

struct FlexLayoutInput<'a> {
    parent_opacity: f32,
    element: &'a Element,
    path: NodePath,
    ancestors: Vec<&'a Element>,
    style: Style,
    stylesheet: &'a Stylesheet,
    style_state: &'a StyleState,
    margin: ResolvedEdges,
    padding: ResolvedEdges,
    border: f32,
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
    element: &'a Element,
    path: NodePath,
    style: Style,
    margin: ResolvedEdges,
    layout: Option<LayoutBox>,
}

struct GridLayoutInput<'a> {
    parent_opacity: f32,
    element: &'a Element,
    path: NodePath,
    ancestors: Vec<&'a Element>,
    style: Style,
    stylesheet: &'a Stylesheet,
    style_state: &'a StyleState,
    margin: ResolvedEdges,
    padding: ResolvedEdges,
    border: f32,
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

struct LayoutNode<'a> {
    element: &'a Element,
    path: NodePath,
    ancestors: &'a [&'a Element],
}

struct ElementLayoutInput<'node, 'style> {
    node: LayoutNode<'node>,
    containing_x: f32,
    containing_y: f32,
    containing_width: f32,
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum LayoutMode {
    Flow,
    Absolute,
    Fixed,
}

struct PositionedNode<'a> {
    element: &'a Element,
    path: NodePath,
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
struct PositionedLayoutContext<'a, 'b> {
    containing_block: ContainingBlock,
    viewport: ContainingBlock,
    viewport_size: Size,
    parent: ParentStyle,
    ancestors: &'b [&'a Element],
    stylesheet: &'a Stylesheet,
    style_state: &'a StyleState,
}

pub fn layout_with_state(
    document: &Document,
    stylesheet: &Stylesheet,
    viewport: Size,
    style_state: &StyleState,
) -> LayoutDocument {
    let _viewport_environment = crate::css::use_layout_environment(crate::css::LayoutEnvironment {
        viewport,
        root_font_size: 16.0,
    });
    let mut computed_state = style_state.clone();
    stylesheet.populate_custom_properties(document, &mut computed_state);
    let style_state = &computed_state;
    let _root_environment = crate::css::use_layout_environment(crate::css::LayoutEnvironment {
        viewport,
        root_font_size: root_font_size(document, stylesheet, viewport, style_state),
    });
    let viewport_block = ContainingBlock {
        x: 0.0,
        y: 0.0,
        width: viewport.width,
        height: viewport.height,
    };
    let mut root = LayoutBox {
        path: Vec::new(),
        tag: "#document".to_string(),
        rect: Rect {
            x: 0.0,
            y: 0.0,
            width: viewport.width,
            height: viewport.height,
        },
        background: Color::transparent(),
        gradient: None,
        border_radius: 0.0,
        opacity: 1.0,
        shadow: None,
        border_width: 0.0,
        border_color: Color::transparent(),
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
        scroll_content: Size {
            width: viewport.width,
            height: viewport.height,
        },
        scroll_offset: (0.0, 0.0),
        margin: ResolvedEdges::default(),
    };
    let mut cursor_y = 0.0;
    let mut positioned = Vec::new();
    for (index, child) in document.root.children.iter().enumerate() {
        let Node::Element(element) = child else {
            continue;
        };
        let path = vec![index];
        let style = stylesheet.style_for(StyleContext {
            element,
            path: &path,
            ancestors: &[],
            parent_font_size: 16.0,
            parent_color: Color::BLACK,
            parent_weight: FontWeight::Normal,
            parent_text: InheritedText::default(),
            state: style_state,
        });
        if style.position.is_out_of_flow() {
            positioned.push(PositionedNode { element, path });
            continue;
        }
        if let Some(child_box) = layout_element(ElementLayoutInput {
            node: LayoutNode {
                element,
                path,
                ancestors: &[],
            },
            containing_x: 0.0,
            containing_y: cursor_y,
            containing_width: viewport.width,
            parent: ParentStyle {
                font_size: 16.0,
                color: Color::BLACK,
                weight: FontWeight::Normal,
                text: InheritedText::default(),
                opacity: 1.0,
            },
            context: LayoutContext {
                viewport,
                containing_block: viewport_block,
                mode: LayoutMode::Flow,
                stylesheet,
                style_state,
            },
        }) {
            cursor_y = child_box.rect.bottom() + child_box.margin.bottom;
            root.children.push(child_box);
        }
    }
    append_positioned_nodes(
        &mut root.children,
        positioned,
        PositionedLayoutContext {
            containing_block: viewport_block,
            viewport: viewport_block,
            viewport_size: viewport,
            parent: ParentStyle {
                font_size: 16.0,
                color: Color::BLACK,
                weight: FontWeight::Normal,
                text: InheritedText::default(),
                opacity: 1.0,
            },
            ancestors: &[],
            stylesheet,
            style_state,
        },
    );
    finalize_scroll(&mut root, style_state);
    LayoutDocument {
        viewport,
        root,
        images: Vec::new(),
    }
}

impl LayoutDocument {
    // maps a point inside a text control to a UTF-16 offset in its value
    pub fn control_text_offset_at(&self, x: f32, y: f32) -> Option<(NodePath, usize)> {
        let mut found = None;
        collect_control_at(&self.root, x, y, (0.0, 0.0), &mut found);
        let (path, runs, origin) = found?;
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
        Some((path, base + offset_within_run(&run, x - origin.0)))
    }
}

fn offset_within_run(run: &TextRun, x: f32) -> usize {
    let fonts = crate::font::active_fonts();
    let mut offset = 0;
    let mut pen = run.x;
    for character in run.text.chars() {
        let advance =
            fonts.advance(character, run.font_stack, run.font_size, run.bold) + run.letter_spacing;
        if x < pen + advance / 2.0 {
            return offset;
        }
        pen += advance;
        offset += character.len_utf16();
    }
    offset
}

#[allow(clippy::type_complexity)]
fn collect_control_at(
    layout_box: &LayoutBox,
    x: f32,
    y: f32,
    ancestor_offset: (f32, f32),
    found: &mut Option<(NodePath, Vec<TextRun>, (f32, f32))>,
) {
    let offset = visual_offset_for(layout_box, ancestor_offset);
    let rect = visual_rect_for(layout_box, ancestor_offset);
    let inside = x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height;
    if inside && layout_box.control_value.is_some() && !layout_box.text.is_empty() {
        *found = Some((layout_box.path.clone(), layout_box.text.clone(), offset));
    }
    for child in &layout_box.children {
        collect_control_at(child, x, y, offset, found);
    }
}

impl LayoutDocument {
    pub fn paint(&self) -> crate::paint::Canvas {
        crate::paint::paint(self)
    }

    pub fn describe(&self) -> String {
        let mut lines = Vec::new();
        describe_box(&self.root, 0, &mut lines);
        lines.join("\n")
    }

    pub fn hit_test(&self, x: f32, y: f32) -> Option<NodePath> {
        hit_test_box(&self.root, x, y, Some(self.root.rect), (0.0, 0.0))
    }

    pub(crate) fn scroll_target_at(&self, x: f32, y: f32) -> Option<ScrollTarget> {
        scroll_target_box(&self.root, x, y, Some(self.root.rect), (0.0, 0.0))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ScrollTarget {
    pub(crate) path: NodePath,
    pub(crate) max_x: f32,
    pub(crate) max_y: f32,
    pub(crate) offset: (f32, f32),
}

pub(crate) const SCROLLBAR_THICKNESS: f32 = 12.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ScrollbarMetrics {
    pub(crate) scrollport: Rect,
    pub(crate) vertical_track: Option<Rect>,
    pub(crate) horizontal_track: Option<Rect>,
}

pub(crate) fn scrollbar_metrics(layout: &LayoutBox) -> ScrollbarMetrics {
    let rect = layout.rect;
    let scroll_container = layout.path.is_empty() || layout.overflow == Overflow::Auto;
    if !scroll_container {
        return ScrollbarMetrics {
            scrollport: rect,
            vertical_track: None,
            horizontal_track: None,
        };
    }
    let mut vertical = layout.scroll_content.height > rect.height;
    let mut horizontal = layout.scroll_content.width
        > (rect.width - if vertical { SCROLLBAR_THICKNESS } else { 0.0 }).max(0.0);
    vertical = layout.scroll_content.height
        > (rect.height - if horizontal { SCROLLBAR_THICKNESS } else { 0.0 }).max(0.0);
    horizontal = layout.scroll_content.width
        > (rect.width - if vertical { SCROLLBAR_THICKNESS } else { 0.0 }).max(0.0);
    let vertical_width = if vertical {
        SCROLLBAR_THICKNESS.min(rect.width)
    } else {
        0.0
    };
    let horizontal_height = if horizontal {
        SCROLLBAR_THICKNESS.min(rect.height)
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
        x: scrollport.x + scrollport.width,
        y: scrollport.y,
        width: vertical_width,
        height: scrollport.height,
    });
    let horizontal_track = horizontal.then_some(Rect {
        x: scrollport.x,
        y: scrollport.y + scrollport.height,
        width: scrollport.width,
        height: horizontal_height,
    });
    ScrollbarMetrics {
        scrollport,
        vertical_track,
        horizontal_track,
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
    nodes: Vec<PositionedNode<'_>>,
    context: PositionedLayoutContext<'_, '_>,
) {
    let PositionedLayoutContext {
        containing_block,
        viewport,
        viewport_size,
        parent,
        ancestors,
        stylesheet,
        style_state,
    } = context;
    for node in nodes {
        let style = stylesheet.style_for(StyleContext {
            element: node.element,
            path: &node.path,
            ancestors,
            parent_font_size: parent.font_size,
            parent_color: parent.color,
            parent_weight: parent.weight,
            parent_text: parent.text,
            state: style_state,
        });
        let mode = if style.position == Position::Fixed {
            LayoutMode::Fixed
        } else {
            LayoutMode::Absolute
        };
        if let Some(layout) = layout_element(ElementLayoutInput {
            node: LayoutNode {
                element: node.element,
                path: node.path,
                ancestors,
            },
            containing_x: 0.0,
            containing_y: 0.0,
            containing_width: containing_block.width,
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

fn hit_test_box(
    layout: &LayoutBox,
    x: f32,
    y: f32,
    clip: Option<Rect>,
    ancestor_offset: (f32, f32),
) -> Option<NodePath> {
    hit_test_context(layout, x, y, clip, ancestor_offset)
}

fn hit_test_context(
    layout: &LayoutBox,
    x: f32,
    y: f32,
    clip: Option<Rect>,
    ancestor_offset: (f32, f32),
) -> Option<NodePath> {
    if let Some(clip) = clip
        && !contains_point(clip, x, y)
    {
        return None;
    }
    let visual_offset = visual_offset_for(layout, ancestor_offset);
    let rect = visual_rect_for(layout, ancestor_offset);
    let inside_layout = contains_point(rect, x, y);
    let mut contexts = stacking_context_entries(layout, clip, ancestor_offset);
    contexts.sort_by_key(|entry| (entry.z_index, entry.order));
    for entry in contexts.iter().rev().filter(|entry| entry.z_index >= 0) {
        if let Some(path) = hit_test_context(entry.layout, x, y, entry.clip, entry.ancestor_offset)
        {
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
    (inside_layout && !layout.path.is_empty()).then(|| layout.path.clone())
}

fn hit_test_non_context_children(
    layout: &LayoutBox,
    x: f32,
    y: f32,
    clip: Option<Rect>,
    visual_offset: (f32, f32),
) -> Option<NodePath> {
    let child_clip = stacking_child_clip(layout, clip, visual_offset);
    for index in paint_order(&layout.children).into_iter().rev() {
        let child = &layout.children[index];
        if child.creates_stacking_context {
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
) -> Option<NodePath> {
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
    contains_point(rect, x, y)
        .then(|| (!layout.path.is_empty()).then(|| layout.path.clone()))
        .flatten()
}

pub(crate) struct StackingContextEntry<'a> {
    pub(crate) layout: &'a LayoutBox,
    pub(crate) clip: Option<Rect>,
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
    collect_stacking_contexts(layout, clip, ancestor_offset, &mut order, &mut entries);
    entries
}

fn collect_stacking_contexts<'a>(
    layout: &'a LayoutBox,
    clip: Option<Rect>,
    ancestor_offset: (f32, f32),
    order: &mut usize,
    entries: &mut Vec<StackingContextEntry<'a>>,
) {
    let visual_offset = visual_offset_for(layout, ancestor_offset);
    let child_clip = stacking_child_clip(layout, clip, visual_offset);
    for index in paint_order(&layout.children) {
        let child = &layout.children[index];
        let child_order = *order;
        *order += 1;
        if child.creates_stacking_context {
            entries.push(StackingContextEntry {
                layout: child,
                clip: child_clip,
                ancestor_offset: visual_offset,
                z_index: child.z_index,
                order: child_order,
            });
        } else {
            collect_stacking_contexts(child, child_clip, visual_offset, order, entries);
        }
    }
}

fn stacking_child_clip(
    layout: &LayoutBox,
    clip: Option<Rect>,
    visual_offset: (f32, f32),
) -> Option<Rect> {
    if matches!(layout.overflow, Overflow::Hidden | Overflow::Auto) || layout.path.is_empty() {
        intersect_rect(
            clip,
            translate_rect(scrollbar_metrics(layout).scrollport, visual_offset),
        )
    } else {
        clip
    }
}

pub(crate) fn paint_order(children: &[LayoutBox]) -> Vec<usize> {
    let mut order = (0..children.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| (children[*index].z_index, *index));
    order
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
    let visual_offset = visual_offset_for(layout, ancestor_offset);
    let rect = visual_rect_for(layout, ancestor_offset);
    let inside_layout = contains_point(rect, x, y);
    let child_clip =
        if matches!(layout.overflow, Overflow::Hidden | Overflow::Auto) || layout.path.is_empty() {
            intersect_rect(
                clip,
                translate_rect(scrollbar_metrics(layout).scrollport, visual_offset),
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
    if inside_layout && (layout.overflow == Overflow::Auto || layout.path.is_empty()) {
        let scrollport = scrollbar_metrics(layout).scrollport;
        let max_x = (layout.scroll_content.width - scrollport.width).max(0.0);
        let max_y = (layout.scroll_content.height - scrollport.height).max(0.0);
        if max_x > 0.0 || max_y > 0.0 {
            return Some(ScrollTarget {
                path: layout.path.clone(),
                max_x,
                max_y,
                offset: layout.scroll_offset,
            });
        }
    }
    None
}

fn contains_point(rect: Rect, x: f32, y: f32) -> bool {
    x >= rect.x && y >= rect.y && x < rect.x + rect.width && y < rect.y + rect.height
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

fn layout_element(input: ElementLayoutInput<'_, '_>) -> Option<LayoutBox> {
    let ElementLayoutInput {
        node,
        containing_x,
        containing_y,
        containing_width,
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
    let LayoutNode {
        element,
        path,
        ancestors,
    } = node;
    let style = stylesheet.style_for(StyleContext {
        element,
        path: &path,
        ancestors,
        parent_font_size: parent.font_size,
        parent_color: parent.color,
        parent_weight: parent.weight,
        parent_text: parent.text,
        state: style_state,
    });
    if style.display == Display::None {
        return None;
    }
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
    let margin = resolve_edges(style.margin, containing_width, font_size);
    let padding = resolve_edges(style.padding, containing_width, font_size);
    let border = style.border_width.max(0.0);
    let horizontal_extras =
        margin.left + margin.right + padding.left + padding.right + (border * 2.0);
    let inset = resolve_insets(style.inset, position_block, font_size);
    // an out-of-flow box with both block insets set stretches between them
    let explicit_height = style
        .height
        .resolve(containing_width, font_size)
        .or_else(|| {
            (mode != LayoutMode::Flow && inset.top.is_some() && inset.bottom.is_some()).then(|| {
                (position_block.height
                    - inset.top.unwrap_or(0.0)
                    - inset.bottom.unwrap_or(0.0)
                    - padding.top
                    - padding.bottom
                    - (border * 2.0))
                    .max(0.0)
            })
        });
    let content_width = style
        .width
        .resolve(containing_width, font_size)
        .or_else(|| {
            (mode != LayoutMode::Flow && inset.left.is_some() && inset.right.is_some()).then(|| {
                (position_block.width
                    - inset.left.unwrap_or(0.0)
                    - inset.right.unwrap_or(0.0)
                    - horizontal_extras)
                    .max(0.0)
            })
        })
        .unwrap_or((containing_width - horizontal_extras).max(0.0))
        .max(0.0);
    let border_box_width = content_width + padding.left + padding.right + (border * 2.0);
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
            .map(|height| height.max(0.0) + padding.top + padding.bottom + (border * 2.0))
            .unwrap_or(0.0),
    };
    let descendant_block = if style.position == Position::Static {
        containing_block
    } else {
        local_containing_block
    };
    let content_x = rect.x + border + padding.left;
    let content_y = rect.y + border + padding.top;
    if style.display == Display::Flex {
        return Some(layout_flex_element(FlexLayoutInput {
            parent_opacity: parent.opacity,
            element,
            path,
            ancestors: ancestors.to_vec(),
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
    if style.display == Display::Grid {
        return Some(layout_grid_element(GridLayoutInput {
            parent_opacity: parent.opacity,
            element,
            path,
            ancestors: ancestors.to_vec(),
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
    let line_height = style
        .line_height
        .resolve(font_size, font_size)
        .unwrap_or(font_size * 1.25)
        .max(1.0);
    let mut line = InlineLine {
        trailing_space: false,
        y: content_y,
        used: 0.0,
        height: line_height,
        fragments: Vec::new(),
    };
    let mut text = Vec::new();
    let control_value = text_control_value(element, &path, style_state);
    let control_placeholder = control_value
        .as_deref()
        .filter(|value| value.is_empty())
        .and_then(|_| text_control_placeholder(element));
    let control_display_value = control_value
        .as_ref()
        .map(|value| text_control_display_value(element, value));
    let control_text_origin = control_value.as_ref().map(|_| (content_x, content_y));
    let letter_spacing = style
        .letter_spacing
        .resolve(font_size, font_size)
        .unwrap_or(0.0);
    if let Some(value) = control_placeholder
        .as_ref()
        .or(control_display_value.as_ref())
    {
        let value_lines = control_text_lines(element, value, content_width, font_size);
        let color = if control_placeholder.is_some() {
            Color::opaque(120, 145, 155)
        } else {
            style.color
        };
        for (line_index, value_line) in value_lines.iter().enumerate() {
            text.push(TextRun {
                text: value_line.clone(),
                x: content_x,
                y: content_y + line_index as f32 * line_height,
                width: measure_text(
                    value_line,
                    style.font_family,
                    font_size,
                    style.font_weight == FontWeight::Bold,
                    letter_spacing,
                ),
                font_size,
                letter_spacing,
                font_stack: style.font_family,
                color,
                bold: style.font_weight == FontWeight::Bold,
            });
        }
        line.y = content_y + value_lines.len() as f32 * line_height;
    }
    let mut children = Vec::new();
    let mut positioned = Vec::new();
    let inline_context = InlineContext {
        font_size,
        line_height,
        color: style.color,
        weight: style.font_weight,
        text: inherited_text(&style),
        opacity: style.opacity * parent.opacity,
        content_width,
        content_x,
        align: style.text_align,
    };
    let mut child_ancestors = ancestors.to_vec();
    child_ancestors.push(element);
    let before = generated_child(
        element,
        &path,
        &child_ancestors,
        stylesheet,
        style_state,
        PseudoElement::Before,
    );
    let after = generated_child(
        element,
        &path,
        &child_ancestors,
        stylesheet,
        style_state,
        PseudoElement::After,
    );
    let mut sequence: Vec<(usize, &Node)> = Vec::with_capacity(element.children.len() + 2);
    if let Some(node) = before.as_ref() {
        sequence.push((element.children.len(), node));
    }
    sequence.extend(element.children.iter().enumerate());
    if let Some(node) = after.as_ref() {
        sequence.push((element.children.len() + 1, node));
    }
    for (index, child) in sequence {
        match child {
            Node::Text(value) if element.name != "textarea" => {
                append_inline_text(value, inline_context, &mut text, &mut line)
            }
            Node::Text(_) => {}
            Node::Element(_) if element.name == "textarea" => {}
            Node::Element(child_element) => {
                let mut child_path = path.clone();
                child_path.push(index);
                let child_style = stylesheet.style_for(StyleContext {
                    element: child_element,
                    path: &child_path,
                    ancestors: &child_ancestors,
                    parent_font_size: font_size,
                    parent_color: style.color,
                    parent_weight: style.font_weight,
                    parent_text: inherited_text(&style),
                    state: style_state,
                });
                if child_style.display == Display::None {
                    continue;
                }
                if child_style.position.is_out_of_flow() {
                    positioned.push(PositionedNode {
                        element: child_element,
                        path: child_path,
                    });
                    continue;
                }
                if matches!(
                    child_style.display,
                    Display::Block | Display::Flex | Display::Grid
                ) {
                    flush_line(
                        &mut line,
                        &mut text,
                        content_x,
                        content_width,
                        style.text_align,
                    );
                    if let Some(child_box) = layout_element(ElementLayoutInput {
                        node: LayoutNode {
                            element: child_element,
                            path: child_path,
                            ancestors: &child_ancestors,
                        },
                        containing_x: content_x,
                        containing_y: line.y,
                        containing_width: content_width,
                        parent: ParentStyle {
                            font_size,
                            color: style.color,
                            weight: style.font_weight,
                            text: inherited_text(&style),
                            opacity: style.opacity * parent.opacity,
                        },
                        context: LayoutContext {
                            viewport,
                            containing_block: descendant_block,
                            mode: LayoutMode::Flow,
                            stylesheet,
                            style_state,
                        },
                    }) {
                        line.y = child_box.rect.bottom() + child_box.margin.bottom;
                        line.used = 0.0;
                        line.height = line_height;
                        line.fragments.clear();
                        children.push(child_box);
                    }
                } else {
                    append_inline_element(
                        InlineElementInput {
                            element: child_element,
                            path: &child_path,
                            parent: ParentStyle {
                                font_size,
                                color: style.color,
                                weight: style.font_weight,
                                text: inherited_text(&style),
                                opacity: style.opacity * parent.opacity,
                            },
                            context: inline_context,
                            stylesheet,
                            style_state,
                            ancestors: &child_ancestors,
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
    let content_height = explicit_height
        .unwrap_or((line.y - content_y).max(0.0))
        .max(0.0);
    let border_box_height = content_height + padding.top + padding.bottom + (border * 2.0);
    let rect = Rect {
        height: border_box_height,
        ..rect
    };
    let mut layout = LayoutBox {
        path: path.clone(),
        tag: element.name.clone(),
        rect,
        background: style.background,
        gradient: style.gradient,
        border_radius: style
            .border_radius
            .resolve(content_width, font_size)
            .unwrap_or(0.0)
            .max(0.0),
        opacity: style.opacity * parent.opacity,
        shadow: resolve_shadow(style.shadow, content_width, font_size),
        border_width: border,
        border_color: style.border_color,
        text,
        control_selection: style_state.text_selections.get(&path).copied(),
        control_focused: style_state.focused_path.as_deref() == Some(path.as_slice()),
        control_text_origin,
        control_value,
        children,
        position: style.position,
        visual_offset: (0.0, 0.0),
        z_index: positioned_z_index(style),
        creates_stacking_context: creates_stacking_context(style),
        sticky_inset: style.inset,
        font_size,
        overflow: style.overflow,
        scroll_content: Size::default(),
        scroll_offset: (0.0, 0.0),
        margin,
    };
    if mode != LayoutMode::Flow {
        position_layout_box(&mut layout, style, mode, position_block, font_size);
    } else {
        apply_relative_offset(&mut layout, style, containing_block, font_size);
    }
    let positioned_block = ContainingBlock {
        x: layout.rect.x,
        y: layout.rect.y,
        width: layout.rect.width,
        height: layout.rect.height,
    };
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
            parent: ParentStyle {
                font_size,
                color: style.color,
                weight: style.font_weight,
                text: inherited_text(&style),
                opacity: style.opacity * parent.opacity,
            },
            ancestors: &child_ancestors,
            stylesheet,
            style_state,
        },
    );
    finalize_scroll(&mut layout, style_state);
    Some(layout)
}

fn layout_flex_element(input: FlexLayoutInput<'_>) -> LayoutBox {
    let FlexLayoutInput {
        parent_opacity,
        element,
        path,
        ancestors,
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
        opacity: style.opacity * parent_opacity,
    };
    let mut child_ancestors = ancestors;
    child_ancestors.push(element);
    let generated = [
        generated_child(
            element,
            &path,
            &child_ancestors,
            stylesheet,
            style_state,
            PseudoElement::Before,
        ),
        generated_child(
            element,
            &path,
            &child_ancestors,
            stylesheet,
            style_state,
            PseudoElement::After,
        ),
    ];
    let mut items = Vec::new();
    let mut positioned = Vec::new();
    let owned_sequence: Vec<(usize, Node)> = {
        let mut sequence: Vec<(usize, Node)> = Vec::with_capacity(element.children.len() + 2);
        if let Some(node) = generated[0].as_ref() {
            sequence.push((element.children.len(), node.clone()));
        }
        for (index, node) in element.children.iter().enumerate() {
            sequence.push((index, node.clone()));
        }
        if let Some(node) = generated[1].as_ref() {
            sequence.push((element.children.len() + 1, node.clone()));
        }
        sequence
    };
    for (index, child) in &owned_sequence {
        let Node::Element(child) = child else {
            continue;
        };
        let mut child_path = path.clone();
        child_path.push(*index);
        let child_style = stylesheet.style_for(StyleContext {
            element: child,
            path: &child_path,
            ancestors: &child_ancestors,
            parent_font_size: font_size,
            parent_color: style.color,
            parent_weight: style.font_weight,
            parent_text: inherited_text(&style),
            state: style_state,
        });
        if child_style.display == Display::None {
            continue;
        }
        if child_style.position.is_out_of_flow() {
            positioned.push(PositionedNode {
                element: child,
                path: child_path,
            });
            continue;
        }
        let child_font_size = child_style
            .font_size
            .resolve(font_size, font_size)
            .unwrap_or(font_size)
            .max(1.0);
        let child_margin = resolve_edges(child_style.margin, content_width, child_font_size);
        let child_padding = resolve_edges(child_style.padding, content_width, child_font_size);
        let child_border = child_style.border_width.max(0.0);
        let main_extras = match style.flex_direction {
            FlexDirection::Row => {
                child_margin.left
                    + child_margin.right
                    + child_padding.left
                    + child_padding.right
                    + child_border * 2.0
            }
            FlexDirection::Column => {
                child_margin.top
                    + child_margin.bottom
                    + child_padding.top
                    + child_padding.bottom
                    + child_border * 2.0
            }
        };
        let explicit_main = match style.flex_direction {
            FlexDirection::Row => child_style.width.resolve(content_width, child_font_size),
            FlexDirection::Column => child_style.height.resolve(content_width, child_font_size),
        };
        items.push(FlexItem {
            element: child,
            path: child_path,
            style: child_style,
            margin: child_margin,
            main_extras,
            explicit_main,
            content_main: explicit_main.unwrap_or(0.0),
            layout: None,
        });
    }

    let gap = style
        .gap
        .resolve(content_width, font_size)
        .unwrap_or(0.0)
        .max(0.0);
    if style.flex_direction == FlexDirection::Row {
        let fixed_outer = items
            .iter()
            .filter_map(|item| item.explicit_main.map(|main| main + item.main_extras))
            .sum::<f32>();
        let total_grow = items.iter().map(|item| item.style.flex_grow).sum::<f32>();
        let auto_count = items
            .iter()
            .filter(|item| item.explicit_main.is_none() && item.style.flex_grow == 0.0)
            .count();
        let available =
            (content_width - gap * items.len().saturating_sub(1) as f32 - fixed_outer).max(0.0);
        for item in &mut items {
            let share = if total_grow > 0.0 {
                available * item.style.flex_grow / total_grow
            } else if item.explicit_main.is_none() && auto_count > 0 {
                available / auto_count as f32
            } else {
                0.0
            };
            item.content_main = item.explicit_main.unwrap_or(0.0) + share;
        }
    }

    for item in &mut items {
        let child_width = if style.flex_direction == FlexDirection::Row {
            item.content_main.max(0.0)
        } else {
            content_width
        };
        item.layout = layout_element(ElementLayoutInput {
            node: LayoutNode {
                element: item.element,
                path: item.path.clone(),
                ancestors: &child_ancestors,
            },
            containing_x: 0.0,
            containing_y: 0.0,
            containing_width: child_width,
            parent: child_parent,
            context: LayoutContext {
                viewport,
                containing_block: if style.position == Position::Static {
                    inherited_containing_block
                } else {
                    containing_block
                },
                mode: LayoutMode::Flow,
                stylesheet,
                style_state,
            },
        });
    }

    let mut children = Vec::new();
    let content_height = match style.flex_direction {
        FlexDirection::Row => {
            let used = items
                .iter()
                .filter_map(|item| item.layout.as_ref())
                .map(|child| child.rect.width)
                .sum::<f32>();
            let used = used
                + items
                    .iter()
                    .filter_map(|item| item.layout.as_ref())
                    .zip(items.iter())
                    .map(|(_child, item)| item.margin.left + item.margin.right)
                    .sum::<f32>();
            let max_cross = items
                .iter()
                .filter_map(|item| item.layout.as_ref())
                .zip(items.iter())
                .map(|(child, item)| child.rect.height + item.margin.top + item.margin.bottom)
                .fold(0.0, f32::max);
            let explicit_cross = style.height.resolve(content_width, font_size);
            let cross_size = explicit_cross.unwrap_or(max_cross).max(0.0);
            let free = (content_width - used - gap * items.len().saturating_sub(1) as f32).max(0.0);
            let (mut cursor, gap_extra) = justify_offsets(style.justify_content, free, items.len());
            for item in &mut items {
                let Some(mut child) = item.layout.take() else {
                    continue;
                };
                if style.align_items == AlignItems::Stretch
                    && explicit_cross.is_some()
                    && item
                        .style
                        .height
                        .resolve(content_width, font_size)
                        .is_none()
                {
                    child.rect.height =
                        (cross_size - item.margin.top - item.margin.bottom).max(0.0);
                }
                let outer_cross = child.rect.height + item.margin.top + item.margin.bottom;
                let cross_offset = align_offset(style.align_items, cross_size, outer_cross);
                let desired_x = content_x + cursor + item.margin.left;
                let desired_y = content_y + cross_offset + item.margin.top;
                let offset_x = desired_x - child.rect.x;
                let offset_y = desired_y - child.rect.y;
                translate_layout_box(&mut child, offset_x, offset_y);
                cursor += child.rect.width + item.margin.left + item.margin.right + gap + gap_extra;
                children.push(child);
            }
            cross_size
        }
        FlexDirection::Column => {
            let used = items
                .iter()
                .filter_map(|item| item.layout.as_ref())
                .zip(items.iter())
                .map(|(child, item)| child.rect.height + item.margin.top + item.margin.bottom)
                .sum::<f32>();
            let explicit_main = style.height.resolve(content_width, font_size);
            let main_size = explicit_main
                .unwrap_or(used + gap * items.len().saturating_sub(1) as f32)
                .max(0.0);
            let free = (main_size - used - gap * items.len().saturating_sub(1) as f32).max(0.0);
            let (mut cursor, gap_extra) = justify_offsets(style.justify_content, free, items.len());
            for item in &mut items {
                let Some(mut child) = item.layout.take() else {
                    continue;
                };
                if style.align_items == AlignItems::Stretch
                    && item.style.width.resolve(content_width, font_size).is_none()
                {
                    child.rect.width =
                        (content_width - item.margin.left - item.margin.right).max(0.0);
                }
                let outer_cross = child.rect.width + item.margin.left + item.margin.right;
                let cross_offset = align_offset(style.align_items, content_width, outer_cross);
                let desired_x = content_x + cross_offset + item.margin.left;
                let desired_y = content_y + cursor + item.margin.top;
                let offset_x = desired_x - child.rect.x;
                let offset_y = desired_y - child.rect.y;
                translate_layout_box(&mut child, offset_x, offset_y);
                cursor +=
                    child.rect.height + item.margin.top + item.margin.bottom + gap + gap_extra;
                children.push(child);
            }
            main_size
        }
    };
    let mut layout = LayoutBox {
        path,
        tag: element.name.clone(),
        rect: Rect {
            height: content_height + padding.top + padding.bottom + (border * 2.0),
            ..rect
        },
        background: style.background,
        gradient: style.gradient,
        border_radius: style
            .border_radius
            .resolve(content_width, font_size)
            .unwrap_or(0.0)
            .max(0.0),
        opacity: style.opacity * parent_opacity,
        shadow: resolve_shadow(style.shadow, content_width, font_size),
        border_width: border,
        border_color: style.border_color,
        text: Vec::new(),
        control_value: None,
        control_selection: None,
        control_focused: false,
        control_text_origin: None,
        children,
        position: style.position,
        visual_offset: (0.0, 0.0),
        z_index: positioned_z_index(style),
        creates_stacking_context: creates_stacking_context(style),
        sticky_inset: style.inset,
        font_size,
        overflow: style.overflow,
        scroll_content: Size::default(),
        scroll_offset: (0.0, 0.0),
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
    } else {
        apply_relative_offset(&mut layout, style, inherited_containing_block, font_size);
    }
    let positioned_block = ContainingBlock {
        x: layout.rect.x,
        y: layout.rect.y,
        width: layout.rect.width,
        height: layout.rect.height,
    };
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
            ancestors: &child_ancestors,
            stylesheet,
            style_state,
        },
    );
    finalize_scroll(&mut layout, style_state);
    layout
}

fn layout_grid_element(input: GridLayoutInput<'_>) -> LayoutBox {
    let GridLayoutInput {
        parent_opacity,
        element,
        path,
        ancestors,
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
        opacity: style.opacity * parent_opacity,
    };
    let mut child_ancestors = ancestors;
    child_ancestors.push(element);
    let generated = [
        generated_child(
            element,
            &path,
            &child_ancestors,
            stylesheet,
            style_state,
            PseudoElement::Before,
        ),
        generated_child(
            element,
            &path,
            &child_ancestors,
            stylesheet,
            style_state,
            PseudoElement::After,
        ),
    ];
    let mut items = Vec::new();
    let mut positioned = Vec::new();
    let mut owned_sequence: Vec<(usize, Node)> = Vec::with_capacity(element.children.len() + 2);
    if let Some(node) = generated[0].as_ref() {
        owned_sequence.push((element.children.len(), node.clone()));
    }
    for (index, node) in element.children.iter().enumerate() {
        owned_sequence.push((index, node.clone()));
    }
    if let Some(node) = generated[1].as_ref() {
        owned_sequence.push((element.children.len() + 1, node.clone()));
    }
    for (index, child) in &owned_sequence {
        let Node::Element(child) = child else {
            continue;
        };
        let mut child_path = path.clone();
        child_path.push(*index);
        let child_style = stylesheet.style_for(StyleContext {
            element: child,
            path: &child_path,
            ancestors: &child_ancestors,
            parent_font_size: font_size,
            parent_color: style.color,
            parent_weight: style.font_weight,
            parent_text: inherited_text(&style),
            state: style_state,
        });
        if child_style.display == Display::None {
            continue;
        }
        if child_style.position.is_out_of_flow() {
            positioned.push(PositionedNode {
                element: child,
                path: child_path,
            });
            continue;
        }
        let child_font_size = child_style
            .font_size
            .resolve(font_size, font_size)
            .unwrap_or(font_size)
            .max(1.0);
        let child_margin = resolve_edges(child_style.margin, content_width, child_font_size);
        items.push(GridItem {
            element: child,
            path: child_path,
            style: child_style,
            margin: child_margin,
            layout: None,
        });
    }
    let column_count = style.grid_template_columns.count.max(1);
    let row_count = style
        .grid_template_rows
        .count
        .max(items.len().div_ceil(column_count));
    let gap = style
        .gap
        .resolve(content_width, font_size)
        .unwrap_or(0.0)
        .max(0.0);
    let mut intrinsic_columns = vec![0.0_f32; column_count];
    for (index, item) in items.iter().enumerate() {
        let column = index % column_count;
        if !matches!(
            style.grid_template_columns.tracks.get(column),
            Some(GridTrack::Auto)
        ) {
            continue;
        }
        let width = max_content_width(MaxContentInput {
            element: item.element,
            path: &item.path,
            ancestors: &child_ancestors,
            stylesheet,
            style_state,
            parent: child_parent,
            containing_width: content_width,
        });
        intrinsic_columns[column] = intrinsic_columns[column].max(width);
    }
    let column_widths = resolve_grid_tracks(
        style.grid_template_columns,
        column_count,
        content_width,
        gap,
        &intrinsic_columns,
    );
    for (index, item) in items.iter_mut().enumerate() {
        let column = index % column_count;
        let column_width = column_widths[column];
        item.layout = layout_element(ElementLayoutInput {
            node: LayoutNode {
                element: item.element,
                path: item.path.clone(),
                ancestors: &child_ancestors,
            },
            containing_x: 0.0,
            containing_y: 0.0,
            containing_width: column_width.max(0.0),
            parent: child_parent,
            context: LayoutContext {
                viewport,
                containing_block: if style.position == Position::Static {
                    inherited_containing_block
                } else {
                    containing_block
                },
                mode: LayoutMode::Flow,
                stylesheet,
                style_state,
            },
        });
    }
    let mut intrinsic_rows = vec![0.0_f32; row_count];
    for (index, item) in items.iter().enumerate() {
        let row = index / column_count;
        if let Some(child) = item.layout.as_ref() {
            intrinsic_rows[row] =
                intrinsic_rows[row].max(child.rect.height + item.margin.top + item.margin.bottom);
        }
    }
    let explicit_height = style.height.resolve(content_width, font_size);
    let row_available = explicit_height.unwrap_or_else(|| {
        intrinsic_rows.iter().sum::<f32>() + gap * row_count.saturating_sub(1) as f32
    });
    let row_heights = resolve_grid_tracks(
        style.grid_template_rows,
        row_count,
        row_available,
        gap,
        &intrinsic_rows,
    );
    let mut column_offsets = Vec::with_capacity(column_count);
    let mut offset = 0.0;
    for width in &column_widths {
        column_offsets.push(offset);
        offset += *width + gap;
    }
    let mut row_offsets = Vec::with_capacity(row_count);
    offset = 0.0;
    for height in &row_heights {
        row_offsets.push(offset);
        offset += *height + gap;
    }
    let mut children = Vec::new();
    for (index, item) in items.iter_mut().enumerate() {
        let Some(mut child) = item.layout.take() else {
            continue;
        };
        let column = index % column_count;
        let row = index / column_count;
        if item.style.width == crate::css::Length::Auto {
            child.rect.width =
                (column_widths[column] - item.margin.left - item.margin.right).max(0.0);
        }
        if explicit_height.is_some() && item.style.height == crate::css::Length::Auto {
            child.rect.height = (row_heights[row] - item.margin.top - item.margin.bottom).max(0.0);
        }
        let desired_x = content_x + column_offsets[column] + item.margin.left;
        let desired_y = content_y + row_offsets[row] + item.margin.top;
        let offset_x = desired_x - child.rect.x;
        let offset_y = desired_y - child.rect.y;
        translate_layout_box(&mut child, offset_x, offset_y);
        children.push(child);
    }
    let content_height = explicit_height.unwrap_or_else(|| {
        row_heights.iter().sum::<f32>() + gap * row_count.saturating_sub(1) as f32
    });
    let mut layout = LayoutBox {
        path,
        tag: element.name.clone(),
        rect: Rect {
            height: content_height + padding.top + padding.bottom + border * 2.0,
            ..rect
        },
        background: style.background,
        gradient: style.gradient,
        border_radius: style
            .border_radius
            .resolve(content_width, font_size)
            .unwrap_or(0.0)
            .max(0.0),
        opacity: style.opacity * parent_opacity,
        shadow: resolve_shadow(style.shadow, content_width, font_size),
        border_width: border,
        border_color: style.border_color,
        text: Vec::new(),
        control_value: None,
        control_selection: None,
        control_focused: false,
        control_text_origin: None,
        children,
        position: style.position,
        visual_offset: (0.0, 0.0),
        z_index: positioned_z_index(style),
        creates_stacking_context: creates_stacking_context(style),
        sticky_inset: style.inset,
        font_size,
        overflow: style.overflow,
        scroll_content: Size::default(),
        scroll_offset: (0.0, 0.0),
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
    } else {
        apply_relative_offset(&mut layout, style, inherited_containing_block, font_size);
    }
    let positioned_block = ContainingBlock {
        x: layout.rect.x,
        y: layout.rect.y,
        width: layout.rect.width,
        height: layout.rect.height,
    };
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
            ancestors: &child_ancestors,
            stylesheet,
            style_state,
        },
    );
    finalize_scroll(&mut layout, style_state);
    layout
}

struct MaxContentInput<'a> {
    element: &'a Element,
    path: &'a [usize],
    ancestors: &'a [&'a Element],
    stylesheet: &'a Stylesheet,
    style_state: &'a StyleState,
    parent: ParentStyle,
    containing_width: f32,
}

// the widest the element can be before wrapping, used for auto grid tracks
fn max_content_width(input: MaxContentInput<'_>) -> f32 {
    let MaxContentInput {
        element,
        path,
        ancestors,
        stylesheet,
        style_state,
        parent,
        containing_width,
    } = input;
    let style = stylesheet.style_for(StyleContext {
        element,
        path,
        ancestors,
        parent_font_size: parent.font_size,
        parent_color: parent.color,
        parent_weight: parent.weight,
        parent_text: parent.text,
        state: style_state,
    });
    if style.display == Display::None {
        return 0.0;
    }
    let font_size = style
        .font_size
        .resolve(parent.font_size, parent.font_size)
        .unwrap_or(parent.font_size)
        .max(1.0);
    let margin = resolve_edges(style.margin, containing_width, font_size);
    let padding = resolve_edges(style.padding, containing_width, font_size);
    let extras = margin.left
        + margin.right
        + padding.left
        + padding.right
        + style.border_width.max(0.0) * 2.0;
    if let Some(width) = style.width.resolve(containing_width, font_size) {
        return extras + width.max(0.0);
    }
    let letter_spacing = style
        .letter_spacing
        .resolve(font_size, font_size)
        .unwrap_or(0.0);
    let bold = style.font_weight == FontWeight::Bold;
    let child_parent = ParentStyle {
        font_size,
        color: style.color,
        weight: style.font_weight,
        text: inherited_text(&style),
        opacity: style.opacity * parent.opacity,
    };
    let mut child_ancestors = ancestors.to_vec();
    child_ancestors.push(element);
    let mut inline = 0.0_f32;
    let mut widest_block = 0.0_f32;
    for (index, child) in element.children.iter().enumerate() {
        match child {
            Node::Text(value) => {
                let transformed = style.text_transform.apply(value);
                inline += measure_text(
                    transformed.trim(),
                    style.font_family,
                    font_size,
                    bold,
                    letter_spacing,
                );
            }
            Node::Element(child) => {
                let mut child_path = path.to_vec();
                child_path.push(index);
                let width = max_content_width(MaxContentInput {
                    element: child,
                    path: &child_path,
                    ancestors: &child_ancestors,
                    stylesheet,
                    style_state,
                    parent: child_parent,
                    containing_width,
                });
                let child_display = stylesheet
                    .style_for(StyleContext {
                        element: child,
                        path: &child_path,
                        ancestors: &child_ancestors,
                        parent_font_size: font_size,
                        parent_color: style.color,
                        parent_weight: style.font_weight,
                        parent_text: inherited_text(&style),
                        state: style_state,
                    })
                    .display;
                if child_display == Display::Inline {
                    inline += width;
                } else {
                    widest_block = widest_block.max(width);
                }
            }
        }
    }
    extras + inline.max(widest_block).min(containing_width.max(0.0))
}

fn resolve_grid_tracks(
    template: GridTemplate,
    count: usize,
    available: f32,
    gap: f32,
    intrinsic: &[f32],
) -> Vec<f32> {
    let tracks = (0..count)
        .map(|index| {
            if index < template.count {
                template.tracks[index]
            } else {
                GridTrack::Auto
            }
        })
        .collect::<Vec<_>>();
    // auto tracks take their content size first, then the remaining space feeds the fr tracks
    let mut sizes = tracks
        .iter()
        .enumerate()
        .map(|(index, track)| match track {
            GridTrack::Px(value) => value.max(0.0),
            GridTrack::Percent(value) => (available * value / 100.0).max(0.0),
            GridTrack::Auto => intrinsic.get(index).copied().unwrap_or(0.0).max(0.0),
            GridTrack::Fr(_) => 0.0,
        })
        .collect::<Vec<_>>();
    let fr_total = tracks
        .iter()
        .map(|track| match track {
            GridTrack::Fr(value) => value.max(0.0),
            _ => 0.0,
        })
        .sum::<f32>();
    let reserved = sizes.iter().sum::<f32>() + gap * count.saturating_sub(1) as f32;
    let free = (available - reserved).max(0.0);
    if fr_total > 0.0 {
        for (index, track) in tracks.iter().enumerate() {
            if let GridTrack::Fr(value) = track {
                sizes[index] = free * value.max(0.0) / fr_total;
            }
        }
        return sizes;
    }
    let stretch: Vec<usize> = tracks
        .iter()
        .enumerate()
        .filter(|(index, track)| {
            matches!(track, GridTrack::Auto) && sizes.get(*index).copied().unwrap_or(0.0) <= 0.0
        })
        .map(|(index, _)| index)
        .collect();
    if !stretch.is_empty() {
        let share = free / stretch.len() as f32;
        for index in stretch {
            sizes[index] = share;
        }
    }
    sizes
}

fn justify_offsets(justify: JustifyContent, free: f32, count: usize) -> (f32, f32) {
    if count == 0 {
        return (0.0, 0.0);
    }
    match justify {
        JustifyContent::Center => (free / 2.0, 0.0),
        JustifyContent::FlexEnd => (free, 0.0),
        JustifyContent::SpaceBetween if count > 1 => (0.0, free / (count - 1) as f32),
        _ => (0.0, 0.0),
    }
}

fn align_offset(align: AlignItems, available: f32, item: f32) -> f32 {
    match align {
        AlignItems::Center => ((available - item) / 2.0).max(0.0),
        AlignItems::FlexEnd => (available - item).max(0.0),
        AlignItems::FlexStart | AlignItems::Stretch => 0.0,
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
    matches!(style.position, Position::Fixed | Position::Sticky)
        || (style.position != Position::Static && style.z_index.is_some())
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

fn finalize_scroll(layout: &mut LayoutBox, style_state: &StyleState) {
    layout.scroll_content = measure_scroll_content(layout);
    let is_document = layout.path.is_empty();
    if layout.overflow != Overflow::Auto && !is_document {
        return;
    }
    let scrollport = scrollbar_metrics(layout).scrollport;
    let max_x = (layout.scroll_content.width - scrollport.width).max(0.0);
    let max_y = (layout.scroll_content.height - scrollport.height).max(0.0);
    let requested = style_state
        .scroll_offsets
        .get(&layout.path)
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

fn measure_scroll_content(layout: &LayoutBox) -> Size {
    let mut right = layout.rect.x + layout.rect.width;
    let mut bottom = layout.rect.y + layout.rect.height;
    for child in &layout.children {
        if child.position == Position::Fixed {
            continue;
        }
        right =
            right.max(child.rect.x + child.visual_offset.0 + child.rect.width + child.margin.right);
        bottom = bottom
            .max(child.rect.y + child.visual_offset.1 + child.rect.height + child.margin.bottom);
    }
    Size {
        width: (right - layout.rect.x).max(layout.rect.width),
        height: (bottom - layout.rect.y).max(layout.rect.height),
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
        element,
        path,
        parent,
        context,
        stylesheet,
        style_state,
        ancestors,
    } = input;
    if element.name == "br" {
        if line.fragments.is_empty() {
            line.y += context.line_height;
            line.height = 0.0;
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
    let style = stylesheet.style_for(StyleContext {
        element,
        path,
        ancestors,
        parent_font_size: parent.font_size,
        parent_color: parent.color,
        parent_weight: parent.weight,
        parent_text: parent.text,
        state: style_state,
    });
    if style.display == Display::None {
        return;
    }
    let font_size = style
        .font_size
        .resolve(parent.font_size, parent.font_size)
        .unwrap_or(parent.font_size)
        .max(1.0);
    let line_height = style
        .line_height
        .resolve(font_size, font_size)
        .unwrap_or(font_size * 1.25)
        .max(1.0);
    let child_context = InlineContext {
        font_size,
        line_height,
        color: style.color,
        weight: style.font_weight,
        text: inherited_text(&style),
        opacity: style.opacity * parent.opacity,
        content_width: context.content_width,
        content_x: context.content_x,
        align: context.align,
    };
    let mut child_ancestors = ancestors.to_vec();
    child_ancestors.push(element);
    for (index, child) in element.children.iter().enumerate() {
        match child {
            Node::Text(value) => append_inline_text(value, child_context, text, line),
            Node::Element(child) => {
                let mut child_path = path.to_vec();
                child_path.push(index);
                append_inline_element(
                    InlineElementInput {
                        element: child,
                        path: &child_path,
                        parent: ParentStyle {
                            font_size,
                            color: style.color,
                            weight: style.font_weight,
                            text: inherited_text(&style),
                            opacity: style.opacity * parent.opacity,
                        },
                        context: child_context,
                        stylesheet,
                        style_state,
                        ancestors: &child_ancestors,
                    },
                    text,
                    line,
                )
            }
        }
    }
}

fn append_inline_text(
    value: &str,
    context: InlineContext,
    text: &mut Vec<TextRun>,
    line: &mut InlineLine,
) {
    let transformed = context.text.transform.apply(value);
    let letter_spacing = context
        .text
        .letter_spacing
        .resolve(context.font_size, context.font_size)
        .unwrap_or(0.0);
    let leading_space = transformed.chars().next().is_some_and(char::is_whitespace);
    let trailing_space = transformed
        .chars()
        .next_back()
        .is_some_and(char::is_whitespace);
    let mut first_word = true;
    for word in transformed.split_ascii_whitespace() {
        let bold = context.weight == FontWeight::Bold;
        let word_width = measure_text(
            word,
            context.text.family,
            context.font_size,
            bold,
            letter_spacing,
        );
        let joins_previous = !line.fragments.is_empty() && (line.trailing_space || leading_space);
        let space_width = if line.fragments.is_empty() || (first_word && !joins_previous) {
            0.0
        } else {
            measure_text(
                " ",
                context.text.family,
                context.font_size,
                bold,
                letter_spacing,
            )
        };
        if !line.fragments.is_empty()
            && line.used + space_width + word_width > context.content_width
        {
            flush_line(
                line,
                text,
                context.content_x,
                context.content_width,
                context.align,
            );
        }
        let prefix = if space_width > 0.0 { " " } else { "" };
        let text = format!("{prefix}{word}");
        let width = measure_text(
            &text,
            context.text.family,
            context.font_size,
            bold,
            letter_spacing,
        );
        line.used += width;
        line.height = line.height.max(context.line_height);
        line.fragments.push(InlineFragment {
            text,
            width,
            font_size: context.font_size,
            letter_spacing,
            font_stack: context.text.family,
            color: context.color.scale_alpha(context.opacity),
            bold,
        });
        first_word = false;
    }
    if !transformed.trim().is_empty() || trailing_space {
        line.trailing_space = trailing_space;
    }
}

fn flush_line(
    line: &mut InlineLine,
    text: &mut Vec<TextRun>,
    content_x: f32,
    content_width: f32,
    align: TextAlign,
) {
    if line.fragments.is_empty() {
        return;
    }
    let offset = match align {
        TextAlign::Left => 0.0,
        TextAlign::Center => ((content_width - line.used) / 2.0).max(0.0),
        TextAlign::Right => (content_width - line.used).max(0.0),
    };
    let mut x = content_x + offset;
    for fragment in line.fragments.drain(..) {
        text.push(TextRun {
            text: fragment.text,
            x,
            y: line.y,
            width: fragment.width,
            font_size: fragment.font_size,
            letter_spacing: fragment.letter_spacing,
            font_stack: fragment.font_stack,
            color: fragment.color,
            bold: fragment.bold,
        });
        x += fragment.width;
    }
    line.y += line.height.max(1.0);
    line.used = 0.0;
    line.height = 0.0;
}

fn resolve_edges(edges: Edges, containing_width: f32, font_size: f32) -> ResolvedEdges {
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

// rem resolves against the root element font size, which itself may only use absolute units
fn root_font_size(
    document: &Document,
    stylesheet: &Stylesheet,
    viewport: Size,
    style_state: &StyleState,
) -> f32 {
    const INITIAL_FONT_SIZE: f32 = 16.0;
    let Some(path) = document.document_element_path() else {
        return INITIAL_FONT_SIZE;
    };
    let Some(element) = document.element_at(&path) else {
        return INITIAL_FONT_SIZE;
    };
    stylesheet
        .style_for(StyleContext {
            element,
            path: &path,
            ancestors: &[],
            parent_font_size: INITIAL_FONT_SIZE,
            parent_color: Color::BLACK,
            parent_weight: FontWeight::Normal,
            parent_text: InheritedText::default(),
            state: style_state,
        })
        .font_size
        .resolve(viewport.width, INITIAL_FONT_SIZE)
        .filter(|size| size.is_finite() && *size > 0.0)
        .unwrap_or(INITIAL_FONT_SIZE)
}

fn measure_text(
    text: &str,
    stack: FontStack,
    font_size: f32,
    bold: bool,
    letter_spacing: f32,
) -> f32 {
    crate::font::active_fonts().measure(text, stack, font_size, bold)
        + letter_spacing * text.chars().count() as f32
}

fn text_control_value(
    element: &Element,
    path: &[usize],
    style_state: &StyleState,
) -> Option<String> {
    if element.name == "textarea" {
        return Some(
            style_state
                .control_values
                .get(path)
                .cloned()
                .unwrap_or_else(|| element_text_value(element)),
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
            .get(path)
            .cloned()
            .or_else(|| element.attr("value").map(str::to_string))
            .unwrap_or_default(),
    )
}

fn element_text_value(element: &Element) -> String {
    let mut value = String::new();
    for child in &element.children {
        match child {
            Node::Text(text) => value.push_str(text),
            Node::Element(child) => value.push_str(&element_text_value(child)),
        }
    }
    value
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
    element: &Element,
    value: &str,
    content_width: f32,
    font_size: f32,
) -> Vec<String> {
    let max_characters = (content_width
        / measure_text("X", FontStack::default(), font_size, false, 0.0).max(1.0))
    .floor()
    .max(1.0) as usize;
    let mut lines = Vec::new();
    for value_line in value.split('\n') {
        if element.name != "textarea"
            || element
                .attr("wrap")
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

    #[test]
    fn inherited_text_transform_and_tracking_reach_layout_runs() {
        let document = Document::parse(
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
        let document = Document::parse(
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
            Document::parse("<html><body><div id='card'><p>inside</p></div></body></html>")
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
            layout.hit_test(20.0, 10.0).as_deref(),
            Some([0, 0, 0, 0].as_slice())
        );
        assert_eq!(
            layout.hit_test(5.0, 30.0).as_deref(),
            Some([0, 0].as_slice()),
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
        let document = Document::parse(
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

        let generated = find_box(&layout.root, "::before").expect("a generated box should exist");
        assert_eq!(
            (generated.rect.width, generated.rect.height),
            (4.0, 40.0),
            "an absolute generated box stretches between its insets"
        );
        assert_eq!(generated.background, Color::opaque(0, 255, 0));
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
        let document = Document::parse(
            "<style>main { margin: 10px; padding: 4px; border: 2px solid #000; } p { height: 20px; }</style><main><p>hello</p><p>world</p></main>",
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
        let main = &result.root.children[0];
        assert_eq!(main.tag, "main");
        assert_eq!(main.rect.x, 10.0);
        assert_eq!(main.rect.width, 180.0);
        assert_eq!(main.children.len(), 2);
        assert!(main.children[1].rect.y > main.children[0].rect.bottom());
    }

    #[test]
    fn wraps_text_when_the_line_is_full() {
        let document = Document::parse("<main>one two three four five six</main>")
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
        let document = Document::parse(
            r#"<input id="name" value="initial" style="display: block; width: 120px; height: 28px">"#,
        )
        .expect("input document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let mut state = StyleState {
            focused_path: Some(vec![0]),
            ..Default::default()
        };
        state.control_values.insert(vec![0], "live".to_string());
        state.text_selections.insert(vec![0], (1, 3, true));
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
        let document = Document::parse(
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
        assert_eq!(input.text[0].color, Color::opaque(120, 145, 155));
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
        state.control_values.insert(vec![0], "live".to_string());
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
    fn password_controls_mask_display_text_without_changing_live_value() {
        let document = Document::parse(
            r#"<input id="secret" type="password" value="s😀cret" style="display: block; width: 120px; height: 28px">"#,
        )
        .expect("password document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let mut state = StyleState {
            focused_path: Some(vec![0]),
            ..Default::default()
        };
        state.text_selections.insert(vec![0], (1, 3, false));
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
        let document = Document::parse(
            "<textarea id='note' style='display: block; width: 120px; height: 54px'>initial\ntext</textarea>",
        )
        .expect("textarea document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let mut state = StyleState {
            focused_path: Some(vec![0]),
            ..Default::default()
        };
        state
            .control_values
            .insert(vec![0], "live\nvalue".to_string());
        state.text_selections.insert(vec![0], (1, 7, false));
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
        let document = Document::parse(
            "<textarea id='wrapped' style='display: block; width: 24px; height: 80px'>old</textarea><textarea id='off' wrap='off' style='display: block; width: 24px; height: 80px'>old</textarea>",
        )
        .expect("textarea document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let mut state = StyleState::default();
        state.control_values.insert(vec![0], "abcdef".to_string());
        state.control_values.insert(vec![1], "abcdef".to_string());
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
        let document = Document::parse(
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
        let document = Document::parse(
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
        let document = Document::parse(
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
        let document = Document::parse(
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
        let document = Document::parse(
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
    fn flex_and_grid_children_include_generated_content_boxes() {
        let document = Document::parse(
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
        let document = Document::parse(
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
        assert_eq!(flow.path, vec![1, 1]);
        assert_eq!(flow.rect.y, 0.0);
        assert_eq!(overlay.path, vec![1, 0]);
        assert_eq!(overlay.rect.x, 30.0);
        assert_eq!(overlay.rect.y, 20.0);
        assert_eq!(overlay.rect.width, 40.0);
        assert_eq!(overlay.rect.height, 20.0);
    }

    #[test]
    fn fixed_children_anchor_to_the_viewport() {
        let document = Document::parse(
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
        let document = Document::parse(
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
        assert_eq!(overlay.path, vec![1, 0, 0]);
        assert_eq!(overlay.rect.y, 80.0);
    }

    #[test]
    fn hidden_overflow_clips_paint_and_hit_testing() {
        let document = Document::parse(
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
        assert_eq!(result.hit_test(10.0, 10.0), Some(vec![1, 0]));
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
    fn auto_overflow_scrolls_paint_and_hit_testing_with_bounded_offsets() {
        let document = Document::parse(
            "<style>#viewport { width: 40px; height: 20px; overflow: auto; margin: 0; } #content { width: 80px; height: 60px; margin: 0; background: #ff0000; }</style><div id='viewport'><div id='content'></div></div>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let mut state = StyleState::default();
        state.scroll_offsets.insert(vec![1], (20.0, 30.0));
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
        assert_eq!(result.hit_test(10.0, 4.0), Some(vec![1, 0]));
        assert_eq!(result.hit_test(50.0, 4.0), None);
        assert_eq!(result.hit_test(10.0, 15.0), Some(vec![1]));
        let target = result
            .scroll_target_at(10.0, 10.0)
            .expect("point should resolve to the scroll container");
        assert_eq!(target.path, vec![1]);
        assert_eq!(target.max_x, 52.0);
        assert_eq!(target.max_y, 52.0);
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
    fn scrollbars_reserve_the_scrollport_and_paint_overflow_tracks() {
        let document = Document::parse(
            "<style>#viewport { width: 40px; height: 40px; overflow: auto; margin: 0; } #content { width: 80px; height: 60px; margin: 0; background: #ff0000; }</style><div id='viewport'><div id='content'></div></div>",
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
        let viewport = &result.root.children[0];
        let metrics = scrollbar_metrics(viewport);
        assert_eq!(metrics.scrollport.width, 28.0);
        assert_eq!(metrics.scrollport.height, 28.0);
        assert_eq!(metrics.vertical_track.unwrap().width, SCROLLBAR_THICKNESS);
        assert_eq!(
            metrics.horizontal_track.unwrap().height,
            SCROLLBAR_THICKNESS
        );
        let canvas = result.paint();
        let content = ((4 * canvas.width() + 10) * 4) as usize;
        let vertical_thumb = ((4 * canvas.width() + 35) * 4) as usize;
        let horizontal_thumb = ((34 * canvas.width() + 4) * 4) as usize;
        let vertical_track = ((20 * canvas.width() + 35) * 4) as usize;
        let horizontal_track = ((34 * canvas.width() + 20) * 4) as usize;
        assert_eq!(
            &canvas.rgba_pixels()[content..content + 4],
            [255, 0, 0, 255]
        );
        assert_eq!(
            &canvas.rgba_pixels()[vertical_thumb..vertical_thumb + 4],
            [126, 138, 150, 255]
        );
        assert_eq!(
            &canvas.rgba_pixels()[horizontal_thumb..horizontal_thumb + 4],
            [126, 138, 150, 255]
        );
        assert_eq!(
            &canvas.rgba_pixels()[vertical_track..vertical_track + 4],
            [232, 236, 240, 255]
        );
        assert_eq!(
            &canvas.rgba_pixels()[horizontal_track..horizontal_track + 4],
            [232, 236, 240, 255]
        );
    }

    #[test]
    fn auto_overflow_keeps_fixed_descendants_at_the_viewport() {
        let document = Document::parse(
            "<style>#viewport { width: 40px; height: 20px; overflow: auto; margin: 0; } #content { width: 40px; height: 80px; margin: 0; } #fixed { position: fixed; left: 70px; top: 12px; width: 8px; height: 8px; margin: 0; }</style><div id='viewport'><div id='content'></div><div id='fixed'></div></div>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let mut state = StyleState::default();
        state.scroll_offsets.insert(vec![1], (0.0, 30.0));
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
        let document = Document::parse(
            "<style>#viewport { width: 80px; height: 40px; overflow: auto; margin: 0; } #sticky { position: sticky; top: 0; width: 80px; height: 10px; margin: 0; background: #00ff00; } #spacer { width: 80px; height: 80px; margin: 0; background: #ff0000; }</style><div id='viewport'><div id='sticky'></div><div id='spacer'></div></div>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let mut state = StyleState::default();
        state.scroll_offsets.insert(vec![1], (0.0, 20.0));
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
        assert_eq!(result.hit_test(10.0, 4.0), Some(sticky.path.clone()));
        let canvas = result.paint();
        let pixel = &canvas.rgba_pixels()[(4 * canvas.width() as usize + 10) * 4..];
        assert_eq!(&pixel[..4], &[0, 255, 0, 255]);
    }

    #[test]
    fn layout_resolves_inherited_custom_properties_before_paint() {
        let document = Document::parse(
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
    fn sticky_children_hold_to_the_viewport_during_document_scroll() {
        let document = Document::parse(
            "<style>#sticky { position: sticky; top: 0; width: 120px; height: 10px; margin: 0; background: #00ff00; } #spacer { width: 120px; height: 160px; margin: 0; background: #ff0000; }</style><div id='sticky'></div><div id='spacer'></div>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let mut state = StyleState::default();
        state.scroll_offsets.insert(Vec::new(), (0.0, 20.0));
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
        assert_eq!(result.hit_test(10.0, 4.0), Some(sticky.path.clone()));
        let canvas = result.paint();
        let pixel = &canvas.rgba_pixels()[(4 * canvas.width() as usize + 10) * 4..];
        assert_eq!(&pixel[..4], &[0, 255, 0, 255]);
    }

    #[test]
    fn document_scroll_moves_flow_content_and_keeps_fixed_chrome_in_place() {
        let document = Document::parse(
            "<style>#content { width: 60px; height: 160px; margin: 0; background: #ff0000; } #fixed { position: fixed; left: 70px; top: 12px; width: 8px; height: 8px; margin: 0; background: #0000ff; }</style><div id='content'></div><div id='fixed'></div>",
        )
        .expect("document should parse");
        let stylesheet = Stylesheet::parse(&document.style_text()).expect("css should parse");
        let mut state = StyleState::default();
        state.scroll_offsets.insert(Vec::new(), (0.0, 30.0));
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
        assert_eq!(metrics.scrollport.height, 88.0);
        assert_eq!(metrics.vertical_track.unwrap().width, SCROLLBAR_THICKNESS);
        let target = result
            .scroll_target_at(10.0, 10.0)
            .expect("viewport should expose the document scroll target");
        assert_eq!(target.path, Vec::<usize>::new());
        assert_eq!(target.max_y, 72.0);
    }

    #[test]
    fn relative_offsets_move_visual_boxes_without_changing_flow() {
        let document = Document::parse(
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
        assert_eq!(result.hit_test(10.0, 5.0), Some(first.path.clone()));
        assert_eq!(result.hit_test(0.0, 0.0), None);
    }

    #[test]
    fn z_index_orders_overlapping_paint_and_hit_testing() {
        let document = Document::parse(
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
        assert_eq!(result.hit_test(10.0, 10.0), Some(high.path.clone()));
        let canvas = result.paint();
        let pixel = &canvas.rgba_pixels()[(10 * canvas.width() as usize + 10) * 4..];
        assert_eq!(&pixel[..4], &[0, 0, 255, 255]);
    }

    #[test]
    fn nested_stacking_contexts_paint_and_hit_test_as_atomic_layers() {
        let document = Document::parse(
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
        assert_eq!(result.hit_test(10.0, 10.0), Some(high.path.clone()));
        let canvas = result.paint();
        let pixel = &canvas.rgba_pixels()[(10 * canvas.width() as usize + 10) * 4..];
        assert_eq!(&pixel[..4], &[0, 255, 0, 255]);
    }

    #[test]
    fn negative_stacking_layers_stay_behind_in_flow_content() {
        let document = Document::parse(
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
        assert_eq!(result.hit_test(10.0, 10.0), Some(flow.path.clone()));
        let canvas = result.paint();
        let pixel = &canvas.rgba_pixels()[(10 * canvas.width() as usize + 10) * 4..];
        assert_eq!(&pixel[..4], &[255, 0, 0, 255]);
    }
}
