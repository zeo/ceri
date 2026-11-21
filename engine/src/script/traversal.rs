// TreeWalker, NodeIterator and NodeFilter, walking a subtree in document order through the
// nodes whatToShow and the filter let through, as the DOM standard's traversal steps say

use super::object::{JsObject, ObjectClass, ObjectHandle, Property, Slot, new_object};
use super::realm::{Builder, Native};
use super::*;
use crate::dom::NodeKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WalkMember {
    Root,
    WhatToShow,
    Filter,
    CurrentNode,
    SetCurrentNode,
    ParentNode,
    FirstChild,
    LastChild,
    PreviousSibling,
    NextSibling,
    PreviousNode,
    NextNode,
    ReferenceNode,
    PointerBeforeReferenceNode,
    Detach,
}

const ACCEPT: u8 = 1;
const REJECT: u8 = 2;
const SKIP: u8 = 3;

pub(crate) struct WalkState {
    root: NodeId,
    what_to_show: u32,
    filter: Value,
    // the walker's current node, or the iterator's reference node
    current: NodeId,
    // an iterator's pointer sits before its reference node
    before: bool,
    iterator: bool,
}

pub(super) struct TraversalInterfaces {
    walker: ObjectHandle,
    iterator: ObjectHandle,
}

fn define_method(build: &Builder, target: &ObjectHandle, name: &str, member: WalkMember) {
    let function = build.function(Native::Walk(member), name, 0);
    target.borrow_mut().define(
        name.to_string(),
        Property {
            enumerable: true,
            ..Property::data(Value::Object(function))
        },
    );
}

fn define_getter(
    build: &Builder,
    target: &ObjectHandle,
    name: &str,
    member: WalkMember,
    setter: Option<WalkMember>,
) {
    let get = build.function(Native::Walk(member), &format!("get {name}"), 0);
    let set = setter.map(|setter| {
        Value::Object(build.function(Native::Walk(setter), &format!("set {name}"), 1))
    });
    target.borrow_mut().define(
        name.to_string(),
        Property {
            slot: Slot::Accessor {
                get: Some(Value::Object(get)),
                set,
            },
            enumerable: true,
            writable: false,
            configurable: true,
        },
    );
}

pub(super) fn create(
    build: &Builder,
    object_prototype: &ObjectHandle,
    globals: &mut Vec<(&'static str, Value)>,
) -> TraversalInterfaces {
    let plain = || {
        new_object(JsObject::with_prototype(
            Some(object_prototype.clone()),
            ObjectClass::Ordinary,
        ))
    };
    let walker = plain();
    let iterator = plain();
    for target in [&walker, &iterator] {
        define_getter(build, target, "root", WalkMember::Root, None);
        define_getter(build, target, "whatToShow", WalkMember::WhatToShow, None);
        define_getter(build, target, "filter", WalkMember::Filter, None);
    }
    define_getter(
        build,
        &walker,
        "currentNode",
        WalkMember::CurrentNode,
        Some(WalkMember::SetCurrentNode),
    );
    for (name, member) in [
        ("parentNode", WalkMember::ParentNode),
        ("firstChild", WalkMember::FirstChild),
        ("lastChild", WalkMember::LastChild),
        ("previousSibling", WalkMember::PreviousSibling),
        ("nextSibling", WalkMember::NextSibling),
        ("previousNode", WalkMember::PreviousNode),
        ("nextNode", WalkMember::NextNode),
    ] {
        define_method(build, &walker, name, member);
    }
    define_getter(
        build,
        &iterator,
        "referenceNode",
        WalkMember::ReferenceNode,
        None,
    );
    define_getter(
        build,
        &iterator,
        "pointerBeforeReferenceNode",
        WalkMember::PointerBeforeReferenceNode,
        None,
    );
    for (name, member) in [
        ("previousNode", WalkMember::PreviousNode),
        ("nextNode", WalkMember::NextNode),
        ("detach", WalkMember::Detach),
    ] {
        define_method(build, &iterator, name, member);
    }
    for (target, tag) in [(&walker, "TreeWalker"), (&iterator, "NodeIterator")] {
        target.borrow_mut().define(
            "@@toStringTag".to_string(),
            Property::hidden(Value::String(tag.into())),
        );
    }
    let filter = plain();
    for (name, value) in [
        ("FILTER_ACCEPT", 1_u32),
        ("FILTER_REJECT", 2),
        ("FILTER_SKIP", 3),
        ("SHOW_ALL", 0xffff_ffff),
        ("SHOW_ELEMENT", 0x1),
        ("SHOW_ATTRIBUTE", 0x2),
        ("SHOW_TEXT", 0x4),
        ("SHOW_CDATA_SECTION", 0x8),
        ("SHOW_PROCESSING_INSTRUCTION", 0x40),
        ("SHOW_COMMENT", 0x80),
        ("SHOW_DOCUMENT", 0x100),
        ("SHOW_DOCUMENT_TYPE", 0x200),
        ("SHOW_DOCUMENT_FRAGMENT", 0x400),
    ] {
        filter.borrow_mut().define(
            name.to_string(),
            Property {
                enumerable: true,
                writable: false,
                configurable: false,
                ..Property::data(Value::Number(f64::from(value)))
            },
        );
    }
    globals.push(("NodeFilter", Value::Object(filter)));
    TraversalInterfaces { walker, iterator }
}

impl ScriptRuntime {
    // document.createTreeWalker and document.createNodeIterator
    pub(super) fn create_walk(
        &mut self,
        arguments: &[Value],
        iterator: bool,
    ) -> Result<Value, ScriptError> {
        let method = if iterator {
            "createNodeIterator"
        } else {
            "createTreeWalker"
        };
        let root = self.node_argument(arguments, 0, method)?;
        let what_to_show = match arguments.get(1).map(Value::resolve) {
            None | Some(Value::Undefined) => 0xffff_ffff,
            Some(value) => self.coerce_uint32(value)?,
        };
        let filter = match arguments.get(2).map(Value::resolve) {
            None | Some(Value::Undefined) => Value::Null,
            Some(value) => value,
        };
        let prototype = if iterator {
            self.intrinsics.traversal.iterator.clone()
        } else {
            self.intrinsics.traversal.walker.clone()
        };
        Ok(Value::Object(new_object(JsObject::with_prototype(
            Some(prototype),
            ObjectClass::Walk(Rc::new(RefCell::new(WalkState {
                root,
                what_to_show,
                filter,
                current: root,
                before: true,
                iterator,
            }))),
        ))))
    }

    // what the filter and whatToShow make of a node
    fn judge(&mut self, state: &Rc<RefCell<WalkState>>, node: NodeId) -> Result<u8, ScriptError> {
        let kind = match self.document.borrow().kind(node) {
            Some(NodeKind::Text(_)) => 3,
            Some(NodeKind::Comment(_)) => 8,
            Some(NodeKind::Document) => 9,
            Some(NodeKind::Fragment) => 11,
            _ => 1,
        };
        let (shown, filter) = {
            let state = state.borrow();
            (
                state.what_to_show & (1 << (kind - 1)) != 0,
                state.filter.clone(),
            )
        };
        if !shown {
            return Ok(SKIP);
        }
        let argument = vec![self.node_value(node)];
        let answer = if is_callable(&filter) {
            self.call_function(filter, Value::Undefined, argument)?
        } else if matches!(filter, Value::Object(_)) {
            let accept = self.get_property(&filter, "acceptNode")?;
            if !is_callable(&accept) {
                return Err(self.type_error("The provided callback is no longer runnable."));
            }
            self.call_function(accept, filter, argument)?
        } else {
            return Ok(ACCEPT);
        };
        Ok(self.coerce_number(answer)? as u8)
    }

    fn first_child(&self, node: NodeId, last: bool) -> Option<NodeId> {
        let document = self.document.borrow();
        let children = document.children(node);
        if last {
            children.last().copied()
        } else {
            children.first().copied()
        }
    }

    fn next_sibling(&self, node: NodeId, previous: bool) -> Option<NodeId> {
        self.document.borrow().sibling(node, !previous)
    }

    fn parent_node(&self, node: NodeId) -> Option<NodeId> {
        self.document.borrow().parent(node)
    }

    // the walker's firstChild and lastChild
    fn walk_children(
        &mut self,
        state: &Rc<RefCell<WalkState>>,
        last: bool,
    ) -> Result<Option<NodeId>, ScriptError> {
        let (root, current) = {
            let state = state.borrow();
            (state.root, state.current)
        };
        let mut node = self.first_child(current, last);
        while let Some(at) = node {
            match self.judge(state, at)? {
                ACCEPT => {
                    state.borrow_mut().current = at;
                    return Ok(Some(at));
                }
                SKIP => {
                    if let Some(child) = self.first_child(at, last) {
                        node = Some(child);
                        continue;
                    }
                }
                _ => {}
            }
            let mut climb = at;
            loop {
                if let Some(sibling) = self.next_sibling(climb, last) {
                    node = Some(sibling);
                    break;
                }
                match self.parent_node(climb) {
                    Some(parent) if parent != root && parent != current => climb = parent,
                    _ => return Ok(None),
                }
            }
        }
        Ok(None)
    }

    // the walker's nextSibling and previousSibling
    fn walk_siblings(
        &mut self,
        state: &Rc<RefCell<WalkState>>,
        previous: bool,
    ) -> Result<Option<NodeId>, ScriptError> {
        let (root, mut node) = {
            let state = state.borrow();
            (state.root, state.current)
        };
        if node == root {
            return Ok(None);
        }
        loop {
            let mut sibling = self.next_sibling(node, previous);
            while let Some(at) = sibling {
                node = at;
                let result = self.judge(state, at)?;
                if result == ACCEPT {
                    state.borrow_mut().current = at;
                    return Ok(Some(at));
                }
                sibling = self.first_child(at, previous);
                if result == REJECT || sibling.is_none() {
                    sibling = self.next_sibling(at, previous);
                }
            }
            match self.parent_node(node) {
                Some(parent) if parent != root => node = parent,
                _ => return Ok(None),
            }
            if self.judge(state, node)? == ACCEPT {
                return Ok(None);
            }
        }
    }

    fn walk_parent(
        &mut self,
        state: &Rc<RefCell<WalkState>>,
    ) -> Result<Option<NodeId>, ScriptError> {
        let (root, mut node) = {
            let state = state.borrow();
            (state.root, state.current)
        };
        while node != root {
            let Some(parent) = self.parent_node(node) else {
                return Ok(None);
            };
            node = parent;
            if self.judge(state, node)? == ACCEPT {
                state.borrow_mut().current = node;
                return Ok(Some(node));
            }
        }
        Ok(None)
    }

    fn walk_previous(
        &mut self,
        state: &Rc<RefCell<WalkState>>,
    ) -> Result<Option<NodeId>, ScriptError> {
        let (root, mut node) = {
            let state = state.borrow();
            (state.root, state.current)
        };
        while node != root {
            let mut sibling = self.next_sibling(node, true);
            while let Some(at) = sibling {
                node = at;
                let mut result = self.judge(state, node)?;
                while result != REJECT
                    && let Some(child) = self.first_child(node, true)
                {
                    node = child;
                    result = self.judge(state, node)?;
                }
                if result == ACCEPT {
                    state.borrow_mut().current = node;
                    return Ok(Some(node));
                }
                sibling = self.next_sibling(node, true);
            }
            let Some(parent) = self.parent_node(node).filter(|_| node != root) else {
                return Ok(None);
            };
            node = parent;
            if self.judge(state, node)? == ACCEPT {
                state.borrow_mut().current = node;
                return Ok(Some(node));
            }
        }
        Ok(None)
    }

    fn walk_next(&mut self, state: &Rc<RefCell<WalkState>>) -> Result<Option<NodeId>, ScriptError> {
        let (root, mut node) = {
            let state = state.borrow();
            (state.root, state.current)
        };
        let mut result = ACCEPT;
        loop {
            while result != REJECT
                && let Some(child) = self.first_child(node, false)
            {
                node = child;
                result = self.judge(state, node)?;
                if result == ACCEPT {
                    state.borrow_mut().current = node;
                    return Ok(Some(node));
                }
            }
            let mut climb = Some(node);
            let mut sibling = None;
            while let Some(at) = climb {
                if at == root {
                    return Ok(None);
                }
                sibling = self.next_sibling(at, false);
                if sibling.is_some() {
                    break;
                }
                climb = self.parent_node(at);
            }
            let Some(next) = sibling else {
                return Ok(None);
            };
            node = next;
            result = self.judge(state, node)?;
            if result == ACCEPT {
                state.borrow_mut().current = node;
                return Ok(Some(node));
            }
        }
    }

    // the node after or before one in document order, inside the root
    fn following(&self, node: NodeId, root: NodeId) -> Option<NodeId> {
        if let Some(child) = self.first_child(node, false) {
            return Some(child);
        }
        let mut at = node;
        while at != root {
            if let Some(sibling) = self.next_sibling(at, false) {
                return Some(sibling);
            }
            at = self.parent_node(at)?;
        }
        None
    }

    fn preceding(&self, node: NodeId, root: NodeId) -> Option<NodeId> {
        if node == root {
            return None;
        }
        let Some(mut at) = self.next_sibling(node, true) else {
            return self.parent_node(node);
        };
        while let Some(child) = self.first_child(at, true) {
            at = child;
        }
        Some(at)
    }

    fn iterate(
        &mut self,
        state: &Rc<RefCell<WalkState>>,
        next: bool,
    ) -> Result<Option<NodeId>, ScriptError> {
        let (root, mut node, mut before) = {
            let state = state.borrow();
            (state.root, state.current, state.before)
        };
        loop {
            if next {
                if before {
                    before = false;
                } else {
                    let Some(following) = self.following(node, root) else {
                        return Ok(None);
                    };
                    node = following;
                }
            } else if before {
                let Some(preceding) = self.preceding(node, root) else {
                    return Ok(None);
                };
                node = preceding;
            } else {
                before = true;
            }
            if self.judge(state, node)? == ACCEPT {
                break;
            }
        }
        let mut state = state.borrow_mut();
        state.current = node;
        state.before = before;
        Ok(Some(node))
    }

    pub(super) fn call_walk(
        &mut self,
        member: WalkMember,
        this: Value,
        arguments: Vec<Value>,
    ) -> Result<Value, ScriptError> {
        let state = match &this {
            Value::Object(object) => match &object.borrow().class {
                ObjectClass::Walk(state) => Some(state.clone()),
                _ => None,
            },
            _ => None,
        };
        let Some(state) = state else {
            return Err(self.type_error("Illegal invocation"));
        };
        let iterator = state.borrow().iterator;
        let found = match member {
            WalkMember::Root => return Ok(self.node_value(state.borrow().root)),
            WalkMember::WhatToShow => {
                return Ok(Value::Number(f64::from(state.borrow().what_to_show)));
            }
            WalkMember::Filter => return Ok(state.borrow().filter.clone()),
            WalkMember::CurrentNode | WalkMember::ReferenceNode => {
                return Ok(self.node_value(state.borrow().current));
            }
            WalkMember::PointerBeforeReferenceNode => {
                return Ok(Value::Bool(state.borrow().before));
            }
            WalkMember::Detach => return Ok(Value::Undefined),
            WalkMember::SetCurrentNode => {
                let node = self.node_argument(&arguments, 0, "currentNode")?;
                state.borrow_mut().current = node;
                return Ok(Value::Undefined);
            }
            WalkMember::NextNode if iterator => self.iterate(&state, true)?,
            WalkMember::PreviousNode if iterator => self.iterate(&state, false)?,
            WalkMember::ParentNode => self.walk_parent(&state)?,
            WalkMember::FirstChild => self.walk_children(&state, false)?,
            WalkMember::LastChild => self.walk_children(&state, true)?,
            WalkMember::NextSibling => self.walk_siblings(&state, false)?,
            WalkMember::PreviousSibling => self.walk_siblings(&state, true)?,
            WalkMember::NextNode => self.walk_next(&state)?,
            WalkMember::PreviousNode => self.walk_previous(&state)?,
        };
        Ok(found.map_or(Value::Null, |node| self.node_value(node)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walkers_and_iterators_visit_what_their_filter_lets_through() {
        let document = crate::dom::Document::parse(
            r#"<html><body><div id="root"><p id="a">one<b id="b">two</b></p><!--note--><p id="c"><i id="d">three</i></p></div><p id="status"></p></body></html>"#,
        )
        .expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        runtime
            .execute(
                r##"
                const root = document.getElementById("root");
                const name = (node) => node === null ? "null" : node.nodeType === 1 ? node.id : node.nodeType === 3 ? node.data : "#" + node.nodeType;
                const seen = [];
                const all = document.createTreeWalker(root);
                const order = [];
                while (all.nextNode()) order.push(name(all.currentNode));
                seen.push(order.join(","));
                const back = [];
                while (all.previousNode()) back.push(name(all.currentNode));
                seen.push(back.join(","));
                const texts = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
                const words = [];
                while (texts.nextNode()) words.push(texts.currentNode.data);
                seen.push(words.join(","));
                const noB = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT, (node) =>
                    node.id === "a" ? NodeFilter.FILTER_SKIP : node.id === "c" ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT);
                seen.push(name(noB.firstChild()), name(noB.nextSibling()), name(noB.parentNode()), name(noB.lastChild()));
                const walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT);
                walker.currentNode = document.getElementById("b");
                seen.push(name(walker.parentNode()), name(walker.nextSibling()), name(walker.previousSibling()), name(walker.parentNode()));
                const iterator = document.createNodeIterator(root, NodeFilter.SHOW_ELEMENT, { acceptNode: (node) => node.id === "b" ? 3 : 1 });
                const visits = [];
                let node;
                while ((node = iterator.nextNode())) visits.push(name(node));
                seen.push(visits.join(","), iterator.pointerBeforeReferenceNode, name(iterator.previousNode()), name(iterator.previousNode()));
                seen.push(String(all), String(iterator), all.whatToShow === NodeFilter.SHOW_ALL, all.root === root);
                document.getElementById("status").textContent = seen.join("|");
                "##,
            )
            .expect("script should run");
        let status = document.borrow().find_element_by_id("status").unwrap();
        assert_eq!(
            document.borrow().element_text_content(status).as_deref(),
            Some(
                "a,one,b,two,#8,c,d,three|d,c,#8,two,b,one,a,root|one,two,three|b|null|root|b|a|c|a|root|root,a,c,d|false|d|c|[object TreeWalker]|[object NodeIterator]|true|true"
            )
        );
    }
}
