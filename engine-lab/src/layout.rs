use crate::css::{
    AlignItems, Color, Display, Edges, FlexDirection, FontWeight, GridTemplate, GridTrack,
    JustifyContent, Style, StyleContext, StyleState, Stylesheet, TextAlign,
};
use crate::dom::{Document, Element, Node, NodePath};

#[derive(Clone, Copy, Debug, PartialEq)]
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
    pub color: Color,
    pub bold: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LayoutBox {
    pub path: NodePath,
    pub tag: String,
    pub rect: Rect,
    pub background: Color,
    pub border_width: f32,
    pub border_color: Color,
    pub text: Vec<TextRun>,
    pub children: Vec<LayoutBox>,
    pub(crate) margin: ResolvedEdges,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LayoutDocument {
    pub viewport: Size,
    pub root: LayoutBox,
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
    fragments: Vec<InlineFragment>,
}

struct InlineFragment {
    text: String,
    width: f32,
    font_size: f32,
    color: Color,
    bold: bool,
}

#[derive(Clone, Copy)]
struct ParentStyle {
    font_size: f32,
    color: Color,
    weight: FontWeight,
}

#[derive(Clone, Copy)]
struct InlineContext {
    font_size: f32,
    line_height: f32,
    color: Color,
    weight: FontWeight,
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
}

struct GridItem<'a> {
    element: &'a Element,
    path: NodePath,
    style: Style,
    margin: ResolvedEdges,
    layout: Option<LayoutBox>,
}

struct GridLayoutInput<'a> {
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
}

struct LayoutNode<'a> {
    element: &'a Element,
    path: NodePath,
    ancestors: &'a [&'a Element],
}

pub fn layout(document: &Document, stylesheet: &Stylesheet, viewport: Size) -> LayoutDocument {
    layout_with_state(document, stylesheet, viewport, &StyleState::default())
}

pub fn layout_with_state(
    document: &Document,
    stylesheet: &Stylesheet,
    viewport: Size,
    style_state: &StyleState,
) -> LayoutDocument {
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
        border_width: 0.0,
        border_color: Color::transparent(),
        text: Vec::new(),
        children: Vec::new(),
        margin: ResolvedEdges::default(),
    };
    let mut cursor_y = 0.0;
    for (index, child) in document.root.children.iter().enumerate() {
        let Node::Element(element) = child else {
            continue;
        };
        let path = vec![index];
        if let Some(child_box) = layout_element(
            LayoutNode {
                element,
                path,
                ancestors: &[],
            },
            0.0,
            cursor_y,
            viewport.width,
            ParentStyle {
                font_size: 16.0,
                color: Color::BLACK,
                weight: FontWeight::Normal,
            },
            stylesheet,
            style_state,
        ) {
            cursor_y = child_box.rect.bottom() + child_box.margin.bottom;
            root.children.push(child_box);
        }
    }
    LayoutDocument { viewport, root }
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
        hit_test_box(&self.root, x, y)
    }
}

fn hit_test_box(layout: &LayoutBox, x: f32, y: f32) -> Option<NodePath> {
    if !contains_point(layout.rect, x, y) {
        return None;
    }
    for child in layout.children.iter().rev() {
        if let Some(path) = hit_test_box(child, x, y) {
            return Some(path);
        }
    }
    (!layout.path.is_empty()).then(|| layout.path.clone())
}

fn contains_point(rect: Rect, x: f32, y: f32) -> bool {
    x >= rect.x && y >= rect.y && x < rect.x + rect.width && y < rect.y + rect.height
}

fn layout_element(
    node: LayoutNode<'_>,
    containing_x: f32,
    containing_y: f32,
    containing_width: f32,
    parent: ParentStyle,
    stylesheet: &Stylesheet,
    style_state: &StyleState,
) -> Option<LayoutBox> {
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
        state: style_state,
    });
    if style.display == Display::None {
        return None;
    }
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
    let content_width = style
        .width
        .resolve(containing_width, font_size)
        .unwrap_or((containing_width - horizontal_extras).max(0.0))
        .max(0.0);
    let border_box_width = content_width + padding.left + padding.right + (border * 2.0);
    let rect = Rect {
        x: containing_x + margin.left,
        y: containing_y + margin.top,
        width: border_box_width,
        height: 0.0,
    };
    let content_x = rect.x + border + padding.left;
    let content_y = rect.y + border + padding.top;
    if style.display == Display::Flex {
        return Some(layout_flex_element(FlexLayoutInput {
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
        }));
    }
    if style.display == Display::Grid {
        return Some(layout_grid_element(GridLayoutInput {
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
        }));
    }
    let line_height = style
        .line_height
        .resolve(font_size, font_size)
        .unwrap_or(font_size * 1.25)
        .max(font_size);
    let mut line = InlineLine {
        y: content_y,
        used: 0.0,
        height: line_height,
        fragments: Vec::new(),
    };
    let mut text = Vec::new();
    let mut children = Vec::new();
    let inline_context = InlineContext {
        font_size,
        line_height,
        color: style.color,
        weight: style.font_weight,
        content_width,
        content_x,
        align: style.text_align,
    };
    let mut child_ancestors = ancestors.to_vec();
    child_ancestors.push(element);
    for (index, child) in element.children.iter().enumerate() {
        match child {
            Node::Text(value) => append_inline_text(value, inline_context, &mut text, &mut line),
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
                    state: style_state,
                });
                if child_style.display == Display::None {
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
                    if let Some(child_box) = layout_element(
                        LayoutNode {
                            element: child_element,
                            path: child_path,
                            ancestors: &child_ancestors,
                        },
                        content_x,
                        line.y,
                        content_width,
                        ParentStyle {
                            font_size,
                            color: style.color,
                            weight: style.font_weight,
                        },
                        stylesheet,
                        style_state,
                    ) {
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
    let content_height = style
        .height
        .resolve(containing_width, font_size)
        .unwrap_or((line.y - content_y).max(0.0))
        .max(0.0);
    let border_box_height = content_height + padding.top + padding.bottom + (border * 2.0);
    let rect = Rect {
        height: border_box_height,
        ..rect
    };
    Some(LayoutBox {
        path,
        tag: element.name.clone(),
        rect,
        background: style.background,
        border_width: border,
        border_color: style.border_color,
        text,
        children,
        margin,
    })
}

fn layout_flex_element(input: FlexLayoutInput<'_>) -> LayoutBox {
    let FlexLayoutInput {
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
    } = input;
    let child_parent = ParentStyle {
        font_size,
        color: style.color,
        weight: style.font_weight,
    };
    let mut child_ancestors = ancestors;
    child_ancestors.push(element);
    let mut items = Vec::new();
    for (index, child) in element.children.iter().enumerate() {
        let Node::Element(child) = child else {
            continue;
        };
        let mut child_path = path.clone();
        child_path.push(index);
        let child_style = stylesheet.style_for(StyleContext {
            element: child,
            path: &child_path,
            ancestors: &child_ancestors,
            parent_font_size: font_size,
            parent_color: style.color,
            parent_weight: style.font_weight,
            state: style_state,
        });
        if child_style.display == Display::None {
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
        item.layout = layout_element(
            LayoutNode {
                element: item.element,
                path: item.path.clone(),
                ancestors: &child_ancestors,
            },
            0.0,
            0.0,
            child_width,
            child_parent,
            stylesheet,
            style_state,
        );
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
    LayoutBox {
        path,
        tag: element.name.clone(),
        rect: Rect {
            height: content_height + padding.top + padding.bottom + (border * 2.0),
            ..rect
        },
        background: style.background,
        border_width: border,
        border_color: style.border_color,
        text: Vec::new(),
        children,
        margin,
    }
}

fn layout_grid_element(input: GridLayoutInput<'_>) -> LayoutBox {
    let GridLayoutInput {
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
    } = input;
    let child_parent = ParentStyle {
        font_size,
        color: style.color,
        weight: style.font_weight,
    };
    let mut child_ancestors = ancestors;
    child_ancestors.push(element);
    let mut items = Vec::new();
    for (index, child) in element.children.iter().enumerate() {
        let Node::Element(child) = child else {
            continue;
        };
        let mut child_path = path.clone();
        child_path.push(index);
        let child_style = stylesheet.style_for(StyleContext {
            element: child,
            path: &child_path,
            ancestors: &child_ancestors,
            parent_font_size: font_size,
            parent_color: style.color,
            parent_weight: style.font_weight,
            state: style_state,
        });
        if child_style.display == Display::None {
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
    let zero_intrinsic = vec![0.0_f32; column_count];
    let column_widths = resolve_grid_tracks(
        style.grid_template_columns,
        column_count,
        content_width,
        gap,
        &zero_intrinsic,
    );
    for (index, item) in items.iter_mut().enumerate() {
        let column = index % column_count;
        let column_width = column_widths[column];
        item.layout = layout_element(
            LayoutNode {
                element: item.element,
                path: item.path.clone(),
                ancestors: &child_ancestors,
            },
            0.0,
            0.0,
            column_width.max(0.0),
            child_parent,
            stylesheet,
            style_state,
        );
    }
    let mut intrinsic_rows = vec![0.0_f32; row_count];
    for (index, item) in items.iter().enumerate() {
        let row = index / column_count;
        if let Some(child) = item.layout.as_ref() {
            intrinsic_rows[row] = intrinsic_rows[row]
                .max(child.rect.height + item.margin.top + item.margin.bottom);
        }
    }
    let explicit_height = style.height.resolve(content_width, font_size);
    let row_available = explicit_height
        .unwrap_or_else(|| intrinsic_rows.iter().sum::<f32>() + gap * row_count.saturating_sub(1) as f32);
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
            child.rect.width = (column_widths[column] - item.margin.left - item.margin.right).max(0.0);
        }
        if explicit_height.is_some() && item.style.height == crate::css::Length::Auto {
            child.rect.height =
                (row_heights[row] - item.margin.top - item.margin.bottom).max(0.0);
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
    LayoutBox {
        path,
        tag: element.name.clone(),
        rect: Rect {
            height: content_height + padding.top + padding.bottom + border * 2.0,
            ..rect
        },
        background: style.background,
        border_width: border,
        border_color: style.border_color,
        text: Vec::new(),
        children,
        margin,
    }
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
    let fixed = tracks
        .iter()
        .map(|track| match track {
            GridTrack::Px(value) => *value,
            GridTrack::Percent(value) => available * *value / 100.0,
            GridTrack::Auto | GridTrack::Fr(_) => 0.0,
        })
        .sum::<f32>();
    let fr_total = tracks
        .iter()
        .map(|track| match track {
            GridTrack::Fr(value) => *value,
            GridTrack::Auto | GridTrack::Px(_) | GridTrack::Percent(_) => 0.0,
        })
        .sum::<f32>();
    let auto_count = tracks
        .iter()
        .filter(|track| matches!(track, GridTrack::Auto))
        .count();
    let free = (available - fixed - gap * count.saturating_sub(1) as f32).max(0.0);
    let auto_share = if fr_total == 0.0 && auto_count > 0 {
        free / auto_count as f32
    } else {
        0.0
    };
    tracks
        .iter()
        .enumerate()
        .map(|(index, track)| match track {
            GridTrack::Px(value) => value.max(0.0),
            GridTrack::Percent(value) => (available * value / 100.0).max(0.0),
            GridTrack::Fr(value) => {
                if fr_total > 0.0 {
                    free * value / fr_total
                } else {
                    intrinsic.get(index).copied().unwrap_or(0.0)
                }
            }
            GridTrack::Auto => intrinsic
                .get(index)
                .copied()
                .filter(|value| *value > 0.0)
                .unwrap_or(auto_share),
        })
        .collect()
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

fn translate_layout_box(layout: &mut LayoutBox, offset_x: f32, offset_y: f32) {
    layout.rect.x += offset_x;
    layout.rect.y += offset_y;
    for text in &mut layout.text {
        text.x += offset_x;
        text.y += offset_y;
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
        .max(font_size);
    let child_context = InlineContext {
        font_size,
        line_height,
        color: style.color,
        weight: style.font_weight,
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
    let mut first_word = true;
    for word in value.split_ascii_whitespace() {
        let word_width = measure_text(word, context.font_size);
        let space_width = if first_word || line.fragments.is_empty() {
            0.0
        } else {
            measure_text(" ", context.font_size)
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
        let prefix = if line.fragments.is_empty() { "" } else { " " };
        let text = format!("{prefix}{word}");
        let width = measure_text(&text, context.font_size);
        line.used += width;
        line.height = line.height.max(context.line_height);
        line.fragments.push(InlineFragment {
            text,
            width,
            font_size: context.font_size,
            color: context.color,
            bold: context.weight == FontWeight::Bold,
        });
        first_word = false;
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

fn measure_text(text: &str, font_size: f32) -> f32 {
    let glyph_advance = (font_size / 9.0).round().max(1.0) * 6.0;
    text.chars().count() as f32 * glyph_advance
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
}
