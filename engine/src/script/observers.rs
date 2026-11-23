// MutationObserver, ResizeObserver and IntersectionObserver: the document journals its
// mutations for the first, and the other two compare a fresh layout at each rendering step
use super::object::{JsObject, ObjectClass, ObjectHandle, Property, Slot, new_object};
use super::operations::{is_callable, is_primitive, plain_object};
use super::realm::{Builder, Native};
use super::*;
use crate::dom::{Mutation, MutationEntry};
use crate::layout::Rect;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ObserverKind {
    Mutation,
    Resize,
    Intersection,
}

impl ObserverKind {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Mutation => "MutationObserver",
            Self::Resize => "ResizeObserver",
            Self::Intersection => "IntersectionObserver",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ObserverMethod {
    Observe,
    Unobserve,
    Disconnect,
    TakeRecords,
}

const METHODS: &[(&str, ObserverMethod, u32)] = &[
    ("observe", ObserverMethod::Observe, 1),
    ("unobserve", ObserverMethod::Unobserve, 1),
    ("disconnect", ObserverMethod::Disconnect, 0),
    ("takeRecords", ObserverMethod::TakeRecords, 0),
];

pub(super) struct ObserverInterfaces {
    prototypes: [ObjectHandle; 3],
}

pub(super) fn create(
    build: &Builder,
    object_prototype: &ObjectHandle,
    globals: &mut Vec<(&'static str, Value)>,
) -> ObserverInterfaces {
    let prototypes = [
        ObserverKind::Mutation,
        ObserverKind::Resize,
        ObserverKind::Intersection,
    ]
    .map(|kind| {
        let prototype = new_object(JsObject::with_prototype(
            Some(object_prototype.clone()),
            ObjectClass::Ordinary,
        ));
        for &(name, method, length) in METHODS {
            // a MutationObserver has no unobserve
            if kind == ObserverKind::Mutation && method == ObserverMethod::Unobserve {
                continue;
            }
            let function = build.function(Native::Observer(method), name, length);
            prototype.borrow_mut().define(
                name.to_string(),
                Property {
                    enumerable: true,
                    ..Property::data(Value::Object(function))
                },
            );
        }
        prototype.borrow_mut().define(
            "@@toStringTag".to_string(),
            Property {
                slot: Slot::Data(Value::String(kind.name().into())),
                enumerable: false,
                writable: false,
                configurable: true,
            },
        );
        let constructor = build.constructor(
            Native::ObserverConstructor(kind),
            kind.name(),
            1,
            &prototype,
        );
        globals.push((kind.name(), Value::Object(constructor)));
        prototype
    });
    ObserverInterfaces { prototypes }
}

#[derive(Clone, Debug, Default)]
struct MutationOptions {
    child_list: bool,
    attributes: bool,
    character_data: bool,
    subtree: bool,
    attribute_old_value: bool,
    character_data_old_value: bool,
    attribute_filter: Option<Vec<String>>,
}

enum Observation {
    Mutation {
        node: NodeId,
        options: MutationOptions,
    },
    Resize {
        node: NodeId,
        border_box: bool,
        // the last size reported, starting at zero so an empty box reports nothing
        last: (f32, f32),
    },
    Intersection {
        node: NodeId,
        // -1 until the first notification
        previous_index: i64,
        previous_intersecting: bool,
    },
}

impl Observation {
    fn node(&self) -> NodeId {
        match self {
            Self::Mutation { node, .. }
            | Self::Resize { node, .. }
            | Self::Intersection { node, .. } => *node,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct IntersectionConfig {
    root: Option<NodeId>,
    // top, right, bottom, left; a percentage is a fraction of the root's size
    margin: [(f32, bool); 4],
    thresholds: Vec<f64>,
}

pub(super) struct ObserverRecord {
    kind: ObserverKind,
    object: Weak<RefCell<JsObject>>,
    // the observer stays alive while it observes something, as the standard requires
    held: Option<ObjectHandle>,
    callback: Value,
    observations: Vec<Observation>,
    queue: Vec<Value>,
    intersection: IntersectionConfig,
}

fn intersect(left: Rect, right: Rect) -> Option<Rect> {
    let x = left.x.max(right.x);
    let y = left.y.max(right.y);
    let right_edge = (left.x + left.width).min(right.x + right.width);
    let bottom = (left.y + left.height).min(right.y + right.height);
    // edge-adjacent boxes still intersect, with an empty rectangle
    (right_edge >= x && bottom >= y).then_some(Rect {
        x,
        y,
        width: right_edge - x,
        height: bottom - y,
    })
}

fn size_object(width: f32, height: f32) -> Value {
    ScriptRuntime::new_array(vec![plain_object([
        ("inlineSize".to_string(), Value::Number(f64::from(width))),
        ("blockSize".to_string(), Value::Number(f64::from(height))),
    ])])
}

impl ScriptRuntime {
    pub(super) fn construct_observer(
        &mut self,
        kind: ObserverKind,
        arguments: Vec<Value>,
        new_target: Value,
    ) -> Result<Value, ScriptError> {
        let callback = arguments.first().cloned().unwrap_or(Value::Undefined);
        if !is_callable(&callback) {
            return Err(self.type_error(format!(
                "Failed to construct '{}': parameter 1 is not of type 'Function'.",
                kind.name()
            )));
        }
        let intersection = if kind == ObserverKind::Intersection {
            self.intersection_config(arguments.get(1))?
        } else {
            IntersectionConfig::default()
        };
        let prototype = match new_target {
            Value::Object(_) => self.prototype_from_constructor(&new_target)?,
            _ => self.intrinsics.observers.prototypes[kind as usize].clone(),
        };
        self.next_observer += 1;
        let id = self.next_observer;
        let object = new_object(JsObject::with_prototype(
            Some(prototype),
            ObjectClass::Observer(id),
        ));
        if kind == ObserverKind::Intersection {
            let root = intersection
                .root
                .map_or(Value::Null, |root| self.node_value(root));
            let margin = intersection
                .margin
                .iter()
                .map(|&(value, percent)| format!("{}{}", value, if percent { "%" } else { "px" }))
                .collect::<Vec<_>>()
                .join(" ");
            let thresholds = Self::new_array(
                intersection
                    .thresholds
                    .iter()
                    .map(|&threshold| Value::Number(threshold))
                    .collect(),
            );
            let mut observer = object.borrow_mut();
            observer.define("root".to_string(), Property::data(root));
            observer.define(
                "rootMargin".to_string(),
                Property::data(Value::String(margin.into())),
            );
            observer.define("thresholds".to_string(), Property::data(thresholds));
        }
        self.observers.insert(
            id,
            ObserverRecord {
                kind,
                object: Rc::downgrade(&object),
                held: None,
                callback,
                observations: Vec::new(),
                queue: Vec::new(),
                intersection,
            },
        );
        Ok(Value::Object(object))
    }

    fn intersection_config(
        &mut self,
        options: Option<&Value>,
    ) -> Result<IntersectionConfig, ScriptError> {
        let mut config = IntersectionConfig {
            thresholds: vec![0.0],
            ..IntersectionConfig::default()
        };
        let Some(options) = options
            .map(Value::resolve)
            .filter(|value| !is_primitive(value))
        else {
            return Ok(config);
        };
        config.root = match self.get_property(&options, "root")?.resolve() {
            Value::Element(handle) => Some(handle.id),
            Value::Document => Some(self.document_node()),
            _ => None,
        };
        let margin = match self.get_property(&options, "rootMargin")? {
            Value::Undefined => "0px".to_string(),
            value => self.coerce_string(value)?,
        };
        let mut parts = Vec::new();
        for token in margin.split_whitespace() {
            let parsed = if let Some(number) = token.strip_suffix("px") {
                number.parse::<f32>().ok().map(|value| (value, false))
            } else if let Some(number) = token.strip_suffix('%') {
                number.parse::<f32>().ok().map(|value| (value, true))
            } else {
                None
            };
            match parsed {
                Some(part) => parts.push(part),
                None => {
                    return Err(self.throw_value(dom_exception(
                        "SyntaxError",
                        "rootMargin must be specified in pixels or percent.",
                    )));
                }
            }
        }
        config.margin = match parts.as_slice() {
            [] => [(0.0, false); 4],
            [all] => [*all; 4],
            [vertical, horizontal] => [*vertical, *horizontal, *vertical, *horizontal],
            [top, horizontal, bottom] => [*top, *horizontal, *bottom, *horizontal],
            [top, right, bottom, left, ..] => [*top, *right, *bottom, *left],
        };
        let threshold = self.get_property(&options, "threshold")?;
        let mut thresholds = match threshold.resolve() {
            Value::Undefined => vec![0.0],
            Value::Array(values) => {
                let values = values.borrow().clone();
                let mut numbers = Vec::new();
                for value in values {
                    numbers.push(self.coerce_number(value)?);
                }
                numbers
            }
            value => vec![self.coerce_number(value)?],
        };
        if thresholds.iter().any(|value| !(0.0..=1.0).contains(value)) {
            return Err(self.range_error(
                "Failed to construct 'IntersectionObserver': Threshold values must be numbers between 0 and 1",
            ));
        }
        if thresholds.is_empty() {
            thresholds.push(0.0);
        }
        thresholds.sort_by(f64::total_cmp);
        config.thresholds = thresholds;
        Ok(config)
    }

    fn this_observer(&mut self, this: &Value) -> Result<u32, ScriptError> {
        if let Value::Object(object) = this
            && let ObjectClass::Observer(id) = object.borrow().class
        {
            return Ok(id);
        }
        Err(self.type_error("Illegal invocation"))
    }

    fn mutation_options(
        &mut self,
        options: Option<&Value>,
    ) -> Result<MutationOptions, ScriptError> {
        let options = options.map(Value::resolve).unwrap_or(Value::Undefined);
        let mut read = |name: &str| -> Result<Option<bool>, ScriptError> {
            if is_primitive(&options) {
                return Ok(None);
            }
            Ok(match self.get_property(&options, name)? {
                Value::Undefined => None,
                value => Some(value.truthy()),
            })
        };
        let child_list = read("childList")?.unwrap_or(false);
        let attributes = read("attributes")?;
        let character_data = read("characterData")?;
        let subtree = read("subtree")?.unwrap_or(false);
        let attribute_old_value = read("attributeOldValue")?;
        let character_data_old_value = read("characterDataOldValue")?;
        let attribute_filter = if is_primitive(&options) {
            None
        } else {
            match self.get_property(&options, "attributeFilter")?.resolve() {
                Value::Undefined => None,
                Value::Array(values) => {
                    let values = values.borrow().clone();
                    let mut names = Vec::new();
                    for value in values {
                        names.push(self.coerce_string(value)?);
                    }
                    Some(names)
                }
                _ => {
                    return Err(self.type_error(
                        "Failed to execute 'observe' on 'MutationObserver': The provided value cannot be converted to a sequence.",
                    ));
                }
            }
        };
        let attributes =
            attributes.unwrap_or(attribute_old_value.is_some() || attribute_filter.is_some());
        let character_data = character_data.unwrap_or(character_data_old_value.is_some());
        let failure = if !child_list && !attributes && !character_data {
            Some(
                "The options object must set at least one of 'attributes', 'characterData', or 'childList' to true.",
            )
        } else if attribute_old_value == Some(true) && !attributes {
            Some(
                "The options object may only set 'attributeOldValue' to true when 'attributes' is true or not present.",
            )
        } else if attribute_filter.is_some() && !attributes {
            Some(
                "The options object may only set 'attributeFilter' when 'attributes' is true or not present.",
            )
        } else if character_data_old_value == Some(true) && !character_data {
            Some(
                "The options object may only set 'characterDataOldValue' to true when 'characterData' is true or not present.",
            )
        } else {
            None
        };
        if let Some(message) = failure {
            return Err(self.type_error(format!(
                "Failed to execute 'observe' on 'MutationObserver': {message}"
            )));
        }
        Ok(MutationOptions {
            child_list,
            attributes,
            character_data,
            subtree,
            attribute_old_value: attribute_old_value.unwrap_or(false),
            character_data_old_value: character_data_old_value.unwrap_or(false),
            attribute_filter,
        })
    }

    pub(super) fn call_observer_method(
        &mut self,
        method: ObserverMethod,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let id = self.this_observer(&this)?;
        let kind = self.observers[&id].kind;
        let target = match arguments.first().map(Value::resolve) {
            Some(Value::Element(handle)) => Some(handle.id),
            Some(Value::Document) => Some(self.document_node()),
            _ => None,
        };
        match method {
            ObserverMethod::Observe => {
                let Some(node) = target else {
                    return Err(self.type_error(format!(
                        "Failed to execute 'observe' on '{}': parameter 1 is not of type '{}'.",
                        kind.name(),
                        if kind == ObserverKind::Mutation {
                            "Node"
                        } else {
                            "Element"
                        }
                    )));
                };
                let observation = match kind {
                    ObserverKind::Mutation => Observation::Mutation {
                        node,
                        options: self.mutation_options(arguments.get(1))?,
                    },
                    ObserverKind::Resize => {
                        let border_box = match arguments.get(1).map(Value::resolve) {
                            Some(options) if !is_primitive(&options) => {
                                let value = self.get_property(&options, "box")?;
                                matches!(value, Value::String(ref name) if &**name == "border-box")
                            }
                            _ => false,
                        };
                        Observation::Resize {
                            node,
                            border_box,
                            last: (0.0, 0.0),
                        }
                    }
                    ObserverKind::Intersection => Observation::Intersection {
                        node,
                        previous_index: -1,
                        previous_intersecting: false,
                    },
                };
                let record = self.observers.get_mut(&id).expect("observer is registered");
                let existing = record
                    .observations
                    .iter()
                    .position(|current| current.node() == node);
                match (existing, kind) {
                    (Some(index), ObserverKind::Mutation) => {
                        record.observations[index] = observation;
                    }
                    (Some(_), _) => {}
                    (None, _) => record.observations.push(observation),
                }
                record.held = record.object.upgrade();
            }
            ObserverMethod::Unobserve => {
                let record = self.observers.get_mut(&id).expect("observer is registered");
                if let Some(node) = target {
                    record.observations.retain(|current| current.node() != node);
                }
                if record.observations.is_empty() {
                    record.held = None;
                }
            }
            ObserverMethod::Disconnect => {
                let record = self.observers.get_mut(&id).expect("observer is registered");
                record.observations.clear();
                record.queue.clear();
                record.held = None;
            }
            ObserverMethod::TakeRecords => {
                self.queue_mutation_records();
                let record = self.observers.get_mut(&id).expect("observer is registered");
                let records = std::mem::take(&mut record.queue);
                return Ok(Self::new_array(records));
            }
        }
        let observing = self
            .observers
            .values()
            .any(|record| record.kind == ObserverKind::Mutation && !record.observations.is_empty());
        self.document.borrow_mut().observe_mutations(observing);
        Ok(Value::Undefined)
    }

    fn mutation_record(&self, entry: &MutationEntry, options: &MutationOptions) -> Value {
        let document = self.document.borrow();
        let node = |id: Option<NodeId>| {
            id.filter(|&id| document.contains(id))
                .map_or(Value::Null, |id| self.node_value(id))
        };
        let list = |nodes: &[NodeId]| {
            self.node_list(
                nodes
                    .iter()
                    .copied()
                    .filter(|&id| document.contains(id))
                    .collect(),
            )
        };
        let (kind, added, removed, previous, next, name, old) = match &entry.mutation {
            Mutation::ChildList {
                added,
                removed,
                previous,
                next,
            } => (
                "childList",
                list(added),
                list(removed),
                node(*previous),
                node(*next),
                Value::Null,
                Value::Null,
            ),
            Mutation::Attribute { name, old } => (
                "attributes",
                list(&[]),
                list(&[]),
                Value::Null,
                Value::Null,
                Value::String(name.clone().into()),
                match old {
                    Some(old) if options.attribute_old_value => Value::String(old.clone().into()),
                    _ => Value::Null,
                },
            ),
            Mutation::CharacterData { old } => (
                "characterData",
                list(&[]),
                list(&[]),
                Value::Null,
                Value::Null,
                Value::Null,
                if options.character_data_old_value {
                    Value::String(old.clone().into())
                } else {
                    Value::Null
                },
            ),
        };
        plain_object([
            ("type".to_string(), Value::String(kind.into())),
            ("target".to_string(), node(Some(entry.target))),
            ("addedNodes".to_string(), added),
            ("removedNodes".to_string(), removed),
            ("previousSibling".to_string(), previous),
            ("nextSibling".to_string(), next),
            ("attributeName".to_string(), name),
            ("attributeNamespace".to_string(), Value::Null),
            ("oldValue".to_string(), old),
        ])
    }

    // the options of the first registration of an observer that wants this mutation
    fn interested(observations: &[Observation], entry: &MutationEntry) -> Option<MutationOptions> {
        observations.iter().find_map(|observation| {
            let Observation::Mutation { node, options } = observation else {
                return None;
            };
            let reaches =
                entry.target == *node || (options.subtree && entry.ancestors.contains(node));
            let wanted = match &entry.mutation {
                Mutation::ChildList { .. } => options.child_list,
                Mutation::Attribute { name, .. } => {
                    options.attributes
                        && options
                            .attribute_filter
                            .as_ref()
                            .is_none_or(|filter| filter.contains(name))
                }
                Mutation::CharacterData { .. } => options.character_data,
            };
            (reaches && wanted).then(|| options.clone())
        })
    }

    pub(super) fn mutation_records_waiting(&self) -> bool {
        self.observers
            .values()
            .any(|record| !record.queue.is_empty())
    }

    fn observer_ids(&self) -> Vec<u32> {
        let mut ids = self.observers.keys().copied().collect::<Vec<_>>();
        ids.sort_unstable();
        ids
    }

    // turns journaled mutations into records queued on the observers that want them
    fn queue_mutation_records(&mut self) {
        let entries = self.document.borrow_mut().take_mutations();
        // a page may hold thousands of intersection and resize observers, which no mutation
        // concerns, so each entry is checked against the mutation observers alone
        let ids: Vec<_> = self
            .observer_ids()
            .into_iter()
            .filter(|id| {
                self.observers
                    .get(id)
                    .is_some_and(|record| record.kind == ObserverKind::Mutation)
            })
            .collect();
        if ids.is_empty() {
            return;
        }
        for entry in &entries {
            for &id in &ids {
                let options = match self.observers.get(&id) {
                    Some(record) => Self::interested(&record.observations, entry),
                    None => None,
                };
                if let Some(options) = options {
                    let value = self.mutation_record(entry, &options);
                    if let Some(record) = self.observers.get_mut(&id) {
                        record.queue.push(value);
                    }
                }
            }
        }
    }

    // calls each mutation observer with the records queued for it, in creation order
    pub(super) fn notify_mutation_observers(&mut self) -> Result<(), ScriptError> {
        self.queue_mutation_records();
        for id in self.observer_ids() {
            let Some(record) = self.observers.get_mut(&id) else {
                continue;
            };
            if record.queue.is_empty() {
                continue;
            }
            let records = Self::new_array(std::mem::take(&mut record.queue));
            let callback = record.callback.clone();
            let observer = record.object.upgrade().map_or(Value::Null, Value::Object);
            self.invoke_observer(callback, records, observer)?;
        }
        Ok(())
    }

    fn invoke_observer(
        &mut self,
        callback: Value,
        entries: Value,
        observer: Value,
    ) -> Result<(), ScriptError> {
        match self.call_function(callback, observer.clone(), vec![entries, observer]) {
            Ok(_) => Ok(()),
            Err(error) if self.interrupted => Err(error),
            Err(error) => {
                self.report_exception(error);
                Ok(())
            }
        }
    }

    // the resize and intersection steps of updating the rendering
    pub(super) fn run_layout_observers(&mut self) -> Result<(), ScriptError> {
        self.observers
            .retain(|_, record| record.held.is_some() || record.object.strong_count() != 0);
        let ids = self
            .observer_ids()
            .into_iter()
            .filter(|id| {
                let record = &self.observers[id];
                record.kind != ObserverKind::Mutation && !record.observations.is_empty()
            })
            .collect::<Vec<_>>();
        let (resize, intersection): (Vec<u32>, Vec<u32>) = ids
            .into_iter()
            .partition(|id| self.observers[id].kind == ObserverKind::Resize);
        for id in resize {
            let entries = self.resize_entries(id);
            self.notify_observer(id, entries)?;
        }
        // every intersection observer reads the same layout before any callback runs, as the
        // update intersection observations steps do, so a callback that changes the page
        // does not lay it out again for each observer after it
        if intersection.is_empty() {
            return Ok(());
        }
        let pending = self.with_pinned_measure(|runtime| {
            intersection
                .into_iter()
                .map(|id| (id, runtime.intersection_entries(id)))
                .collect::<Vec<_>>()
        });
        for (id, entries) in pending {
            // a callback before this one may have disconnected it, which drops its entries
            if self
                .observers
                .get(&id)
                .is_some_and(|record| !record.observations.is_empty())
            {
                self.notify_observer(id, entries)?;
            }
        }
        Ok(())
    }

    fn notify_observer(&mut self, id: u32, entries: Vec<Value>) -> Result<(), ScriptError> {
        if entries.is_empty() {
            return Ok(());
        }
        let record = &self.observers[&id];
        let callback = record.callback.clone();
        let observer = record.object.upgrade().map_or(Value::Null, Value::Object);
        self.invoke_observer(callback, Self::new_array(entries), observer)?;
        self.drain_microtasks()
    }

    fn resize_entries(&mut self, id: u32) -> Vec<Value> {
        let nodes = self.observers[&id]
            .observations
            .iter()
            .map(Observation::node)
            .collect::<Vec<_>>();
        let mut entries = Vec::new();
        for (index, node) in nodes.into_iter().enumerate() {
            let (border, content) = self
                .box_sizes(node)
                .unwrap_or((Rect::default(), Rect::default()));
            let Some(Observation::Resize {
                border_box, last, ..
            }) = self
                .observers
                .get_mut(&id)
                .and_then(|record| record.observations.get_mut(index))
            else {
                continue;
            };
            let size = if *border_box {
                (border.width, border.height)
            } else {
                (content.width, content.height)
            };
            if size == *last {
                continue;
            }
            *last = size;
            entries.push(plain_object([
                ("target".to_string(), self.node_value(node)),
                ("contentRect".to_string(), self.rect_object(content)),
                (
                    "borderBoxSize".to_string(),
                    size_object(border.width, border.height),
                ),
                (
                    "contentBoxSize".to_string(),
                    size_object(content.width, content.height),
                ),
                (
                    "devicePixelContentBoxSize".to_string(),
                    size_object(
                        content.width * self.viewport.device_pixel_ratio,
                        content.height * self.viewport.device_pixel_ratio,
                    ),
                ),
            ]));
        }
        entries
    }

    fn intersection_entries(&mut self, id: u32) -> Vec<Value> {
        let config = self.observers[&id].intersection.clone();
        let root = match config.root {
            Some(root) => self.bounding_box(root),
            None => Some(Rect {
                x: 0.0,
                y: 0.0,
                width: self.viewport.width,
                height: self.viewport.height,
            }),
        };
        let root = root.map(|root| {
            let side = |(value, percent): (f32, bool), extent: f32| {
                if percent {
                    value / 100.0 * extent
                } else {
                    value
                }
            };
            let [top, right, bottom, left] = config.margin;
            let (top, bottom) = (side(top, root.height), side(bottom, root.height));
            let (left, right) = (side(left, root.width), side(right, root.width));
            Rect {
                x: root.x - left,
                y: root.y - top,
                width: root.width + left + right,
                height: root.height + top + bottom,
            }
        });
        let nodes = self.observers[&id]
            .observations
            .iter()
            .map(Observation::node)
            .collect::<Vec<_>>();
        let mut entries = Vec::new();
        for (index, node) in nodes.into_iter().enumerate() {
            let connected = self.document.borrow().is_connected(node);
            let target = self
                .bounding_box(node)
                .filter(|_| connected)
                .unwrap_or_default();
            let overlap = root
                .filter(|_| connected)
                .and_then(|root| intersect(target, root));
            let intersecting = overlap.is_some();
            let area = target.width * target.height;
            let ratio = match overlap {
                Some(overlap) if area > 0.0 => f64::from(overlap.width * overlap.height / area),
                Some(_) => 1.0,
                None => 0.0,
            };
            let threshold_index = config
                .thresholds
                .iter()
                .position(|&threshold| threshold > ratio)
                .unwrap_or(config.thresholds.len()) as i64;
            let Some(Observation::Intersection {
                previous_index,
                previous_intersecting,
                ..
            }) = self
                .observers
                .get_mut(&id)
                .and_then(|record| record.observations.get_mut(index))
            else {
                continue;
            };
            if threshold_index == *previous_index && intersecting == *previous_intersecting {
                continue;
            }
            *previous_index = threshold_index;
            *previous_intersecting = intersecting;
            entries.push(plain_object([
                ("time".to_string(), Value::Number(self.clock as f64)),
                (
                    "rootBounds".to_string(),
                    root.map_or(Value::Null, |rect| self.rect_object(rect)),
                ),
                ("boundingClientRect".to_string(), self.rect_object(target)),
                (
                    "intersectionRect".to_string(),
                    self.rect_object(overlap.unwrap_or_default()),
                ),
                ("isIntersecting".to_string(), Value::Bool(intersecting)),
                ("intersectionRatio".to_string(), Value::Number(ratio)),
                ("target".to_string(), self.node_value(node)),
            ]));
        }
        entries
    }
}
