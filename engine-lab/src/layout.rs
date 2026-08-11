use crate::css::{Color, Display, Edges, FontWeight, Stylesheet, TextAlign};
use crate::dom::{Document, Element, Node};

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

pub fn layout(document: &Document, stylesheet: &Stylesheet, viewport: Size) -> LayoutDocument {
    let mut root = LayoutBox {
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
    for child in &document.root.children {
        let Node::Element(element) = child else {
            continue;
        };
        if let Some(child_box) = layout_element(
            element,
            0.0,
            cursor_y,
            viewport.width,
            ParentStyle {
                font_size: 16.0,
                color: Color::BLACK,
                weight: FontWeight::Normal,
            },
            stylesheet,
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
}

fn layout_element(
    element: &Element,
    containing_x: f32,
    containing_y: f32,
    containing_width: f32,
    parent: ParentStyle,
    stylesheet: &Stylesheet,
) -> Option<LayoutBox> {
    let style =
        stylesheet.style_for_inherited(element, parent.font_size, parent.color, parent.weight);
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
    for child in &element.children {
        match child {
            Node::Text(value) => append_inline_text(value, inline_context, &mut text, &mut line),
            Node::Element(child_element) => {
                let child_style = stylesheet.style_for_inherited(
                    child_element,
                    font_size,
                    style.color,
                    style.font_weight,
                );
                if child_style.display == Display::None {
                    continue;
                }
                if child_style.display == Display::Block {
                    flush_line(
                        &mut line,
                        &mut text,
                        content_x,
                        content_width,
                        style.text_align,
                    );
                    if let Some(child_box) = layout_element(
                        child_element,
                        content_x,
                        line.y,
                        content_width,
                        ParentStyle {
                            font_size,
                            color: style.color,
                            weight: style.font_weight,
                        },
                        stylesheet,
                    ) {
                        line.y = child_box.rect.bottom() + child_box.margin.bottom;
                        line.used = 0.0;
                        line.height = line_height;
                        line.fragments.clear();
                        children.push(child_box);
                    }
                } else {
                    append_inline_element(
                        child_element,
                        ParentStyle {
                            font_size,
                            color: style.color,
                            weight: style.font_weight,
                        },
                        inline_context,
                        stylesheet,
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

fn append_inline_element(
    element: &Element,
    parent: ParentStyle,
    context: InlineContext,
    stylesheet: &Stylesheet,
    text: &mut Vec<TextRun>,
    line: &mut InlineLine,
) {
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
    let style =
        stylesheet.style_for_inherited(element, parent.font_size, parent.color, parent.weight);
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
    for child in &element.children {
        match child {
            Node::Text(value) => append_inline_text(value, child_context, text, line),
            Node::Element(child) => append_inline_element(
                child,
                ParentStyle {
                    font_size,
                    color: style.color,
                    weight: style.font_weight,
                },
                child_context,
                stylesheet,
                text,
                line,
            ),
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
}
