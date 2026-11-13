// the DOM domain: each session mirrors the part of the tree its frontend holds, so a flush can
// tell it what the document changed since, as chromium's mutation events would have

use super::{FRAME_ID, Failure};
use crate::dom::{Document, Namespace, NodeId, NodeKind};
use crate::navigation::Page;
use crate::script::{NodeArgument, NodeCall};
use serde_json::{Value as Json, json};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Default)]
pub(super) struct Mirror {
    // the frontend asked for the document, so changes to what it holds are reported
    active: bool,
    ids: HashMap<NodeId, u64>,
    nodes: Vec<NodeId>,
    known: BTreeMap<u64, Known>,
    version: u64,
    searches: HashMap<String, Vec<NodeId>>,
    next_search: u64,
}

// one node as the frontend last heard of it
#[derive(Clone, Default)]
struct Known {
    // its children, once the frontend has them
    children: Option<Vec<u64>>,
    count: usize,
    attributes: Vec<(String, String)>,
    value: Option<String>,
    // its shadow root and template content
    owned: Vec<u64>,
    shadow: bool,
}

impl Mirror {
    pub(super) fn active(&self) -> bool {
        self.active
    }

    fn id(&mut self, node: NodeId) -> u64 {
        *self.ids.entry(node).or_insert_with(|| {
            self.nodes.push(node);
            self.nodes.len() as u64
        })
    }

    fn node(&self, id: u64) -> Option<NodeId> {
        self.nodes
            .get(usize::try_from(id).ok()?.checked_sub(1)?)
            .copied()
    }

    // a node as the protocol sends it, with its children to depth (-1 for all of them); a
    // tracked node is one the frontend now holds
    fn describe(
        &mut self,
        document: &Document,
        url: &str,
        node: NodeId,
        depth: i64,
        tracked: bool,
    ) -> Json {
        let id = self.id(node);
        let children = visible_children(document, node);
        let mut known = Known {
            count: children.len(),
            ..Known::default()
        };
        let mut described = match document.kind(node) {
            Some(NodeKind::Document) => json!({
                "nodeType": 9,
                "nodeName": "#document",
                "localName": "",
                "nodeValue": "",
                "documentURL": url,
                "baseURL": url,
                "xmlVersion": "",
                "compatibilityMode": "NoQuirksMode",
            }),
            Some(NodeKind::Element(element)) => {
                known.attributes = element
                    .attrs
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect();
                let name = match element.namespace {
                    Namespace::Html => element.name.to_ascii_uppercase(),
                    _ => element.name.clone(),
                };
                json!({
                    "nodeType": 1,
                    "nodeName": name,
                    "localName": element.name,
                    "nodeValue": "",
                    "attributes": flat_attributes(&known.attributes),
                })
            }
            Some(NodeKind::Text(text)) => {
                known.value = Some(text.clone());
                json!({ "nodeType": 3, "nodeName": "#text", "localName": "", "nodeValue": text })
            }
            Some(NodeKind::Comment(text)) => {
                known.value = Some(text.clone());
                json!({ "nodeType": 8, "nodeName": "#comment", "localName": "", "nodeValue": text })
            }
            Some(NodeKind::Fragment) | None => {
                let mut fragment = json!({
                    "nodeType": 11,
                    "nodeName": "#document-fragment",
                    "localName": "",
                    "nodeValue": "",
                });
                if let Some((_, open)) = document.shadow_host(node) {
                    fragment["shadowRootType"] = if open { "open" } else { "closed" }.into();
                }
                fragment
            }
        };
        described["nodeId"] = if tracked { id } else { 0 }.into();
        described["backendNodeId"] = id.into();
        described["childNodeCount"] = children.len().into();
        // chromium sends an only text child along, so short text shows without expanding
        let only_text = children.len() == 1 && document.is_text(children[0]);
        if depth != 0 || only_text {
            let deeper = if depth > 0 { depth - 1 } else { depth };
            let nodes: Vec<Json> = children
                .iter()
                .map(|child| self.describe(document, url, *child, deeper, tracked))
                .collect();
            known.children = Some(children.iter().map(|child| self.id(*child)).collect());
            described["children"] = nodes.into();
        }
        if let Some(root) = document.shadow_root(node) {
            described["shadowRoots"] = json!([self.describe(document, url, root, 0, tracked)]);
            known.owned.push(self.id(root));
            known.shadow = true;
        }
        if let Some(content) = document.template_content(node) {
            described["templateContent"] = self.describe(document, url, content, 0, tracked);
            known.owned.push(self.id(content));
        }
        if tracked {
            self.known.insert(id, known);
        }
        described
    }

    // sends the frontend every node between the document and this one it does not hold yet
    fn push(
        &mut self,
        document: &Document,
        url: &str,
        node: NodeId,
        events: &mut Vec<Json>,
    ) -> Option<u64> {
        let mut chain = vec![node];
        while let Some(parent) = document.composed_parent(*chain.last()?) {
            chain.push(parent);
        }
        if *chain.last()? != document.root() {
            return None;
        }
        let root = self.id(document.root());
        self.known.get(&root)?;
        for pair in chain.windows(2).rev() {
            let (child, parent) = (pair[0], pair[1]);
            let parent_id = self.id(parent);
            if self.known.get(&parent_id)?.children.is_none() {
                self.send_children(document, url, parent, 1, events);
            }
            let child_id = self.id(child);
            self.known.get(&child_id)?;
        }
        Some(self.id(node))
    }

    fn send_children(
        &mut self,
        document: &Document,
        url: &str,
        node: NodeId,
        depth: i64,
        events: &mut Vec<Json>,
    ) {
        let id = self.id(node);
        let deeper = if depth > 0 { depth - 1 } else { depth };
        if let Some(children) = self.known.get(&id).and_then(|known| known.children.clone()) {
            if deeper != 0 {
                let children: Vec<NodeId> = children
                    .into_iter()
                    .filter_map(|child| self.node(child))
                    .collect();
                for child in children {
                    self.send_children(document, url, child, deeper, events);
                }
            }
            return;
        }
        let children = visible_children(document, node);
        let nodes: Vec<Json> = children
            .iter()
            .map(|child| self.describe(document, url, *child, deeper, true))
            .collect();
        let ids = children.iter().map(|child| self.id(*child)).collect();
        if let Some(known) = self.known.get_mut(&id) {
            known.children = Some(ids);
        }
        events.push(event(
            "DOM.setChildNodes",
            json!({ "parentId": id, "nodes": nodes }),
        ));
    }

    fn forget(&mut self, id: u64) {
        if let Some(known) = self.known.remove(&id) {
            for child in known.children.into_iter().flatten().chain(known.owned) {
                self.forget(child);
            }
        }
    }

    // the events that bring the frontend's copy up to the document's present state
    pub(super) fn reconcile(&mut self, document: &Document, url: &str, events: &mut Vec<Json>) {
        if !self.active || document.version() == self.version {
            return;
        }
        self.version = document.version();
        // every removal goes out before any insertion, so a node that moved leaves its old
        // parent before it joins the new one
        let mut insertions = Vec::new();
        let ids: Vec<u64> = self.known.keys().copied().collect();
        for id in ids {
            let Some(known) = self.known.get(&id).cloned() else {
                continue;
            };
            let Some(node) = self.node(id).filter(|node| document.contains(*node)) else {
                continue;
            };
            let mut updated = known.clone();
            if let Some(element) = document.element(node) {
                let attributes: Vec<(String, String)> = element
                    .attrs
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect();
                for (name, value) in &attributes {
                    if !known.attributes.contains(&(name.clone(), value.clone())) {
                        events.push(event(
                            "DOM.attributeModified",
                            json!({ "nodeId": id, "name": name, "value": value }),
                        ));
                    }
                }
                for (name, _) in &known.attributes {
                    if !attributes.iter().any(|(held, _)| held == name) {
                        events.push(event(
                            "DOM.attributeRemoved",
                            json!({ "nodeId": id, "name": name }),
                        ));
                    }
                }
                updated.attributes = attributes;
            }
            if let Some(text) = known.value.as_ref()
                && let Some(current) = document.character_data(node)
                && current != text
            {
                events.push(event(
                    "DOM.characterDataModified",
                    json!({ "nodeId": id, "characterData": current }),
                ));
                updated.value = Some(current.to_string());
            }
            if !known.shadow
                && let Some(root) = document.shadow_root(node)
            {
                let described = self.describe(document, url, root, 0, true);
                events.push(event(
                    "DOM.shadowRootPushed",
                    json!({ "hostId": id, "root": described }),
                ));
                updated.owned.push(self.id(root));
                updated.shadow = true;
            }
            let current = visible_children(document, node);
            updated.count = current.len();
            match &known.children {
                Some(old) => {
                    let current: Vec<u64> = current.iter().map(|child| self.id(*child)).collect();
                    // the old children that keep their order stay; the rest leave and come back
                    let mut last = None;
                    let mut stay = HashSet::new();
                    for child in &current {
                        if let Some(position) = old.iter().position(|held| held == child)
                            && last.is_none_or(|last| position > last)
                        {
                            last = Some(position);
                            stay.insert(*child);
                        }
                    }
                    for child in old.iter().filter(|child| !stay.contains(child)) {
                        events.push(event(
                            "DOM.childNodeRemoved",
                            json!({ "parentNodeId": id, "nodeId": child }),
                        ));
                        self.forget(*child);
                    }
                    insertions.push((id, current.clone(), stay));
                    updated.children = Some(current);
                }
                None if known.count != updated.count => events.push(event(
                    "DOM.childNodeCountUpdated",
                    json!({ "nodeId": id, "childNodeCount": updated.count }),
                )),
                None => {}
            }
            if let Some(held) = self.known.get_mut(&id) {
                *held = updated;
            }
        }
        for (parent, children, stay) in insertions {
            if !self.known.contains_key(&parent) {
                continue;
            }
            let mut previous = 0;
            for child in children {
                if !stay.contains(&child)
                    && let Some(node) = self.node(child)
                {
                    let described = self.describe(document, url, node, 0, true);
                    events.push(event(
                        "DOM.childNodeInserted",
                        json!({ "parentNodeId": parent, "previousNodeId": previous, "node": described }),
                    ));
                }
                previous = child;
            }
        }
    }
}

// the children the frontend shows; like chromium's default, text that is only whitespace is left
// out
fn visible_children(document: &Document, node: NodeId) -> Vec<NodeId> {
    document
        .children(node)
        .iter()
        .copied()
        .filter(|child| {
            !document.text(*child).is_some_and(|text| {
                text.chars()
                    .all(|letter| matches!(letter, ' ' | '\t' | '\n' | '\x0c' | '\r'))
            })
        })
        .collect()
}

fn flat_attributes(attributes: &[(String, String)]) -> Vec<&str> {
    attributes
        .iter()
        .flat_map(|(name, value)| [name.as_str(), value.as_str()])
        .collect()
}

fn event(method: &str, params: Json) -> Json {
    json!({ "method": method, "params": params })
}

fn failure(message: &str) -> Failure {
    (-32000, message.to_string())
}

// the node a call names by nodeId, backendNodeId or objectId
fn target(mirror: &Mirror, page: &Page, params: &Json) -> Result<NodeId, Failure> {
    let node = if let Some(id) = params["nodeId"]
        .as_u64()
        .or_else(|| params["backendNodeId"].as_u64())
    {
        mirror.node(id)
    } else if let Some(object) = params["objectId"].as_str() {
        page.runtime.inspector_object_node(object)
    } else {
        return Err(failure(
            "Either nodeId, backendNodeId or objectId must be specified",
        ));
    };
    node.filter(|node| page.document.borrow().contains(*node))
        .ok_or_else(|| failure("Could not find node with given id"))
}

fn node_param(mirror: &Mirror, page: &Page, params: &Json, key: &str) -> Result<NodeId, Failure> {
    target(mirror, page, &json!({ "nodeId": params[key] }))
}

fn text(params: &Json, key: &str) -> NodeArgument {
    NodeArgument::Text(params[key].as_str().unwrap_or_default().to_string())
}

fn call(
    page: &mut Page,
    node: NodeId,
    function: &str,
    arguments: Vec<NodeArgument>,
) -> Result<NodeCall, Failure> {
    page.runtime
        .inspector_node_call(node, function, arguments)
        .map_err(|message| failure(&message))
}

// the requested depth, which chromium refuses as zero
fn depth(params: &Json, default: i64) -> Result<i64, Failure> {
    match params["depth"].as_i64().unwrap_or(default) {
        0 => Err(failure(
            "Please provide a positive integer as a depth or -1 for entire subtree",
        )),
        depth => Ok(depth.max(-1)),
    }
}

const BOX_MODEL: &str = "function () {
    const rect = this.getBoundingClientRect();
    const style = getComputedStyle(this);
    const edges = (prefix, suffix) => ['top', 'right', 'bottom', 'left']
        .map((side) => parseFloat(style.getPropertyValue(prefix + side + suffix)) || 0);
    return [rect.left, rect.top, rect.width, rect.height]
        .concat(edges('margin-', ''), edges('border-', '-width'), edges('padding-', ''));
}";

const SET_ATTRIBUTES_AS_TEXT: &str = "function (text, name) {
    const template = document.createElement('template');
    template.innerHTML = '<span ' + text + '></span>';
    const parsed = template.content.firstElementChild;
    if (!parsed) throw new Error('Could not parse value as attributes');
    let replaced = false;
    for (const attribute of parsed.getAttributeNames()) {
        if (attribute === name) replaced = true;
        this.setAttribute(attribute, parsed.getAttribute(attribute));
    }
    if (name && !replaced) this.removeAttribute(name);
}";

const SET_NODE_NAME: &str = "function (name) {
    const renamed = document.createElementNS(this.namespaceURI, name);
    for (const attribute of this.getAttributeNames()) {
        renamed.setAttribute(attribute, this.getAttribute(attribute));
    }
    while (this.firstChild) renamed.appendChild(this.firstChild);
    this.replaceWith(renamed);
    return renamed;
}";

const SCROLL_INTO_VIEW: &str = "function () {
    const element = this.nodeType === 1 ? this : this.parentElement;
    if (!element) return;
    const rect = element.getBoundingClientRect();
    if (rect.top < 0 || rect.left < 0 || rect.bottom > innerHeight || rect.right > innerWidth) {
        element.scrollIntoView({ block: 'center', inline: 'center' });
    }
}";

// answers one DOM method, or None for a method this domain does not have
pub(super) fn answer(
    mirror: &mut Mirror,
    page: &mut Page,
    url: &str,
    method: &str,
    params: &Json,
    events: &mut Vec<Json>,
) -> Option<Result<Json, Failure>> {
    let pushed =
        |mirror: &mut Mirror, page: &Page, node: Option<NodeId>, events: &mut Vec<Json>| {
            node.and_then(|node| mirror.push(&page.document.borrow(), url, node, events))
                .unwrap_or(0)
        };
    let result = match method {
        "DOM.enable" | "DOM.markUndoableState" => Ok(json!({})),
        "DOM.disable" => {
            *mirror = Mirror::default();
            Ok(json!({}))
        }
        "DOM.getDocument" => depth(params, 2).map(|depth| {
            let document = page.document.borrow();
            mirror.known.clear();
            mirror.active = true;
            mirror.version = document.version();
            let root = mirror.describe(&document, url, document.root(), depth, true);
            json!({ "root": root })
        }),
        "DOM.requestChildNodes" => depth(params, 1).and_then(|depth| {
            let node = target(mirror, page, params)?;
            mirror.send_children(&page.document.borrow(), url, node, depth, events);
            Ok(json!({}))
        }),
        "DOM.describeNode" => depth(params, 1).and_then(|depth| {
            let node = target(mirror, page, params)?;
            let described = mirror.describe(&page.document.borrow(), url, node, depth, false);
            Ok(json!({ "node": described }))
        }),
        "DOM.resolveNode" => target(mirror, page, params).map(|node| {
            let group = params["objectGroup"].as_str().unwrap_or_default();
            json!({ "object": page.runtime.inspector_node_object(node, group) })
        }),
        "DOM.requestNode" => {
            let object = params["objectId"].as_str().unwrap_or_default();
            match page.runtime.inspector_object_node(object) {
                Some(node) => Ok(json!({ "nodeId": pushed(mirror, page, Some(node), events) })),
                None => Err(failure("No node with given id found")),
            }
        }
        "DOM.pushNodesByBackendIdsToFrontend" => {
            let ids: Vec<u64> = params["backendNodeIds"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|id| {
                    let node = id.as_u64().and_then(|id| mirror.node(id));
                    pushed(mirror, page, node, events)
                })
                .collect();
            Ok(json!({ "nodeIds": ids }))
        }
        "DOM.getAttributes" => target(mirror, page, params).map(|node| {
            let document = page.document.borrow();
            let attributes: Vec<(String, String)> = document
                .element(node)
                .map(|element| element.attrs.clone().into_iter().collect())
                .unwrap_or_default();
            json!({ "attributes": flat_attributes(&attributes) })
        }),
        "DOM.getOuterHTML" => target(mirror, page, params).map(|node| {
            let document = page.document.borrow();
            let html = match document.kind(node) {
                Some(NodeKind::Document) => document.inner_html(node),
                _ => document.outer_html(node),
            };
            json!({ "outerHTML": html })
        }),
        "DOM.collectClassNamesFromSubtree" => target(mirror, page, params).map(|node| {
            let document = page.document.borrow();
            let mut names: Vec<&str> = std::iter::once(node)
                .chain(document.descendants(node))
                .filter_map(|node| document.attribute(node, "class"))
                .flat_map(str::split_ascii_whitespace)
                .collect();
            names.sort_unstable();
            names.dedup();
            json!({ "classNames": names })
        }),
        "DOM.setInspectedNode" => target(mirror, page, params).map(|node| {
            page.runtime.inspect_node(node);
            json!({})
        }),
        "DOM.setAttributeValue" => target(mirror, page, params).and_then(|node| {
            let function = "function (name, value) { this.setAttribute(name, value) }";
            call(
                page,
                node,
                function,
                vec![text(params, "name"), text(params, "value")],
            )
            .map(|_| json!({}))
        }),
        "DOM.setAttributesAsText" => target(mirror, page, params).and_then(|node| {
            let arguments = vec![text(params, "text"), text(params, "name")];
            call(page, node, SET_ATTRIBUTES_AS_TEXT, arguments).map(|_| json!({}))
        }),
        "DOM.removeAttribute" => target(mirror, page, params).and_then(|node| {
            let function = "function (name) { this.removeAttribute(name) }";
            call(page, node, function, vec![text(params, "name")]).map(|_| json!({}))
        }),
        "DOM.setNodeValue" => target(mirror, page, params).and_then(|node| {
            let function = "function (value) { this.nodeValue = value }";
            call(page, node, function, vec![text(params, "value")]).map(|_| json!({}))
        }),
        "DOM.setOuterHTML" => target(mirror, page, params).and_then(|node| {
            let function = "function (html) { this.outerHTML = html }";
            call(page, node, function, vec![text(params, "outerHTML")]).map(|_| json!({}))
        }),
        "DOM.removeNode" => target(mirror, page, params).and_then(|node| {
            call(page, node, "function () { this.remove() }", Vec::new()).map(|_| json!({}))
        }),
        "DOM.setNodeName" => target(mirror, page, params).and_then(|node| {
            let renamed = call(page, node, SET_NODE_NAME, vec![text(params, "name")])?;
            let id = pushed(mirror, page, renamed.nodes.first().copied(), events);
            Ok(json!({ "nodeId": id }))
        }),
        "DOM.moveTo" | "DOM.copyTo" => (|| {
            let node = target(mirror, page, params)?;
            let into = node_param(mirror, page, params, "targetNodeId")?;
            let before = match params.get("insertBeforeNodeId") {
                Some(_) => Some(node_param(mirror, page, params, "insertBeforeNodeId")?),
                None => None,
            };
            let function = match method {
                "DOM.moveTo" => {
                    "function (into, before) { return into.insertBefore(this, before) }"
                }
                _ => {
                    "function (into, before) { return into.insertBefore(this.cloneNode(true), before) }"
                }
            };
            let arguments = vec![NodeArgument::Node(Some(into)), NodeArgument::Node(before)];
            let placed = call(page, node, function, arguments)?;
            // the frontend learns of the node from the insertion events before the answer
            mirror.reconcile(&page.document.borrow(), url, events);
            let id = pushed(mirror, page, placed.nodes.first().copied(), events);
            Ok(json!({ "nodeId": id }))
        })(),
        "DOM.focus" => target(mirror, page, params).and_then(|node| {
            call(page, node, "function () { this.focus() }", Vec::new()).map(|_| json!({}))
        }),
        "DOM.scrollIntoViewIfNeeded" => target(mirror, page, params)
            .and_then(|node| call(page, node, SCROLL_INTO_VIEW, Vec::new()).map(|_| json!({}))),
        "DOM.querySelector" | "DOM.querySelectorAll" => {
            target(mirror, page, params).and_then(|node| {
                let function = match method {
                    "DOM.querySelector" => {
                        "function (selector) { return this.querySelector(selector) }"
                    }
                    _ => {
                        "function (selector) { return Array.from(this.querySelectorAll(selector)) }"
                    }
                };
                let found = call(page, node, function, vec![text(params, "selector")])?;
                let ids: Vec<u64> = found
                    .nodes
                    .into_iter()
                    .map(|node| pushed(mirror, page, Some(node), events))
                    .collect();
                Ok(match method {
                    "DOM.querySelector" => json!({ "nodeId": ids.first().copied().unwrap_or(0) }),
                    _ => json!({ "nodeIds": ids }),
                })
            })
        }
        "DOM.getBoxModel" | "DOM.getContentQuads" => {
            target(mirror, page, params).and_then(|node| {
                let measured = call(page, node, BOX_MODEL, Vec::new())
                    .map_err(|_| failure("Could not compute box model."))?;
                let numbers: Vec<f64> = measured
                    .value
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Json::as_f64)
                    .collect();
                let [left, top, width, height, edges @ ..] = numbers.as_slice() else {
                    return Err(failure("Could not compute box model."));
                };
                let quad = |inset: [f64; 4]| {
                    let (x0, y0) = (left + inset[3], top + inset[0]);
                    let (x1, y1) = (left + width - inset[1], top + height - inset[2]);
                    json!([x0, y0, x1, y0, x1, y1, x0, y1])
                };
                let border = quad([0.0; 4]);
                if method == "DOM.getContentQuads" {
                    return Ok(json!({ "quads": [border] }));
                }
                let side =
                    |from: usize| -> [f64; 4] { std::array::from_fn(|index| edges[from + index]) };
                let (margin, borders, padding) = (side(0), side(4), side(8));
                let inner = |depth: usize| -> [f64; 4] {
                    std::array::from_fn(|index| {
                        borders[index] + if depth > 1 { padding[index] } else { 0.0 }
                    })
                };
                Ok(json!({ "model": {
                    "content": quad(inner(2)),
                    "padding": quad(inner(1)),
                    "border": border,
                    "margin": quad(margin.map(|edge| -edge)),
                    "width": width.round() as i64,
                    "height": height.round() as i64,
                }}))
            })
        }
        "DOM.getNodeForLocation" => {
            let point = [params["x"].as_f64(), params["y"].as_f64()];
            let root = page.document.borrow().root();
            let function = "function (x, y) { return document.elementFromPoint(x, y) }";
            let arguments = point
                .iter()
                .map(|value| NodeArgument::Text(value.unwrap_or_default().to_string()))
                .collect();
            call(page, root, function, arguments).and_then(|found| {
                let node = found
                    .nodes
                    .first()
                    .copied()
                    .ok_or_else(|| failure("No node found at given location"))?;
                let backend = mirror.id(node);
                let id = match mirror.active {
                    true => pushed(mirror, page, Some(node), events),
                    false => 0,
                };
                Ok(json!({ "backendNodeId": backend, "frameId": FRAME_ID, "nodeId": id }))
            })
        }
        "DOM.performSearch" => {
            let query = params["query"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .to_string();
            let root = page.document.borrow().root();
            let function = "function (selector) {
                try { return Array.from(this.querySelectorAll(selector)) } catch { return [] }
            }";
            let selected = call(
                page,
                root,
                function,
                vec![NodeArgument::Text(query.clone())],
            )
            .map(|found| found.nodes)
            .unwrap_or_default();
            let document = page.document.borrow();
            let mut found: Vec<NodeId> = document
                .descendants(root)
                .filter(|node| matches_text(&document, *node, &query))
                .chain(selected)
                .collect();
            found.sort_by(|left, right| document.tree_order(*left, *right));
            found.dedup();
            let id = mirror.next_search.to_string();
            mirror.next_search += 1;
            let count = found.len();
            mirror.searches.insert(id.clone(), found);
            Ok(json!({ "searchId": id, "resultCount": count }))
        }
        "DOM.getSearchResults" => {
            let found = mirror
                .searches
                .get(params["searchId"].as_str().unwrap_or_default())
                .cloned();
            match found {
                Some(found) => {
                    let from = params["fromIndex"].as_u64().unwrap_or(0) as usize;
                    let to = (params["toIndex"].as_u64().unwrap_or(0) as usize).min(found.len());
                    let ids: Vec<u64> = found
                        .get(from..to)
                        .unwrap_or_default()
                        .iter()
                        .map(|node| pushed(mirror, page, Some(*node), events))
                        .collect();
                    Ok(json!({ "nodeIds": ids }))
                }
                None => Err(failure("No search session with given id found")),
            }
        }
        "DOM.discardSearchResults" => {
            mirror
                .searches
                .remove(params["searchId"].as_str().unwrap_or_default());
            Ok(json!({}))
        }
        _ => return None,
    };
    Some(result)
}

// the elements panel's plain text search: tag names, attributes and text, without regard to case
fn matches_text(document: &Document, node: NodeId, query: &str) -> bool {
    if query.is_empty() {
        return false;
    }
    let query = query.to_lowercase();
    let contains = |text: &str| text.to_lowercase().contains(&query);
    match document.kind(node) {
        Some(NodeKind::Element(element)) => {
            let tag = query.trim_start_matches('<').trim_end_matches('>');
            (!tag.is_empty() && element.name.to_lowercase().contains(tag))
                || element
                    .attrs
                    .iter()
                    .any(|(name, value)| contains(name) || contains(value))
        }
        Some(NodeKind::Text(text) | NodeKind::Comment(text)) => contains(text),
        _ => false,
    }
}
