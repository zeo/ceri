// a cycle collector for the reference-counted script heap: scopes, objects and arrays register
// weakly when made, and a collection counts the strong references each one gets from the
// others; one held by more than those is reachable from outside the heap (the VM stack, the
// runtime, a promise, anything not traced here) and keeps everything it reaches alive, and the
// rest are cycles nothing can reach, cleared so their memory goes. a reference left untraced
// can only keep a node alive, never free one that is in use

use super::object::{JsObject, ObjectClass, Slot};
use super::vm::{FieldKind, Scope, ScopeRef};
use super::*;
use std::rc::Weak;

#[derive(Default)]
struct Registry {
    // each node with the generation of the script runtime that was newest when it was made
    scopes: Vec<(u32, Weak<RefCell<Scope>>)>,
    objects: Vec<(u32, Weak<RefCell<JsObject>>)>,
    arrays: Vec<(u32, Weak<RefCell<ArrayData>>)>,
    generation: u32,
    // registrations since the last collection, and how many trigger the next one
    since: usize,
    threshold: usize,
    // registrations since dead entries were last dropped
    since_prune: usize,
}

// how often entries whose node is gone are dropped between collections; a weak entry keeps
// its node's block reserved, so the allocator cannot reuse it until then
const PRUNE_EVERY: usize = 65_536;

impl Registry {
    // a prune walks every entry, so with a large heap it waits for a share of that many
    // registrations, which keeps its cost per registration flat
    fn prune_due(&self) -> bool {
        let entries = self.scopes.len() + self.objects.len() + self.arrays.len();
        self.since_prune >= PRUNE_EVERY.max(entries / 4)
    }

    fn prune(&mut self) {
        self.scopes.retain(|(_, weak)| weak.strong_count() > 0);
        self.objects.retain(|(_, weak)| weak.strong_count() > 0);
        self.arrays.retain(|(_, weak)| weak.strong_count() > 0);
        self.since_prune = 0;
    }
}

const MINIMUM_THRESHOLD: usize = 200_000;

thread_local! {
    static REGISTRY: RefCell<Registry> = RefCell::new(Registry {
        threshold: MINIMUM_THRESHOLD,
        ..Registry::default()
    });
}

pub(super) fn register_scope(handle: &ScopeRef) {
    let due = REGISTRY.with(|registry| {
        let mut registry = registry.borrow_mut();
        let generation = registry.generation;
        registry.scopes.push((generation, Rc::downgrade(handle)));
        registry.since += 1;
        registry.since_prune += 1;
        if registry.prune_due() {
            registry.prune();
        }
        registry.since >= registry.threshold
    });
    // a long script makes its garbage between idle ticks too, and the weak entries of what
    // already went keep their blocks reserved until a collection prunes them
    if due {
        collect_cycles();
    }
}

pub(super) fn register_object(handle: &ObjectHandle) {
    let due = REGISTRY.with(|registry| {
        let mut registry = registry.borrow_mut();
        let generation = registry.generation;
        registry.objects.push((generation, Rc::downgrade(handle)));
        registry.since += 1;
        registry.since_prune += 1;
        if registry.prune_due() {
            registry.prune();
        }
        registry.since >= registry.threshold
    });
    // a long script makes its garbage between idle ticks too, and the weak entries of what
    // already went keep their blocks reserved until a collection prunes them
    if due {
        collect_cycles();
    }
}

pub(super) fn register_array(handle: &ArrayHandle) {
    let due = REGISTRY.with(|registry| {
        let mut registry = registry.borrow_mut();
        let generation = registry.generation;
        registry.arrays.push((generation, Rc::downgrade(handle)));
        registry.since += 1;
        registry.since_prune += 1;
        if registry.prune_due() {
            registry.prune();
        }
        registry.since >= registry.threshold
    });
    // a long script makes its garbage between idle ticks too, and the weak entries of what
    // already went keep their blocks reserved until a collection prunes them
    if due {
        collect_cycles();
    }
}

// starts the generation a new script runtime's nodes are made in
pub(super) fn begin_generation() -> u32 {
    REGISTRY.with(|registry| {
        let mut registry = registry.borrow_mut();
        registry.generation = registry.generation.wrapping_add(1);
        registry.generation
    })
}

// empties every node a runtime made, once its page is gone: nothing can reach them then, but
// cycles through references the collector does not trace would hold them for good
pub(super) fn clear_generation(generation: u32) -> usize {
    let nodes: Vec<Node> = REGISTRY.with(|registry| {
        let mut registry = registry.borrow_mut();
        let mut nodes = Vec::new();
        registry.scopes.retain(|(made, weak)| {
            if *made != generation {
                return true;
            }
            nodes.extend(weak.upgrade().map(Node::Scope));
            false
        });
        registry.objects.retain(|(made, weak)| {
            if *made != generation {
                return true;
            }
            nodes.extend(weak.upgrade().map(Node::Object));
            false
        });
        registry.arrays.retain(|(made, weak)| {
            if *made != generation {
                return true;
            }
            nodes.extend(weak.upgrade().map(Node::Array));
            false
        });
        nodes
    });
    let (mut dropped, mut classes) = (Vec::new(), Vec::new());
    for node in &nodes {
        node.clear(&mut dropped, &mut classes);
    }
    let cleared = nodes.len();
    drop(dropped);
    drop(classes);
    drop(nodes);
    cleared
}

// collects once enough has been made since the last collection that it pays for itself
pub(super) fn collect_if_due() -> usize {
    let due = REGISTRY.with(|registry| {
        let registry = registry.borrow();
        registry.since >= registry.threshold
    });
    if due { collect_cycles() } else { 0 }
}

enum Node {
    Scope(ScopeRef),
    Object(ObjectHandle),
    Array(ArrayHandle),
}

impl Node {
    fn address(&self) -> usize {
        match self {
            Node::Scope(scope) => Rc::as_ptr(scope) as *const u8 as usize,
            Node::Object(object) => Rc::as_ptr(object) as *const u8 as usize,
            Node::Array(array) => Rc::as_ptr(array) as *const u8 as usize,
        }
    }

    fn strong(&self) -> usize {
        match self {
            Node::Scope(scope) => Rc::strong_count(scope),
            Node::Object(object) => Rc::strong_count(object),
            Node::Array(array) => Rc::strong_count(array),
        }
    }

    // every strong reference this node holds to a scope, object or array, by address; None
    // when the node is borrowed and cannot be read now
    fn edges(&self, out: &mut Vec<usize>) -> Option<()> {
        match self {
            Node::Scope(scope) => {
                let scope = scope.try_borrow().ok()?;
                for value in &scope.slots {
                    value_edge(value, out);
                }
                if let Some(parent) = &scope.parent {
                    out.push(Rc::as_ptr(parent) as *const u8 as usize);
                }
            }
            Node::Object(object) => {
                let object = object.try_borrow().ok()?;
                for property in object.properties() {
                    slot_edges(&property.slot, out);
                }
                if let Some(prototype) = object.own_prototype() {
                    out.push(Rc::as_ptr(prototype) as *const u8 as usize);
                }
                match &object.class {
                    // a closure only this function holds belongs to it, so its scope and home
                    // are this object's references; a shared one is left untraced
                    ObjectClass::Function(closure) if Rc::strong_count(closure) == 1 => {
                        if let Some(scope) = &closure.scope {
                            out.push(Rc::as_ptr(scope) as *const u8 as usize);
                        }
                        if let Ok(home) = closure.home.try_borrow()
                            && let Some(home) = home.as_ref()
                        {
                            out.push(Rc::as_ptr(home) as *const u8 as usize);
                        }
                        let class = closure.class.try_borrow().ok()?;
                        for fields in class
                            .iter()
                            .flat_map(|class| [&class.fields, &class.statics])
                        {
                            for field in fields.iter() {
                                match &field.kind {
                                    FieldKind::Field(Some(value))
                                    | FieldKind::Method(value)
                                    | FieldKind::Getter(value)
                                    | FieldKind::Setter(value)
                                    | FieldKind::StaticBlock(value) => value_edge(value, out),
                                    FieldKind::Field(None) => {}
                                }
                            }
                        }
                    }
                    ObjectClass::Bound(bound) => {
                        value_edge(&bound.target, out);
                        value_edge(&bound.this, out);
                        for value in &bound.arguments {
                            value_edge(value, out);
                        }
                    }
                    _ => {}
                }
            }
            Node::Array(array) => {
                let array = array.try_borrow().ok()?;
                for value in array.iter() {
                    value_edge(value, out);
                }
                for (_, property) in &array.properties {
                    slot_edges(&property.slot, out);
                }
                if let Some(Some(prototype)) = &array.prototype {
                    out.push(Rc::as_ptr(prototype) as *const u8 as usize);
                }
            }
        }
        Some(())
    }

    // empties a node no one can reach, handing back what it held so it drops once no node
    // is borrowed
    fn clear(&self, dropped: &mut Vec<Value>, classes: &mut Vec<ObjectClass>) {
        match self {
            Node::Scope(scope) => {
                if let Ok(mut scope) = scope.try_borrow_mut() {
                    dropped.append(&mut scope.slots);
                    scope.parent = None;
                }
            }
            Node::Object(object) => {
                if let Ok(mut object) = object.try_borrow_mut() {
                    let (values, class) = object.take_for_collection();
                    dropped.extend(values);
                    classes.push(class);
                }
            }
            Node::Array(array) => {
                if let Ok(mut array) = array.try_borrow_mut() {
                    dropped.append(&mut array.values);
                    for (_, property) in array.properties.drain(..) {
                        match property.slot {
                            Slot::Data(value) => dropped.push(value),
                            Slot::Accessor(accessors) => {
                                dropped.extend(accessors.get.into_iter().chain(accessors.set))
                            }
                        }
                    }
                    if let Some(Some(prototype)) = array.prototype.take() {
                        dropped.push(Value::Object(prototype));
                    }
                }
            }
        }
    }
}

fn value_edge(value: &Value, out: &mut Vec<usize>) {
    match value {
        Value::Object(object) => out.push(Rc::as_ptr(object) as *const u8 as usize),
        Value::Array(array) => out.push(Rc::as_ptr(array) as *const u8 as usize),
        _ => {}
    }
}

fn slot_edges(slot: &Slot, out: &mut Vec<usize>) {
    match slot {
        Slot::Data(value) => value_edge(value, out),
        Slot::Accessor(accessors) => {
            for value in accessors.get.iter().chain(accessors.set.iter()) {
                value_edge(value, out);
            }
        }
    }
}

// how many registered nodes are still allocated
#[cfg(test)]
fn live_nodes() -> usize {
    REGISTRY.with(|registry| {
        let registry = registry.borrow();
        registry
            .scopes
            .iter()
            .filter(|(_, weak)| weak.strong_count() > 0)
            .count()
            + registry
                .objects
                .iter()
                .filter(|(_, weak)| weak.strong_count() > 0)
                .count()
            + registry
                .arrays
                .iter()
                .filter(|(_, weak)| weak.strong_count() > 0)
                .count()
    })
}

// node addresses are already well spread, so a multiply is all the hashing they need
#[derive(Default)]
struct AddressHasher(u64);

impl std::hash::Hasher for AddressHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 = (self.0 << 8 | u64::from(byte)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        }
    }

    fn write_usize(&mut self, value: usize) {
        self.0 = (value as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}

type AddressMap =
    std::collections::HashMap<usize, usize, std::hash::BuildHasherDefault<AddressHasher>>;

// a full collection; the number of nodes cleared
pub(super) fn collect_cycles() -> usize {
    let nodes: Vec<Node> = REGISTRY.with(|registry| {
        let mut registry = registry.borrow_mut();
        let mut nodes = Vec::new();
        registry.scopes.retain(|(_, weak)| match weak.upgrade() {
            Some(scope) => {
                nodes.push(Node::Scope(scope));
                true
            }
            None => false,
        });
        registry.objects.retain(|(_, weak)| match weak.upgrade() {
            Some(object) => {
                nodes.push(Node::Object(object));
                true
            }
            None => false,
        });
        registry.arrays.retain(|(_, weak)| match weak.upgrade() {
            Some(array) => {
                nodes.push(Node::Array(array));
                true
            }
            None => false,
        });
        nodes
    });
    let mut index = AddressMap::with_capacity_and_hasher(nodes.len(), Default::default());
    for (position, node) in nodes.iter().enumerate() {
        index.insert(node.address(), position);
    }
    // every node's targets laid end to end, with where each node's run starts
    let mut targets: Vec<usize> = Vec::new();
    let mut starts: Vec<usize> = Vec::with_capacity(nodes.len() + 1);
    let mut internal = vec![0usize; nodes.len()];
    let mut out = Vec::new();
    for node in &nodes {
        out.clear();
        // a node in use cannot be read, so nothing is collected this time
        if node.edges(&mut out).is_none() {
            return finish(nodes, 0);
        }
        starts.push(targets.len());
        for address in &out {
            if let Some(&target) = index.get(address) {
                internal[target] += 1;
                targets.push(target);
            }
        }
    }
    starts.push(targets.len());
    // the clone in nodes is one strong reference of each
    let mut live = vec![false; nodes.len()];
    let mut pending: Vec<usize> = (0..nodes.len())
        .filter(|&position| nodes[position].strong() > internal[position] + 1)
        .collect();
    for &position in &pending {
        live[position] = true;
    }
    while let Some(position) = pending.pop() {
        for &target in &targets[starts[position]..starts[position + 1]] {
            if !live[target] {
                live[target] = true;
                pending.push(target);
            }
        }
    }
    let mut dropped = Vec::new();
    let mut classes = Vec::new();
    let mut cleared = 0;
    for (position, node) in nodes.iter().enumerate() {
        if !live[position] {
            node.clear(&mut dropped, &mut classes);
            cleared += 1;
        }
    }
    drop(dropped);
    drop(classes);
    finish(nodes, cleared)
}

fn finish(nodes: Vec<Node>, cleared: usize) -> usize {
    let survivors = nodes.len() - cleared;
    drop(nodes);
    REGISTRY.with(|registry| {
        let mut registry = registry.borrow_mut();
        registry.since = 0;
        registry.threshold = (survivors * 2).max(MINIMUM_THRESHOLD);
    });
    cleared
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_released_runtime_leaves_none_of_its_nodes_whatever_holds_them() {
        let document = |markup: &str| {
            Rc::new(RefCell::new(
                crate::dom::Document::parse(markup).expect("document should parse"),
            ))
        };
        let kept = ScriptRuntime::new(document("<p></p>"));
        let baseline = live_nodes();
        let mut page = ScriptRuntime::new(document("<p id='out'></p>"));
        page.execute(
            r#"
            const map = new Map();
            for (let i = 0; i < 500; i++) { const box = { i }; box.self = () => map; map.set(box, [box]); }
            document.getElementById("out").addEventListener("click", () => map);
            "#,
        )
        .expect("script should run");
        assert!(live_nodes() > baseline + 1000);
        page.release_heap();
        assert_eq!(live_nodes(), baseline, "a released runtime's nodes all go");
        drop(kept);
    }

    #[test]
    fn closures_that_hold_their_own_scope_are_collected() {
        let document =
            crate::dom::Document::parse(r#"<p id="out"></p>"#).expect("document should parse");
        let document = Rc::new(RefCell::new(document));
        let mut runtime = ScriptRuntime::new(document.clone());
        collect_cycles();
        let baseline = live_nodes();
        runtime
            .execute(
                r#"
                function make(n) { const data = [n]; const self = () => self && data; return data.length; }
                const kept = (() => { const box = { n: 1 }; const self = () => box; box.self = self; return self; })();
                let total = 0;
                for (let i = 0; i < 2000; i++) total += make(i);
                globalThis.check = () => kept().n + total;
                "#,
            )
            .expect("script should run");
        collect_cycles();
        // the 2000 calls each left a scope, an array and a closure in a cycle; what is left
        // beyond the runtime itself is the script's own top level
        let grown = live_nodes().saturating_sub(baseline);
        assert!(
            grown < 200,
            "{grown} nodes outlived the calls that made them"
        );
        runtime
            .execute(r#"document.getElementById("out").textContent = String(check());"#)
            .expect("what is still reachable keeps working");
        let out = document.borrow().find_element_by_id("out").unwrap();
        assert_eq!(
            document.borrow().element_text_content(out).as_deref(),
            Some("2001")
        );
    }
}
