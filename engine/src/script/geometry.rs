// what script can measure: element boxes from a layout of the current document, computed
// style, media queries, the performance timeline and idle callbacks

use super::object::{JsObject, Property, new_object};
use super::operations::{is_callable, is_primitive, number_to_string, plain_object};
use super::realm::Native;
use super::*;
use crate::css::{
    AlignItems, FlexDirection, FontWeight, JustifyContent, Position, Style, StyleState, Stylesheet,
    TextAlign, TextTransform,
};
use crate::font::FontSet;
use crate::layout::{
    BoxGeometry, LayoutDocument, Rect, Size, layout_with_state, resolve_edges, used_border,
};

// what the embedder adds to a layout beyond the document and the runtime's own state
pub(crate) struct LayoutInputs {
    pub(crate) stylesheet: Rc<Stylesheet>,
    // each style element and stylesheet link the sheet was built from with the text it gave,
    // and the document version they were last compared at
    pub(crate) sheets: Vec<(crate::navigation::SheetSource, String)>,
    pub(crate) sheets_version: u64,
    // what rebuilding the sheet dropped or failed to load, for the page's log
    pub(crate) notes: Vec<String>,
    pub(crate) fonts: Rc<FontSet>,
    // which change of the document's script fonts the font set holds
    pub(crate) fonts_stamp: u64,
    // hover, preferences, running transitions and animations, and decoded image sizes
    pub(crate) extras: StyleState,
}

// a layout of the document as it stood, with every node's boxes indexed
pub(super) struct MeasuredLayout {
    version: u64,
    // the sheet it was styled by, which devtools can replace while the document stays
    stylesheet: crate::navigation::SheetIdentity,
    state: StyleState,
    // nothing in it sampled motion, so it holds at any clock time
    timeless: bool,
    boxes: HashMap<NodeId, Vec<BoxGeometry>>,
    layout: LayoutDocument,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowMethod {
    GetComputedStyle,
    MatchMedia,
    MediaAddListener,
    MediaRemoveListener,
    PropertyValue,
    PerformanceNow,
    // a visualViewport getter, by name
    VisualViewport(&'static str),
    PerformanceMark,
    PerformanceMeasure,
    PerformanceEntries,
    PerformanceEntriesByName,
    PerformanceEntriesByType,
    PerformanceClearMarks,
    PerformanceClearMeasures,
    RequestIdleCallback,
    CancelIdleCallback,
    IdleTimeRemaining,
}

pub(super) struct MediaQueryRecord {
    object: Weak<RefCell<JsObject>>,
    query: String,
    matches: bool,
}

#[derive(Clone)]
pub(super) struct PerformanceEntry {
    name: String,
    kind: &'static str,
    start: f64,
    duration: f64,
    detail: Value,
}

impl PerformanceEntry {
    pub(super) fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn kind(&self) -> &'static str {
        self.kind
    }
}

// longhand properties getComputedStyle reports, with the value used when nothing sets them;
// inherited ones fall back to the nearest ancestor that declares them
const PLAIN: &[(&str, &str)] = &[
    ("box-sizing", "content-box"),
    ("transition-property", "all"),
    ("transition-duration", "0s"),
    ("transition-timing-function", "ease"),
    ("transition-delay", "0s"),
    ("animation-name", "none"),
    ("animation-duration", "0s"),
    ("animation-timing-function", "ease"),
    ("animation-delay", "0s"),
    ("animation-iteration-count", "1"),
    ("animation-direction", "normal"),
    ("animation-fill-mode", "none"),
    ("animation-play-state", "running"),
    ("transform", "none"),
    ("transform-origin", "50% 50%"),
    ("flex-wrap", "nowrap"),
    ("flex-shrink", "1"),
    ("flex-basis", "auto"),
    ("align-self", "auto"),
    ("align-content", "normal"),
    ("order", "0"),
    ("min-width", "auto"),
    ("min-height", "auto"),
    ("max-width", "none"),
    ("max-height", "none"),
    ("float", "none"),
    ("clear", "none"),
    ("text-decoration-line", "none"),
    ("background-image", "none"),
    ("background-repeat", "repeat"),
    ("background-size", "auto"),
    ("background-position", "0% 0%"),
    ("outline-style", "none"),
    ("outline-width", "0px"),
    ("content", "normal"),
    ("will-change", "auto"),
    ("user-select", "auto"),
    ("object-fit", "fill"),
    ("object-position", "50% 50%"),
    ("vertical-align", "baseline"),
    ("text-overflow", "clip"),
    ("resize", "none"),
    ("appearance", "none"),
];

// properties whose computed value keeps a declared percentage or auto, where the resolved
// value getComputedStyle reports is a length
const DECLARED_WHEN_TYPED: &[&str] = &[
    "width",
    "height",
    "min-width",
    "min-height",
    "max-width",
    "max-height",
    "margin-top",
    "margin-right",
    "margin-bottom",
    "margin-left",
    "padding-top",
    "padding-right",
    "padding-bottom",
    "padding-left",
    "top",
    "right",
    "bottom",
    "left",
    "flex-basis",
    "text-indent",
];

const INHERITED: &[(&str, &str)] = &[
    ("visibility", "visible"),
    ("cursor", "auto"),
    ("pointer-events", "auto"),
    ("white-space", "normal"),
    ("word-break", "normal"),
    ("overflow-wrap", "normal"),
    ("word-spacing", "0px"),
    ("direction", "ltr"),
    ("font-variant", "normal"),
    ("list-style-type", "disc"),
    ("text-indent", "0px"),
    ("caret-color", "auto"),
    ("color-scheme", "normal"),
];

const COMPUTED: &[&str] = &[
    "display",
    "font-style",
    "position",
    "color",
    "background-color",
    "box-shadow",
    "opacity",
    "font-size",
    "font-weight",
    "font-family",
    "line-height",
    "width",
    "height",
    "margin-top",
    "margin-right",
    "margin-bottom",
    "margin-left",
    "padding-top",
    "padding-right",
    "padding-bottom",
    "padding-left",
    "border-top-width",
    "border-right-width",
    "border-bottom-width",
    "border-left-width",
    "border-top-style",
    "border-right-style",
    "border-bottom-style",
    "border-left-style",
    "border-top-color",
    "border-right-color",
    "border-bottom-color",
    "border-left-color",
    "border-top-left-radius",
    "border-top-right-radius",
    "border-bottom-right-radius",
    "border-bottom-left-radius",
    "top",
    "right",
    "bottom",
    "left",
    "z-index",
    "overflow-x",
    "overflow-y",
    "flex-direction",
    "flex-grow",
    "justify-content",
    "align-items",
    "row-gap",
    "column-gap",
    "text-align",
    "text-transform",
    "letter-spacing",
];

// blink prints css numbers to six significant digits
// a number as Blink prints CSS numbers, printf's %g with six significant digits: exponent
// form below 1e-4 or from 1e6, and no trailing zeros
pub(super) fn css_number(value: f64) -> String {
    if value == 0.0 || !value.is_finite() {
        return if value.is_finite() {
            "0".into()
        } else {
            number_to_string(value)
        };
    }
    let formatted = format!("{:.5e}", value.abs());
    let (mantissa, exponent) = formatted.split_once('e').expect("exponent form");
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let exponent: i32 = exponent.parse().expect("an exponent");
    let sign = if value < 0.0 { "-" } else { "" };
    let trim = |text: String| {
        if !text.contains('.') {
            return text;
        }
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    };
    if !(-4..6).contains(&exponent) {
        let mantissa = trim(format!("{}.{}", &digits[..1], &digits[1..]));
        let exponent_sign = if exponent < 0 { '-' } else { '+' };
        return format!("{sign}{mantissa}e{exponent_sign}{:02}", exponent.abs());
    }
    let point = exponent + 1;
    let text = if point <= 0 {
        format!("0.{}{digits}", "0".repeat((-point) as usize))
    } else {
        let point = point as usize;
        format!("{}.{}", &digits[..point], &digits[point..])
    };
    format!("{sign}{}", trim(text))
}

// a list of times as their computed value gives them, in seconds
fn seconds_list(value: &str) -> String {
    value
        .split(',')
        .map(|item| {
            let item = item.trim();
            let seconds = match item.to_ascii_lowercase().strip_suffix("ms") {
                Some(milliseconds) => milliseconds
                    .trim()
                    .parse::<f64>()
                    .ok()
                    .map(|ms| ms / 1000.0),
                None => item
                    .strip_suffix(['s', 'S'])
                    .and_then(|seconds| seconds.trim().parse::<f64>().ok()),
            };
            seconds.map_or_else(
                || item.to_string(),
                |seconds| format!("{}s", css_number(seconds)),
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

// url() references made absolute and quoted, as computed values carry them
fn absolute_urls(value: &str, base: &crate::AppUrl) -> String {
    let mut output = String::new();
    let mut rest = value;
    while let Some(start) = rest.find("url(") {
        let Some(end) = rest[start..].find(')') else {
            break;
        };
        output.push_str(&rest[..start]);
        let inner = rest[start + 4..start + end].trim();
        let reference = inner
            .strip_prefix('"')
            .and_then(|inner| inner.strip_suffix('"'))
            .or_else(|| {
                inner
                    .strip_prefix('\'')
                    .and_then(|inner| inner.strip_suffix('\''))
            })
            .unwrap_or(inner);
        let resolved = base
            .resolve(reference)
            .map_or_else(|_| reference.to_string(), |url| url.to_string());
        output.push_str(&format!("url(\"{}\")", resolved.replace('"', "\\\"")));
        rest = &rest[start + end + 1..];
    }
    output.push_str(rest);
    output
}

// a transform list resolved against the border box, as matrix() or matrix3d()
fn transform_matrix(value: &str, width: f32, height: f32, font_size: f32) -> Option<String> {
    let ops = crate::transform::parse_transform_list(value)?;
    let matrix = ops
        .iter()
        .fold(crate::transform::Matrix::IDENTITY, |matrix, op| {
            matrix.multiply(&op.matrix(width, height, font_size))
        });
    let m = matrix.0;
    let flat = [2, 3, 6, 7, 8, 9, 11, 14]
        .iter()
        .all(|&index| m[index] == 0.0)
        && m[10] == 1.0
        && m[15] == 1.0;
    let text = |indices: &[usize]| {
        indices
            .iter()
            .map(|&index| css_number(f64::from(m[index])))
            .collect::<Vec<_>>()
            .join(", ")
    };
    Some(if flat {
        format!("matrix({})", text(&[0, 1, 4, 5, 12, 13]))
    } else {
        format!("matrix3d({})", text(&(0..16).collect::<Vec<_>>()))
    })
}

fn px(value: f32) -> String {
    format!("{}px", css_number(f64::from(value)))
}

// a family stays bare when it is one css identifier and is quoted otherwise, as chrome serializes it
fn family_text(name: &str) -> String {
    let ident = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_' || !c.is_ascii();
    let body = name.strip_prefix('-').unwrap_or(name);
    if body.starts_with(|c: char| ident(c) && !c.is_ascii_digit()) && name.chars().all(ident) {
        name.to_string()
    } else {
        format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

fn color_text(color: crate::Color) -> String {
    if color.alpha == 255 {
        format!("rgb({}, {}, {})", color.red, color.green, color.blue)
    } else {
        let alpha = (f64::from(color.alpha) / 255.0 * 1000.0).round() / 1000.0;
        format!(
            "rgba({}, {}, {}, {})",
            color.red,
            color.green,
            color.blue,
            number_to_string(alpha)
        )
    }
}

fn camel_case(property: &str) -> String {
    let mut result = String::new();
    let mut upper = false;
    for character in property.chars() {
        if character == '-' {
            upper = true;
        } else if upper {
            result.push(character.to_ascii_uppercase());
            upper = false;
        } else {
            result.push(character);
        }
    }
    result
}

impl ScriptRuntime {
    pub(crate) fn set_layout_inputs(&mut self, inputs: Rc<RefCell<LayoutInputs>>) {
        self.layout_inputs = Some(inputs);
        *self.measured.borrow_mut() = None;
    }

    // the style state a layout of the document runs with right now
    pub(crate) fn current_style_state(&self) -> Option<StyleState> {
        let inputs = self.layout_inputs.as_ref()?.borrow();
        Some(self.layout_state(&inputs))
    }

    fn layout_state(&self, inputs: &LayoutInputs) -> StyleState {
        let extras = &inputs.extras;
        let mut state = self.style_state(extras.hovered);
        state.viewport_width = self.viewport.width;
        state.viewport_height = self.viewport.height;
        state.color_scheme = extras.color_scheme;
        state.reduced_motion = extras.reduced_motion;
        state.forced_colors = extras.forced_colors;
        state.clock = self.clock;
        state.transitions = extras.transitions.clone();
        state.animation_starts = extras.animation_starts.clone();
        state.image_sizes = extras.image_sizes.clone();
        // a canvas and a video know their own sizes before the page has first laid out
        let lists = self.element_lists();
        let document = self.document.borrow();
        for &node in &lists[2] {
            let size = super::canvas_size(&document, node);
            state.image_sizes.insert(node, size);
        }
        drop(document);
        for &node in &lists[3] {
            state.image_sizes.insert(node, self.video_size(node));
        }
        // the last measurement knows container sizes the page's own layout may not yet
        state.container_sizes = self.measured.borrow().as_ref().map_or_else(
            || extras.container_sizes.clone(),
            |measured| measured.state.container_sizes.clone(),
        );
        state
    }

    // script reads styles as the document's style elements and stylesheet links now stand
    pub(super) fn refresh_stylesheet(&self) {
        let Some(inputs) = &self.layout_inputs else {
            return;
        };
        let resources = self.resources.clone().unwrap_or_default();
        let provider = self.resource_provider.as_deref();
        let load = |url: &AppUrl| match resolve_runtime_resource(&resources, provider, url) {
            Ok(Some(resource)) => Ok(resource),
            Ok(None) => Err(NavigationError::ResourceNotFound(url.path().to_string())),
            Err(error) => Err(NavigationError::ResourceProvider(error.message)),
        };
        let base = self.location.borrow().clone();
        crate::navigation::refresh_stylesheet(&self.document.borrow(), inputs, &base, &load);
    }

    // a fresh layout when the document or its state changed since the last measurement
    fn measure(&self) -> Option<std::cell::Ref<'_, MeasuredLayout>> {
        if self.measure_pinned.get() && self.measured.borrow().is_some() {
            return std::cell::Ref::filter_map(self.measured.borrow(), Option::as_ref).ok();
        }
        self.refresh_stylesheet();
        let inputs = self.layout_inputs.as_ref()?.borrow();
        let state = self.layout_state(&inputs);
        let version = self.document.borrow().version();
        let stylesheet = crate::navigation::SheetIdentity::of(&inputs.stylesheet);
        let stale = self.measured.borrow().as_ref().is_none_or(|measured| {
            measured.version != version
                || measured.stylesheet != stylesheet
                || (measured.state != state
                    && !(measured.timeless
                        && measured.state
                            == StyleState {
                                clock: measured.state.clock,
                                ..state.clone()
                            }))
        });
        if stale {
            let mut state = state;
            let reported = crate::css::clock_reads();
            let mut layout = self.run_layout(&inputs, &state);
            // container queries read sizes from a layout, so a change in them lays out again
            if inputs.stylesheet.has_container_queries() {
                let measured = crate::layout::take_layout_containers();
                if measured != state.container_sizes {
                    state.container_sizes = measured;
                    layout = self.run_layout(&inputs, &state);
                }
            }
            *self.measured.borrow_mut() = Some(MeasuredLayout {
                version,
                stylesheet,
                timeless: crate::css::clock_reads() == reported && state.transitions.is_empty(),
                state,
                boxes: layout.box_index(),
                layout,
            });
        }
        std::cell::Ref::filter_map(self.measured.borrow(), Option::as_ref).ok()
    }

    // reads boxes through one measurement taken at the start, for a pass that runs no script
    pub(super) fn with_pinned_measure<T>(&mut self, pass: impl FnOnce(&mut Self) -> T) -> T {
        drop(self.measure());
        self.measure_pinned.set(true);
        let result = pass(self);
        self.measure_pinned.set(false);
        result
    }

    fn run_layout(&self, inputs: &LayoutInputs, state: &StyleState) -> LayoutDocument {
        let _fonts = crate::font::use_fonts(inputs.fonts.clone(), self.viewport.device_pixel_ratio);
        layout_with_state(
            &self.document.borrow(),
            &inputs.stylesheet,
            Size {
                width: self.viewport.width,
                height: self.viewport.height,
            },
            state,
        )
    }

    fn node_boxes(&self, node: NodeId) -> Vec<BoxGeometry> {
        let Some(measured) = self.measure() else {
            return Vec::new();
        };
        if let Some(boxes) = measured.boxes.get(&node) {
            return boxes.clone();
        }
        // an inline element has no box of its own: it is the text and boxes inside it, one
        // rectangle per line
        let document = self.document.borrow();
        if document.element(node).is_none() {
            return Vec::new();
        }
        let mut lines: Vec<BoxGeometry> = Vec::new();
        for part in document
            .descendants(node)
            .filter_map(|descendant| measured.boxes.get(&descendant))
            .flatten()
        {
            let rect = part.rect;
            match lines
                .iter_mut()
                .find(|line| (line.rect.y - rect.y).abs() < 0.5)
            {
                Some(line) => {
                    let left = line.rect.x.min(rect.x);
                    let right = (line.rect.x + line.rect.width).max(rect.x + rect.width);
                    let bottom = (line.rect.y + line.rect.height).max(rect.y + rect.height);
                    line.rect = Rect {
                        x: left,
                        y: line.rect.y,
                        width: right - left,
                        height: bottom - line.rect.y,
                    };
                    line.scrollport = line.rect;
                    let (a, b) = (line.client, part.client);
                    let (left, top) = (a.x.min(b.x), a.y.min(b.y));
                    line.client = Rect {
                        x: left,
                        y: top,
                        width: (a.x + a.width).max(b.x + b.width) - left,
                        height: (a.y + a.height).max(b.y + b.height) - top,
                    };
                }
                None => lines.push(BoxGeometry {
                    rect,
                    client: part.client,
                    border: Default::default(),
                    position: part.position,
                    scrollport: rect,
                    scroll_size: part.scroll_size,
                }),
            }
        }
        lines
    }

    // the element on top at a viewport point, as cssom view's elementFromPoint finds it: text
    // answers with its element and a point over nothing with the root element
    pub(super) fn element_from_point(&self, x: f64, y: f64) -> Option<NodeId> {
        let (width, height) = (
            f64::from(self.viewport.width),
            f64::from(self.viewport.height),
        );
        if !(0.0..=width).contains(&x) || !(0.0..=height).contains(&y) {
            return None;
        }
        let hit = self.measure()?.layout.hit_test(x as f32, y as f32);
        let document = self.document.borrow();
        hit.into_iter()
            .flat_map(|node| std::iter::once(node).chain(document.ancestors(node)))
            .find(|&node| document.element(node).is_some())
            .or_else(|| document.document_element())
    }

    // every element at a viewport point from the top down; this follows the top element's
    // ancestry, so a box overlapping it from elsewhere in the tree is left out
    pub(super) fn elements_from_point(&self, x: f64, y: f64) -> Vec<NodeId> {
        let Some(top) = self.element_from_point(x, y) else {
            return Vec::new();
        };
        let document = self.document.borrow();
        std::iter::once(top)
            .chain(document.ancestors(top))
            .filter(|&node| document.element(node).is_some())
            .collect()
    }

    // whether an element is rendered, as Element.checkVisibility answers: it needs a box, and
    // the options can also ask for a nonzero opacity up its ancestry and a visible visibility
    pub(super) fn check_visibility(
        &mut self,
        node: NodeId,
        options: &Value,
    ) -> Result<bool, ScriptError> {
        if self.node_boxes(node).is_empty() {
            return Ok(false);
        }
        let mut asked = |names: [&str; 2]| -> Result<bool, ScriptError> {
            if is_primitive(options) {
                return Ok(false);
            }
            for name in names {
                if self.get_property(options, name)?.truthy() {
                    return Ok(true);
                }
            }
            Ok(false)
        };
        let opacity = asked(["checkOpacity", "opacityProperty"])?;
        let visibility = asked(["checkVisibilityCSS", "visibilityProperty"])?;
        if visibility
            && self
                .computed_snapshot(node)
                .get("visibility")
                .map(String::as_str)
                != Some("visible")
        {
            return Ok(false);
        }
        if opacity {
            let ancestry: Vec<NodeId> = {
                let document = self.document.borrow();
                std::iter::once(node)
                    .chain(document.ancestors(node))
                    .filter(|&ancestor| document.element(ancestor).is_some())
                    .collect()
            };
            for ancestor in ancestry {
                if self
                    .computed_snapshot(ancestor)
                    .get("opacity")
                    .map(String::as_str)
                    == Some("0")
                {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    // scrolls each box around an element, innermost first, until the element shows where the
    // block and inline alignments put it, as Element.scrollIntoView does
    pub(super) fn scroll_into_view(
        &mut self,
        node: NodeId,
        options: &Value,
    ) -> Result<(), ScriptError> {
        let (block, inline) = match options {
            Value::Bool(false) => ("end".to_string(), "nearest".to_string()),
            options if !is_primitive(options) => {
                let mut read = |name: &str, default: &str| -> Result<String, ScriptError> {
                    match self.get_property(options, name)? {
                        Value::Undefined => Ok(default.to_string()),
                        value => self.coerce_string(value),
                    }
                };
                (read("block", "start")?, read("inline", "nearest")?)
            }
            _ => ("start".to_string(), "nearest".to_string()),
        };
        let Some(mut rect) = self.bounding_box(node) else {
            return Ok(());
        };
        let ancestors: Vec<NodeId> = self.document.borrow().ancestors(node).collect();
        let boxes: Vec<_> = match self.measure() {
            Some(measured) => ancestors
                .into_iter()
                .filter_map(|ancestor| measured.layout.scroll_box(ancestor))
                .collect(),
            None => return Ok(()),
        };
        let root = self.document.borrow().root();
        for (target, port) in boxes {
            let x = align_delta(rect.x, rect.width, port.x, port.width, &inline);
            let y = align_delta(rect.y, rect.height, port.y, port.height, &block);
            let offset = (
                (target.offset.0 + x).clamp(0.0, target.max_x),
                (target.offset.1 + y).clamp(0.0, target.max_y),
            );
            rect.x -= offset.0 - target.offset.0;
            rect.y -= offset.1 - target.offset.1;
            if self.set_scroll_offset(target.node, offset) {
                if target.node == root {
                    self.dispatch_window_event("scroll")?;
                } else {
                    self.dispatch_node_event(target.node, "scroll")?;
                }
            }
        }
        Ok(())
    }

    // a range's rects as cssom view lists them: the boxes of each element it selects whose
    // parent it does not, then the selected part of each text node, one rect per line
    pub(super) fn range_rects(&self, range: crate::dom::LiveRange) -> Vec<Rect> {
        let Some(measured) = self.measure() else {
            return Vec::new();
        };
        let runs = measured.layout.text_runs();
        let (order, elements, spans) = {
            let document = self.document.borrow();
            let root = document.tree_root(range.start.node);
            let order = document
                .descendants(root)
                .enumerate()
                .map(|(index, node)| (node, index))
                .collect::<HashMap<_, _>>();
            let elements = document
                .descendants(root)
                .filter(|&node| {
                    document.element(node).is_some()
                        && self.contained(node, range)
                        && !document
                            .parent(node)
                            .is_some_and(|parent| self.contained(parent, range))
                })
                .collect::<Vec<_>>();
            let span = |node: NodeId| -> Option<(usize, usize)> {
                let length = document.node_length(node);
                let from = if node == range.start.node {
                    range.start.offset
                } else {
                    0
                };
                let to = if node == range.end.node {
                    range.end.offset
                } else {
                    length
                };
                (node == range.start.node || node == range.end.node || self.contained(node, range))
                    .then_some((from, to))
            };
            let spans = runs
                .iter()
                .map(|run| span(run.node).filter(|_| document.is_text(run.node)))
                .collect::<Vec<_>>();
            (order, elements, spans)
        };
        let place = |node: NodeId| order.get(&node).copied().unwrap_or(usize::MAX);
        let mut rects = Vec::new();
        for element in elements {
            rects.extend(
                self.node_boxes(element)
                    .iter()
                    .map(|geometry| (place(element), geometry.client)),
            );
        }
        // text measures with the page's fonts, which only layout and paint have active
        let _active = self.layout_inputs.as_ref().map(|inputs| {
            crate::font::use_fonts(
                inputs.borrow().fonts.clone(),
                self.viewport.device_pixel_ratio,
            )
        });
        let fonts = crate::font::active_fonts();
        let mut texts: Vec<(NodeId, Rect)> = Vec::new();
        for (prose, span) in runs.iter().zip(spans) {
            let Some((from, to)) = span else {
                continue;
            };
            let run = &prose.run;
            let length = run.text.encode_utf16().count();
            let prefix = usize::from(run.text.starts_with(' '));
            // the run's own text indices for node offsets, a collapsed range keeping its caret
            let units = if from == to {
                (run.node_offset..=run.node_offset + run.source_len)
                    .contains(&from)
                    .then(|| {
                        let at = (prefix + from - run.node_offset).min(length);
                        (at, at)
                    })
            } else {
                run.selected_units(from, to, false)
            };
            let Some((first, last)) = units else {
                continue;
            };
            let x_at = |index: usize| {
                crate::paint::width_of_characters(
                    run,
                    crate::paint::utf16_char_offset(&run.text, index),
                )
            };
            let left = run.x + x_at(first);
            let rect = Rect {
                x: left,
                y: run.y,
                width: run.x + x_at(last) - left,
                height: fonts.content_height(run.font_stack, run.font_size),
            };
            let rect = prose.transforms.iter().rev().fold(rect, |rect, transform| {
                let corners = [
                    (rect.x, rect.y),
                    (rect.x + rect.width, rect.y),
                    (rect.x, rect.y + rect.height),
                    (rect.x + rect.width, rect.y + rect.height),
                ]
                .map(|(x, y)| transform.map_point(x, y).unwrap_or((x, y)));
                let left = corners.iter().map(|point| point.0).fold(f32::MAX, f32::min);
                let top = corners.iter().map(|point| point.1).fold(f32::MAX, f32::min);
                let right = corners.iter().map(|point| point.0).fold(f32::MIN, f32::max);
                let bottom = corners.iter().map(|point| point.1).fold(f32::MIN, f32::max);
                Rect {
                    x: left,
                    y: top,
                    width: right - left,
                    height: bottom - top,
                }
            });
            // layout keeps a run per word; chrome reports a text node's line as one box
            match texts.last_mut() {
                Some((node, line))
                    if *node == prose.node && (line.y - rect.y).abs() < 0.5 && rect.x >= line.x =>
                {
                    line.width = rect.x + rect.width - line.x;
                }
                _ => texts.push((prose.node, rect)),
            }
        }
        rects.extend(texts.into_iter().map(|(node, rect)| (place(node), rect)));
        // tree order, an element before the text inside it
        rects.sort_by_key(|(place, _)| *place);
        rects.into_iter().map(|(_, rect)| rect).collect()
    }

    pub(super) fn bounding_box(&self, node: NodeId) -> Option<Rect> {
        let boxes = self.node_boxes(node);
        let first = boxes.first()?.client;
        Some(boxes.iter().skip(1).fold(first, |union, geometry| {
            let rect = geometry.client;
            let left = union.x.min(rect.x);
            let top = union.y.min(rect.y);
            let right = (union.x + union.width).max(rect.x + rect.width);
            let bottom = (union.y + union.height).max(rect.y + rect.height);
            Rect {
                x: left,
                y: top,
                width: right - left,
                height: bottom - top,
            }
        }))
    }

    // an element's border box, and its content box relative to the padding edge as
    // ResizeObserver reports it
    pub(super) fn box_sizes(&self, node: NodeId) -> Option<(Rect, Rect)> {
        let geometry = *self.node_boxes(node).first()?;
        let parent_width = self
            .document
            .borrow()
            .parent_element(node)
            .and_then(|parent| self.node_boxes(parent).first().copied())
            .map_or(self.viewport.width, |parent| parent.rect.width);
        let inputs = self.layout_inputs.as_ref()?.borrow();
        let mut state = self.layout_state(&inputs);
        let document = self.document.borrow();
        let _environment = crate::css::use_layout_environment(crate::css::LayoutEnvironment {
            viewport: Size {
                width: self.viewport.width,
                height: self.viewport.height,
            },
            root_font_size: 16.0,
        });
        inputs
            .stylesheet
            .populate_custom_properties(&document, &mut state);
        let (style, font_size) = inputs.stylesheet.computed_style(&document, node, &state)?;
        let padding = resolve_edges(style.padding, parent_width, font_size);
        let rect = geometry.rect;
        let border = geometry.border;
        Some((
            rect,
            Rect {
                x: padding.left,
                y: padding.top,
                width: (rect.width - border.left - border.right - padding.left - padding.right)
                    .max(0.0),
                height: (rect.height - border.top - border.bottom - padding.top - padding.bottom)
                    .max(0.0),
            },
        ))
    }

    // the nearest positioned ancestor, or the body, that offsetTop and offsetLeft measure from
    fn offset_parent(&self, node: NodeId) -> Option<NodeId> {
        let own = *self.node_boxes(node).first()?;
        let document = self.document.borrow();
        let tag = document.element(node)?.name.clone();
        if own.position == Position::Fixed || tag == "body" || tag == "html" {
            return None;
        }
        let ancestors = document.ancestors(node).collect::<Vec<_>>();
        drop(document);
        for ancestor in ancestors {
            let name = self.document.borrow().element(ancestor)?.name.clone();
            if name == "body" {
                return Some(ancestor);
            }
            let positioned = self
                .node_boxes(ancestor)
                .first()
                .is_some_and(|geometry| geometry.position != Position::Static);
            if positioned || matches!(name.as_str(), "td" | "th" | "table") {
                return Some(ancestor);
            }
        }
        None
    }

    // SVGGraphicsElement.getBBox: what an element inside an svg draws, in its own user space
    pub(super) fn svg_bounding_box(&self, node: NodeId) -> Value {
        let root = {
            let document = self.document.borrow();
            let mut root = None;
            let mut current = Some(node);
            while let Some(id) = current {
                if document.element(id).is_some_and(|element| {
                    element.namespace == crate::dom::Namespace::Svg && element.name == "svg"
                }) {
                    root = Some(id);
                }
                current = document.parent(id);
            }
            root
        };
        let extent = root.and_then(|root| {
            let measured = self.measure()?;
            let scene = measured.layout.svg_scene(root)?;
            scene.bounding_box(&self.document.borrow(), node)
        });
        let [left, top, right, bottom] = extent.unwrap_or_default();
        self.rect_object(crate::layout::Rect {
            x: left,
            y: top,
            width: right - left,
            height: bottom - top,
        })
    }

    // element measurements such as getBoundingClientRect and offsetWidth; None hands the
    // member on
    pub(super) fn element_geometry(
        &mut self,
        node: NodeId,
        name: &str,
    ) -> Result<Option<Value>, ScriptError> {
        let boxes = || self.node_boxes(node);
        let number = |value: f32| Value::Number(f64::from(value.round()));
        Ok(Some(match name {
            "getBoundingClientRect" => {
                self.rect_object(self.bounding_box(node).unwrap_or_default())
            }
            "getClientRects" => Self::new_array(
                boxes()
                    .into_iter()
                    .map(|geometry| self.rect_object(geometry.client))
                    .collect(),
            ),
            "clientWidth" | "clientHeight" | "clientTop" | "clientLeft" => {
                let Some(geometry) = boxes().first().copied() else {
                    return Ok(Some(Value::Number(0.0)));
                };
                let border = geometry.border;
                number(match name {
                    "clientWidth" => {
                        (geometry.scrollport.width - border.left - border.right).max(0.0)
                    }
                    "clientHeight" => {
                        (geometry.scrollport.height - border.top - border.bottom).max(0.0)
                    }
                    "clientTop" => border.top,
                    _ => border.left,
                })
            }
            "scrollWidth" | "scrollHeight" => {
                let Some(geometry) = boxes().first().copied() else {
                    return Ok(Some(Value::Number(0.0)));
                };
                let border = geometry.border;
                // the scroll size and scrollport both span the borders, which scrollWidth leaves out
                number(
                    if name == "scrollWidth" {
                        geometry.scroll_size.width.max(geometry.scrollport.width)
                            - border.left
                            - border.right
                    } else {
                        geometry.scroll_size.height.max(geometry.scrollport.height)
                            - border.top
                            - border.bottom
                    }
                    .max(0.0),
                )
            }
            "offsetWidth" | "offsetHeight" => {
                let rect = self.bounding_box(node).unwrap_or_default();
                number(if name == "offsetWidth" {
                    rect.width
                } else {
                    rect.height
                })
            }
            "offsetTop" | "offsetLeft" => {
                let Some(rect) = self.bounding_box(node) else {
                    return Ok(Some(Value::Number(0.0)));
                };
                let origin = self
                    .offset_parent(node)
                    .and_then(|parent| self.node_boxes(parent).first().copied())
                    .map_or((0.0, 0.0), |parent| {
                        (
                            parent.rect.x + parent.border.left,
                            parent.rect.y + parent.border.top,
                        )
                    });
                number(if name == "offsetTop" {
                    rect.y - origin.1
                } else {
                    rect.x - origin.0
                })
            }
            "offsetParent" => self
                .offset_parent(node)
                .map_or(Value::Null, |parent| self.node_value(parent)),
            _ => return Ok(None),
        }))
    }

    fn computed_snapshot(&mut self, node: NodeId) -> BTreeMap<String, String> {
        self.computed_values(node, false)
    }

    // the values getComputedStyle reports, or with typed the computed values the Typed OM
    // reifies, which keep declared percentages, auto, a unitless line-height and the
    // transform functions where getComputedStyle resolves them
    pub(super) fn computed_values(
        &mut self,
        node: NodeId,
        typed: bool,
    ) -> BTreeMap<String, String> {
        let mut values = BTreeMap::new();
        let Some(inputs) = self.layout_inputs.clone() else {
            return values;
        };
        self.refresh_stylesheet();
        if inputs.borrow().stylesheet.has_container_queries() {
            drop(self.measure());
        }
        let inputs = inputs.borrow();
        let mut state = self.layout_state(&inputs);
        let document = self.document.borrow();
        if document.element(node).is_none() {
            return values;
        }
        let _environment = crate::css::use_layout_environment(crate::css::LayoutEnvironment {
            viewport: Size {
                width: self.viewport.width,
                height: self.viewport.height,
            },
            root_font_size: 16.0,
        });
        inputs
            .stylesheet
            .populate_custom_properties(&document, &mut state);
        // script often reads the computed styles of many elements between two changes, and
        // each one cascades its ancestors, so the styles worked out stay for the next read
        let (version, sheet) = (
            document.version(),
            crate::navigation::SheetIdentity::of(&inputs.stylesheet),
        );
        let mut kept = self
            .computed_styles
            .take()
            .filter(|kept| kept.version == version && kept.sheet == sheet && *kept.state == state)
            .unwrap_or_else(|| KeptComputed {
                version,
                sheet,
                state: Box::new(state),
                styles: crate::css::KeptStyles::default(),
            });
        let KeptComputed { state, styles, .. } = &mut kept;
        let state: &StyleState = state;
        let worked_out = crate::css::with_kept_styles(styles, || {
            let (style, font_size) = inputs.stylesheet.computed_style(&document, node, state)?;
            let declared = |target: NodeId| {
                inputs.stylesheet.declared_values(crate::css::StyleContext {
                    document: &document,
                    node: target,
                    pseudo: None,
                    parent_font_size: 16.0,
                    parent_color: crate::Color::BLACK,
                    parent_weight: FontWeight::NORMAL,
                    parent_text: crate::css::InheritedText::default(),
                    state,
                })
            };
            let own = declared(node);
            let ancestors = document
                .ancestors(node)
                .filter(|&ancestor| document.element(ancestor).is_some())
                .map(declared)
                .collect::<Vec<_>>();
            let item = document
                .parent_element(node)
                .and_then(|parent| inputs.stylesheet.computed_style(&document, parent, state))
                .is_some_and(|(parent, _)| {
                    matches!(
                        parent.display,
                        crate::Display::Flex
                            | crate::Display::InlineFlex
                            | crate::Display::Grid
                            | crate::Display::InlineGrid
                    )
                });
            Some((style, font_size, own, ancestors, item))
        });
        self.computed_styles = Some(kept);
        let Some((style, font_size, own, ancestors, item)) = worked_out else {
            return values;
        };
        let blockified = item
            || style.float != crate::css::Float::None
            || style.position.is_out_of_flow()
            || document.document_element() == Some(node);
        drop(document);
        let inherited = |property: &str| {
            std::iter::once(&own)
                .chain(&ancestors)
                .find_map(|values| values.get(property).cloned())
        };
        let geometry = self.node_boxes(node).first().copied();
        let parent_width = self
            .document
            .borrow()
            .parent_element(node)
            .and_then(|parent| self.node_boxes(parent).first().copied())
            .map_or(self.viewport.width, |parent| parent.rect.width);
        let padding = resolve_edges(style.padding, parent_width, font_size);
        let margin = resolve_edges(style.margin, parent_width, font_size);
        let computed = ComputedInputs {
            style: &style,
            font_size,
            geometry,
            padding,
            margin,
            inherited: &inherited,
            own: &own,
            blockified,
        };
        // a box that is not laid out still snaps its border widths to device pixels
        let fonts = crate::font::use_fonts(inputs.fonts.clone(), self.viewport.device_pixel_ratio);
        for &property in COMPUTED {
            let value = computed_value(property, &computed);
            values.insert(property.to_string(), value);
        }
        drop(fonts);
        for &(property, initial) in PLAIN {
            let value = own
                .get(property)
                .cloned()
                .unwrap_or_else(|| initial.to_string());
            values.insert(property.to_string(), value);
        }
        for &(property, initial) in INHERITED {
            let value = inherited(property).unwrap_or_else(|| initial.to_string());
            values.insert(property.to_string(), value);
        }
        for (property, value) in &own {
            values
                .entry(property.clone())
                .or_insert_with(|| value.clone());
        }
        for property in [
            "transition-duration",
            "transition-delay",
            "animation-duration",
            "animation-delay",
        ] {
            if let Some(value) = values.get_mut(property) {
                *value = seconds_list(value);
            }
        }
        if let Some(base) = self.base_url.clone() {
            for property in [
                "background-image",
                "list-style-image",
                "border-image-source",
                "mask-image",
            ] {
                if let Some(value) = values.get_mut(property) {
                    *value = absolute_urls(value, &base);
                }
            }
        }
        if typed {
            // a side's declared value, from its longhand or the 1-4 values of its shorthand
            let declared = |property: &str| -> Option<String> {
                if let Some(value) = own.get(property) {
                    return Some(value.clone());
                }
                let (shorthand, side) = property.rsplit_once('-').unwrap_or(("inset", property));
                let index = ["top", "right", "bottom", "left"]
                    .iter()
                    .position(|&found| found == side)?;
                let parts: Vec<&str> = own.get(shorthand)?.split_ascii_whitespace().collect();
                let pick = match (parts.len(), index) {
                    (1, _) => 0,
                    (2, index) => index % 2,
                    (3, 3) => 1,
                    (_, index) => index,
                };
                parts.get(pick).map(|part| part.to_string())
            };
            for &property in DECLARED_WHEN_TYPED {
                match declared(property).map(|value| value.trim().to_ascii_lowercase()) {
                    Some(value) if value.ends_with('%') || value == "auto" => {
                        values.insert(property.to_string(), value);
                    }
                    None if matches!(
                        property,
                        "width" | "height" | "top" | "right" | "bottom" | "left"
                    ) =>
                    {
                        values.insert(property.to_string(), "auto".to_string());
                    }
                    _ => {}
                }
            }
            if let Some(line_height) = inherited("line-height")
                .map(|value| value.trim().to_ascii_lowercase())
                .filter(|value| value == "normal" || value.parse::<f64>().is_ok())
            {
                values.insert("line-height".to_string(), line_height);
            }
            return values;
        }
        if let Some(value) = values.get_mut("transform")
            && value != "none"
        {
            let (width, height) =
                geometry.map_or((0.0, 0.0), |found| (found.rect.width, found.rect.height));
            if let Some(matrix) = transform_matrix(value, width, height, font_size) {
                *value = matrix;
            }
        }
        values
    }

    fn computed_style_object(&mut self, node: NodeId) -> Value {
        let values = self.computed_snapshot(node);
        let mut object = JsObject::default();
        let mut names = Vec::new();
        for (property, value) in &values {
            let value = Value::String(value.clone().into());
            object.define(property.clone(), Property::data(value.clone()));
            if property.contains('-') && !property.starts_with("--") {
                object.define(camel_case(property), Property::data(value));
            }
            names.push(Value::String(property.clone().into()));
        }
        object.define(
            "length".to_string(),
            Property::hidden(Value::Number(names.len() as f64)),
        );
        for (index, name) in names.into_iter().enumerate() {
            object.define(index.to_string(), Property::hidden(name));
        }
        object.define(
            "getPropertyValue".to_string(),
            Property::hidden(self.native_function(
                Native::Window(WindowMethod::PropertyValue),
                "getPropertyValue",
                1,
            )),
        );
        Value::Object(new_object(object))
    }

    // a media query list from script, kept so a change in the answer fires its change event
    fn match_media(&mut self, query: String) -> Value {
        let prototype = match &self.media_list_prototype {
            Some(prototype) => prototype.clone(),
            None => {
                let prototype = self.intrinsics.hosts["MediaQueryList"].clone();
                prototype
                    .borrow_mut()
                    .set_prototype(Some(self.intrinsics.dom.prototype("EventTarget")));
                {
                    let mut prototype = prototype.borrow_mut();
                    prototype.define(
                        "addListener".to_string(),
                        Property::hidden(self.native_function(
                            Native::Window(WindowMethod::MediaAddListener),
                            "addListener",
                            1,
                        )),
                    );
                    prototype.define(
                        "removeListener".to_string(),
                        Property::hidden(self.native_function(
                            Native::Window(WindowMethod::MediaRemoveListener),
                            "removeListener",
                            1,
                        )),
                    );
                    prototype.define(
                        "@@toStringTag".to_string(),
                        Property::hidden(Value::String("MediaQueryList".into())),
                    );
                }
                self.media_list_prototype = Some(prototype.clone());
                prototype
            }
        };
        let matches = self.media_matches(&query);
        let object = self.new_event_target(prototype);
        {
            let mut list = object.borrow_mut();
            list.define(
                "media".to_string(),
                Property::data(Value::String(query.clone().into())),
            );
            list.define("matches".to_string(), Property::data(Value::Bool(matches)));
            list.define("onchange".to_string(), Property::data(Value::Null));
        }
        self.media_queries
            .retain(|record| record.object.strong_count() != 0);
        self.media_queries.push(MediaQueryRecord {
            object: Rc::downgrade(&object),
            query,
            matches,
        });
        Value::Object(object)
    }

    fn media_matches(&self, query: &str) -> bool {
        let extras = self
            .layout_inputs
            .as_ref()
            .map(|inputs| inputs.borrow().extras.clone())
            .unwrap_or_default();
        let state = StyleState {
            viewport_width: self.viewport.width,
            viewport_height: self.viewport.height,
            color_scheme: extras.color_scheme,
            reduced_motion: extras.reduced_motion,
            forced_colors: extras.forced_colors,
            ..StyleState::default()
        };
        crate::css::media_query_matches(query, &state)
    }

    // re-evaluates live media query lists after the viewport or preferences change, firing
    // change at each whose answer flipped
    pub(crate) fn evaluate_media_queries(&mut self) -> Result<(), ScriptError> {
        self.media_queries
            .retain(|record| record.object.strong_count() != 0);
        let mut changed = Vec::new();
        for index in 0..self.media_queries.len() {
            let matches = self.media_matches(&self.media_queries[index].query);
            let record = &mut self.media_queries[index];
            if matches != record.matches {
                record.matches = matches;
                if let Some(object) = record.object.upgrade() {
                    changed.push((object, record.query.clone(), matches));
                }
            }
        }
        for (object, query, matches) in changed {
            object
                .borrow_mut()
                .define("matches".to_string(), Property::data(Value::Bool(matches)));
            let event = self.new_event(
                "MediaQueryListEvent",
                "change".to_string(),
                false,
                false,
                false,
                true,
            );
            event.set_field("matches", Value::Bool(matches));
            event.set_field("media", Value::String(query.into()));
            if let Some(target) = self.listener_target(&Value::Object(object)) {
                self.dispatch(&event, target)?;
            }
        }
        self.drain_microtasks()
    }

    pub(super) fn performance_entry(&self, entry: &PerformanceEntry) -> Value {
        plain_object([
            ("name".to_string(), Value::String(entry.name.clone().into())),
            ("entryType".to_string(), Value::String(entry.kind.into())),
            ("startTime".to_string(), Value::Number(entry.start)),
            ("duration".to_string(), Value::Number(entry.duration)),
            ("detail".to_string(), entry.detail.clone()),
        ])
    }

    pub(super) fn performance_entry_list(&self) -> Vec<PerformanceEntry> {
        self.performance_entries.clone()
    }

    fn record_performance_entry(&mut self, entry: PerformanceEntry) -> Value {
        self.performance_entries.push(entry.clone());
        self.notify_perf_observers(&entry);
        self.performance_entry(&entry)
    }

    fn performance_entries(&self, filter: impl Fn(&PerformanceEntry) -> bool) -> Value {
        Self::new_array(
            self.performance_entries
                .iter()
                .filter(|entry| filter(entry))
                .map(|entry| self.performance_entry(entry))
                .collect(),
        )
    }

    // a mark's time by name, or a number given directly
    fn performance_time(&mut self, value: Option<&Value>) -> Result<Option<f64>, ScriptError> {
        match value.map(Value::resolve) {
            None | Some(Value::Undefined) => Ok(None),
            Some(Value::Number(time)) => Ok(Some(time)),
            Some(other) => {
                let name = self.coerce_string(other)?;
                match self
                    .performance_entries
                    .iter()
                    .rev()
                    .find(|entry| entry.kind == "mark" && entry.name == name)
                {
                    Some(entry) => Ok(Some(entry.start)),
                    None => Err(self.throw_value(dom_exception(
                        "SyntaxError",
                        &format!("The mark '{name}' does not exist."),
                    ))),
                }
            }
        }
    }

    pub(super) fn performance_object(&self) -> Value {
        let methods = [
            ("now", WindowMethod::PerformanceNow),
            ("mark", WindowMethod::PerformanceMark),
            ("measure", WindowMethod::PerformanceMeasure),
            ("getEntries", WindowMethod::PerformanceEntries),
            ("getEntriesByName", WindowMethod::PerformanceEntriesByName),
            ("getEntriesByType", WindowMethod::PerformanceEntriesByType),
            ("clearMarks", WindowMethod::PerformanceClearMarks),
            ("clearMeasures", WindowMethod::PerformanceClearMeasures),
        ];
        let performance = plain_object(
            methods
                .into_iter()
                .map(|(name, method)| {
                    let length = u32::from(!matches!(
                        method,
                        WindowMethod::PerformanceNow | WindowMethod::PerformanceEntries
                    ));
                    (
                        name.to_string(),
                        self.native_function(Native::Window(method), name, length),
                    )
                })
                .chain([("timeOrigin".to_string(), Value::Number(self.time_origin))]),
        );
        self.intrinsics.hosts["Performance"]
            .borrow_mut()
            .set_prototype(Some(self.intrinsics.dom.prototype("EventTarget")));
        self.host_object(performance, "Performance")
    }

    pub(super) fn call_window_method(
        &mut self,
        method: WindowMethod,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let first = arguments.first().cloned().unwrap_or(Value::Undefined);
        let now = self.clock as f64;
        Ok(match method {
            WindowMethod::GetComputedStyle => match first.resolve() {
                Value::Element(handle) => self.computed_style_object(handle.id),
                _ => {
                    return Err(self.type_error(
                        "Failed to execute 'getComputedStyle' on 'Window': parameter 1 is not of type 'Element'.",
                    ));
                }
            },
            WindowMethod::MatchMedia => {
                let query = self.coerce_string(first)?;
                self.match_media(query)
            }
            WindowMethod::MediaAddListener | WindowMethod::MediaRemoveListener => {
                let Some(target) = self.listener_target(&this) else {
                    return Err(self.type_error("Illegal invocation"));
                };
                let arguments = vec![Value::String("change".into()), first];
                if method == WindowMethod::MediaAddListener {
                    self.add_event_listener(target, &arguments)?;
                } else {
                    self.remove_event_listener(target, &arguments)?;
                }
                Value::Undefined
            }
            WindowMethod::PropertyValue => {
                let name = self.coerce_string(first)?;
                let value = match &this {
                    Value::Object(object) => {
                        object.borrow().get(&name.to_ascii_lowercase()).cloned()
                    }
                    _ => None,
                };
                value.unwrap_or_else(|| Value::String("".into()))
            }
            WindowMethod::PerformanceNow => Value::Number(now),
            WindowMethod::VisualViewport(name) => {
                let (left, top) = self.document_scroll_offset();
                Value::Number(f64::from(match name {
                    "width" => self.viewport.width,
                    "height" => self.viewport.height,
                    "pageLeft" => left,
                    "pageTop" => top,
                    "scale" => 1.0,
                    _ => 0.0,
                }))
            }
            WindowMethod::PerformanceMark => {
                let name = self.coerce_string(first)?;
                let options = arguments.get(1).cloned().unwrap_or(Value::Undefined);
                let (mut start, mut detail) = (now, Value::Null);
                if let Value::Object(_) = &options {
                    let given = self.get_property(&options, "startTime")?;
                    if let Some(time) = self.performance_time(Some(&given))? {
                        if time < 0.0 {
                            return Err(self.type_error("Failed to execute 'mark' on 'Performance': 'startTime' cannot be negative."));
                        }
                        start = time;
                    }
                    detail = match self.get_property(&options, "detail")? {
                        Value::Undefined => Value::Null,
                        value => clone_payload(&value, MAX_CLONE_BYTES)
                            .and_then(CloneValue::into_value)?,
                    };
                }
                self.record_performance_entry(PerformanceEntry {
                    name,
                    kind: "mark",
                    start,
                    duration: 0.0,
                    detail,
                })
            }
            WindowMethod::PerformanceMeasure => {
                let name = self.coerce_string(first)?;
                let options = arguments.get(1).cloned().unwrap_or(Value::Undefined);
                // a measure takes a start mark and an end mark, or one options object
                let (start, end, duration, detail) = if let Value::Object(_) = &options {
                    let start = self.get_property(&options, "start")?;
                    let end = self.get_property(&options, "end")?;
                    let duration = match self.get_property(&options, "duration")? {
                        Value::Undefined => None,
                        value => Some(self.coerce_number(value)?),
                    };
                    let detail = self.get_property(&options, "detail")?;
                    (
                        self.performance_time(Some(&start))?,
                        self.performance_time(Some(&end))?,
                        duration,
                        detail,
                    )
                } else {
                    (
                        self.performance_time(arguments.get(1))?,
                        self.performance_time(arguments.get(2))?,
                        None,
                        Value::Undefined,
                    )
                };
                let (start, end) = match (start, end, duration) {
                    (Some(start), None, Some(duration)) => (start, start + duration),
                    (None, Some(end), Some(duration)) => (end - duration, end),
                    (start, end, _) => (start.unwrap_or(0.0), end.unwrap_or(now)),
                };
                let detail = match detail {
                    Value::Undefined => Value::Null,
                    value => {
                        clone_payload(&value, MAX_CLONE_BYTES).and_then(CloneValue::into_value)?
                    }
                };
                self.record_performance_entry(PerformanceEntry {
                    name,
                    kind: "measure",
                    start,
                    duration: end - start,
                    detail,
                })
            }
            WindowMethod::PerformanceEntries => self.performance_entries(|_| true),
            WindowMethod::PerformanceEntriesByName => {
                let name = self.coerce_string(first)?;
                let kind = match arguments.get(1).map(Value::resolve) {
                    Some(Value::Undefined) | None => None,
                    Some(kind) => Some(self.coerce_string(kind)?),
                };
                self.performance_entries(|entry| {
                    entry.name == name && kind.as_deref().is_none_or(|kind| entry.kind == kind)
                })
            }
            WindowMethod::PerformanceEntriesByType => {
                let kind = self.coerce_string(first)?;
                self.performance_entries(|entry| entry.kind == kind)
            }
            WindowMethod::PerformanceClearMarks | WindowMethod::PerformanceClearMeasures => {
                let kind = if method == WindowMethod::PerformanceClearMarks {
                    "mark"
                } else {
                    "measure"
                };
                let name = match first.resolve() {
                    Value::Undefined => None,
                    other => Some(self.coerce_string(other)?),
                };
                self.performance_entries.retain(|entry| {
                    entry.kind != kind || name.as_ref().is_some_and(|name| *name != entry.name)
                });
                Value::Undefined
            }
            WindowMethod::RequestIdleCallback => {
                if !is_callable(&first) {
                    return Err(self.type_error(
                        "Failed to execute 'requestIdleCallback' on 'Window': The callback provided as parameter 1 is not a function.",
                    ));
                }
                self.schedule_task(vec![first], TaskKind::Idle)?
            }
            WindowMethod::CancelIdleCallback => {
                let id = self.coerce_number(first)? as u64;
                self.tasks.retain(|task| task.id != id);
                Value::Undefined
            }
            WindowMethod::IdleTimeRemaining => Value::Number(IDLE_PERIOD),
        })
    }

    // the IdleDeadline handed to an idle callback; the page clock stands still while a task
    // runs, so the whole idle period remains
    pub(super) fn idle_deadline(&self) -> Value {
        let deadline = plain_object([
            (
                "timeRemaining".to_string(),
                self.native_function(
                    Native::Window(WindowMethod::IdleTimeRemaining),
                    "timeRemaining",
                    0,
                ),
            ),
            ("didTimeout".to_string(), Value::Bool(false)),
        ]);
        self.host_object(deadline, "IdleDeadline")
    }
}

// the longest idle period the HTML standard allows a callback
const IDLE_PERIOD: f64 = 50.0;

// what one element's computed values are read from
struct ComputedInputs<'a> {
    style: &'a Style,
    font_size: f32,
    geometry: Option<BoxGeometry>,
    padding: crate::layout::ResolvedEdges,
    margin: crate::layout::ResolvedEdges,
    inherited: &'a dyn Fn(&str) -> Option<String>,
    own: &'a BTreeMap<String, String>,
    // a flex or grid item, a float, an out of flow box and the root compute to a block-level
    // display whatever they declare
    blockified: bool,
}

fn computed_value(property: &str, inputs: &ComputedInputs<'_>) -> String {
    let ComputedInputs {
        style,
        font_size,
        geometry,
        padding,
        margin,
        inherited,
        own,
        blockified,
    } = *inputs;
    let border = geometry.map_or_else(|| used_border(style), |geometry| geometry.border);
    let border_widths = [border.top, border.right, border.bottom, border.left];
    let side_index = |name: &str| {
        ["top", "right", "bottom", "left"].iter().position(|side| {
            name.strip_prefix("border-")
                .is_some_and(|rest| rest.starts_with(side))
        })
    };
    let length = |value: crate::css::Length, basis: f32| {
        value
            .resolve(basis, font_size)
            .map_or_else(|| "auto".to_string(), px)
    };
    match property {
        "display" => match style.display {
            crate::Display::Inline | crate::Display::InlineBlock if blockified => {
                crate::Display::Block
            }
            crate::Display::InlineFlex if blockified => crate::Display::Flex,
            crate::Display::InlineGrid if blockified => crate::Display::Grid,
            crate::Display::InlineTable if blockified => crate::Display::Table,
            display => display,
        }
        .name()
        .to_string(),
        "position" => match style.position {
            Position::Static => "static",
            Position::Relative => "relative",
            Position::Sticky => "sticky",
            Position::Absolute => "absolute",
            Position::Fixed => "fixed",
        }
        .to_string(),
        "color" => color_text(style.color),
        "background-color" => color_text(style.background),
        "opacity" => number_to_string(f64::from(style.opacity)),
        "font-size" => px(font_size),
        "font-weight" => style.font_weight.0.to_string(),
        "font-style" => if style.italic { "italic" } else { "normal" }.to_string(),
        "font-family" => crate::font::font_stack_names(style.font_family).map_or_else(
            || "sans-serif".to_string(),
            |names| {
                names
                    .iter()
                    .map(|name| family_text(name))
                    .collect::<Vec<_>>()
                    .join(", ")
            },
        ),
        // each shadow as its colour, offsets, blur and spread, inset last
        "box-shadow" => style.shadows.map_or_else(
            || "none".to_string(),
            |list| {
                list.shadows()
                    .iter()
                    .map(|shadow| {
                        let lengths =
                            [shadow.offset_x, shadow.offset_y, shadow.blur, shadow.spread]
                                .map(|value| length(value, 0.0))
                                .join(" ");
                        let inset = if shadow.inset { " inset" } else { "" };
                        format!("{} {lengths}{inset}", color_text(shadow.color))
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            },
        ),
        "line-height" => style
            .line_height
            .resolve(font_size, font_size)
            .map_or_else(|| "normal".to_string(), px),
        "width" | "height" => match geometry {
            Some(geometry) => {
                let (extent, edges) = if property == "width" {
                    (
                        geometry.rect.width,
                        padding.left + padding.right + border.left + border.right,
                    )
                } else {
                    (
                        geometry.rect.height,
                        padding.top + padding.bottom + border.top + border.bottom,
                    )
                };
                // the resolved value is the size box-sizing names: the border box or the content
                if style.box_sizing == crate::css::BoxSizing::BorderBox {
                    px(extent)
                } else {
                    px((extent - edges).max(0.0))
                }
            }
            None => "auto".to_string(),
        },
        "margin-top" => px(margin.top),
        "margin-right" => px(margin.right),
        "margin-bottom" => px(margin.bottom),
        "margin-left" => px(margin.left),
        "padding-top" => px(padding.top),
        "padding-right" => px(padding.right),
        "padding-bottom" => px(padding.bottom),
        "padding-left" => px(padding.left),
        corner if corner.ends_with("-radius") => {
            let index = match corner {
                "border-top-left-radius" => 0,
                "border-top-right-radius" => 1,
                "border-bottom-right-radius" => 2,
                _ => 3,
            };
            let (horizontal, vertical) = style.border_radius[index];
            let rect = geometry.map(|geometry| geometry.rect).unwrap_or_default();
            let (horizontal, vertical) = (
                length(horizontal, rect.width),
                length(vertical, rect.height),
            );
            if horizontal == vertical {
                horizontal
            } else {
                format!("{horizontal} {vertical}")
            }
        }
        side if side.ends_with("-width") => {
            px(side_index(side).map_or(0.0, |index| border_widths[index]))
        }
        side if side.ends_with("-style") => side_index(side)
            .map_or("none", |index| style.border[index].style.name())
            .to_string(),
        side if side.ends_with("-color") => color_text(
            side_index(side)
                .and_then(|index| style.border[index].color)
                .unwrap_or(style.color),
        ),
        "top" => length(style.inset.top, 0.0),
        "right" => length(style.inset.right, 0.0),
        "bottom" => length(style.inset.bottom, 0.0),
        "left" => length(style.inset.left, 0.0),
        "z-index" => style
            .z_index
            .map_or_else(|| "auto".to_string(), |index| index.to_string()),
        "overflow-x" => style.overflow_axes()[0].name().to_string(),
        "overflow-y" => style.overflow_axes()[1].name().to_string(),
        "flex-direction" => match style.flex_direction {
            FlexDirection::Row => "row",
            FlexDirection::RowReverse => "row-reverse",
            FlexDirection::Column => "column",
            FlexDirection::ColumnReverse => "column-reverse",
        }
        .to_string(),
        "flex-grow" => number_to_string(f64::from(style.flex_grow)),
        "justify-content" => match style.justify_content {
            JustifyContent::FlexStart => own
                .get("justify-content")
                .cloned()
                .unwrap_or_else(|| "normal".to_string()),
            JustifyContent::Center => "center".to_string(),
            JustifyContent::FlexEnd => "flex-end".to_string(),
            JustifyContent::SpaceBetween => "space-between".to_string(),
            JustifyContent::SpaceAround => "space-around".to_string(),
            JustifyContent::SpaceEvenly => "space-evenly".to_string(),
        },
        "align-items" => match style.align_items {
            AlignItems::Stretch => own
                .get("align-items")
                .cloned()
                .unwrap_or_else(|| "normal".to_string()),
            AlignItems::FlexStart => "flex-start".to_string(),
            AlignItems::Center => "center".to_string(),
            AlignItems::FlexEnd => "flex-end".to_string(),
            AlignItems::Baseline => "baseline".to_string(),
        },
        "row-gap" | "column-gap" => style
            .gap
            .resolve(0.0, font_size)
            .map_or_else(|| "normal".to_string(), px),
        "text-align" => match style.text_align {
            TextAlign::Start => inherited("text-align").unwrap_or_else(|| "start".to_string()),
            TextAlign::End => "end".to_string(),
            TextAlign::Left => "left".to_string(),
            TextAlign::Center => "center".to_string(),
            TextAlign::Right => "right".to_string(),
        },
        "text-transform" => match style.text_transform {
            TextTransform::None => "none",
            TextTransform::Uppercase => "uppercase",
            TextTransform::Lowercase => "lowercase",
            TextTransform::Capitalize => "capitalize",
        }
        .to_string(),
        _ => style
            .letter_spacing
            .resolve(0.0, font_size)
            .filter(|spacing| *spacing != 0.0)
            .map_or_else(|| "normal".to_string(), px),
    }
}

// how far a scrollport moves so a span inside it lines up as the alignment asks; nearest
// moves the least that shows the span, or not at all when it already shows or overfills
fn align_delta(start: f32, size: f32, port: f32, port_size: f32, alignment: &str) -> f32 {
    let (end, port_end) = (start + size, port + port_size);
    match alignment {
        "start" => start - port,
        "end" => end - port_end,
        "center" => (start + end - port - port_end) / 2.0,
        _ if (start < port) == (end > port_end) => 0.0,
        _ if (start < port) == (size <= port_size) => start - port,
        _ => end - port_end,
    }
}

// the styles getComputedStyle worked out, kept for the next read while the document version,
// sheet and state they were worked out under stand
pub(super) struct KeptComputed {
    version: u64,
    sheet: crate::navigation::SheetIdentity,
    state: Box<StyleState>,
    styles: crate::css::KeptStyles,
}

#[cfg(test)]
mod tests {
    use super::css_number;

    #[test]
    fn css_numbers_print_as_percent_g_with_six_digits() {
        let printed: Vec<String> = [
            1.0,
            0.5,
            1.0 / 3.0,
            1e21,
            -0.0,
            1234567.5,
            123456.7,
            0.0001,
            0.00001234,
            -2.5,
        ]
        .into_iter()
        .map(css_number)
        .collect();
        assert_eq!(
            printed,
            [
                "1",
                "0.5",
                "0.333333",
                "1e+21",
                "0",
                "1.23457e+06",
                "123457",
                "0.0001",
                "1.234e-05",
                "-2.5"
            ]
        );
    }
}
