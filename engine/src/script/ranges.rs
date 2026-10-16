// Range and Selection as the DOM standard defines them: boundary points are a node and an
// offset into its children or its character data, the document keeps live ranges current
// through every mutation, and the selection holds one such range with a direction

use super::object::{JsObject, ObjectClass, ObjectHandle, Property, Slot, new_object};
use super::realm::{Builder, Native};
use super::*;
use crate::dom::{Boundary, LiveRange, SharedRange};
use std::cmp::Ordering as BoundaryOrder;

#[derive(Clone, Copy, Debug)]
enum Kind {
    Get,
    Method(u32),
}

// one member of Range or Selection, carried by the native that implements it
#[derive(Clone, Copy, Debug)]
pub(super) struct RangeMember {
    entry: &'static (&'static str, Kind),
}

use Kind::{Get, Method};

static RANGE_MEMBERS: &[(&str, Kind)] = &[
    ("startContainer", Get),
    ("startOffset", Get),
    ("endContainer", Get),
    ("endOffset", Get),
    ("collapsed", Get),
    ("commonAncestorContainer", Get),
    ("setStart", Method(2)),
    ("setEnd", Method(2)),
    ("setStartBefore", Method(1)),
    ("setStartAfter", Method(1)),
    ("setEndBefore", Method(1)),
    ("setEndAfter", Method(1)),
    ("collapse", Method(0)),
    ("selectNode", Method(1)),
    ("selectNodeContents", Method(1)),
    ("compareBoundaryPoints", Method(2)),
    ("deleteContents", Method(0)),
    ("extractContents", Method(0)),
    ("cloneContents", Method(0)),
    ("insertNode", Method(1)),
    ("surroundContents", Method(1)),
    ("cloneRange", Method(0)),
    ("detach", Method(0)),
    ("isPointInRange", Method(2)),
    ("comparePoint", Method(2)),
    ("intersectsNode", Method(1)),
    ("createContextualFragment", Method(1)),
    ("toString", Method(0)),
];

static SELECTION_MEMBERS: &[(&str, Kind)] = &[
    ("anchorNode", Get),
    ("anchorOffset", Get),
    ("focusNode", Get),
    ("focusOffset", Get),
    ("isCollapsed", Get),
    ("rangeCount", Get),
    ("type", Get),
    ("direction", Get),
    ("getRangeAt", Method(1)),
    ("addRange", Method(1)),
    ("removeRange", Method(1)),
    ("removeAllRanges", Method(0)),
    ("empty", Method(0)),
    ("collapse", Method(1)),
    ("setPosition", Method(1)),
    ("collapseToStart", Method(0)),
    ("collapseToEnd", Method(0)),
    ("extend", Method(1)),
    ("setBaseAndExtent", Method(4)),
    ("selectAllChildren", Method(1)),
    ("deleteFromDocument", Method(0)),
    ("containsNode", Method(1)),
    ("toString", Method(0)),
];

const HOW: &[(&str, f64)] = &[
    ("START_TO_START", 0.0),
    ("START_TO_END", 1.0),
    ("END_TO_END", 2.0),
    ("END_TO_START", 3.0),
];

pub(super) struct RangeInterfaces {
    range: ObjectHandle,
    selection: ObjectHandle,
}

pub(super) fn create(
    build: &Builder,
    object_prototype: &ObjectHandle,
    globals: &mut Vec<(&'static str, Value)>,
) -> RangeInterfaces {
    let mut define = |name: &'static str, members: &'static [(&'static str, Kind)]| {
        let prototype = new_object(JsObject::with_prototype(
            Some(object_prototype.clone()),
            ObjectClass::Ordinary,
        ));
        for entry in members {
            let (member, kind) = *entry;
            let native = Native::Range(RangeMember { entry });
            let property = match kind {
                Method(length) => Property {
                    enumerable: true,
                    ..Property::data(Value::Object(build.function(native, member, length)))
                },
                Get => Property {
                    slot: Slot::Accessor {
                        get: Some(Value::Object(build.function(
                            native,
                            &format!("get {member}"),
                            0,
                        ))),
                        set: None,
                    },
                    enumerable: true,
                    writable: false,
                    configurable: true,
                },
            };
            prototype.borrow_mut().define(member.to_string(), property);
        }
        prototype.borrow_mut().define(
            "@@toStringTag".to_string(),
            Property {
                writable: false,
                ..Property::hidden(Value::String(name.into()))
            },
        );
        let constructor = build.constructor(Native::RangeConstructor(name), name, 0, &prototype);
        if name == "Range" {
            for &(constant, value) in HOW {
                for target in [&constructor, &prototype] {
                    target.borrow_mut().define(
                        constant.to_string(),
                        Property {
                            slot: Slot::Data(Value::Number(value)),
                            enumerable: true,
                            writable: false,
                            configurable: false,
                        },
                    );
                }
            }
        }
        globals.push((name, Value::Object(constructor)));
        prototype
    };
    RangeInterfaces {
        range: define("Range", RANGE_MEMBERS),
        selection: define("Selection", SELECTION_MEMBERS),
    }
}

pub(super) struct RangeRecord {
    pub(super) live: SharedRange,
    // the wrapper that carries Range.prototype and expando properties
    pub(super) object: ObjectHandle,
}

impl RangeRecord {
    pub(super) fn get(&self) -> LiveRange {
        *self.live.borrow()
    }

    fn set(&self, range: LiveRange) {
        *self.live.borrow_mut() = range;
    }
}

#[derive(Default)]
pub(super) struct SelectionState {
    pub(super) range: Option<RangeHandle>,
    // the anchor is the range end when the focus moved before it
    pub(super) backward: bool,
}

pub(super) struct SelectionRecord {
    pub(super) state: RefCell<SelectionState>,
    pub(super) object: ObjectHandle,
}

impl SelectionRecord {
    pub(super) fn range(&self) -> Option<RangeHandle> {
        self.state.borrow().range.clone()
    }

    fn set(&self, range: Option<RangeHandle>, backward: bool) {
        let mut state = self.state.borrow_mut();
        state.range = range;
        state.backward = backward;
    }

    // the anchor and focus boundaries, in that order
    pub(super) fn anchor_focus(&self) -> Option<(Boundary, Boundary)> {
        let state = self.state.borrow();
        let range = state.range.as_ref()?.get();
        Some(if state.backward {
            (range.end, range.start)
        } else {
            (range.start, range.end)
        })
    }
}

// what selectionchange compares: the selection's range object, its boundaries and direction
pub(super) type SelectionSnapshot = Option<(usize, LiveRange, bool)>;

pub(super) fn new_selection(intrinsics: &Intrinsics) -> SelectionHandle {
    Rc::new(SelectionRecord {
        state: RefCell::new(SelectionState::default()),
        object: new_object(JsObject::with_prototype(
            Some(intrinsics.ranges.selection.clone()),
            ObjectClass::Ordinary,
        )),
    })
}

fn dom_failure(name: &str, message: &str) -> Value {
    dom_exception(name, message)
}

impl ScriptRuntime {
    pub(super) fn new_range(&mut self, range: LiveRange) -> RangeHandle {
        let live = Rc::new(RefCell::new(range));
        self.document.borrow_mut().track_range(&live);
        Rc::new(RangeRecord {
            live,
            object: new_object(JsObject::with_prototype(
                Some(self.intrinsics.ranges.range.clone()),
                ObjectClass::Ordinary,
            )),
        })
    }

    pub(super) fn construct_range(
        &mut self,
        name: &str,
        new_target: Value,
    ) -> Result<Value, ScriptError> {
        if name != "Range" {
            return Err(self.type_error("Illegal constructor"));
        }
        let range = self.new_range(LiveRange::collapsed(Boundary {
            node: self.document_node(),
            offset: 0,
        }));
        if let Value::Object(_) = new_target {
            let prototype = self.prototype_from_constructor(&new_target)?;
            range.object.borrow_mut().set_prototype(Some(prototype));
        }
        Ok(Value::Range(range))
    }

    pub(super) fn selection_snapshot(&self) -> SelectionSnapshot {
        let state = self.selection.state.borrow();
        let range = state.range.as_ref()?;
        Some((Rc::as_ptr(range) as usize, range.get(), state.backward))
    }

    // fires selectionchange at the document once the selection differs from what listeners
    // last saw, which coalesces every change made since the previous checkpoint
    pub(super) fn sync_selection_change(&mut self) -> Result<(), ScriptError> {
        let current = self.selection_snapshot();
        if current == self.reported_selection {
            return Ok(());
        }
        self.reported_selection = current;
        self.dispatch_event(self.document_node(), "selectionchange")?;
        Ok(())
    }

    pub(super) fn boundary_position(&self, boundary: Boundary) -> Option<usize> {
        self.document.borrow().boundary_text_position(boundary)
    }

    fn compare(&self, left: Boundary, right: Boundary) -> Option<BoundaryOrder> {
        self.document.borrow().compare_boundaries(left, right)
    }

    fn same_root(&self, left: NodeId, right: NodeId) -> bool {
        let document = self.document.borrow();
        document.tree_root(left) == document.tree_root(right)
    }

    fn in_document(&self, node: NodeId) -> bool {
        self.document.borrow().is_connected(node)
    }

    fn range_node_argument(
        &mut self,
        arguments: &[Value],
        index: usize,
        method: &str,
        interface: &str,
    ) -> Result<NodeId, ScriptError> {
        match arguments.get(index).map(Value::resolve) {
            Some(Value::Element(handle)) => Ok(handle.id),
            Some(Value::Document) => Ok(self.document_node()),
            _ => Err(self.type_error(format!(
                "Failed to execute '{method}' on '{interface}': parameter {} is not of type 'Node'.",
                index + 1
            ))),
        }
    }

    // an unsigned long argument, wrapped modulo 2^32 as WebIDL converts it
    fn offset_argument(&mut self, arguments: &[Value], index: usize) -> Result<usize, ScriptError> {
        let value = arguments.get(index).cloned().unwrap_or(Value::Undefined);
        let number = self.coerce_number(value)?;
        if !number.is_finite() {
            return Ok(0);
        }
        Ok(number.trunc().rem_euclid(4_294_967_296.0) as usize)
    }

    // a boundary point from script, checked against the node's length
    fn checked_boundary(&mut self, node: NodeId, offset: usize) -> Result<Boundary, ScriptError> {
        let length = self.document.borrow().node_length(node);
        if offset > length {
            return Err(self.throw_value(dom_failure(
                "IndexSizeError",
                &format!("The offset {offset} is larger than the node's length ({length})."),
            )));
        }
        Ok(Boundary { node, offset })
    }

    fn boundary_argument(
        &mut self,
        arguments: &[Value],
        index: usize,
        method: &str,
        interface: &str,
    ) -> Result<Boundary, ScriptError> {
        let node = self.range_node_argument(arguments, index, method, interface)?;
        let offset = self.offset_argument(arguments, index + 1)?;
        self.checked_boundary(node, offset)
    }

    // the boundary just before or after a node, in its parent
    fn boundary_beside(&mut self, node: NodeId, after: bool) -> Result<Boundary, ScriptError> {
        let document = self.document.borrow();
        let place = document.parent(node).zip(document.index_in_parent(node));
        drop(document);
        match place {
            Some((parent, index)) => Ok(Boundary {
                node: parent,
                offset: index + usize::from(after),
            }),
            None => Err(self.throw_value(dom_failure(
                "InvalidNodeTypeError",
                "the given Node has no parent.",
            ))),
        }
    }

    fn set_range_start(&self, range: &RangeRecord, start: Boundary) {
        let mut live = range.get();
        if !self.same_root(start.node, live.start.node)
            || self.compare(start, live.end) == Some(BoundaryOrder::Greater)
        {
            live.end = start;
        }
        live.start = start;
        range.set(live);
    }

    fn set_range_end(&self, range: &RangeRecord, end: Boundary) {
        let mut live = range.get();
        if !self.same_root(end.node, live.start.node)
            || self.compare(end, live.start) == Some(BoundaryOrder::Less)
        {
            live.start = end;
        }
        live.end = end;
        range.set(live);
    }

    fn is_collapsed(range: LiveRange) -> bool {
        range.start == range.end
    }

    // a node whose whole extent lies inside the range
    fn contained(&self, node: NodeId, range: LiveRange) -> bool {
        let document = self.document.borrow();
        let length = document.node_length(node);
        document.tree_root(node) == document.tree_root(range.start.node)
            && document.compare_boundaries(Boundary { node, offset: 0 }, range.start)
                == Some(BoundaryOrder::Greater)
            && document.compare_boundaries(
                Boundary {
                    node,
                    offset: length,
                },
                range.end,
            ) == Some(BoundaryOrder::Less)
    }

    fn partially_contained(&self, node: NodeId, range: LiveRange) -> bool {
        let document = self.document.borrow();
        document.is_inclusive_ancestor(node, range.start.node)
            != document.is_inclusive_ancestor(node, range.end.node)
    }

    pub(super) fn range_text(&self, range: LiveRange) -> String {
        let document = self.document.borrow();
        let (start, end) = (range.start, range.end);
        let slice = |node: NodeId, from: usize, to: usize| -> String {
            let units = document
                .text(node)
                .unwrap_or_default()
                .encode_utf16()
                .collect::<Vec<_>>();
            let to = to.min(units.len());
            String::from_utf16_lossy(&units[from.min(to)..to])
        };
        if start.node == end.node && document.is_text(start.node) {
            return slice(start.node, start.offset, end.offset);
        }
        let mut text = String::new();
        if document.is_text(start.node) {
            text.push_str(&slice(start.node, start.offset, usize::MAX));
        }
        let root = document.tree_root(start.node);
        let contained = |node: NodeId| {
            document.compare_boundaries(Boundary { node, offset: 0 }, start)
                == Some(BoundaryOrder::Greater)
                && document.compare_boundaries(
                    Boundary {
                        node,
                        offset: document.node_length(node),
                    },
                    end,
                ) == Some(BoundaryOrder::Less)
        };
        for node in document.descendants(root) {
            if let Some(data) = document.text(node)
                && contained(node)
            {
                text.push_str(data);
            }
        }
        if document.is_text(end.node) {
            text.push_str(&slice(end.node, 0, end.offset));
        }
        text
    }

    // where an extraction or deletion leaves the range: right after the start's last
    // ancestor that does not hold the end
    fn collapse_point(&self, range: LiveRange) -> Boundary {
        let document = self.document.borrow();
        if document.is_inclusive_ancestor(range.start.node, range.end.node) {
            return range.start;
        }
        let mut reference = range.start.node;
        while let Some(parent) = document.parent(reference) {
            if document.is_inclusive_ancestor(parent, range.end.node) {
                break;
            }
            reference = parent;
        }
        Boundary {
            node: document.parent(reference).unwrap_or(reference),
            offset: document
                .index_in_parent(reference)
                .map_or(0, |index| index + 1),
        }
    }

    fn is_character_data(&self, node: NodeId) -> bool {
        self.document.borrow().character_data(node).is_some()
    }

    fn delete_range_contents(&mut self, range: &RangeRecord) -> Result<(), ScriptError> {
        let live = range.get();
        if Self::is_collapsed(live) {
            return Ok(());
        }
        let (start, end) = (live.start, live.end);
        if start.node == end.node && self.is_character_data(start.node) {
            self.document.borrow_mut().replace_data(
                start.node,
                start.offset,
                end.offset - start.offset,
                "",
            );
            return Ok(());
        }
        let removals = {
            let document = self.document.borrow();
            let common = document
                .common_ancestor(start.node, end.node)
                .unwrap_or(start.node);
            document.descendants(common).collect::<Vec<_>>()
        };
        let contained = removals
            .into_iter()
            .filter(|&node| self.contained(node, live))
            .collect::<Vec<_>>();
        let outermost = contained
            .iter()
            .copied()
            .filter(|&node| {
                self.document
                    .borrow()
                    .parent(node)
                    .is_none_or(|parent| !contained.contains(&parent))
            })
            .collect::<Vec<_>>();
        let point = self.collapse_point(live);
        if self.is_character_data(start.node) {
            let length = self.document.borrow().node_length(start.node);
            self.document.borrow_mut().replace_data(
                start.node,
                start.offset,
                length - start.offset,
                "",
            );
        }
        for node in outermost {
            self.remove_node(node)?;
        }
        if self.is_character_data(end.node) {
            self.document
                .borrow_mut()
                .replace_data(end.node, 0, end.offset, "");
        }
        range.set(LiveRange::collapsed(point));
        Ok(())
    }

    // the DOM's extract and clone the contents; extract moves the nodes into the fragment
    fn range_contents(&mut self, live: LiveRange, extract: bool) -> Result<NodeId, ScriptError> {
        let fragment = self.document.borrow_mut().create_fragment();
        if Self::is_collapsed(live) {
            return Ok(fragment);
        }
        let (start, end) = (live.start, live.end);
        if start.node == end.node && self.is_character_data(start.node) {
            let clone = self.clone_data(start.node, start.offset, end.offset)?;
            self.append_quiet(fragment, clone);
            if extract {
                self.document.borrow_mut().replace_data(
                    start.node,
                    start.offset,
                    end.offset - start.offset,
                    "",
                );
            }
            return Ok(fragment);
        }
        let (first_partial, last_partial, contained_children) = {
            let document = self.document.borrow();
            let common = document
                .common_ancestor(start.node, end.node)
                .unwrap_or(start.node);
            let children = document.children(common).to_vec();
            drop(document);
            let first = (!self
                .document
                .borrow()
                .is_inclusive_ancestor(start.node, end.node))
            .then(|| {
                children
                    .iter()
                    .copied()
                    .find(|&child| self.partially_contained(child, live))
            })
            .flatten();
            let last = (!self
                .document
                .borrow()
                .is_inclusive_ancestor(end.node, start.node))
            .then(|| {
                children
                    .iter()
                    .rev()
                    .copied()
                    .find(|&child| self.partially_contained(child, live))
            })
            .flatten();
            let contained = children
                .iter()
                .copied()
                .filter(|&child| self.contained(child, live))
                .collect::<Vec<_>>();
            (first, last, contained)
        };
        if let Some(first) = first_partial {
            if self.is_character_data(first) {
                let length = self.document.borrow().node_length(start.node);
                let clone = self.clone_data(start.node, start.offset, length)?;
                self.append_quiet(fragment, clone);
                if extract {
                    self.document.borrow_mut().replace_data(
                        start.node,
                        start.offset,
                        length - start.offset,
                        "",
                    );
                }
            } else {
                let clone = self.shallow_clone(first)?;
                self.append_quiet(fragment, clone);
                let length = self.document.borrow().node_length(first);
                let inner = self.range_contents(
                    LiveRange {
                        start,
                        end: Boundary {
                            node: first,
                            offset: length,
                        },
                    },
                    extract,
                )?;
                self.append_quiet(clone, inner);
            }
        }
        for child in contained_children {
            if extract {
                self.remove_node(child)?;
                self.append_quiet(fragment, child);
            } else {
                let clone = self
                    .document
                    .borrow_mut()
                    .clone_node(child, true)
                    .ok_or_else(|| runtime_error("range node no longer exists"))?;
                self.append_quiet(fragment, clone);
            }
        }
        if let Some(last) = last_partial {
            if self.is_character_data(last) {
                let clone = self.clone_data(end.node, 0, end.offset)?;
                self.append_quiet(fragment, clone);
                if extract {
                    self.document
                        .borrow_mut()
                        .replace_data(end.node, 0, end.offset, "");
                }
            } else {
                let clone = self.shallow_clone(last)?;
                self.append_quiet(fragment, clone);
                let inner = self.range_contents(
                    LiveRange {
                        start: Boundary {
                            node: last,
                            offset: 0,
                        },
                        end,
                    },
                    extract,
                )?;
                self.append_quiet(clone, inner);
            }
        }
        Ok(fragment)
    }

    fn shallow_clone(&mut self, node: NodeId) -> Result<NodeId, ScriptError> {
        self.document
            .borrow_mut()
            .clone_node(node, false)
            .ok_or_else(|| runtime_error("range node no longer exists"))
    }

    // a copy of character data holding only the units from start to end
    fn clone_data(
        &mut self,
        node: NodeId,
        start: usize,
        end: usize,
    ) -> Result<NodeId, ScriptError> {
        let clone = self.shallow_clone(node)?;
        let mut document = self.document.borrow_mut();
        let length = document.node_length(clone);
        document.replace_data(clone, end, length.saturating_sub(end), "");
        document.replace_data(clone, 0, start, "");
        Ok(clone)
    }

    // appends a node that no script state refers to yet, such as a fresh clone or fragment
    fn append_quiet(&mut self, parent: NodeId, child: NodeId) {
        self.document
            .borrow_mut()
            .append_child(parent, child)
            .expect("a fragment or a fresh clone accepts any node");
    }

    fn extract_range(&mut self, range: &RangeRecord) -> Result<NodeId, ScriptError> {
        let live = range.get();
        let point = self.collapse_point(live);
        let fragment = self.range_contents(live, true)?;
        range.set(LiveRange::collapsed(point));
        Ok(fragment)
    }

    fn insert_into_range(&mut self, range: &RangeRecord, node: NodeId) -> Result<(), ScriptError> {
        let start = range.get().start;
        let hierarchy = || {
            dom_failure(
                "HierarchyRequestError",
                "The node to be inserted is not allowed at the range's start.",
            )
        };
        let (start_is_text, start_parent) = {
            let document = self.document.borrow();
            let comment = matches!(document.kind(start.node), Some(NodeKind::Comment(_)));
            let text = document.is_text(start.node);
            if comment || (text && document.parent(start.node).is_none()) || start.node == node {
                drop(document);
                return Err(self.throw_value(hierarchy()));
            }
            (text, document.parent(start.node))
        };
        let mut reference = if start_is_text {
            Some(start.node)
        } else {
            self.document
                .borrow()
                .children(start.node)
                .get(start.offset)
                .copied()
        };
        let parent = match reference {
            Some(_) if start_is_text => start_parent.expect("checked above"),
            _ => start.node,
        };
        let checked = self.document.borrow().validate_insertion(
            parent,
            node,
            reference.filter(|_| !start_is_text),
        );
        if let Err(error) = checked {
            return Err(self.dom_error(error));
        }
        if start_is_text {
            reference = self
                .document
                .borrow_mut()
                .split_text(start.node, start.offset);
        }
        if reference == Some(node) {
            reference = self.document.borrow().sibling(node, true);
        }
        if self.document.borrow().parent(node).is_some() {
            self.remove_node(node)?;
        }
        let new_offset = {
            let document = self.document.borrow();
            let base = reference.map_or(document.node_length(parent), |reference| {
                document.index_in_parent(reference).unwrap_or(0)
            });
            base + match document.kind(node) {
                Some(NodeKind::Fragment) => document.children(node).len(),
                _ => 1,
            }
        };
        self.insert_node(parent, node, reference)?;
        let live = range.get();
        if Self::is_collapsed(live) {
            range.set(LiveRange {
                start: live.start,
                end: Boundary {
                    node: parent,
                    offset: new_offset,
                },
            });
        }
        Ok(())
    }

    fn surround_range(&mut self, range: &RangeRecord, wrapper: NodeId) -> Result<(), ScriptError> {
        let live = range.get();
        let blocked = {
            let document = self.document.borrow();
            let common = document
                .common_ancestor(live.start.node, live.end.node)
                .unwrap_or(live.start.node);
            document.descendants(common).any(|node| {
                !document.is_text(node)
                    && document.is_inclusive_ancestor(node, live.start.node)
                        != document.is_inclusive_ancestor(node, live.end.node)
            })
        };
        if blocked {
            return Err(self.throw_value(dom_failure(
                "InvalidStateError",
                "The Range has partially selected a non-Text node.",
            )));
        }
        if matches!(
            self.document.borrow().kind(wrapper),
            Some(NodeKind::Document | NodeKind::Fragment)
        ) {
            return Err(self.throw_value(dom_failure(
                "InvalidNodeTypeError",
                "The node provided is of a type that may not be used to surround a Range.",
            )));
        }
        let fragment = self.extract_range(range)?;
        let children = self.document.borrow().children(wrapper).to_vec();
        for child in children {
            self.remove_node(child)?;
        }
        self.insert_into_range(range, wrapper)?;
        self.insert_node(wrapper, fragment, None)?;
        let place = self.boundary_beside(wrapper, false)?;
        range.set(LiveRange {
            start: place,
            end: Boundary {
                node: place.node,
                offset: place.offset + 1,
            },
        });
        Ok(())
    }

    fn this_range(&mut self, this: &Value, name: &str) -> Result<RangeHandle, ScriptError> {
        match this.resolve() {
            Value::Range(range) => Ok(range),
            _ => Err(self.type_error(format!(
                "Illegal invocation of '{name}' on a value that is not a Range"
            ))),
        }
    }

    fn this_selection(&mut self, this: &Value, name: &str) -> Result<SelectionHandle, ScriptError> {
        match this.resolve() {
            Value::Selection(selection) => Ok(selection),
            _ => Err(self.type_error(format!(
                "Illegal invocation of '{name}' on a value that is not a Selection"
            ))),
        }
    }

    pub(super) fn call_range_member(
        &mut self,
        member: RangeMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let (name, _) = *member.entry;
        let result = if RANGE_MEMBERS
            .iter()
            .any(|entry| std::ptr::eq(entry, member.entry))
        {
            let range = self.this_range(&this, name)?;
            self.call_range(name, &range, &arguments)
        } else {
            let selection = self.this_selection(&this, name)?;
            self.call_selection(name, &selection, &arguments)
        };
        // changes made from inside another script reach selectionchange at its checkpoint
        if self.nested_runs == 0 {
            self.sync_selection_change()?;
        }
        result
    }

    fn call_range(
        &mut self,
        name: &str,
        range: &RangeHandle,
        arguments: &[Value],
    ) -> Result<Value, ScriptError> {
        let live = range.get();
        let node_value = |runtime: &Self, node: NodeId| runtime.node_value(node);
        Ok(match name {
            "startContainer" => node_value(self, live.start.node),
            "startOffset" => Value::Number(live.start.offset as f64),
            "endContainer" => node_value(self, live.end.node),
            "endOffset" => Value::Number(live.end.offset as f64),
            "collapsed" => Value::Bool(Self::is_collapsed(live)),
            "commonAncestorContainer" => {
                let common = self
                    .document
                    .borrow()
                    .common_ancestor(live.start.node, live.end.node)
                    .unwrap_or(live.start.node);
                node_value(self, common)
            }
            "setStart" | "setEnd" => {
                let boundary = self.boundary_argument(arguments, 0, name, "Range")?;
                if name == "setStart" {
                    self.set_range_start(range, boundary);
                } else {
                    self.set_range_end(range, boundary);
                }
                Value::Undefined
            }
            "setStartBefore" | "setStartAfter" | "setEndBefore" | "setEndAfter" => {
                let node = self.range_node_argument(arguments, 0, name, "Range")?;
                let boundary = self.boundary_beside(node, name.ends_with("After"))?;
                if name.starts_with("setStart") {
                    self.set_range_start(range, boundary);
                } else {
                    self.set_range_end(range, boundary);
                }
                Value::Undefined
            }
            "collapse" => {
                let to_start = arguments.first().is_some_and(Value::truthy);
                range.set(LiveRange::collapsed(if to_start {
                    live.start
                } else {
                    live.end
                }));
                Value::Undefined
            }
            "selectNode" => {
                let node = self.range_node_argument(arguments, 0, name, "Range")?;
                let start = self.boundary_beside(node, false)?;
                range.set(LiveRange {
                    start,
                    end: Boundary {
                        node: start.node,
                        offset: start.offset + 1,
                    },
                });
                Value::Undefined
            }
            "selectNodeContents" => {
                let node = self.range_node_argument(arguments, 0, name, "Range")?;
                let length = self.document.borrow().node_length(node);
                range.set(LiveRange {
                    start: Boundary { node, offset: 0 },
                    end: Boundary {
                        node,
                        offset: length,
                    },
                });
                Value::Undefined
            }
            "compareBoundaryPoints" => {
                let how = self.offset_argument(arguments, 0)? & 0xffff;
                let Some(Value::Range(source)) = arguments.get(1).map(Value::resolve) else {
                    return Err(self.type_error(
                        "Failed to execute 'compareBoundaryPoints' on 'Range': parameter 2 is not of type 'Range'.",
                    ));
                };
                if how > 3 {
                    return Err(self.throw_value(dom_failure(
                        "NotSupportedError",
                        "The comparison method provided must be one of 'START_TO_START', 'START_TO_END', 'END_TO_END', or 'END_TO_START'.",
                    )));
                }
                let source = source.get();
                let (this_point, source_point) = match how {
                    0 => (live.start, source.start),
                    1 => (live.end, source.start),
                    2 => (live.end, source.end),
                    _ => (live.start, source.end),
                };
                match self.compare(this_point, source_point) {
                    Some(order) => Value::Number(order as i8 as f64),
                    None => {
                        return Err(self.throw_value(dom_failure(
                            "WrongDocumentError",
                            "The two Ranges are not in the same tree.",
                        )));
                    }
                }
            }
            "deleteContents" => {
                self.delete_range_contents(range)?;
                Value::Undefined
            }
            "extractContents" => {
                let fragment = self.extract_range(range)?;
                self.node_value(fragment)
            }
            "cloneContents" => {
                let fragment = self.range_contents(live, false)?;
                self.node_value(fragment)
            }
            "insertNode" => {
                let node = self.range_node_argument(arguments, 0, name, "Range")?;
                self.insert_into_range(range, node)?;
                Value::Undefined
            }
            "surroundContents" => {
                let node = self.range_node_argument(arguments, 0, name, "Range")?;
                self.surround_range(range, node)?;
                Value::Undefined
            }
            "cloneRange" => Value::Range(self.new_range(live)),
            "detach" => Value::Undefined,
            "isPointInRange" | "comparePoint" => {
                let node = self.range_node_argument(arguments, 0, name, "Range")?;
                if !self.same_root(node, live.start.node) {
                    if name == "isPointInRange" {
                        return Ok(Value::Bool(false));
                    }
                    return Err(self.throw_value(dom_failure(
                        "WrongDocumentError",
                        "The node provided and the Range are not in the same tree.",
                    )));
                }
                let offset = self.offset_argument(arguments, 1)?;
                let point = self.checked_boundary(node, offset)?;
                let position = if self.compare(point, live.start) == Some(BoundaryOrder::Less) {
                    -1.0
                } else if self.compare(point, live.end) == Some(BoundaryOrder::Greater) {
                    1.0
                } else {
                    0.0
                };
                if name == "isPointInRange" {
                    Value::Bool(position == 0.0)
                } else {
                    Value::Number(position)
                }
            }
            "intersectsNode" => {
                let node = self.range_node_argument(arguments, 0, name, "Range")?;
                if !self.same_root(node, live.start.node) {
                    return Ok(Value::Bool(false));
                }
                let place = {
                    let document = self.document.borrow();
                    document.parent(node).zip(document.index_in_parent(node))
                };
                Value::Bool(match place {
                    None => true,
                    Some((parent, offset)) => {
                        self.compare(
                            Boundary {
                                node: parent,
                                offset,
                            },
                            live.end,
                        ) == Some(BoundaryOrder::Less)
                            && self.compare(
                                Boundary {
                                    node: parent,
                                    offset: offset + 1,
                                },
                                live.start,
                            ) == Some(BoundaryOrder::Greater)
                    }
                })
            }
            "createContextualFragment" => {
                let source =
                    self.coerce_string(arguments.first().cloned().unwrap_or(Value::Undefined))?;
                let fragment = self
                    .document
                    .borrow_mut()
                    .parse_fragment(&source)
                    .map_err(|error| runtime_error(error.to_string()))?;
                self.node_value(fragment)
            }
            _ => Value::String(self.range_text(live).into()),
        })
    }

    fn select_new_range(&mut self, selection: &SelectionRecord, range: LiveRange, backward: bool) {
        let handle = self.new_range(range);
        selection.set(Some(handle), backward);
    }

    fn call_selection(
        &mut self,
        name: &str,
        selection: &SelectionHandle,
        arguments: &[Value],
    ) -> Result<Value, ScriptError> {
        let range = selection.range();
        let live = range.as_ref().map(|range| range.get());
        let anchor_focus = selection.anchor_focus();
        let no_range = |runtime: &mut Self| {
            runtime.throw_value(dom_failure("InvalidStateError", "There is no selection."))
        };
        Ok(match name {
            "anchorNode" | "focusNode" => anchor_focus.map_or(Value::Null, |(anchor, focus)| {
                self.node_value(if name == "anchorNode" { anchor } else { focus }.node)
            }),
            "anchorOffset" | "focusOffset" => {
                Value::Number(anchor_focus.map_or(0.0, |(anchor, focus)| {
                    (if name == "anchorOffset" {
                        anchor
                    } else {
                        focus
                    })
                    .offset as f64
                }))
            }
            "isCollapsed" => Value::Bool(live.is_none_or(Self::is_collapsed)),
            "rangeCount" => Value::Number(if live.is_some() { 1.0 } else { 0.0 }),
            "type" => Value::String(
                match live {
                    None => "None",
                    Some(live) if Self::is_collapsed(live) => "Caret",
                    Some(_) => "Range",
                }
                .into(),
            ),
            "direction" => Value::String(
                match live {
                    Some(live) if !Self::is_collapsed(live) => {
                        if selection.state.borrow().backward {
                            "backward"
                        } else {
                            "forward"
                        }
                    }
                    _ => "none",
                }
                .into(),
            ),
            "getRangeAt" => {
                let index = self.offset_argument(arguments, 0)?;
                match range {
                    Some(range) if index == 0 => Value::Range(range),
                    _ => {
                        return Err(self.throw_value(dom_failure(
                            "IndexSizeError",
                            &format!("The index provided ({index}) is greater than or equal to the maximum bound."),
                        )));
                    }
                }
            }
            "addRange" => {
                let Some(Value::Range(added)) = arguments.first().map(Value::resolve) else {
                    return Err(self.type_error(
                        "Failed to execute 'addRange' on 'Selection': parameter 1 is not of type 'Range'.",
                    ));
                };
                if range.is_none() && self.in_document(added.get().start.node) {
                    selection.set(Some(added), false);
                }
                Value::Undefined
            }
            "removeRange" => {
                let Some(Value::Range(removed)) = arguments.first().map(Value::resolve) else {
                    return Err(self.type_error(
                        "Failed to execute 'removeRange' on 'Selection': parameter 1 is not of type 'Range'.",
                    ));
                };
                if !range.is_some_and(|range| Rc::ptr_eq(&range, &removed)) {
                    return Err(self.throw_value(dom_failure(
                        "NotFoundError",
                        "The given range isn't in document.",
                    )));
                }
                selection.set(None, false);
                Value::Undefined
            }
            "removeAllRanges" | "empty" => {
                selection.set(None, false);
                Value::Undefined
            }
            "collapse" | "setPosition" => {
                let target = arguments.first().map(Value::resolve);
                if matches!(target, None | Some(Value::Null | Value::Undefined)) {
                    selection.set(None, false);
                    return Ok(Value::Undefined);
                }
                let point = self.boundary_argument(arguments, 0, name, "Selection")?;
                if self.in_document(point.node) {
                    self.select_new_range(selection, LiveRange::collapsed(point), false);
                }
                Value::Undefined
            }
            "collapseToStart" | "collapseToEnd" => {
                let Some(live) = live else {
                    return Err(no_range(self));
                };
                let point = if name == "collapseToStart" {
                    live.start
                } else {
                    live.end
                };
                self.select_new_range(selection, LiveRange::collapsed(point), false);
                Value::Undefined
            }
            "extend" => {
                let focus = self.boundary_argument(arguments, 0, name, "Selection")?;
                if !self.in_document(focus.node) {
                    return Ok(Value::Undefined);
                }
                let Some((anchor, _)) = anchor_focus else {
                    return Err(no_range(self));
                };
                self.select_points(selection, anchor, focus, true);
                Value::Undefined
            }
            "setBaseAndExtent" => {
                let anchor = self.boundary_argument(arguments, 0, name, "Selection")?;
                let focus = self.boundary_argument(arguments, 2, name, "Selection")?;
                if self.in_document(anchor.node) && self.in_document(focus.node) {
                    self.select_points(selection, anchor, focus, true);
                }
                Value::Undefined
            }
            "selectAllChildren" => {
                let node = self.range_node_argument(arguments, 0, name, "Selection")?;
                if self.in_document(node) {
                    let length = self.document.borrow().children(node).len();
                    let range = LiveRange {
                        start: Boundary { node, offset: 0 },
                        end: Boundary {
                            node,
                            offset: length,
                        },
                    };
                    self.select_new_range(selection, range, false);
                }
                Value::Undefined
            }
            "deleteFromDocument" => {
                if let Some(range) = range {
                    self.delete_range_contents(&range)?;
                }
                Value::Undefined
            }
            "containsNode" => {
                let node = self.range_node_argument(arguments, 0, name, "Selection")?;
                let partial = arguments.get(1).is_some_and(Value::truthy);
                let Some(live) = live.filter(|live| self.same_root(node, live.start.node)) else {
                    return Ok(Value::Bool(false));
                };
                let length = self.document.borrow().node_length(node);
                let (first, last) = if partial { (length, 0) } else { (0, length) };
                let not_after = |order| order != Some(BoundaryOrder::Greater);
                let not_before = |order| order != Some(BoundaryOrder::Less);
                Value::Bool(
                    not_after(self.compare(
                        live.start,
                        Boundary {
                            node,
                            offset: first,
                        },
                    )) && not_before(self.compare(live.end, Boundary { node, offset: last })),
                )
            }
            _ => Value::String(
                live.map(|live| self.range_text(live))
                    .unwrap_or_default()
                    .into(),
            ),
        })
    }

    // selects from an anchor to a focus with a new range, ordered and with its direction
    pub(super) fn select_points(
        &mut self,
        selection: &SelectionRecord,
        anchor: Boundary,
        focus: Boundary,
        fresh: bool,
    ) {
        let backward = self.compare(focus, anchor) == Some(BoundaryOrder::Less);
        let live = if !self.same_root(anchor.node, focus.node) {
            LiveRange::collapsed(focus)
        } else if backward {
            LiveRange {
                start: focus,
                end: anchor,
            }
        } else {
            LiveRange {
                start: anchor,
                end: focus,
            }
        };
        match selection.range() {
            Some(range) if !fresh => {
                range.set(live);
                selection.state.borrow_mut().backward = backward;
            }
            _ => self.select_new_range(selection, live, backward),
        }
    }
}
