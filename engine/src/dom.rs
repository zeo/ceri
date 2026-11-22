use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::rc::{Rc, Weak};

// a node in one document arena; the generation tells a reused slot apart from a freed node
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId {
    index: u32,
    generation: u32,
}

impl NodeId {
    pub fn raw(self) -> u64 {
        (u64::from(self.generation) << 32) | u64::from(self.index)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Namespace {
    #[default]
    Html,
    Svg,
    MathMl,
}

impl Namespace {
    pub fn uri(self) -> &'static str {
        match self {
            Self::Html => "http://www.w3.org/1999/xhtml",
            Self::Svg => "http://www.w3.org/2000/svg",
            Self::MathMl => "http://www.w3.org/1998/Math/MathML",
        }
    }

    pub fn from_uri(uri: &str) -> Option<Self> {
        [Self::Html, Self::Svg, Self::MathMl]
            .into_iter()
            .find(|namespace| namespace.uri() == uri)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Element {
    pub name: String,
    pub namespace: Namespace,
    pub attrs: Attributes,
}

// an element's attributes in the order they were set, the order markup, attribute lists and
// serialization show them in
#[derive(Clone, Debug, Default, Eq)]
pub struct Attributes(Vec<(String, String)>);

impl Attributes {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, name: &str) -> Option<&String> {
        self.0
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value)
    }

    pub fn contains_key(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    // changes a value where it stands, or adds the attribute last
    pub fn insert(&mut self, name: String, value: String) -> Option<String> {
        match self.0.iter_mut().find(|(key, _)| *key == name) {
            Some((_, slot)) => Some(std::mem::replace(slot, value)),
            None => {
                self.0.push((name, value));
                None
            }
        }
    }

    // keeps a value already set, as a repeated attribute in markup is dropped
    pub fn insert_missing(&mut self, name: String, value: String) {
        if !self.contains_key(&name) {
            self.0.push((name, value));
        }
    }

    pub fn remove(&mut self, name: &str) -> Option<String> {
        let index = self.0.iter().position(|(key, _)| key == name)?;
        Some(self.0.remove(index).1)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &String)> {
        self.0.iter().map(|(key, value)| (key, value))
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.0.iter().map(|(key, _)| key)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

// two elements' attributes are the same whatever order they were set in
impl PartialEq for Attributes {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len()
            && self
                .iter()
                .all(|(name, value)| other.get(name) == Some(value))
    }
}

impl<'a> IntoIterator for &'a Attributes {
    type Item = (&'a String, &'a String);
    type IntoIter = std::iter::Map<
        std::slice::Iter<'a, (String, String)>,
        fn(&'a (String, String)) -> (&'a String, &'a String),
    >;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter().map(|(key, value)| (key, value))
    }
}

impl IntoIterator for Attributes {
    type Item = (String, String);
    type IntoIter = std::vec::IntoIter<(String, String)>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl FromIterator<(String, String)> for Attributes {
    fn from_iter<I: IntoIterator<Item = (String, String)>>(entries: I) -> Self {
        let mut attributes = Self::new();
        for (name, value) in entries {
            attributes.insert(name, value);
        }
        attributes
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Document,
    Element(Element),
    Text(String),
    Comment(String),
    Fragment,
}

#[derive(Clone, Debug)]
pub struct NodeData {
    pub kind: NodeKind,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
    // the document's change stamp when the node itself was last written, and when it or
    // anything beneath it was
    written: u64,
    written_within: u64,
}

#[derive(Clone, Debug)]
struct Slot {
    generation: u32,
    node: Option<NodeData>,
}

// a boundary point: a node and an offset into its children, or into the UTF-16 units of
// its data for text and comments
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Boundary {
    pub node: NodeId,
    pub offset: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveRange {
    pub start: Boundary,
    pub end: Boundary,
}

impl LiveRange {
    pub fn collapsed(at: Boundary) -> Self {
        Self { start: at, end: at }
    }
}

pub type SharedRange = Rc<RefCell<LiveRange>>;

// one change to the tree as a MutationObserver sees it
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mutation {
    ChildList {
        added: Vec<NodeId>,
        removed: Vec<NodeId>,
        previous: Option<NodeId>,
        next: Option<NodeId>,
    },
    Attribute {
        name: String,
        old: Option<String>,
    },
    CharacterData {
        old: String,
    },
}

// a mutation with its target and the target's ancestors when it happened, which subtree
// observers match against
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MutationEntry {
    pub target: NodeId,
    pub ancestors: Vec<NodeId>,
    pub mutation: Mutation,
}

// ranges that tree mutations keep up to date; a cloned document starts without any
#[derive(Debug, Default)]
struct LiveRanges(Vec<Weak<RefCell<LiveRange>>>);

impl Clone for LiveRanges {
    fn clone(&self) -> Self {
        Self::default()
    }
}

// tells a document from every other, a copy of it included: what is kept about one must
// not be taken for the other once they differ
#[derive(Debug)]
struct Identity(u64);

impl Clone for Identity {
    fn clone(&self) -> Self {
        Self(next_stamp())
    }
}

// a number no other change to any document has had, so two documents never share one
fn next_stamp() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

#[derive(Clone, Debug)]
pub struct Document {
    slots: Vec<Slot>,
    free: Vec<u32>,
    root: NodeId,
    identity: Identity,
    // bumped by every change to the tree, an attribute or character data
    version: u64,
    // the stamp nodes written now are marked with, and whether anyone has read it
    stamp: u64,
    stamp_read: std::cell::Cell<bool>,
    // renewed by every change a selector can see unless it reads text or the style attribute:
    // to the elements of the tree, or to an attribute other than style
    selector_version: u64,
    // nodes that were created or lost their parent, so their owner can decide whether to free them
    orphans: Vec<NodeId>,
    // script elements that were prepared already, which inserting again never runs: every
    // script a parse makes counts, as the page's own run once by the navigation
    started_scripts: std::collections::HashSet<NodeId>,
    template_contents: HashMap<NodeId, NodeId>,
    // shadow roots by host, and each root's host and whether script may reach it from the host
    shadow_roots: HashMap<NodeId, NodeId>,
    shadow_hosts: HashMap<NodeId, (NodeId, bool)>,
    live_ranges: LiveRanges,
    // mutations since the observers last looked, recorded only while something observes
    mutations: Option<Vec<MutationEntry>>,
    // the same changes for custom element reactions, recorded once an element is defined
    element_changes: Option<Vec<MutationEntry>>,
    content_security_policy: Rc<crate::csp::Policies>,
    // elements whose markup style attribute the policy refused, until script sets one
    blocked_styles: HashSet<NodeId>,
    // the rules of a style element's sheet once script changed them through the CSSOM, with
    // the element's text they were made from; new text replaces them
    sheet_rules: HashMap<NodeId, (String, Vec<String>)>,
    // the text of the sheets script adopted, for the document and for each shadow root
    adopted_sheets: HashMap<NodeId, Vec<String>>,
    // style elements whose sheet script disabled
    disabled_sheets: HashSet<NodeId>,
    // faces script added through document.fonts, and a count of the changes to them
    script_fonts: Vec<ScriptFont>,
    script_fonts_stamp: u64,
}

// a face a FontFace object loaded and document.fonts holds
#[derive(Clone)]
pub(crate) struct ScriptFont {
    pub(crate) family: String,
    pub(crate) weights: Option<(u16, u16)>,
    pub(crate) italic: bool,
    pub(crate) face: Rc<crate::font::FontFace>,
}

impl fmt::Debug for ScriptFont {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScriptFont")
            .field("family", &self.family)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessibilityNode {
    pub role: String,
    pub name: String,
    pub value: Option<String>,
    pub node: NodeId,
    pub disabled: bool,
    pub focusable: bool,
    pub children: Vec<Self>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DomError {
    // the insertion would make an invalid tree, such as a node inside itself
    HierarchyRequest,
    // the reference child does not belong to the parent
    NotFound,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScriptSource {
    Inline(String),
    External(String),
    InlineModule(String),
    ExternalModule(String),
    ImportMap(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StylesheetSource {
    Inline(String),
    External(String),
    // rules script gave a sheet through the CSSOM, which no policy checks, as in browsers
    Cssom(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub offset: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "parse error at byte {}: {}",
            self.offset, self.message
        )
    }
}

impl std::error::Error for ParseError {}

// preorder walk of the nodes below a root, not including the root
pub struct Descendants<'a> {
    document: &'a Document,
    stack: Vec<(&'a [NodeId], usize)>,
}

impl Iterator for Descendants<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        loop {
            let (children, index) = self.stack.last_mut()?;
            if let Some(&child) = children.get(*index) {
                *index += 1;
                self.stack.push((self.document.children(child), 0));
                return Some(child);
            }
            self.stack.pop();
        }
    }
}

pub struct Ancestors<'a> {
    document: &'a Document,
    next: Option<NodeId>,
}

impl Iterator for Ancestors<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        let current = self.next?;
        self.next = self.document.parent(current);
        Some(current)
    }
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

impl Document {
    pub fn new() -> Self {
        let mut document = Self {
            slots: Vec::new(),
            free: Vec::new(),
            root: NodeId {
                index: 0,
                generation: 0,
            },
            identity: Identity(next_stamp()),
            version: 0,
            stamp: next_stamp(),
            stamp_read: std::cell::Cell::new(false),
            selector_version: next_stamp(),
            orphans: Vec::new(),
            started_scripts: std::collections::HashSet::new(),
            template_contents: HashMap::new(),
            shadow_roots: HashMap::new(),
            shadow_hosts: HashMap::new(),
            live_ranges: LiveRanges::default(),
            mutations: None,
            element_changes: None,
            content_security_policy: Rc::default(),
            blocked_styles: HashSet::new(),
            sheet_rules: HashMap::new(),
            adopted_sheets: HashMap::new(),
            disabled_sheets: HashSet::new(),
            script_fonts: Vec::new(),
            script_fonts_stamp: 0,
        };
        document.root = document.allocate(NodeKind::Document);
        document.orphans.clear();
        document
    }

    pub fn parse(source: &str) -> Result<Self, ParseError> {
        let mut document = Self::new();
        let root = document.root;
        document.parse_into(root, source, true)?;
        document.attach_declarative_shadows(root);
        document.orphans.clear();
        Ok(document)
    }

    // each <template shadowrootmode> a parse built becomes its parent's shadow root holding
    // the template's content; a parent that already has a shadow root keeps the template
    pub fn attach_declarative_shadows(&mut self, root: NodeId) {
        let templates: Vec<(NodeId, bool)> = self
            .subtree(root)
            .into_iter()
            .filter_map(|id| {
                let element = self.element(id)?;
                if element.namespace != Namespace::Html || element.name != "template" {
                    return None;
                }
                match element
                    .attr("shadowrootmode")?
                    .to_ascii_lowercase()
                    .as_str()
                {
                    "open" => Some((id, true)),
                    "closed" => Some((id, false)),
                    _ => None,
                }
            })
            .collect();
        for (template, open) in templates {
            let Some(host) = self
                .parent(template)
                .filter(|&host| self.element(host).is_some())
            else {
                continue;
            };
            let Some(shadow) = self.attach_shadow(host, open) else {
                continue;
            };
            if let Some(content) = self.template_content(template) {
                for child in self.children(content).to_vec() {
                    self.append_child(shadow, child)
                        .expect("parsed content always fits in a new shadow root");
                }
            }
            self.remove(template);
        }
    }

    // markup with no implied html, head or body, as an svg image or a bare fragment of markup
    // is built
    pub(crate) fn parse_markup(source: &str) -> Result<Self, ParseError> {
        let mut document = Self::new();
        let root = document.root;
        document.parse_into(root, source, false)?;
        document.orphans.clear();
        Ok(document)
    }

    // parses a whole document under a new document node beside this one, as DOMParser
    // does; markup that is not html is built as written, with no implied html, head or body
    pub fn parse_document(&mut self, source: &str, html: bool) -> Result<NodeId, ParseError> {
        let root = self.allocate(NodeKind::Document);
        if let Err(error) = self.parse_into(root, source, html) {
            self.free_tree(root);
            return Err(error);
        }
        Ok(root)
    }

    // parses markup into a new fragment of this document
    pub fn parse_fragment(&mut self, source: &str) -> Result<NodeId, ParseError> {
        let fragment = self.create_fragment();
        if let Err(error) = self.parse_into(fragment, source, false) {
            self.free_tree(fragment);
            return Err(error);
        }
        Ok(fragment)
    }

    fn parse_into(
        &mut self,
        parent: NodeId,
        source: &str,
        whole_document: bool,
    ) -> Result<(), ParseError> {
        // the input stream's newline normalization: CR LF and lone CR become LF
        let normalized;
        let source = if source.contains('\r') {
            normalized = source.replace("\r\n", "\n").replace('\r', "\n");
            normalized.as_str()
        } else {
            source
        };
        let tokens = tokenize(source)?;
        let mut builder = TreeBuilder {
            parent,
            whole_document,
            stack: Vec::new(),
            created: Vec::new(),
            html: None,
            head: None,
            body: None,
        };
        for token in tokens {
            builder.token(self, token);
        }
        builder.finish(self);
        let created = builder.created;
        let created_set: std::collections::HashSet<NodeId> = created.iter().copied().collect();
        self.orphans.retain(|id| !created_set.contains(id));
        Ok(())
    }

    fn allocate(&mut self, kind: NodeKind) -> NodeId {
        if !matches!(kind, NodeKind::Text(_) | NodeKind::Comment(_)) {
            self.selector_version = next_stamp();
        }
        let stamp = self.writing_stamp();
        let node = NodeData {
            kind,
            parent: None,
            children: Vec::new(),
            written: stamp,
            written_within: stamp,
        };
        self.version += 1;
        let id = if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            slot.node = Some(node);
            NodeId {
                index,
                generation: slot.generation,
            }
        } else {
            let index = u32::try_from(self.slots.len()).expect("node arena exceeds u32 slots");
            self.slots.push(Slot {
                generation: 0,
                node: Some(node),
            });
            NodeId {
                index,
                generation: 0,
            }
        };
        self.orphans.push(id);
        id
    }

    fn data(&self, id: NodeId) -> Option<&NodeData> {
        let slot = self.slots.get(id.index as usize)?;
        if slot.generation != id.generation {
            return None;
        }
        slot.node.as_ref()
    }

    // every write to a node comes through here or through an element edit, so these are
    // where the node and the chain above it are stamped as written
    fn data_mut(&mut self, id: NodeId) -> Option<&mut NodeData> {
        self.mark_written(id)?;
        self.slots[id.index as usize].node.as_mut()
    }

    fn mark_written(&mut self, id: NodeId) -> Option<()> {
        let stamp = self.writing_stamp();
        let mut above = self.data(id)?.parent;
        let data = self.slots[id.index as usize].node.as_mut()?;
        data.written = stamp;
        // a chain already stamped at this stamp was stamped all the way up
        if std::mem::replace(&mut data.written_within, stamp) != stamp {
            while let Some(data) = above.and_then(|node| {
                self.slots
                    .get_mut(node.index as usize)
                    .filter(|slot| slot.generation == node.generation)?
                    .node
                    .as_mut()
            }) {
                if std::mem::replace(&mut data.written_within, stamp) == stamp {
                    break;
                }
                above = data.parent;
            }
        }
        Some(())
    }

    // the stamp writes take now: a new one once the last has been read, so whoever read it
    // can tell later writes from the ones it saw
    fn writing_stamp(&mut self) -> u64 {
        if self.stamp_read.replace(false) {
            self.stamp = next_stamp();
        }
        self.stamp
    }

    // the stamp of the writes so far
    pub(crate) fn change_stamp(&self) -> u64 {
        self.stamp_read.set(true);
        self.stamp
    }

    // whether nothing in the node's subtree, nor any ancestor itself, was written after the
    // stamp: what an element lays out as hangs on its subtree and on what it inherits
    pub(crate) fn unwritten_since(&self, id: NodeId, stamp: u64) -> bool {
        let Some(data) = self.data(id) else {
            return false;
        };
        let mut above = data.parent;
        if data.written_within > stamp {
            return false;
        }
        while let Some(data) = above.and_then(|node| self.data(node)) {
            if data.written > stamp {
                return false;
            }
            above = data.parent;
        }
        true
    }

    fn append_raw(&mut self, parent: NodeId, child: NodeId) {
        if let Some(data) = self.data_mut(child) {
            data.parent = Some(parent);
        }
        if let Some(data) = self.data_mut(parent) {
            data.children.push(child);
        }
    }

    // unlinks a node from its parent, returning the parent and the index it had; live range
    // boundaries inside it move to where it was
    fn detach_raw(&mut self, id: NodeId) -> Option<(NodeId, usize)> {
        self.detach_recorded(id, true)
    }

    fn detach_recorded(&mut self, id: NodeId, record: bool) -> Option<(NodeId, usize)> {
        let parent = self.data(id)?.parent?;
        let index = self
            .children(parent)
            .iter()
            .position(|&child| child == id)?;
        if record {
            let previous = index
                .checked_sub(1)
                .map(|previous| self.children(parent)[previous]);
            let next = self.children(parent).get(index + 1).copied();
            self.record(
                parent,
                Mutation::ChildList {
                    added: Vec::new(),
                    removed: vec![id],
                    previous,
                    next,
                },
            );
        }
        self.update_ranges(|document, boundary| {
            if document.is_inclusive_ancestor(id, boundary.node) {
                *boundary = Boundary {
                    node: parent,
                    offset: index,
                };
            } else if boundary.node == parent && boundary.offset > index {
                boundary.offset -= 1;
            }
        });
        let siblings = &mut self.data_mut(parent)?.children;
        siblings.remove(index);
        self.data_mut(id)?.parent = None;
        self.version += 1;
        if !self.is_character_data(id) {
            self.selector_version = next_stamp();
        }
        Some((parent, index))
    }

    // the node reached by child indices from the root, for tests written against tree shape
    #[cfg(test)]
    pub(crate) fn node_at(&self, path: &[usize]) -> NodeId {
        path.iter().fold(self.root, |node, &index| {
            *self
                .children(node)
                .get(index)
                .unwrap_or_else(|| panic!("no node at {path:?}"))
        })
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub(crate) fn selector_version(&self) -> u64 {
        self.selector_version
    }

    pub(crate) fn identity(&self) -> u64 {
        self.identity.0
    }

    fn is_character_data(&self, id: NodeId) -> bool {
        matches!(
            self.kind(id),
            Some(NodeKind::Text(_) | NodeKind::Comment(_))
        )
    }

    pub fn contains(&self, id: NodeId) -> bool {
        self.data(id).is_some()
    }

    pub fn kind(&self, id: NodeId) -> Option<&NodeKind> {
        self.data(id).map(|data| &data.kind)
    }

    pub fn element(&self, id: NodeId) -> Option<&Element> {
        match self.kind(id)? {
            NodeKind::Element(element) => Some(element),
            _ => None,
        }
    }

    pub fn element_mut(&mut self, id: NodeId) -> Option<&mut Element> {
        self.version += 1;
        self.selector_version = next_stamp();
        match &mut self.data_mut(id)?.kind {
            NodeKind::Element(element) => Some(element),
            _ => None,
        }
    }

    pub fn is_element(&self, id: NodeId) -> bool {
        self.element(id).is_some()
    }

    pub fn is_text(&self, id: NodeId) -> bool {
        matches!(self.kind(id), Some(NodeKind::Text(_)))
    }

    // text of a Text node only
    pub fn text(&self, id: NodeId) -> Option<&str> {
        match self.kind(id)? {
            NodeKind::Text(text) => Some(text),
            _ => None,
        }
    }

    // data of a Text or Comment node
    pub fn character_data(&self, id: NodeId) -> Option<&str> {
        match self.kind(id)? {
            NodeKind::Text(text) | NodeKind::Comment(text) => Some(text),
            _ => None,
        }
    }

    // replaces all of a text or comment node's data, which collapses live boundaries in it
    // to its start
    pub fn set_character_data(&mut self, id: NodeId, data: String) -> bool {
        let length = self.node_length(id);
        self.replace_data(id, 0, length, &data)
    }

    // the DOM's replace data: count UTF-16 units at offset become data, and live boundaries
    // inside the replaced span move to its start while later ones shift
    pub fn replace_data(&mut self, id: NodeId, offset: usize, count: usize, data: &str) -> bool {
        let Some(current) = self.character_data(id) else {
            return false;
        };
        let units = current.encode_utf16().collect::<Vec<_>>();
        let offset = offset.min(units.len());
        let count = count.min(units.len() - offset);
        let mut replaced = String::from_utf16_lossy(&units[..offset]);
        replaced.push_str(data);
        replaced.push_str(&String::from_utf16_lossy(&units[offset + count..]));
        let inserted = data.encode_utf16().count();
        let old = current.to_string();
        self.record(id, Mutation::CharacterData { old });
        match &mut self
            .data_mut(id)
            .expect("character data was read above")
            .kind
        {
            NodeKind::Text(text) | NodeKind::Comment(text) => *text = replaced,
            _ => unreachable!("only character data has text"),
        }
        self.version += 1;
        self.update_ranges(|_, boundary| {
            if boundary.node != id {
                return;
            }
            if boundary.offset > offset + count {
                boundary.offset = boundary.offset + inserted - count;
            } else if boundary.offset > offset {
                boundary.offset = offset;
            }
        });
        true
    }

    // the length a boundary offset is measured against: UTF-16 units of character data, or
    // the number of children
    pub fn node_length(&self, id: NodeId) -> usize {
        match self.character_data(id) {
            Some(data) => data.encode_utf16().count(),
            None => self.children(id).len(),
        }
    }

    // starts or stops recording mutations for observers
    pub fn observe_mutations(&mut self, observe: bool) {
        match (observe, &self.mutations) {
            (true, None) => self.mutations = Some(Vec::new()),
            (false, _) => self.mutations = None,
            _ => {}
        }
    }

    pub fn has_mutations(&self) -> bool {
        self.mutations
            .as_ref()
            .is_some_and(|mutations| !mutations.is_empty())
    }

    pub fn take_mutations(&mut self) -> Vec<MutationEntry> {
        self.mutations
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    }

    pub fn record_element_changes(&mut self, record: bool) {
        match (record, &self.element_changes) {
            (true, None) => self.element_changes = Some(Vec::new()),
            (false, _) => self.element_changes = None,
            _ => {}
        }
    }

    pub fn take_element_changes(&mut self) -> Vec<MutationEntry> {
        self.element_changes
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    }

    fn record(&mut self, target: NodeId, mutation: Mutation) {
        if self.mutations.is_none() && self.element_changes.is_none() {
            return;
        }
        let entry = MutationEntry {
            target,
            ancestors: self.ancestors(target).collect(),
            mutation,
        };
        if let Some(changes) = self.element_changes.as_mut() {
            changes.push(entry.clone());
        }
        if let Some(mutations) = self.mutations.as_mut() {
            mutations.push(entry);
        }
    }

    // changes an element through a closure, recording the attribute it touches
    pub fn edit_element<R>(
        &mut self,
        id: NodeId,
        attribute: &str,
        edit: impl FnOnce(&mut Element) -> R,
    ) -> Option<R> {
        let old = self.element(id)?.attr(attribute).map(str::to_string);
        self.record(
            id,
            Mutation::Attribute {
                name: attribute.to_string(),
                old: old.clone(),
            },
        );
        // a refused style attribute reads as empty, so script edits start from nothing
        let unblocked = attribute == "style" && self.blocked_styles.remove(&id);
        let NodeKind::Element(element) = &mut self.slots[id.index as usize].node.as_mut()?.kind
        else {
            return None;
        };
        if unblocked {
            element.attrs.remove("style");
        }
        let result = edit(element);
        // an edit that leaves the attribute as it was is one for observers to see and for
        // nothing else: frameworks set a class or a flag again on every update, and that
        // must not cost the styles and boxes kept for the page
        if unblocked || element.attr(attribute) != old.as_deref() {
            self.version += 1;
            if attribute != "style" {
                self.selector_version = next_stamp();
            }
            self.mark_written(id);
        }
        Some(result)
    }

    pub(crate) fn content_security_policy(&self) -> &Rc<crate::csp::Policies> {
        &self.content_security_policy
    }

    // the policy for a freshly parsed document, which drops the style attributes it refuses
    pub(crate) fn set_content_security_policy(&mut self, policies: Rc<crate::csp::Policies>) {
        self.blocked_styles = self
            .connected_elements()
            .filter_map(|(node, element)| {
                let style = element.attr("style")?;
                let check = policies.inline(
                    crate::csp::Directive::StyleAttr,
                    style,
                    crate::csp::Initiator {
                        nonce: None,
                        parser_inserted: true,
                    },
                );
                (!policies.permits(check)).then_some(node)
            })
            .collect();
        self.content_security_policy = policies;
        // which style attributes apply is no node's own write
        self.selector_version = next_stamp();
    }

    pub(crate) fn style_blocked(&self, node: NodeId) -> bool {
        self.blocked_styles.contains(&node)
    }

    // policies from <meta http-equiv="content-security-policy"> in the head
    pub(crate) fn meta_policies(&self) -> Vec<String> {
        self.connected_elements()
            .filter(|(node, element)| {
                element.name == "meta"
                    && element.attr("http-equiv").is_some_and(|name| {
                        name.trim().eq_ignore_ascii_case("content-security-policy")
                    })
                    && self
                        .parent_element(*node)
                        .and_then(|parent| self.element(parent))
                        .is_some_and(|parent| parent.name == "head")
            })
            .filter_map(|(_, element)| element.attr("content").map(str::to_string))
            .collect()
    }

    pub fn track_range(&mut self, range: &SharedRange) {
        let ranges = &mut self.live_ranges.0;
        ranges.retain(|weak| weak.strong_count() != 0);
        if !ranges
            .iter()
            .any(|weak| std::ptr::eq(weak.as_ptr(), Rc::as_ptr(range)))
        {
            ranges.push(Rc::downgrade(range));
        }
    }

    // the boundary nodes of every live range, which keep detached trees from being freed
    pub fn range_nodes(&self) -> Vec<NodeId> {
        self.live_ranges
            .0
            .iter()
            .filter_map(Weak::upgrade)
            .flat_map(|range| {
                let range = *range.borrow();
                [range.start.node, range.end.node]
            })
            .collect()
    }

    fn update_ranges(&mut self, mut adjust: impl FnMut(&Self, &mut Boundary)) {
        self.live_ranges.0.retain(|weak| weak.strong_count() != 0);
        if self.live_ranges.0.is_empty() {
            return;
        }
        let ranges = self
            .live_ranges
            .0
            .iter()
            .filter_map(Weak::upgrade)
            .collect::<Vec<_>>();
        for range in ranges {
            let mut range = range.borrow_mut();
            adjust(self, &mut range.start);
            adjust(self, &mut range.end);
        }
    }

    // where one boundary point sits relative to another, or None across trees
    pub fn compare_boundaries(&self, left: Boundary, right: Boundary) -> Option<Ordering> {
        if self.tree_root(left.node) != self.tree_root(right.node) {
            return None;
        }
        if left.node == right.node {
            return Some(left.offset.cmp(&right.offset));
        }
        if self.tree_order(left.node, right.node) == Ordering::Greater {
            return self.compare_boundaries(right, left).map(Ordering::reverse);
        }
        if self.is_inclusive_ancestor(left.node, right.node) {
            let child = std::iter::once(right.node)
                .chain(self.ancestors(right.node))
                .find(|&node| self.parent(node) == Some(left.node))?;
            let index = self.index_in_parent(child)?;
            if index < left.offset {
                return Some(Ordering::Greater);
            }
        }
        Some(Ordering::Less)
    }

    // the flat UTF-16 position of a boundary in the document's text, for connected nodes
    pub fn boundary_text_position(&self, boundary: Boundary) -> Option<usize> {
        let node = boundary.node;
        if let Some(data) = self.character_data(node) {
            let start = self.node_text_offset(node)?;
            let inside = if self.is_text(node) {
                boundary.offset.min(data.encode_utf16().count())
            } else {
                0
            };
            return Some(start + inside);
        }
        match self.children(node).get(boundary.offset) {
            Some(&child) => self.node_text_offset(child),
            None => {
                Some(self.node_text_offset(node)? + self.text_content(node).encode_utf16().count())
            }
        }
    }

    // the DOM's normalize: drops empty text nodes and merges runs of adjacent ones, moving live
    // boundaries into the text that survives
    pub fn normalize(&mut self, root: NodeId) {
        let texts = self
            .descendants(root)
            .filter(|&node| self.is_text(node))
            .collect::<Vec<_>>();
        for node in texts {
            if self.parent(node).is_none() {
                continue;
            }
            let mut length = self.node_length(node);
            if length == 0 {
                self.remove(node);
                continue;
            }
            let mut run = Vec::new();
            let mut next = self.sibling(node, true);
            while let Some(current) = next.filter(|&current| self.is_text(current)) {
                run.push(current);
                next = self.sibling(current, true);
            }
            if run.is_empty() {
                continue;
            }
            let appended = run
                .iter()
                .filter_map(|&current| self.text(current))
                .collect::<String>();
            self.replace_data(node, length, 0, &appended);
            for &current in &run {
                let parent = self.parent(current);
                let index = self.index_in_parent(current);
                self.update_ranges(|_, boundary| {
                    if boundary.node == current {
                        *boundary = Boundary {
                            node,
                            offset: boundary.offset + length,
                        };
                    } else if Some(boundary.node) == parent && Some(boundary.offset) == index {
                        *boundary = Boundary {
                            node,
                            offset: length,
                        };
                    }
                });
                length += self.node_length(current);
            }
            for current in run {
                self.remove(current);
            }
        }
    }

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.data(id)?.parent
    }

    pub fn parent_element(&self, id: NodeId) -> Option<NodeId> {
        self.parent(id).filter(|&parent| self.is_element(parent))
    }

    pub fn children(&self, id: NodeId) -> &[NodeId] {
        self.data(id).map_or(&[], |data| data.children.as_slice())
    }

    pub fn element_children(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        self.children(id)
            .iter()
            .copied()
            .filter(|&child| self.is_element(child))
    }

    pub fn index_in_parent(&self, id: NodeId) -> Option<usize> {
        let parent = self.parent(id)?;
        self.children(parent).iter().position(|&child| child == id)
    }

    pub fn sibling(&self, id: NodeId, next: bool) -> Option<NodeId> {
        let parent = self.parent(id)?;
        let siblings = self.children(parent);
        let index = siblings.iter().position(|&child| child == id)?;
        if next {
            siblings.get(index + 1).copied()
        } else {
            index.checked_sub(1).map(|previous| siblings[previous])
        }
    }

    pub fn element_sibling(&self, id: NodeId, next: bool) -> Option<NodeId> {
        let parent = self.parent(id)?;
        let siblings = self.children(parent);
        let index = siblings.iter().position(|&child| child == id)?;
        if next {
            siblings[index + 1..]
                .iter()
                .copied()
                .find(|&sibling| self.is_element(sibling))
        } else {
            siblings[..index]
                .iter()
                .rev()
                .copied()
                .find(|&sibling| self.is_element(sibling))
        }
    }

    // the parent chain, starting at the parent
    pub fn ancestors(&self, id: NodeId) -> Ancestors<'_> {
        Ancestors {
            document: self,
            next: self.parent(id),
        }
    }

    pub fn descendants(&self, id: NodeId) -> Descendants<'_> {
        Descendants {
            document: self,
            stack: vec![(self.children(id), 0)],
        }
    }

    pub fn is_inclusive_ancestor(&self, ancestor: NodeId, node: NodeId) -> bool {
        ancestor == node || self.ancestors(node).any(|candidate| candidate == ancestor)
    }

    pub fn common_ancestor(&self, left: NodeId, right: NodeId) -> Option<NodeId> {
        std::iter::once(left)
            .chain(self.ancestors(left))
            .find(|&candidate| self.is_inclusive_ancestor(candidate, right))
    }

    pub fn tree_root(&self, id: NodeId) -> NodeId {
        self.ancestors(id).last().unwrap_or(id)
    }

    // connected through any number of shadow roots to their hosts, as the DOM's
    // shadow-including root defines it
    pub fn is_connected(&self, id: NodeId) -> bool {
        self.contains(id) && self.shadow_including_root(id) == self.root
    }

    pub fn shadow_including_root(&self, id: NodeId) -> NodeId {
        let mut root = self.tree_root(id);
        while let Some(&(host, _)) = self.shadow_hosts.get(&root) {
            root = self.tree_root(host);
        }
        root
    }

    // a host's shadow root, with whether it is open
    pub fn attach_shadow(&mut self, host: NodeId, open: bool) -> Option<NodeId> {
        if self.element(host).is_none() || self.shadow_roots.contains_key(&host) {
            return None;
        }
        let root = self.allocate(NodeKind::Fragment);
        self.orphans.retain(|&orphan| orphan != root);
        self.shadow_roots.insert(host, root);
        self.shadow_hosts.insert(root, (host, open));
        Some(root)
    }

    pub fn has_shadow_roots(&self) -> bool {
        !self.shadow_roots.is_empty()
    }

    pub fn shadow_root(&self, host: NodeId) -> Option<NodeId> {
        self.shadow_roots.get(&host).copied()
    }

    // the host of a shadow root and whether the root is open
    pub fn shadow_host(&self, root: NodeId) -> Option<(NodeId, bool)> {
        self.shadow_hosts.get(&root).copied()
    }

    // the parent, or the host for a shadow root, which events and inheritance follow
    pub fn composed_parent(&self, id: NodeId) -> Option<NodeId> {
        self.parent(id)
            .or_else(|| self.shadow_host(id).map(|(host, _)| host))
    }

    // where an event goes next: a slotted node's slot, a shadow root's host, or the parent
    pub fn event_parent(&self, id: NodeId) -> Option<NodeId> {
        if self.has_shadow_roots()
            && let Some(slot) = self.assigned_slot(id)
        {
            return Some(slot);
        }
        self.composed_parent(id)
    }

    // the slot inside a shadow tree that a light child of its host renders in
    pub fn assigned_slot(&self, node: NodeId) -> Option<NodeId> {
        let host = self.parent(node)?;
        let root = self.shadow_root(host)?;
        let name = self
            .element(node)
            .and_then(|element| element.attr("slot"))
            .unwrap_or_default();
        if self.is_text(node) && self.text(node).is_some_and(|text| text.trim().is_empty()) {
            return None;
        }
        self.descendants(root).find(|&candidate| {
            self.element(candidate).is_some_and(|element| {
                element.name == "slot" && element.attr("name").unwrap_or_default() == name
            })
        })
    }

    // the light children of the host that a slot shows
    pub fn assigned_nodes(&self, slot: NodeId) -> Vec<NodeId> {
        let root = self.tree_root(slot);
        let Some((host, _)) = self.shadow_host(root) else {
            return Vec::new();
        };
        self.children(host)
            .iter()
            .copied()
            .filter(|&child| self.assigned_slot(child) == Some(slot))
            .collect()
    }

    // the children a box is built from: a host shows its shadow tree, and a slot shows the
    // nodes assigned to it, or its own children when nothing is
    pub fn flat_children(&self, id: NodeId) -> std::borrow::Cow<'_, [NodeId]> {
        if let Some(root) = self.shadow_root(id) {
            return self.children(root).into();
        }
        let is_slot = self
            .element(id)
            .is_some_and(|element| element.name == "slot");
        if is_slot && self.shadow_host(self.tree_root(id)).is_some() {
            let assigned = self.assigned_nodes(id);
            if !assigned.is_empty() {
                return assigned.into();
            }
        }
        self.children(id).into()
    }

    // the text of the style elements directly inside a shadow root
    pub fn shadow_style_text(&self, root: NodeId) -> String {
        let policies = &self.content_security_policy;
        self.children(root)
            .iter()
            .filter_map(|&child| {
                let element = self
                    .element(child)
                    .filter(|element| element.name == "style")?;
                if self.disabled_sheets.contains(&child) {
                    return None;
                }
                if let Some(rules) = self.cssom_rules(child) {
                    return Some(rules.join("\n"));
                }
                let text = self.text_content(child);
                let check = policies.inline(
                    crate::csp::Directive::StyleElem,
                    &text,
                    crate::csp::Initiator {
                        nonce: element.attr("nonce"),
                        parser_inserted: false,
                    },
                );
                policies.permits(check).then_some(text)
            })
            .chain(
                self.adopted_sheets
                    .get(&root)
                    .into_iter()
                    .flatten()
                    .cloned(),
            )
            .collect::<Vec<_>>()
            .join("\n")
    }

    // the rules script set on a style element's sheet, while its text is still what they came from
    pub(crate) fn cssom_rules(&self, node: NodeId) -> Option<&Vec<String>> {
        let (text, rules) = self.sheet_rules.get(&node)?;
        (self.text_content(node) == *text).then_some(rules)
    }

    pub(crate) fn set_cssom_rules(&mut self, node: NodeId, rules: Vec<String>) {
        let text = self.text_content(node);
        self.sheet_rules.insert(node, (text, rules));
        self.version += 1;
        self.selector_version = next_stamp();
    }

    pub(crate) fn set_adopted_sheets(&mut self, root: NodeId, sheets: Vec<String>) {
        if sheets.is_empty() {
            self.adopted_sheets.remove(&root);
        } else {
            self.adopted_sheets.insert(root, sheets);
        }
        self.version += 1;
        self.selector_version = next_stamp();
    }

    pub(crate) fn set_sheet_disabled(&mut self, node: NodeId, disabled: bool) {
        let changed = if disabled {
            self.disabled_sheets.insert(node)
        } else {
            self.disabled_sheets.remove(&node)
        };
        if changed {
            self.version += 1;
            self.selector_version = next_stamp();
        }
    }

    pub(crate) fn script_fonts(&self) -> (&[ScriptFont], u64) {
        (&self.script_fonts, self.script_fonts_stamp)
    }

    // the faces document.fonts holds now, which the next layout's font set takes up
    pub(crate) fn set_script_fonts(&mut self, fonts: Vec<ScriptFont>) {
        self.script_fonts = fonts;
        self.script_fonts_stamp += 1;
        self.version += 1;
    }

    pub(crate) fn sheet_disabled(&self, node: NodeId) -> bool {
        self.disabled_sheets.contains(&node)
    }

    // document order of two nodes in one tree; nodes in different trees order by id
    pub fn tree_order(&self, left: NodeId, right: NodeId) -> Ordering {
        if left == right {
            return Ordering::Equal;
        }
        let chain = |node: NodeId| {
            let mut chain = std::iter::once(node)
                .chain(self.ancestors(node))
                .collect::<Vec<_>>();
            chain.reverse();
            chain
        };
        let (left_chain, right_chain) = (chain(left), chain(right));
        if left_chain[0] != right_chain[0] {
            return left_chain[0].cmp(&right_chain[0]);
        }
        let shared = left_chain
            .iter()
            .zip(&right_chain)
            .take_while(|(a, b)| a == b)
            .count();
        match (left_chain.get(shared), right_chain.get(shared)) {
            (None, _) => Ordering::Less,
            (_, None) => Ordering::Greater,
            (Some(&a), Some(&b)) => {
                let siblings = self.children(left_chain[shared - 1]);
                let position = |node| siblings.iter().position(|&child| child == node);
                position(a).cmp(&position(b))
            }
        }
    }

    pub fn template_content(&self, id: NodeId) -> Option<NodeId> {
        self.template_contents.get(&id).copied()
    }

    // the template content of a template element, created on first use for scripted templates
    pub fn ensure_template_content(&mut self, id: NodeId) -> Option<NodeId> {
        if let Some(content) = self.template_content(id) {
            return Some(content);
        }
        if self.element(id)?.name != "template" {
            return None;
        }
        let content = self.allocate(NodeKind::Fragment);
        self.orphans.retain(|&orphan| orphan != content);
        self.template_contents.insert(id, content);
        Some(content)
    }

    pub fn document_element(&self) -> Option<NodeId> {
        self.document_element_of(self.root)
    }

    // the same parts of any document node, such as one DOMParser made
    pub fn document_element_of(&self, document: NodeId) -> Option<NodeId> {
        self.element_children(document).next()
    }

    fn document_child_named(&self, document: NodeId, name: &str) -> Option<NodeId> {
        let html = self.document_element_of(document)?;
        self.element_children(html).find(|&child| {
            self.element(child)
                .is_some_and(|element| element.name == name)
        })
    }

    pub fn body(&self) -> Option<NodeId> {
        self.body_of(self.root)
    }

    pub fn body_of(&self, document: NodeId) -> Option<NodeId> {
        self.document_child_named(document, "body")
    }

    pub fn head(&self) -> Option<NodeId> {
        self.head_of(self.root)
    }

    pub fn head_of(&self, document: NodeId) -> Option<NodeId> {
        self.document_child_named(document, "head")
    }

    pub fn title(&self) -> Option<String> {
        self.title_of(self.root)
    }

    pub fn title_of(&self, document: NodeId) -> Option<String> {
        let title = self.descendants(document).find(|&id| {
            self.element(id)
                .is_some_and(|element| element.name == "title")
        })?;
        let text = self.text_content(title);
        let title = text.split_whitespace().collect::<Vec<_>>().join(" ");
        (!title.is_empty()).then_some(title)
    }

    pub fn create_element(&mut self, name: &str) -> NodeId {
        self.allocate(NodeKind::Element(Element::new(name)))
    }

    pub fn create_element_ns(&mut self, namespace: Namespace, name: &str) -> NodeId {
        let name = if namespace == Namespace::Html {
            name.to_ascii_lowercase()
        } else {
            name.to_string()
        };
        self.allocate(NodeKind::Element(Element {
            name,
            namespace,
            attrs: Attributes::new(),
        }))
    }

    pub fn create_text(&mut self, data: impl Into<String>) -> NodeId {
        self.allocate(NodeKind::Text(data.into()))
    }

    pub fn create_comment(&mut self, data: impl Into<String>) -> NodeId {
        self.allocate(NodeKind::Comment(data.into()))
    }

    pub fn create_fragment(&mut self) -> NodeId {
        self.allocate(NodeKind::Fragment)
    }

    pub fn adopt_element(&mut self, element: Element) -> NodeId {
        self.allocate(NodeKind::Element(element))
    }

    fn can_have_children(&self, id: NodeId) -> bool {
        matches!(
            self.kind(id),
            Some(NodeKind::Document | NodeKind::Element(_) | NodeKind::Fragment)
        )
    }

    // inserts a node, or the children of a fragment, before the reference or at the end;
    // returns the nodes that were inserted
    pub fn insert_before(
        &mut self,
        parent: NodeId,
        node: NodeId,
        reference: Option<NodeId>,
    ) -> Result<Vec<NodeId>, DomError> {
        self.validate_insertion(parent, node, reference)?;
        let from_fragment = matches!(self.kind(node), Some(NodeKind::Fragment));
        let inserted = if from_fragment {
            self.children(node).to_vec()
        } else {
            vec![node]
        };
        let reference = match reference {
            Some(reference) if inserted.contains(&reference) => {
                let mut candidate = self.sibling(reference, true);
                while let Some(next) = candidate {
                    if !inserted.contains(&next) {
                        break;
                    }
                    candidate = self.sibling(next, true);
                }
                candidate
            }
            other => other,
        };
        // a fragment gives up its children without a removal record of its own
        for &child in &inserted {
            self.detach_recorded(child, !from_fragment);
        }
        let index = match reference {
            Some(reference) => self
                .children(parent)
                .iter()
                .position(|&child| child == reference)
                .ok_or(DomError::NotFound)?,
            None => self.children(parent).len(),
        };
        for &child in &inserted {
            if let Some(data) = self.data_mut(child) {
                data.parent = Some(parent);
            }
        }
        if let Some(data) = self.data_mut(parent) {
            data.children.splice(index..index, inserted.iter().copied());
        }
        let count = inserted.len();
        if !inserted.is_empty() {
            let siblings = self.children(parent);
            let previous = index.checked_sub(1).map(|previous| siblings[previous]);
            let next = siblings.get(index + count).copied();
            self.record(
                parent,
                Mutation::ChildList {
                    added: inserted.clone(),
                    removed: Vec::new(),
                    previous,
                    next,
                },
            );
        }
        self.update_ranges(|_, boundary| {
            if boundary.node == parent && boundary.offset > index {
                boundary.offset += count;
            }
        });
        self.version += 1;
        if inserted.iter().any(|&node| !self.is_character_data(node)) {
            self.selector_version = next_stamp();
        }
        Ok(inserted)
    }

    // the checks insert_before makes before it changes anything
    pub fn validate_insertion(
        &self,
        parent: NodeId,
        node: NodeId,
        reference: Option<NodeId>,
    ) -> Result<(), DomError> {
        if !self.can_have_children(parent)
            || !self.contains(node)
            || matches!(self.kind(node), Some(NodeKind::Document))
            || self.is_inclusive_ancestor(node, parent)
        {
            return Err(DomError::HierarchyRequest);
        }
        if let Some(reference) = reference
            && self.parent(reference) != Some(parent)
        {
            return Err(DomError::NotFound);
        }
        Ok(())
    }

    pub fn append_child(&mut self, parent: NodeId, node: NodeId) -> Result<Vec<NodeId>, DomError> {
        self.insert_before(parent, node, None)
    }

    // removes a node from its parent, keeping it in the arena; returns the parent and index
    pub fn remove(&mut self, id: NodeId) -> Option<(NodeId, usize)> {
        let removed = self.detach_raw(id)?;
        self.orphans.push(id);
        Some(removed)
    }

    pub fn take_orphans(&mut self) -> Vec<NodeId> {
        std::mem::take(&mut self.orphans)
    }

    // frees a parentless node and everything below it, including template contents
    pub fn free_tree(&mut self, id: NodeId) -> Vec<NodeId> {
        if id == self.root || self.parent(id).is_some() || !self.contains(id) {
            return Vec::new();
        }
        let freed = self.subtree(id);
        if freed.iter().any(|&node| !self.is_character_data(node)) {
            self.selector_version = next_stamp();
        }
        for &node in &freed {
            self.template_contents.remove(&node);
            if let Some(root) = self.shadow_roots.remove(&node) {
                self.shadow_hosts.remove(&root);
            }
            let slot = &mut self.slots[node.index as usize];
            slot.node = None;
            slot.generation = slot.generation.wrapping_add(1);
            self.free.push(node.index);
        }
        self.version += 1;
        freed
    }

    // a node, everything below it and the contents of templates among them
    pub fn subtree(&self, root: NodeId) -> Vec<NodeId> {
        let mut nodes = Vec::new();
        let mut pending = vec![root];
        while let Some(current) = pending.pop() {
            let Some(data) = self.data(current) else {
                continue;
            };
            pending.extend(data.children.iter().copied());
            pending.extend(self.template_content(current));
            pending.extend(self.shadow_root(current));
            nodes.push(current);
        }
        nodes
    }

    pub fn set_attribute(&mut self, id: NodeId, name: &str, value: String) -> bool {
        let Some(name) = self.attribute_name(id, name) else {
            return false;
        };
        self.edit_element(id, &name.clone(), |element| {
            element.attrs.insert(name, value);
        })
        .is_some()
    }

    pub fn remove_attribute(&mut self, id: NodeId, name: &str) -> bool {
        let Some(name) = self.attribute_name(id, name) else {
            return false;
        };
        if self.attribute(id, &name).is_none() {
            return false;
        }
        self.edit_element(id, &name.clone(), |element| {
            element.attrs.remove(&name).is_some()
        })
        .unwrap_or(false)
    }

    // attribute names on HTML elements are case-insensitive
    fn attribute_name(&self, id: NodeId, name: &str) -> Option<String> {
        let element = self.element(id)?;
        Some(if element.namespace == Namespace::Html {
            name.to_ascii_lowercase()
        } else {
            name.to_string()
        })
    }

    pub fn attribute(&self, id: NodeId, name: &str) -> Option<&str> {
        self.element(id)?.attr(name)
    }

    pub fn has_class(&self, id: NodeId, class: &str) -> bool {
        self.element(id)
            .is_some_and(|element| element.has_class(class))
    }

    pub fn style_property(&self, id: NodeId, property: &str) -> Option<String> {
        if self.style_blocked(id) {
            return None;
        }
        self.element(id)?.style_property(property)
    }

    pub fn set_style_property(&mut self, id: NodeId, property: &str, value: String) -> bool {
        self.edit_element(id, "style", |element| {
            element.set_style_property(property, value)
        })
        .unwrap_or(false)
    }

    // copies a node, and with deep its descendants and template contents, as a new orphan
    pub fn clone_node(&mut self, id: NodeId, deep: bool) -> Option<NodeId> {
        let kind = self.kind(id)?.clone();
        let kind = match kind {
            NodeKind::Document => NodeKind::Fragment,
            other => other,
        };
        let copy = self.allocate(kind);
        if deep {
            for child in self.children(id).to_vec() {
                if let Some(child_copy) = self.clone_node(child, true) {
                    self.orphans.retain(|&orphan| orphan != child_copy);
                    self.append_raw(copy, child_copy);
                }
            }
            if let Some(content) = self.template_content(id) {
                let content_copy = self.ensure_template_content(copy)?;
                for child in self.children(content).to_vec() {
                    if let Some(child_copy) = self.clone_node(child, true) {
                        self.orphans.retain(|&orphan| orphan != child_copy);
                        self.append_raw(content_copy, child_copy);
                    }
                }
            }
        }
        Some(copy)
    }

    // splits a text node at a UTF-16 offset; the tail follows it in the parent, if any
    pub fn split_text(&mut self, id: NodeId, offset: usize) -> Option<NodeId> {
        let text = self.text(id)?.to_string();
        let length = self.node_length(id);
        let (_, tail) = split_utf16(&text, offset);
        let created = self.create_text(tail);
        if let Some(parent) = self.parent(id) {
            let next = self.sibling(id, true);
            self.insert_before(parent, created, next).ok()?;
            self.orphans.retain(|&orphan| orphan != created);
            let after = self.index_in_parent(id)? + 1;
            self.update_ranges(|_, boundary| {
                if boundary.node == id && boundary.offset > offset {
                    *boundary = Boundary {
                        node: created,
                        offset: boundary.offset - offset,
                    };
                } else if boundary.node == parent && boundary.offset == after {
                    boundary.offset += 1;
                }
            });
        }
        self.replace_data(id, offset, length.saturating_sub(offset), "");
        Some(created)
    }

    pub fn element_text_content(&self, id: NodeId) -> Option<String> {
        self.element(id)?;
        Some(self.text_content(id))
    }

    // textContent: the data of character data, or every descendant text node joined
    pub fn text_content(&self, id: NodeId) -> String {
        if let Some(data) = self.character_data(id) {
            return data.to_string();
        }
        self.descendants(id)
            .filter_map(|node| self.text(node))
            .collect()
    }

    pub fn node_text_content(&self, id: NodeId) -> Option<String> {
        self.contains(id).then(|| self.text_content(id))
    }

    pub fn document_text(&self) -> String {
        self.text_content(self.root)
    }

    // the flat UTF-16 offset of a node in the document's concatenated text
    pub fn node_text_offset(&self, id: NodeId) -> Option<usize> {
        if !self.is_connected(id) {
            return None;
        }
        let mut offset = 0;
        for node in self.descendants(self.root) {
            if node == id {
                return Some(offset);
            }
            if let Some(text) = self.text(node) {
                offset += text.encode_utf16().count();
            }
        }
        None
    }

    // the flat UTF-16 offset of every connected text node, from one walk
    pub fn text_node_offsets(&self) -> HashMap<NodeId, usize> {
        let mut offsets = HashMap::new();
        let mut position = 0;
        for node in self.descendants(self.root) {
            if let Some(text) = self.text(node) {
                offsets.insert(node, position);
                position += text.encode_utf16().count();
            }
        }
        offsets
    }

    // text nodes overlapping the flat UTF-16 range [start, end), as node-local spans
    pub fn text_node_spans(&self, start: usize, end: usize) -> Vec<(NodeId, usize, usize)> {
        let mut spans = Vec::new();
        if start >= end {
            return spans;
        }
        let mut position = 0;
        for node in self.descendants(self.root) {
            if position >= end {
                break;
            }
            let Some(text) = self.text(node) else {
                continue;
            };
            let length = text.encode_utf16().count();
            let from = start.max(position);
            let to = end.min(position + length);
            if from < to {
                spans.push((node, from - position, to - position));
            }
            position += length;
        }
        spans
    }

    // the text node and offset at a flat UTF-16 position, preferring the node that starts there
    pub fn text_point(&self, position: usize) -> Option<(NodeId, usize)> {
        if let Some((node, start, _)) = self.text_node_spans(position, position + 1).pop() {
            return Some((node, start));
        }
        let (node, _, end) = self
            .text_node_spans(position.checked_sub(1)?, position)
            .pop()?;
        Some((node, end))
    }

    pub fn inner_html(&self, id: NodeId) -> String {
        let mut output = String::new();
        let source = self.template_content(id).unwrap_or(id);
        let raw = self
            .element(id)
            .is_some_and(|element| is_raw_text_element(&element.name));
        for &child in self.children(source) {
            self.serialize(child, raw, &mut output);
        }
        output
    }

    pub fn outer_html(&self, id: NodeId) -> String {
        let mut output = String::new();
        self.serialize(id, false, &mut output);
        output
    }

    fn serialize(&self, id: NodeId, raw_text: bool, output: &mut String) {
        match self.kind(id) {
            Some(NodeKind::Element(element)) => {
                output.push('<');
                output.push_str(&element.name);
                for (name, value) in &element.attrs {
                    output.push(' ');
                    output.push_str(name);
                    output.push_str("=\"");
                    escape_html_attribute(value, output);
                    output.push('"');
                }
                output.push('>');
                if is_void_element(&element.name) {
                    return;
                }
                output.push_str(&self.inner_html(id));
                output.push_str("</");
                output.push_str(&element.name);
                output.push('>');
            }
            Some(NodeKind::Text(text)) if raw_text => output.push_str(text),
            Some(NodeKind::Text(text)) => escape_html(text, output),
            Some(NodeKind::Comment(text)) => {
                output.push_str("<!--");
                output.push_str(text);
                output.push_str("-->");
            }
            Some(NodeKind::Document | NodeKind::Fragment) => {
                for &child in self.children(id) {
                    self.serialize(child, false, output);
                }
            }
            None => {}
        }
    }

    pub fn style_text(&self) -> String {
        let mut output = String::new();
        for node in self.descendants(self.root) {
            if self
                .element(node)
                .is_some_and(|element| element.name == "style")
            {
                output.push_str(&self.text_content(node));
            }
        }
        output
    }

    pub fn script_text(&self) -> Vec<String> {
        self.descendants(self.root)
            .filter(|&node| {
                self.element(node).is_some_and(|element| {
                    element.name == "script" && element.attr("src").is_none()
                })
            })
            .map(|node| self.text_content(node))
            .collect()
    }

    pub fn script_sources(&self) -> Vec<ScriptSource> {
        self.script_elements()
            .into_iter()
            .map(|(_, source)| source)
            .collect()
    }

    pub(crate) fn script_elements(&self) -> Vec<(NodeId, ScriptSource)> {
        self.descendants(self.root)
            .filter_map(|node| Some((node, self.script_source(node)?)))
            .collect()
    }

    // what a script element runs, or none for other elements and for data blocks such as
    // type="application/json" or a template, which never run
    pub(crate) fn script_source(&self, node: NodeId) -> Option<ScriptSource> {
        let element = self
            .element(node)
            .filter(|element| element.name == "script")?;
        let kind = element
            .attr("type")
            .map(|kind| kind.trim().to_ascii_lowercase())
            .unwrap_or_default();
        if kind == "importmap" {
            return Some(ScriptSource::ImportMap(self.text_content(node)));
        }
        let is_module = kind == "module";
        let runs = matches!(
            kind.as_str(),
            "" | "text/javascript"
                | "application/javascript"
                | "application/ecmascript"
                | "text/ecmascript"
                | "module"
        );
        runs.then(|| match (element.attr("src"), is_module) {
            (Some(source), true) => ScriptSource::ExternalModule(source.to_string()),
            (Some(source), false) => ScriptSource::External(source.to_string()),
            (None, true) => ScriptSource::InlineModule(self.text_content(node)),
            (None, false) => ScriptSource::Inline(self.text_content(node)),
        })
    }

    pub(crate) fn script_started(&self, node: NodeId) -> bool {
        self.started_scripts.contains(&node)
    }

    pub(crate) fn mark_script_started(&mut self, node: NodeId) {
        self.started_scripts.insert(node);
    }

    pub fn stylesheet_sources(&self) -> Vec<StylesheetSource> {
        self.stylesheet_elements()
            .into_iter()
            .map(|(_, source)| source)
            .collect()
    }

    pub(crate) fn stylesheet_elements(&self) -> Vec<(NodeId, StylesheetSource)> {
        let mut stylesheets = Vec::new();
        for node in self.descendants(self.root) {
            let Some(element) = self.element(node) else {
                continue;
            };
            if self.disabled_sheets.contains(&node) {
                continue;
            }
            if element.name == "style" {
                let source = match self.cssom_rules(node) {
                    Some(rules) => StylesheetSource::Cssom(rules.join("\n")),
                    None => StylesheetSource::Inline(self.text_content(node)),
                };
                stylesheets.push((node, source));
            } else if element.name == "link"
                && element.attr("rel").is_some_and(|rel| {
                    rel.split_ascii_whitespace()
                        .any(|token| token.eq_ignore_ascii_case("stylesheet"))
                })
                && let Some(href) = element.attr("href")
            {
                stylesheets.push((node, StylesheetSource::External(href.to_string())));
            }
        }
        for text in self.adopted_sheets.get(&self.root).into_iter().flatten() {
            stylesheets.push((self.root, StylesheetSource::Cssom(text.clone())));
        }
        stylesheets
    }

    fn connected_elements(&self) -> impl Iterator<Item = (NodeId, &Element)> + '_ {
        self.descendants(self.root)
            .filter_map(|node| self.element(node).map(|element| (node, element)))
    }

    pub fn find_element_by_id(&self, id: &str) -> Option<NodeId> {
        self.connected_elements()
            .find(|(_, element)| element.attr("id") == Some(id))
            .map(|(node, _)| node)
    }

    pub fn autofocus_candidates(&self) -> Vec<NodeId> {
        self.connected_elements()
            .filter(|(_, element)| element.attrs.contains_key("autofocus"))
            .map(|(node, _)| node)
            .collect()
    }

    fn elements_named(&self, id: NodeId, name: &str) -> bool {
        self.element(id).is_some_and(|element| element.name == name)
    }

    fn closest_named(&self, id: NodeId, name: &str) -> Option<NodeId> {
        self.ancestors(id)
            .find(|&ancestor| self.elements_named(ancestor, name))
    }

    pub fn form_controls(&self, form: NodeId) -> Option<Vec<NodeId>> {
        if !self.elements_named(form, "form") {
            return None;
        }
        let scope = self.tree_root(form);
        Some(
            self.descendants(scope)
                .filter(|&node| {
                    self.element(node).is_some_and(|element| {
                        matches!(
                            element.name.as_str(),
                            "button" | "input" | "select" | "textarea"
                        )
                    }) && self.form_owner(node) == Some(form)
                })
                .collect(),
        )
    }

    pub fn form_owner(&self, id: NodeId) -> Option<NodeId> {
        let element = self.element(id)?;
        if !matches!(
            element.name.as_str(),
            "button" | "input" | "select" | "textarea"
        ) {
            return None;
        }
        if let Some(form_id) = element.attr("form") {
            if !self.is_connected(id) {
                return None;
            }
            let candidate = self.find_element_by_id(form_id)?;
            return self.elements_named(candidate, "form").then_some(candidate);
        }
        self.form_ancestor(id)
    }

    pub fn form_ancestor(&self, id: NodeId) -> Option<NodeId> {
        self.closest_named(id, "form")
    }

    pub fn select_ancestor(&self, id: NodeId) -> Option<NodeId> {
        self.closest_named(id, "select")
    }

    pub fn label_ancestor(&self, id: NodeId) -> Option<NodeId> {
        self.closest_named(id, "label")
    }

    // the option elements of a select, skipping options of nested selects
    pub fn options(&self, select: NodeId) -> Option<Vec<NodeId>> {
        if !self.elements_named(select, "select") {
            return None;
        }
        fn collect(document: &Document, parent: NodeId, options: &mut Vec<NodeId>) {
            for child in document.element_children(parent) {
                match document.element(child).map(|element| element.name.as_str()) {
                    Some("option") => options.push(child),
                    Some("select") => {}
                    _ => collect(document, child, options),
                }
            }
        }
        let mut options = Vec::new();
        collect(self, select, &mut options);
        Some(options)
    }

    pub fn label_control(&self, label: NodeId) -> Option<NodeId> {
        let element = self.element(label)?;
        if element.name != "label" {
            return None;
        }
        if let Some(id) = element.attr("for") {
            let candidate = self.find_element_by_id(id)?;
            return self
                .element(candidate)
                .is_some_and(is_labelable)
                .then_some(candidate);
        }
        self.descendants(label)
            .find(|&node| self.element(node).is_some_and(is_labelable))
    }

    pub fn radio_group(&self, id: NodeId) -> Option<Vec<NodeId>> {
        let is_radio = |element: &Element| {
            element.name == "input"
                && element
                    .attr("type")
                    .is_some_and(|kind| kind.eq_ignore_ascii_case("radio"))
        };
        let radio = self.element(id).filter(|element| is_radio(element))?;
        let name = radio.attr("name")?;
        let form = self.form_owner(id)?;
        Some(
            self.form_controls(form)?
                .into_iter()
                .filter(|&candidate| {
                    self.element(candidate).is_some_and(|element| {
                        is_radio(element) && element.attr("name") == Some(name)
                    })
                })
                .collect(),
        )
    }

    pub fn link_reference(&self, id: NodeId) -> Option<String> {
        std::iter::once(id)
            .chain(self.ancestors(id))
            .find_map(|node| {
                let element = self.element(node)?;
                (element.name == "a")
                    .then(|| element.attr("href").map(str::to_string))
                    .flatten()
            })
    }

    pub fn find_element_by_selector(&self, selector: &str) -> Option<NodeId> {
        self.find_element_by_selector_within(self.root, selector)
    }

    pub fn find_elements_by_selector(&self, selector: &str) -> Option<Vec<NodeId>> {
        self.find_elements_by_selector_within(self.root, selector)
    }

    pub fn find_element_by_selector_within(&self, scope: NodeId, selector: &str) -> Option<NodeId> {
        let selectors = crate::css::Selector::parse_list(selector).ok()?;
        self.descendants(scope)
            .find(|&node| self.matches_selectors(node, &selectors))
    }

    pub fn find_elements_by_selector_within(
        &self,
        scope: NodeId,
        selector: &str,
    ) -> Option<Vec<NodeId>> {
        let selectors = crate::css::Selector::parse_list(selector).ok()?;
        Some(
            self.descendants(scope)
                .filter(|&node| self.matches_selectors(node, &selectors))
                .collect(),
        )
    }

    pub(crate) fn matches_selectors(
        &self,
        node: NodeId,
        selectors: &[crate::css::Selector],
    ) -> bool {
        self.matches_selectors_in(node, selectors, &crate::css::StyleState::default())
    }

    // matching against what the page is doing: focus, hover, checkedness and the top layer
    pub(crate) fn matches_selectors_in(
        &self,
        node: NodeId,
        selectors: &[crate::css::Selector],
        state: &crate::css::StyleState,
    ) -> bool {
        self.is_element(node)
            && selectors
                .iter()
                .any(|selector| selector.matches(self, node, None, state))
    }

    pub(crate) fn select_within(
        &self,
        scope: NodeId,
        selector: &str,
        state: &crate::css::StyleState,
    ) -> Option<Vec<NodeId>> {
        let selectors = crate::css::Selector::parse_list(selector).ok()?;
        Some(
            self.descendants(scope)
                .filter(|&node| self.matches_selectors_in(node, &selectors, state))
                .collect(),
        )
    }

    pub fn accessibility_tree(&self) -> AccessibilityNode {
        AccessibilityNode {
            role: "document".to_string(),
            name: String::new(),
            value: None,
            node: self.root,
            disabled: false,
            focusable: false,
            children: self.accessibility_children(self.root),
        }
    }

    // html and body carry no role of their own and head nothing to present, so their content
    // sits directly under the document
    fn accessibility_children(&self, id: NodeId) -> Vec<AccessibilityNode> {
        let mut children = Vec::new();
        for child in self.element_children(id) {
            match self.element(child).map(|element| element.name.as_str()) {
                Some("head") => {}
                Some("html" | "body") => children.extend(self.accessibility_children(child)),
                _ => children.extend(self.accessibility_node(child)),
            }
        }
        children
    }

    fn accessibility_node(&self, id: NodeId) -> Option<AccessibilityNode> {
        let element = self.element(id)?;
        if is_accessibility_hidden(element) {
            return None;
        }
        Some(AccessibilityNode {
            role: accessibility_role(element),
            name: self.accessibility_name(id, element),
            value: self.accessibility_value(id, element),
            node: id,
            disabled: is_accessibility_disabled(element),
            focusable: is_accessibility_focusable(element),
            children: self.accessibility_children(id),
        })
    }

    fn accessibility_name(&self, id: NodeId, element: &Element) -> String {
        if let Some(label) = element
            .attr("aria-label")
            .filter(|label| !label.trim().is_empty())
        {
            return label.trim().to_string();
        }
        if element.name == "img"
            && let Some(alt) = element.attr("alt")
        {
            return alt.to_string();
        }
        if let Some(title) = element
            .attr("title")
            .filter(|title| !title.trim().is_empty())
        {
            return title.trim().to_string();
        }
        if matches!(
            element.name.as_str(),
            "a" | "button" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "label" | "li" | "option"
        ) || self.element_children(id).next().is_none()
        {
            return self.text_content(id).trim().to_string();
        }
        if matches!(element.name.as_str(), "input" | "textarea") {
            return element
                .attr("placeholder")
                .or_else(|| element.attr("value"))
                .unwrap_or_default()
                .trim()
                .to_string();
        }
        String::new()
    }

    fn accessibility_value(&self, id: NodeId, element: &Element) -> Option<String> {
        element
            .attr("aria-valuenow")
            .or_else(|| element.attr("value"))
            .map(str::to_string)
            .or_else(|| {
                (element.name == "textarea")
                    .then(|| self.text_content(id).trim().to_string())
                    .filter(|value| !value.is_empty())
            })
    }
}

impl Element {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_ascii_lowercase(),
            namespace: Namespace::Html,
            attrs: Attributes::new(),
        }
    }

    // tagName: upper case for HTML elements, as written otherwise
    pub fn tag_name(&self) -> String {
        if self.namespace == Namespace::Html {
            self.name.to_ascii_uppercase()
        } else {
            self.name.clone()
        }
    }

    pub fn attr(&self, name: &str) -> Option<&str> {
        if let Some(value) = self.attrs.get(name) {
            return Some(value);
        }
        self.attrs
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    pub fn classes(&self) -> impl Iterator<Item = &str> {
        self.attr("class").unwrap_or("").split_ascii_whitespace()
    }

    pub fn has_class(&self, class: &str) -> bool {
        let Some(class) = class_token(class) else {
            return false;
        };
        self.classes().any(|candidate| candidate == class)
    }

    pub fn add_class(&mut self, class: &str) -> bool {
        let Some(class) = class_token(class) else {
            return false;
        };
        if self.has_class(class) {
            return false;
        }
        let classes = self
            .classes()
            .chain(std::iter::once(class))
            .collect::<Vec<_>>()
            .join(" ");
        self.attrs.insert("class".to_string(), classes);
        true
    }

    pub fn remove_class(&mut self, class: &str) -> bool {
        let Some(class) = class_token(class) else {
            return false;
        };
        if !self.has_class(class) {
            return false;
        }
        let classes = self
            .classes()
            .filter(|candidate| *candidate != class)
            .collect::<Vec<_>>()
            .join(" ");
        self.attrs.insert("class".to_string(), classes);
        true
    }

    pub fn toggle_class(&mut self, class: &str, force: Option<bool>) -> bool {
        let present = self.has_class(class);
        let enabled = force.unwrap_or(!present);
        if enabled {
            self.add_class(class);
        } else {
            self.remove_class(class);
        }
        enabled
    }

    pub fn style_property(&self, property: &str) -> Option<String> {
        let style = self.attr("style")?;
        let property = css_property_name(property);
        style.split(';').find_map(|declaration| {
            let (name, value) = declaration.split_once(':')?;
            (name.trim().eq_ignore_ascii_case(&property)).then(|| value.trim().to_string())
        })
    }

    pub fn set_style_property(&mut self, property: &str, value: String) -> bool {
        let property = css_property_name(property);
        let mut declarations = self
            .attr("style")
            .unwrap_or("")
            .split(';')
            .filter_map(|declaration| {
                let (name, value) = declaration.split_once(':')?;
                Some((name.trim().to_ascii_lowercase(), value.trim().to_string()))
            })
            .collect::<Vec<_>>();
        if value.is_empty() {
            declarations.retain(|(name, _)| !name.eq_ignore_ascii_case(&property));
        } else if let Some(declaration) = declarations
            .iter_mut()
            .find(|(name, _)| name.eq_ignore_ascii_case(&property))
        {
            declaration.1 = value;
        } else {
            declarations.push((property, value));
        }
        let style = declarations
            .into_iter()
            .map(|(name, value)| format!("{name}: {value}"))
            .collect::<Vec<_>>()
            .join("; ");
        self.attrs.insert("style".to_string(), style);
        true
    }
}

fn accessibility_role(element: &Element) -> String {
    if let Some(role) = element.attr("role").filter(|role| !role.trim().is_empty()) {
        return role.trim().to_ascii_lowercase();
    }
    match element.name.as_str() {
        "main" => "main",
        "nav" => "navigation",
        "header" => "banner",
        "footer" => "contentinfo",
        "aside" => "complementary",
        "form" => "form",
        "button" => "button",
        "a" if element.attr("href").is_some() => "link",
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => "heading",
        "img" => "img",
        "input" => input_accessibility_role(element),
        "textarea" => "textbox",
        "select" => "combobox",
        "option" => "option",
        "ul" | "ol" => "list",
        "li" => "listitem",
        "table" => "table",
        "tr" => "row",
        "th" => "columnheader",
        "td" => "cell",
        "label" => "label",
        _ => "generic",
    }
    .to_string()
}

fn input_accessibility_role(element: &Element) -> &'static str {
    match element
        .attr("type")
        .unwrap_or("text")
        .to_ascii_lowercase()
        .as_str()
    {
        "button" | "submit" | "reset" | "image" => "button",
        "checkbox" => "checkbox",
        "radio" => "radio",
        "range" => "slider",
        "hidden" => "none",
        _ => "textbox",
    }
}

fn is_accessibility_disabled(element: &Element) -> bool {
    element.attr("disabled").is_some()
        || element
            .attr("aria-disabled")
            .is_some_and(|value| value.eq_ignore_ascii_case("true"))
}

fn is_accessibility_focusable(element: &Element) -> bool {
    if is_accessibility_disabled(element)
        || (element.name == "input"
            && element
                .attr("type")
                .is_some_and(|kind| kind.eq_ignore_ascii_case("hidden")))
    {
        return false;
    }
    if let Some(tabindex) = element.attr("tabindex") {
        return tabindex.parse::<i32>().is_ok_and(|value| value >= 0);
    }
    matches!(
        element.name.as_str(),
        "a" if element.attr("href").is_some()
    ) || matches!(
        element.name.as_str(),
        "button" | "input" | "select" | "textarea"
    )
}

fn is_accessibility_hidden(element: &Element) -> bool {
    if matches!(
        element.name.as_str(),
        "head" | "link" | "meta" | "noscript" | "script" | "style" | "template" | "title"
    ) || (element.name == "input"
        && element
            .attr("type")
            .is_some_and(|kind| kind.eq_ignore_ascii_case("hidden")))
    {
        return true;
    }
    if element.attr("hidden").is_some()
        || element
            .attr("aria-hidden")
            .is_some_and(|value| value.eq_ignore_ascii_case("true"))
    {
        return true;
    }
    let style = element
        .attr("style")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .replace(' ', "");
    style.contains("display:none") || style.contains("visibility:hidden")
}

fn class_token(class: &str) -> Option<&str> {
    let class = class.trim();
    (!class.is_empty()
        && !class
            .chars()
            .any(|character| character.is_ascii_whitespace()))
    .then_some(class)
}

pub(crate) fn is_labelable(element: &Element) -> bool {
    match element.name.as_str() {
        "button" | "meter" | "output" | "progress" | "select" | "textarea" => true,
        "input" => !element
            .attr("type")
            .is_some_and(|kind| kind.eq_ignore_ascii_case("hidden")),
        _ => false,
    }
}

fn css_property_name(property: &str) -> String {
    if property.starts_with("--") {
        return property.to_string();
    }
    let mut output = String::with_capacity(property.len() + 2);
    for character in property.chars() {
        if character.is_ascii_uppercase() {
            output.push('-');
            output.push(character.to_ascii_lowercase());
        } else {
            output.push(character);
        }
    }
    output
}

pub(crate) fn split_utf16(value: &str, offset: usize) -> (String, String) {
    let units = value.encode_utf16().collect::<Vec<_>>();
    let offset = offset.min(units.len());
    (
        String::from_utf16_lossy(&units[..offset]),
        String::from_utf16_lossy(&units[offset..]),
    )
}

fn is_raw_text_element(name: &str) -> bool {
    matches!(
        name,
        "style" | "script" | "xmp" | "iframe" | "noembed" | "noframes" | "plaintext"
    )
}

fn escape_html(text: &str, output: &mut String) {
    for character in text.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '\u{a0}' => output.push_str("&nbsp;"),
            _ => output.push(character),
        }
    }
}

fn escape_html_attribute(text: &str, output: &mut String) {
    for character in text.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '"' => output.push_str("&quot;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '\u{a0}' => output.push_str("&nbsp;"),
            _ => output.push(character),
        }
    }
}

// elements whose start tag closes an open p, as the html tree construction rules list them
const CLOSES_PARAGRAPH: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "center",
    "details",
    "dialog",
    "dir",
    "div",
    "dl",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hgroup",
    "hr",
    "li",
    "dd",
    "dt",
    "listing",
    "main",
    "menu",
    "nav",
    "ol",
    "p",
    "plaintext",
    "pre",
    "search",
    "section",
    "summary",
    "table",
    "ul",
    "xmp",
];
// elements that only belong in head before the body starts
const HEAD_CONTENT: &[&str] = &[
    "base", "basefont", "bgsound", "link", "meta", "noframes", "script", "style", "template",
    "title",
];
// the default scope's boundaries, past which a search for an open element stops
const SCOPE_BOUNDARIES: &[&str] = &[
    "applet",
    "caption",
    "html",
    "table",
    "td",
    "th",
    "marquee",
    "object",
    "template",
    "foreignObject",
    "desc",
];

fn is_heading(name: &str) -> bool {
    matches!(name, "h1" | "h2" | "h3" | "h4" | "h5" | "h6")
}

// the html tree construction rules a document and its fragments are built by: implied html,
// head and body in a whole document, implied end tags, implied table sections, and end tags
// with nothing open to close ignored
struct TreeBuilder {
    parent: NodeId,
    whole_document: bool,
    // open elements, with the node their children go into
    stack: Vec<(NodeId, NodeId)>,
    created: Vec<NodeId>,
    html: Option<NodeId>,
    head: Option<NodeId>,
    body: Option<NodeId>,
}

impl TreeBuilder {
    fn target(&self) -> NodeId {
        self.stack.last().map_or(self.parent, |&(_, target)| target)
    }

    fn name(document: &Document, id: NodeId) -> &str {
        document
            .element(id)
            .map_or("", |element| element.name.as_str())
    }

    fn current(&self, document: &Document) -> Option<String> {
        self.stack
            .last()
            .map(|&(open, _)| Self::name(document, open).to_string())
    }

    // where an open element sits on the stack, when no boundary of the default scope or the
    // extra ones hides it
    fn in_scope(&self, document: &Document, names: &[&str], extra: &[&str]) -> Option<usize> {
        self.in_bounds(document, names, |name| {
            SCOPE_BOUNDARIES.contains(&name) || extra.contains(&name)
        })
    }

    // the table scope stops only at html, table and template
    fn in_table_scope(&self, document: &Document, names: &[&str]) -> Option<usize> {
        self.in_bounds(document, names, |name| {
            matches!(name, "html" | "table" | "template")
        })
    }

    fn in_bounds(
        &self,
        document: &Document,
        names: &[&str],
        boundary: impl Fn(&str) -> bool,
    ) -> Option<usize> {
        for (index, &(open, _)) in self.stack.iter().enumerate().rev() {
            let name = Self::name(document, open);
            if names.contains(&name) {
                return Some(index);
            }
            if boundary(name) {
                return None;
            }
        }
        None
    }

    fn close_paragraph(&mut self, document: &Document) {
        if let Some(index) = self.in_scope(document, &["p"], &["button"]) {
            self.stack.truncate(index);
        }
    }

    fn insert(
        &mut self,
        document: &mut Document,
        target: NodeId,
        name: String,
        attrs: Attributes,
        self_closing: bool,
    ) -> NodeId {
        let in_svg = self.stack.iter().any(|&(open, _)| {
            document
                .element(open)
                .is_some_and(|element| element.namespace == Namespace::Svg)
        });
        let namespace = if name == "svg" || in_svg {
            Namespace::Svg
        } else if name == "math" {
            Namespace::MathMl
        } else {
            Namespace::Html
        };
        let void = is_void_element(&name);
        let template = name == "template";
        let script = name == "script";
        let id = document.allocate(NodeKind::Element(Element {
            name,
            namespace,
            attrs,
        }));
        if script {
            document.started_scripts.insert(id);
        }
        self.created.push(id);
        document.append_raw(target, id);
        if !(self_closing || void) {
            let content = if template {
                let content = document.allocate(NodeKind::Fragment);
                document.template_contents.insert(id, content);
                content
            } else {
                id
            };
            self.stack.push((id, content));
        }
        id
    }

    fn implied(&mut self, document: &mut Document, name: &str) -> NodeId {
        let target = self.target();
        self.insert(document, target, name.to_string(), Attributes::new(), false)
    }

    fn ensure_html(&mut self, document: &mut Document) -> NodeId {
        if let Some(html) = self.html {
            return html;
        }
        let html = self.implied(document, "html");
        self.html = Some(html);
        html
    }

    fn ensure_head(&mut self, document: &mut Document) -> NodeId {
        if let Some(head) = self.head {
            return head;
        }
        self.ensure_html(document);
        let head = self.implied(document, "head");
        self.head = Some(head);
        head
    }

    fn ensure_body(&mut self, document: &mut Document) {
        if self.body.is_some() {
            return;
        }
        self.ensure_head(document);
        self.leave_head(document);
        let body = self.implied(document, "body");
        self.body = Some(body);
    }

    fn leave_head(&mut self, document: &Document) {
        if let Some(index) = self
            .stack
            .iter()
            .position(|&(open, _)| Self::name(document, open) == "head")
        {
            self.stack.truncate(index);
        }
    }

    fn merge_attributes(document: &mut Document, id: NodeId, attrs: Attributes) {
        if let Some(NodeKind::Element(element)) = document.data_mut(id).map(|data| &mut data.kind) {
            for (name, value) in attrs {
                element.attrs.insert_missing(name, value);
            }
        }
    }

    fn token(&mut self, document: &mut Document, token: Token) {
        match token {
            Token::Start {
                name,
                attrs,
                self_closing,
            } => self.start(document, name, attrs, self_closing),
            Token::End { name } => self.end(document, &name),
            Token::Text(text) => {
                if text.is_empty() {
                    return;
                }
                if self.whole_document && self.body.is_none() {
                    let in_head_element = self
                        .current(document)
                        .is_some_and(|current| current != "head" && current != "html");
                    if !in_head_element {
                        if text
                            .chars()
                            .all(|character| character.is_ascii_whitespace())
                        {
                            // before html and before head, whitespace is dropped
                            if self.html.is_none() || self.head.is_none() {
                                return;
                            }
                        } else {
                            self.ensure_body(document);
                        }
                    }
                }
                let target = self.target();
                // characters inserted after a text node join it, as when an ignored tag
                // stood between them
                let last = document.children(target).last().copied();
                if let Some(last) = last
                    && self.created.last() == Some(&last)
                    && matches!(document.kind(last), Some(NodeKind::Text(_)))
                    && let Some(NodeKind::Text(existing)) =
                        document.data_mut(last).map(|data| &mut data.kind)
                {
                    existing.push_str(&text);
                    return;
                }
                let id = document.allocate(NodeKind::Text(text));
                self.created.push(id);
                document.append_raw(target, id);
            }
            Token::Comment(text) => {
                let id = document.allocate(NodeKind::Comment(text));
                self.created.push(id);
                let target = self.target();
                document.append_raw(target, id);
            }
        }
    }

    fn start(
        &mut self,
        document: &mut Document,
        name: String,
        attrs: Attributes,
        self_closing: bool,
    ) {
        let foreign = self.stack.iter().any(|&(open, _)| {
            document
                .element(open)
                .is_some_and(|element| element.namespace != Namespace::Html)
        });
        if self.whole_document && !foreign {
            match name.as_str() {
                "html" => {
                    match self.html {
                        Some(html) => Self::merge_attributes(document, html, attrs),
                        None => {
                            let target = self.target();
                            let html = self.insert(document, target, name, attrs, false);
                            self.html = Some(html);
                        }
                    }
                    return;
                }
                "head" => {
                    if self.head.is_none() && self.body.is_none() {
                        self.ensure_html(document);
                        let target = self.target();
                        let head = self.insert(document, target, name, attrs, false);
                        self.head = Some(head);
                    }
                    return;
                }
                "body" => {
                    match self.body {
                        Some(body) => Self::merge_attributes(document, body, attrs),
                        None => {
                            self.ensure_head(document);
                            self.leave_head(document);
                            let target = self.target();
                            let body = self.insert(document, target, name, attrs, false);
                            self.body = Some(body);
                        }
                    }
                    return;
                }
                _ if self.body.is_none() && HEAD_CONTENT.contains(&name.as_str()) => {
                    let head = self.ensure_head(document);
                    let inside = self.stack.iter().any(|&(open, _)| open == head);
                    let target = if inside { self.target() } else { head };
                    self.insert(document, target, name, attrs, self_closing);
                    return;
                }
                _ => self.ensure_body(document),
            }
        }
        if !foreign {
            self.imply_end_tags(document, &name);
        }
        let target = self.target();
        self.insert(document, target, name, attrs, self_closing);
    }

    // closes what a start tag implies is finished, and opens the table sections it needs
    fn imply_end_tags(&mut self, document: &mut Document, name: &str) {
        match name {
            "li" => {
                if let Some(index) = self.in_scope(document, &["li"], &["ol", "ul"]) {
                    self.stack.truncate(index);
                }
            }
            "dd" | "dt" => {
                if let Some(index) = self.in_scope(document, &["dd", "dt"], &["dl"]) {
                    self.stack.truncate(index);
                }
            }
            "option" => {
                if self.current(document).as_deref() == Some("option") {
                    self.stack.pop();
                }
                return;
            }
            "optgroup" => {
                if self.current(document).as_deref() == Some("option") {
                    self.stack.pop();
                }
                if self.current(document).as_deref() == Some("optgroup") {
                    self.stack.pop();
                }
                return;
            }
            "tbody" | "thead" | "tfoot" | "caption" | "colgroup" => {
                if let Some(index) = self.in_table_scope(
                    document,
                    &["tbody", "thead", "tfoot", "caption", "colgroup"],
                ) {
                    self.stack.truncate(index);
                }
                return;
            }
            "tr" => {
                if let Some(index) = self.in_table_scope(document, &["tr"]) {
                    self.stack.truncate(index);
                }
                if self.current(document).as_deref() == Some("table") {
                    self.implied(document, "tbody");
                }
                return;
            }
            "td" | "th" => {
                if let Some(index) = self.in_table_scope(document, &["td", "th"]) {
                    self.stack.truncate(index);
                }
                if self.current(document).as_deref() == Some("table") {
                    self.implied(document, "tbody");
                }
                if matches!(
                    self.current(document).as_deref(),
                    Some("tbody" | "thead" | "tfoot")
                ) {
                    self.implied(document, "tr");
                }
                return;
            }
            "col" => {
                if self.current(document).as_deref() == Some("table") {
                    self.implied(document, "colgroup");
                }
                return;
            }
            _ => {}
        }
        if CLOSES_PARAGRAPH.contains(&name) {
            self.close_paragraph(document);
        }
        if is_heading(name)
            && self
                .current(document)
                .is_some_and(|current| is_heading(&current))
        {
            self.stack.pop();
        }
    }

    fn end(&mut self, document: &mut Document, name: &str) {
        if self.whole_document && matches!(name, "html" | "body") {
            return;
        }
        if self.whole_document && name == "head" {
            self.leave_head(document);
            return;
        }
        if name == "p" && self.in_scope(document, &["p"], &["button"]).is_none() {
            // a closing p with no paragraph open stands for an empty one
            self.implied(document, "p");
        }
        let found = if is_heading(name) {
            self.in_scope(document, &["h1", "h2", "h3", "h4", "h5", "h6"], &[])
        } else {
            self.stack
                .iter()
                .rposition(|&(open, _)| Self::name(document, open) == name)
        };
        if let Some(index) = found {
            self.stack.truncate(index);
        }
    }

    // a whole document always has its html, head and body
    fn finish(&mut self, document: &mut Document) {
        if self.whole_document {
            self.ensure_body(document);
        }
    }
}

fn tokenize(source: &str) -> Result<Vec<Token>, ParseError> {
    let mut tokens = Vec::new();
    let mut cursor = 0;
    while cursor < source.len() {
        let Some(relative_start) = source[cursor..].find('<') else {
            let text = decode_entities(&source[cursor..]);
            if !text.is_empty() {
                tokens.push(Token::Text(text));
            }
            break;
        };
        let start = cursor + relative_start;
        if start > cursor {
            let text = decode_entities(&source[cursor..start]);
            if !text.is_empty() {
                tokens.push(Token::Text(text));
            }
        }
        if source[start..].starts_with("<!--") {
            let Some(end) = source[start + 4..].find("-->") else {
                return Err(ParseError {
                    offset: start,
                    message: "unterminated comment".to_string(),
                });
            };
            tokens.push(Token::Comment(
                source[start + 4..start + 4 + end].to_string(),
            ));
            cursor = start + 4 + end + 3;
            continue;
        }
        if source[start + 1..].starts_with(['!', '?']) {
            // any other declaration and a processing instruction end at the first > as a
            // comment of what they hold, so solid's <!> markers stay in its templates
            let close = source[start..]
                .find('>')
                .map_or(source.len(), |at| start + at);
            let body = &source[start + 1..close];
            if !body
                .get(..8)
                .is_some_and(|head| head.eq_ignore_ascii_case("!doctype"))
            {
                tokens.push(Token::Comment(
                    body.strip_prefix('!').unwrap_or(body).to_string(),
                ));
            }
            cursor = (close + 1).min(source.len());
            continue;
        }
        let Some(end) = find_tag_end(source, start + 1) else {
            return Err(ParseError {
                offset: start,
                message: "unterminated tag".to_string(),
            });
        };
        let raw = source[start + 1..end].trim();
        if raw.starts_with('!') || raw.starts_with('?') {
            cursor = end + 1;
            continue;
        }
        if let Some(raw) = raw.strip_prefix('/') {
            let name = parse_name(raw).ok_or_else(|| ParseError {
                offset: start,
                message: "closing tag has no name".to_string(),
            })?;
            tokens.push(Token::End {
                name: name.to_ascii_lowercase(),
            });
        } else {
            let (name, attrs, self_closing) = parse_start_tag(raw, start)?;
            tokens.push(Token::Start {
                name: name.clone(),
                attrs,
                self_closing,
            });
            if !self_closing && matches!(name.as_str(), "script" | "style") {
                let rest = &source[end + 1..];
                let marker = format!("</{name}");
                let lower = rest.to_ascii_lowercase();
                if let Some(close_start) = lower.find(&marker) {
                    let raw_text = &rest[..close_start];
                    if !raw_text.is_empty() {
                        tokens.push(Token::Text(raw_text.to_string()));
                    }
                    cursor = end + 1 + close_start;
                    continue;
                }
            }
        }
        cursor = end + 1;
    }
    Ok(tokens)
}

fn parse_start_tag(raw: &str, offset: usize) -> Result<(String, Attributes, bool), ParseError> {
    let mut cursor = 0;
    let name = parse_name_at(raw, &mut cursor).ok_or_else(|| ParseError {
        offset,
        message: "start tag has no name".to_string(),
    })?;
    let mut attrs = Attributes::new();
    let mut self_closing = false;
    while cursor < raw.len() {
        skip_ascii_whitespace(raw, &mut cursor);
        if cursor >= raw.len() {
            break;
        }
        if raw[cursor..].starts_with('/') {
            self_closing = true;
            cursor += 1;
            continue;
        }
        let Some(attr_name) = parse_attribute_name_at(raw, &mut cursor) else {
            return Err(ParseError {
                offset: offset + cursor,
                message: "invalid attribute name".to_string(),
            });
        };
        skip_ascii_whitespace(raw, &mut cursor);
        let mut value = String::new();
        if raw[cursor..].starts_with('=') {
            cursor += 1;
            skip_ascii_whitespace(raw, &mut cursor);
            if cursor >= raw.len() {
                return Err(ParseError {
                    offset: offset + cursor,
                    message: "attribute has no value".to_string(),
                });
            }
            let quote = raw.as_bytes()[cursor];
            if quote == b'\'' || quote == b'"' {
                cursor += 1;
                let value_start = cursor;
                while cursor < raw.len() && raw.as_bytes()[cursor] != quote {
                    cursor += 1;
                }
                if cursor >= raw.len() {
                    return Err(ParseError {
                        offset: offset + value_start,
                        message: "unterminated attribute value".to_string(),
                    });
                }
                value = decode_entities(&raw[value_start..cursor]);
                cursor += 1;
            } else {
                let value_start = cursor;
                while cursor < raw.len() && !raw.as_bytes()[cursor].is_ascii_whitespace() {
                    cursor += 1;
                }
                value = decode_entities(&raw[value_start..cursor]);
            }
        }
        attrs.insert_missing(attr_name, value);
    }
    Ok((name, attrs, self_closing))
}

fn parse_name(raw: &str) -> Option<&str> {
    let end = raw
        .char_indices()
        .find_map(|(index, character)| character.is_ascii_whitespace().then_some(index))
        .unwrap_or(raw.len());
    let name = raw[..end].trim_end_matches('/');
    (!name.is_empty() && name.chars().all(is_name_character)).then_some(name)
}

fn parse_name_at(raw: &str, cursor: &mut usize) -> Option<String> {
    let start = *cursor;
    while *cursor < raw.len() && is_name_character(raw.as_bytes()[*cursor] as char) {
        *cursor += 1;
    }
    (*cursor > start).then(|| raw[start..*cursor].to_ascii_lowercase())
}

// an attribute name runs to whitespace, '/', '>' or '=', so names such as Vue's @click
// and :key parse as HTML does
fn parse_attribute_name_at(raw: &str, cursor: &mut usize) -> Option<String> {
    let start = *cursor;
    while let Some(&byte) = raw.as_bytes().get(*cursor) {
        if byte.is_ascii_whitespace() || matches!(byte, b'/' | b'>' | b'=') {
            break;
        }
        *cursor += 1;
    }
    (*cursor > start).then(|| raw[start..*cursor].to_ascii_lowercase())
}

fn is_name_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | ':' | '.')
}

fn skip_ascii_whitespace(raw: &str, cursor: &mut usize) {
    while *cursor < raw.len() && raw.as_bytes()[*cursor].is_ascii_whitespace() {
        *cursor += 1;
    }
}

fn find_tag_end(source: &str, mut cursor: usize) -> Option<usize> {
    let mut quote = None;
    while cursor < source.len() {
        let byte = source.as_bytes()[cursor];
        if quote == Some(byte) {
            quote = None;
        } else if quote.is_none() && (byte == b'\'' || byte == b'"') {
            quote = Some(byte);
        } else if quote.is_none() && byte == b'>' {
            return Some(cursor);
        }
        cursor += 1;
    }
    None
}

pub(crate) fn is_void_element(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

#[derive(Debug)]
enum Token {
    Start {
        name: String,
        attrs: Attributes,
        self_closing: bool,
    },
    End {
        name: String,
    },
    Text(String),
    Comment(String),
}

fn decode_entities(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    while cursor < input.len() {
        let Some(relative_ampersand) = input[cursor..].find('&') else {
            output.push_str(&input[cursor..]);
            break;
        };
        let ampersand = cursor + relative_ampersand;
        output.push_str(&input[cursor..ampersand]);
        let Some(relative_semicolon) = input[ampersand + 1..].find(';') else {
            output.push('&');
            cursor = ampersand + 1;
            continue;
        };
        let semicolon = ampersand + 1 + relative_semicolon;
        let entity = &input[ampersand + 1..semicolon];
        if let Some(decoded) = decode_entity(entity) {
            output.push(decoded);
            cursor = semicolon + 1;
        } else {
            output.push_str(&input[ampersand..=semicolon]);
            cursor = semicolon + 1;
        }
    }
    output
}

fn decode_entity(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "apos" => Some('\''),
        "gt" => Some('>'),
        "lt" => Some('<'),
        "nbsp" => Some('\u{a0}'),
        "quot" => Some('"'),
        _ => {
            let number = entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"));
            let number = number
                .map(|number| u32::from_str_radix(number, 16).ok())
                .or_else(|| entity.strip_prefix('#').map(|number| number.parse().ok()))??;
            char::from_u32(number)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element_named(document: &Document, name: &str) -> NodeId {
        document
            .descendants(document.root())
            .find(|&node| {
                document
                    .element(node)
                    .is_some_and(|element| element.name == name)
            })
            .expect("element should exist")
    }

    #[test]
    fn parses_nested_elements_and_attributes() {
        let document =
            Document::parse(r#"<main data-kind='demo'><p>Hello <span>world</span></p></main>"#)
                .expect("document should parse");
        let html = document.document_element().expect("html should exist");
        let names = |id| {
            document
                .element_children(id)
                .filter_map(|child| document.element(child).map(|element| element.name.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(document.element(html).expect("html").name, "html");
        assert_eq!(names(html), ["head", "body"]);
        let main = element_named(&document, "main");
        let element = document.element(main).expect("main is an element");
        assert_eq!(element.attr("data-kind"), Some("demo"));
        assert_eq!(document.children(main).len(), 1);
        assert_eq!(document.text_content(main), "Hello world");
    }

    #[test]
    fn builds_implied_document_elements_and_end_tags() {
        let outline = |document: &Document| {
            let html = document.document_element().expect("html");
            document.outer_html(html)
        };
        let document = Document::parse(
            "<!doctype html><title>t</title><link rel=icon href=a><p>one<p>two<ul><li>a<li>b</ul><dl><dt>x<dd>y</dl><select><option>1<option>2</select><table><tr><td>c<td>d<tr><th>e</table></p><h1>h<h2>i</h2>",
        )
        .expect("document should parse");
        assert_eq!(
            outline(&document),
            "<html><head><title>t</title><link rel=\"icon\" href=\"a\"></head><body><p>one</p><p>two</p><ul><li>a</li><li>b</li></ul><dl><dt>x</dt><dd>y</dd></dl><select><option>1</option><option>2</option></select><table><tbody><tr><td>c</td><td>d</td></tr><tr><th>e</th></tr></tbody></table><p></p><h1>h</h1><h2>i</h2></body></html>"
        );
        let explicit = Document::parse(
            "<html lang=en><head><style>p{}</style></head>\n<body class=app><p>x</p></body></html>\n<body id=late>",
        )
        .expect("document should parse");
        assert_eq!(
            outline(&explicit),
            "<html lang=\"en\"><head><style>p{}</style></head>\n<body class=\"app\" id=\"late\"><p>x</p>\n</body></html>"
        );
        let empty = Document::parse("").expect("an empty document parses");
        assert_eq!(outline(&empty), "<html><head></head><body></body></html>");
        let svg = Document::parse("<svg><p>kept</p></svg>").expect("document should parse");
        assert_eq!(
            outline(&svg),
            "<html><head></head><body><svg><p>kept</p></svg></body></html>"
        );
    }

    #[test]
    fn keeps_comments_and_template_contents_out_of_the_tree_text() {
        let document = Document::parse(
            "<main>a<!-- note -->b<template id='row'><li>item</li></template></main>",
        )
        .expect("document should parse");
        let main = element_named(&document, "main");
        assert_eq!(document.children(main).len(), 4);
        assert_eq!(
            document.character_data(document.children(main)[1]),
            Some(" note ")
        );
        assert_eq!(document.text_content(main), "ab");
        let template = document
            .find_element_by_id("row")
            .expect("template should exist");
        assert!(document.children(template).is_empty());
        let content = document
            .template_content(template)
            .expect("template has content");
        assert_eq!(document.text_content(content), "item");
        assert_eq!(
            document.outer_html(main),
            "<main>a<!-- note -->b<template id=\"row\"><li>item</li></template></main>"
        );
    }

    #[test]
    fn turns_bogus_declarations_into_comments_as_the_html_parser_does() {
        let document = Document::parse("<!DOCTYPE html><p>down <!> up<?php x ?><!x-y z></p>")
            .expect("document should parse");
        let p = element_named(&document, "p");
        let data: Vec<_> = document
            .children(p)
            .iter()
            .map(|child| document.character_data(*child).unwrap_or_default())
            .collect();
        assert_eq!(data, ["down ", "", " up", "?php x ?", "x-y z"]);
        assert_eq!(
            document.outer_html(p),
            "<p>down <!----> up<!--?php x ?--><!--x-y z--></p>"
        );
    }

    #[test]
    fn reads_the_document_title_and_collapses_its_whitespace() {
        let document = Document::parse(
            "<html><head><title>  Ceri\n  Settings </title></head><body><p>body</p></body></html>",
        )
        .expect("document should parse");
        assert_eq!(document.title().as_deref(), Some("Ceri Settings"));
        assert_eq!(document.body(), Some(element_named(&document, "body")));
        assert_eq!(document.head(), Some(element_named(&document, "head")));

        let untitled = Document::parse("<main><title> </title></main>").expect("should parse");
        assert_eq!(untitled.title(), None);
    }

    #[test]
    fn builds_accessibility_roles_names_and_nodes() {
        let document = Document::parse(
            r#"<main><h1>Settings</h1><button aria-label="Save changes">Save</button><input type="checkbox" aria-label="Enable sync" disabled><a href="/help">Help</a><p hidden>secret</p></main>"#,
        )
        .expect("document should parse");
        let tree = document.accessibility_tree();
        assert_eq!(tree.role, "document");
        let main = &tree.children[0];
        assert_eq!(main.role, "main");
        assert_eq!(main.node, element_named(&document, "main"));
        let summary = main
            .children
            .iter()
            .map(|child| {
                (
                    child.role.as_str(),
                    child.name.as_str(),
                    child.disabled,
                    child.focusable,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            summary,
            vec![
                ("heading", "Settings", false, false),
                ("button", "Save changes", false, true),
                ("checkbox", "Enable sync", true, false),
                ("link", "Help", false, true),
            ]
        );
    }

    #[test]
    fn decodes_entities_and_collects_styles() {
        let document =
            Document::parse("<style>.accent { color: #fff; }</style><p>A &amp; B &#x21;</p>")
                .expect("document should parse");
        assert_eq!(document.style_text(), ".accent { color: #fff; }");
        let paragraph = element_named(&document, "p");
        assert_eq!(document.text_content(paragraph), "A & B !");
        assert_eq!(document.inner_html(paragraph), "A &amp; B !");
    }

    #[test]
    fn rejects_unterminated_tags() {
        let error = Document::parse("<main").expect_err("unterminated tag should fail");
        assert!(error.message.contains("unterminated tag"));
    }

    #[test]
    fn preserves_script_text_that_contains_comparison_operators() {
        let document = Document::parse(
            "<script>if (count < 2) { count = count + 1; }</script><button id='go'>go</button>",
        )
        .expect("document should parse");
        assert_eq!(
            document.script_text(),
            vec!["if (count < 2) { count = count + 1; }".to_string()]
        );
        let script = element_named(&document, "script");
        assert_eq!(
            document.inner_html(script),
            "if (count < 2) { count = count + 1; }"
        );
    }

    #[test]
    fn edits_style_properties_and_removes_empty_ones() {
        let mut document = Document::parse("<button id='go' style='color: red'>go</button>")
            .expect("document should parse");
        let button = document
            .find_element_by_id("go")
            .expect("button should exist");
        assert!(document.set_style_property(button, "backgroundColor", "#fff".to_string()));
        assert_eq!(
            document
                .style_property(button, "background-color")
                .as_deref(),
            Some("#fff")
        );
        assert!(document.set_style_property(button, "color", String::new()));
        assert_eq!(
            document.attribute(button, "style"),
            Some("background-color: #fff")
        );
    }

    #[test]
    fn finds_selectors_in_document_order_and_within_a_scope() {
        let document = Document::parse(
            "<main class='shell'><p data-check='first'>one</p><section><p class='card' data-check='second'>two</p></section><p class='direct' data-check='third'>three</p></main>",
        )
        .expect("document should parse");
        let texts = |nodes: Vec<NodeId>| {
            nodes
                .into_iter()
                .map(|node| document.text_content(node))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            texts(
                document
                    .find_elements_by_selector("[data-check]")
                    .expect("valid")
            ),
            vec!["one", "two", "three"]
        );
        let section = document
            .find_element_by_selector("section")
            .expect("section");
        assert_eq!(
            texts(
                document
                    .find_elements_by_selector_within(section, "[data-check]")
                    .expect("valid")
            ),
            vec!["two"]
        );
        assert!(document.find_element_by_selector(".shell .card").is_some());
        assert!(
            document
                .find_element_by_selector(".shell > .direct")
                .is_some()
        );
        assert!(document.find_element_by_selector("section.card").is_none());
        assert!(document.find_elements_by_selector("p[").is_none());
        assert_eq!(
            document
                .find_element_by_selector_within(section, ".shell .card")
                .map(|node| document.text_content(node)),
            Some("two".to_string())
        );
    }

    #[test]
    fn resolves_forms_labels_options_and_radio_groups() {
        let document = Document::parse(
            "<form id='profile'><input id='name' name='name'><fieldset><textarea name='note'></textarea></fieldset><select id='pick'><option>a</option><optgroup><option>b</option></optgroup></select><input type='radio' name='size' id='small'><input type='radio' name='size' id='large'></form><input id='outside' form='profile'><label id='explicit' for='name'>Name</label><label id='wrapped'><span>Remember</span><input id='remember' type='checkbox'></label>",
        )
        .expect("document should parse");
        let id = |value: &str| document.find_element_by_id(value).expect("element exists");
        let form = id("profile");
        assert_eq!(
            document.form_controls(form).map(|controls| controls.len()),
            Some(6)
        );
        assert_eq!(document.form_owner(id("outside")), Some(form));
        assert_eq!(document.form_owner(id("name")), Some(form));
        assert_eq!(
            document.options(id("pick")).map(|options| options.len()),
            Some(2)
        );
        assert_eq!(document.label_control(id("explicit")), Some(id("name")));
        assert_eq!(document.label_control(id("wrapped")), Some(id("remember")));
        assert_eq!(
            document.radio_group(id("small")),
            Some(vec![id("small"), id("large")])
        );
    }

    #[test]
    fn inserts_moves_and_removes_nodes_with_stable_ids() {
        let mut document = Document::parse(
            "<main id='source'><p id='move'>move</p><p id='stay'>stay</p></main><main id='target'><p id='reference'>reference</p></main>",
        )
        .expect("document should parse");
        let id = |document: &Document, value: &str| {
            document.find_element_by_id(value).expect("element exists")
        };
        let (source, target) = (id(&document, "source"), id(&document, "target"));
        let (moved, stay, reference) = (
            id(&document, "move"),
            id(&document, "stay"),
            id(&document, "reference"),
        );
        assert_eq!(
            document.insert_before(target, moved, Some(reference)),
            Ok(vec![moved])
        );
        assert_eq!(document.children(source), &[stay]);
        assert_eq!(document.children(target), &[moved, reference]);
        assert_eq!(document.parent(moved), Some(target));
        assert_eq!(
            document.insert_before(moved, target, None),
            Err(DomError::HierarchyRequest)
        );
        assert_eq!(
            document.insert_before(source, moved, Some(reference)),
            Err(DomError::NotFound)
        );
        assert_eq!(document.remove(moved), Some((target, 0)));
        assert!(!document.is_connected(moved));
        assert_eq!(document.text_content(moved), "move");
        assert!(document.find_element_by_id("move").is_none());
        assert_eq!(document.append_child(stay, moved), Ok(vec![moved]));
        assert!(document.is_connected(moved));
    }

    #[test]
    fn builds_detached_trees_and_inserts_fragments() {
        let mut document = Document::parse("<ul id='list'></ul>").expect("document should parse");
        let list = document
            .find_element_by_id("list")
            .expect("list should exist");
        let fragment = document.create_fragment();
        let first = document.create_element("li");
        let text = document.create_text("one");
        document.append_child(first, text).expect("text appends");
        let second = document.create_element("LI");
        document.append_child(fragment, first).expect("li appends");
        document.append_child(fragment, second).expect("li appends");
        assert_eq!(
            document.element(second).map(Element::tag_name),
            Some("LI".to_string())
        );
        assert_eq!(
            document.append_child(list, fragment),
            Ok(vec![first, second])
        );
        assert!(document.children(fragment).is_empty());
        assert_eq!(document.inner_html(list), "<li>one</li><li></li>");
        assert_eq!(document.tree_order(first, second), Ordering::Less);
        assert_eq!(document.tree_order(text, list), Ordering::Greater);
    }

    #[test]
    fn frees_orphaned_trees_and_rejects_their_stale_ids() {
        let mut document = Document::parse("<main id='root'><p id='gone'>gone</p></main>")
            .expect("document should parse");
        assert!(document.take_orphans().is_empty());
        let gone = document.find_element_by_id("gone").expect("paragraph");
        let text = document.children(gone)[0];
        document.remove(gone);
        assert_eq!(document.take_orphans(), vec![gone]);
        assert_eq!(document.free_tree(gone).len(), 2);
        assert!(!document.contains(gone));
        assert!(!document.contains(text));
        let reused = document.create_element("p");
        assert!(document.contains(reused));
        assert!(!document.contains(gone));
        let root = document.find_element_by_id("root").expect("main");
        assert!(document.free_tree(root).is_empty());
    }

    #[test]
    fn clones_splits_and_parses_fragments() {
        let mut document = Document::parse("<p id='text'>hello world</p>").expect("should parse");
        let paragraph = document.find_element_by_id("text").expect("paragraph");
        let copy = document.clone_node(paragraph, true).expect("clone");
        assert_eq!(document.outer_html(copy), "<p id=\"text\">hello world</p>");
        let shallow = document.clone_node(paragraph, false).expect("clone");
        assert!(document.children(shallow).is_empty());
        let text = document.children(paragraph)[0];
        let tail = document.split_text(text, 5).expect("split");
        assert_eq!(document.children(paragraph), &[text, tail]);
        assert_eq!(document.character_data(tail), Some(" world"));
        let fragment = document
            .parse_fragment("<b>bold</b> text")
            .expect("fragment parses");
        assert_eq!(document.children(fragment).len(), 2);
        // an end tag with nothing open to close is dropped
        let stray = document.parse_fragment("<b></i>").expect("fragment parses");
        assert_eq!(document.inner_html(stray), "<b></b>");
        assert_eq!(document.node_text_offset(tail), Some(5));
        assert_eq!(document.text_point(7), Some((tail, 2)));
    }
}
