// what script can measure: element boxes from a layout of the current document, computed
// style, media queries, the performance timeline and idle callbacks

use super::object::{JsObject, ObjectClass, Property, new_object};
use super::operations::{is_callable, number_to_string, plain_object};
use super::realm::Native;
use super::*;
use crate::css::{
    AlignItems, Display, FlexDirection, FontWeight, JustifyContent, Overflow, Position, Style,
    StyleState, Stylesheet, TextAlign, TextTransform,
};
use crate::font::FontSet;
use crate::layout::{BoxGeometry, LayoutDocument, Rect, Size, layout_with_state, resolve_edges};

// what the embedder adds to a layout beyond the document and the runtime's own state
pub(crate) struct LayoutInputs {
    pub(crate) stylesheet: Rc<Stylesheet>,
    pub(crate) fonts: Rc<FontSet>,
    // hover, preferences, running transitions and animations, and decoded image sizes
    pub(crate) extras: StyleState,
}

// a layout of the document as it stood, with every node's boxes indexed
pub(super) struct MeasuredLayout {
    version: u64,
    state: StyleState,
    boxes: HashMap<NodeId, Vec<BoxGeometry>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowMethod {
    GetComputedStyle,
    MatchMedia,
    MediaAddListener,
    MediaRemoveListener,
    PropertyValue,
    PerformanceNow,
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
    ("box-shadow", "none"),
    ("outline-style", "none"),
    ("outline-width", "0px"),
    ("content", "normal"),
    ("will-change", "auto"),
    ("user-select", "auto"),
    ("object-fit", "fill"),
    ("vertical-align", "baseline"),
    ("text-overflow", "clip"),
    ("resize", "none"),
    ("appearance", "none"),
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
    ("font-style", "normal"),
    ("font-variant", "normal"),
    ("list-style-type", "disc"),
    ("text-indent", "0px"),
    ("caret-color", "auto"),
    ("color-scheme", "normal"),
];

const COMPUTED: &[&str] = &[
    "display",
    "position",
    "color",
    "background-color",
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

fn px(value: f32) -> String {
    format!("{}px", number_to_string(f64::from(value)))
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

pub(super) fn rect_object(rect: Rect) -> Value {
    let entries = [
        ("x", rect.x),
        ("y", rect.y),
        ("width", rect.width),
        ("height", rect.height),
        ("top", rect.y),
        ("right", rect.x + rect.width),
        ("bottom", rect.y + rect.height),
        ("left", rect.x),
    ];
    plain_object(
        entries
            .into_iter()
            .map(|(name, value)| (name.to_string(), Value::Number(f64::from(value)))),
    )
}

impl ScriptRuntime {
    pub(crate) fn set_layout_inputs(&mut self, inputs: Rc<RefCell<LayoutInputs>>) {
        self.layout_inputs = Some(inputs);
        *self.measured.borrow_mut() = None;
    }

    // the style state a layout of the document runs with right now
    fn layout_state(&self, inputs: &LayoutInputs) -> StyleState {
        let extras = &inputs.extras;
        let mut state = self.style_state(extras.hovered);
        state.viewport_width = self.viewport.width;
        state.viewport_height = self.viewport.height;
        state.color_scheme = extras.color_scheme;
        state.reduced_motion = extras.reduced_motion;
        state.clock = self.clock;
        state.transitions = extras.transitions.clone();
        state.animation_starts = extras.animation_starts.clone();
        state.image_sizes = extras.image_sizes.clone();
        state
    }

    // a fresh layout when the document or its state changed since the last measurement
    fn measure(&self) -> Option<std::cell::Ref<'_, MeasuredLayout>> {
        let inputs = self.layout_inputs.as_ref()?.borrow();
        let state = self.layout_state(&inputs);
        let version = self.document.borrow().version();
        let stale = self
            .measured
            .borrow()
            .as_ref()
            .is_none_or(|measured| measured.version != version || measured.state != state);
        if stale {
            let layout = self.run_layout(&inputs, &state);
            *self.measured.borrow_mut() = Some(MeasuredLayout {
                version,
                state,
                boxes: layout.box_index(),
            });
        }
        std::cell::Ref::filter_map(self.measured.borrow(), Option::as_ref).ok()
    }

    fn run_layout(&self, inputs: &LayoutInputs, state: &StyleState) -> LayoutDocument {
        let _fonts = crate::font::use_fonts(inputs.fonts.clone());
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
        self.measure()
            .and_then(|measured| measured.boxes.get(&node).cloned())
            .unwrap_or_default()
    }

    pub(super) fn bounding_box(&self, node: NodeId) -> Option<Rect> {
        let boxes = self.node_boxes(node);
        let first = boxes.first()?.rect;
        Some(boxes.iter().skip(1).fold(first, |union, geometry| {
            let rect = geometry.rect;
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
        let border = geometry.border * 2.0;
        Some((
            rect,
            Rect {
                x: padding.left,
                y: padding.top,
                width: (rect.width - border - padding.left - padding.right).max(0.0),
                height: (rect.height - border - padding.top - padding.bottom).max(0.0),
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
            let name = match self.document.borrow().element(ancestor) {
                Some(element) => element.name.clone(),
                None => return None,
            };
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
            "getBoundingClientRect" => rect_object(self.bounding_box(node).unwrap_or_default()),
            "getClientRects" => Self::new_array(
                boxes()
                    .into_iter()
                    .map(|geometry| rect_object(geometry.rect))
                    .collect(),
            ),
            "clientWidth" | "clientHeight" | "clientTop" | "clientLeft" => {
                let Some(geometry) = boxes().first().copied() else {
                    return Ok(Some(Value::Number(0.0)));
                };
                let border = geometry.border;
                number(match name {
                    "clientWidth" => (geometry.scrollport.width - border * 2.0).max(0.0),
                    "clientHeight" => (geometry.scrollport.height - border * 2.0).max(0.0),
                    _ => border,
                })
            }
            "scrollWidth" | "scrollHeight" => {
                let Some(geometry) = boxes().first().copied() else {
                    return Ok(Some(Value::Number(0.0)));
                };
                let client = |extent: f32| (extent - geometry.border * 2.0).max(0.0);
                number(if name == "scrollWidth" {
                    geometry
                        .scroll_size
                        .width
                        .max(client(geometry.scrollport.width))
                } else {
                    geometry
                        .scroll_size
                        .height
                        .max(client(geometry.scrollport.height))
                })
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
                        (parent.rect.x + parent.border, parent.rect.y + parent.border)
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
        let mut values = BTreeMap::new();
        let Some(inputs) = self.layout_inputs.clone() else {
            return values;
        };
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
        let Some((style, font_size)) = inputs.stylesheet.computed_style(&document, node, &state)
        else {
            return values;
        };
        let declared = |target: NodeId| {
            inputs.stylesheet.declared_values(crate::css::StyleContext {
                document: &document,
                node: target,
                pseudo: None,
                parent_font_size: 16.0,
                parent_color: crate::Color::BLACK,
                parent_weight: FontWeight::Normal,
                parent_text: crate::css::InheritedText::default(),
                state: &state,
            })
        };
        let own = declared(node);
        let ancestors = document
            .ancestors(node)
            .filter(|&ancestor| document.element(ancestor).is_some())
            .map(declared)
            .collect::<Vec<_>>();
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
        };
        for &property in COMPUTED {
            let value = computed_value(property, &computed);
            values.insert(property.to_string(), value);
        }
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
                let prototype = new_object(JsObject::with_prototype(
                    Some(self.intrinsics.dom.prototype("EventTarget")),
                    ObjectClass::Ordinary,
                ));
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

    fn performance_entry(&self, entry: &PerformanceEntry) -> Value {
        plain_object([
            ("name".to_string(), Value::String(entry.name.clone().into())),
            ("entryType".to_string(), Value::String(entry.kind.into())),
            ("startTime".to_string(), Value::Number(entry.start)),
            ("duration".to_string(), Value::Number(entry.duration)),
        ])
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
        plain_object(
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
        )
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
            WindowMethod::PerformanceMark => {
                let name = self.coerce_string(first)?;
                let entry = PerformanceEntry {
                    name,
                    kind: "mark",
                    start: now,
                    duration: 0.0,
                };
                self.performance_entries.push(entry.clone());
                self.performance_entry(&entry)
            }
            WindowMethod::PerformanceMeasure => {
                let name = self.coerce_string(first)?;
                let start = self.performance_time(arguments.get(1))?.unwrap_or(0.0);
                let end = self.performance_time(arguments.get(2))?.unwrap_or(now);
                let entry = PerformanceEntry {
                    name,
                    kind: "measure",
                    start,
                    duration: end - start,
                };
                self.performance_entries.push(entry.clone());
                self.performance_entry(&entry)
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
        plain_object([
            (
                "timeRemaining".to_string(),
                self.native_function(
                    Native::Window(WindowMethod::IdleTimeRemaining),
                    "timeRemaining",
                    0,
                ),
            ),
            ("didTimeout".to_string(), Value::Bool(false)),
        ])
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
    } = *inputs;
    let border = geometry.map_or(style.border_width, |geometry| geometry.border);
    let length = |value: crate::css::Length, basis: f32| {
        value
            .resolve(basis, font_size)
            .map_or_else(|| "auto".to_string(), px)
    };
    match property {
        "display" => match style.display {
            Display::Block => "block",
            Display::Flex => "flex",
            Display::Grid => "grid",
            Display::Inline => "inline",
            Display::InlineBlock => "inline-block",
            Display::None => "none",
        }
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
        "font-weight" => match style.font_weight {
            FontWeight::Normal => "400",
            FontWeight::Bold => "700",
        }
        .to_string(),
        "font-family" => inherited("font-family").unwrap_or_else(|| "sans-serif".to_string()),
        "line-height" => style
            .line_height
            .resolve(font_size, font_size)
            .map_or_else(|| "normal".to_string(), px),
        "width" | "height" => match geometry {
            Some(geometry) => {
                let (extent, edges) = if property == "width" {
                    (geometry.rect.width, padding.left + padding.right)
                } else {
                    (geometry.rect.height, padding.top + padding.bottom)
                };
                px((extent - border * 2.0 - edges).max(0.0))
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
        side if side.ends_with("-width") => px(border),
        side if side.ends_with("-style") => if border > 0.0 { "solid" } else { "none" }.to_string(),
        side if side.ends_with("-color") => color_text(style.border_color),
        corner if corner.ends_with("-radius") => length(style.border_radius, 0.0),
        "top" => length(style.inset.top, 0.0),
        "right" => length(style.inset.right, 0.0),
        "bottom" => length(style.inset.bottom, 0.0),
        "left" => length(style.inset.left, 0.0),
        "z-index" => style
            .z_index
            .map_or_else(|| "auto".to_string(), |index| index.to_string()),
        "overflow-x" | "overflow-y" => match style.overflow {
            Overflow::Visible => "visible",
            Overflow::Hidden => "hidden",
            Overflow::Auto => "auto",
        }
        .to_string(),
        "flex-direction" => match style.flex_direction {
            FlexDirection::Row => "row",
            FlexDirection::Column => "column",
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
        },
        "align-items" => match style.align_items {
            AlignItems::Stretch => own
                .get("align-items")
                .cloned()
                .unwrap_or_else(|| "normal".to_string()),
            AlignItems::FlexStart => "flex-start".to_string(),
            AlignItems::Center => "center".to_string(),
            AlignItems::FlexEnd => "flex-end".to_string(),
        },
        "row-gap" | "column-gap" => style
            .gap
            .resolve(0.0, font_size)
            .map_or_else(|| "normal".to_string(), px),
        "text-align" => match style.text_align {
            TextAlign::Left => inherited("text-align").unwrap_or_else(|| "start".to_string()),
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
