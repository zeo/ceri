// semantic accessibility snapshot and host action dispatch shared by platform bridges

use super::{EngineHost, EngineWorkerCommand, InputEvent, KeyModifiers, PointerButton};
use crate::AccessibilityNode;
use crate::dom::{Document, NodeId};
use crate::hashing::HashMap;
use crate::layout::{ControlPaint, LayoutBox, Rect, Size, visual_offset_for, visual_rect_for};
use crate::navigation::NavigationError;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Toggle {
    Off,
    On,
    Indeterminate,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct A11yNode {
    pub(crate) node: NodeId,
    pub(crate) parent: Option<usize>,
    pub(crate) first_child: Option<usize>,
    pub(crate) last_child: Option<usize>,
    pub(crate) prev_sibling: Option<usize>,
    pub(crate) next_sibling: Option<usize>,
    pub(crate) role: String,
    pub(crate) name: String,
    pub(crate) value: Option<String>,
    pub(crate) automation_id: Option<String>,
    pub(crate) disabled: bool,
    pub(crate) focusable: bool,
    pub(crate) focused: bool,
    pub(crate) read_only: bool,
    pub(crate) password: bool,
    pub(crate) toggle: Option<Toggle>,
    pub(crate) heading_level: Option<u8>,
    // css pixels relative to the viewport top-left corner: [x, y, width, height]
    pub(crate) bounds: Option<[f32; 4]>,
    pub(crate) visible: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AccessibilitySnapshot {
    pub(crate) title: String,
    pub(crate) viewport: Size,
    pub(crate) scale: f32,
    pub(crate) focused: Option<usize>,
    pub(crate) nodes: Vec<A11yNode>,
}

#[cfg_attr(
    not(all(target_os = "windows", feature = "windowed")),
    allow(dead_code)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AccessibilityAction {
    Focus(u64),
    Invoke(u64),
    Toggle(u64),
    SetValue(u64, String),
}

// shared between the engine worker thread and platform accessibility bridges; inactive by default
// so windows that never receive an assistive-technology query pay zero per-frame snapshot cost
pub(crate) struct SharedAccessibility {
    active: AtomicBool,
    snapshot: Mutex<Option<Arc<AccessibilitySnapshot>>>,
    commands: Mutex<SyncSender<EngineWorkerCommand>>,
}

impl SharedAccessibility {
    pub(crate) fn new(commands: SyncSender<EngineWorkerCommand>) -> Self {
        Self {
            active: AtomicBool::new(false),
            snapshot: Mutex::new(None),
            commands: Mutex::new(commands),
        }
    }

    pub(crate) fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    pub(crate) fn publish(&self, snapshot: Option<Arc<AccessibilitySnapshot>>) {
        *self.snapshot.lock().unwrap_or_else(PoisonError::into_inner) = snapshot;
    }

    pub(crate) fn current(&self) -> Option<Arc<AccessibilitySnapshot>> {
        self.snapshot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    #[cfg_attr(
        not(all(target_os = "windows", feature = "windowed")),
        allow(dead_code)
    )]
    pub(crate) fn activate_and_fetch(&self) -> Option<Arc<AccessibilitySnapshot>> {
        let was_active = self.active.swap(true, Ordering::AcqRel);
        if was_active && let Some(cached) = self.current() {
            return Some(cached);
        }
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        let sent = self
            .commands
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .try_send(EngineWorkerCommand::AccessibilitySnapshot { reply: reply_tx })
            .is_ok();
        if sent && let Ok(snapshot) = reply_rx.recv_timeout(Duration::from_millis(250)) {
            self.publish(snapshot.clone());
            return snapshot;
        }
        self.current()
    }

    #[cfg_attr(
        not(all(target_os = "windows", feature = "windowed")),
        allow(dead_code)
    )]
    pub(crate) fn send_action(&self, action: AccessibilityAction) -> bool {
        self.commands
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .try_send(EngineWorkerCommand::AccessibilityAction(action))
            .is_ok()
    }
}

struct LayoutMeta {
    rect: Rect,
    focused: bool,
    control_value: Option<String>,
    toggle: Option<Toggle>,
    range_fraction: Option<f32>,
}

fn union_rect(a: Rect, b: Rect) -> Rect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let right = a.right().max(b.right());
    let bottom = a.bottom().max(b.bottom());
    Rect {
        x,
        y,
        width: (right - x).max(0.0),
        height: (bottom - y).max(0.0),
    }
}

fn collect_layout(
    layout: &LayoutBox,
    ancestor_offset: (f32, f32),
    out: &mut HashMap<NodeId, LayoutMeta>,
) {
    let rect = visual_rect_for(layout, ancestor_offset);
    let offset = visual_offset_for(layout, ancestor_offset);
    let toggle = match layout.control {
        Some(ControlPaint::Checkbox {
            checked,
            indeterminate,
            ..
        }) => Some(if indeterminate {
            Toggle::Indeterminate
        } else if checked {
            Toggle::On
        } else {
            Toggle::Off
        }),
        Some(ControlPaint::Radio { checked, .. }) => {
            Some(if checked { Toggle::On } else { Toggle::Off })
        }
        _ => None,
    };
    let range_fraction = match layout.control {
        Some(
            ControlPaint::Range { fraction, .. }
            | ControlPaint::Meter { fraction, .. }
            | ControlPaint::Progress {
                fraction: Some(fraction),
                ..
            },
        ) => Some(fraction),
        _ => None,
    };
    out.entry(layout.node)
        .and_modify(|entry| {
            entry.rect = union_rect(entry.rect, rect);
            entry.focused |= layout.control_focused;
            if entry.control_value.is_none() {
                entry.control_value = layout.control_value.clone();
            }
            if entry.toggle.is_none() {
                entry.toggle = toggle;
            }
            if entry.range_fraction.is_none() {
                entry.range_fraction = range_fraction;
            }
        })
        .or_insert_with(|| LayoutMeta {
            rect,
            focused: layout.control_focused,
            control_value: layout.control_value.clone(),
            toggle,
            range_fraction,
        });
    for child in &layout.children {
        collect_layout(child, offset, out);
    }
}

fn parse_toggle_attr(value: &str) -> Option<Toggle> {
    if value.eq_ignore_ascii_case("true") {
        Some(Toggle::On)
    } else if value.eq_ignore_ascii_case("false") {
        Some(Toggle::Off)
    } else if value.eq_ignore_ascii_case("mixed") {
        Some(Toggle::Indeterminate)
    } else {
        None
    }
}

fn heading_level(document: &Document, node: NodeId, role: &str) -> Option<u8> {
    if role != "heading" {
        return None;
    }
    let element = document.element(node)?;
    if let Some(level) = element
        .attr("aria-level")
        .and_then(|value| value.trim().parse::<u8>().ok())
        .filter(|level| (1..=6).contains(level))
    {
        return Some(level);
    }
    match element.name.as_str() {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3),
        "h4" => Some(4),
        "h5" => Some(5),
        "h6" => Some(6),
        _ => Some(2),
    }
}

fn rect_visible_in_viewport(rect: [f32; 4], viewport: Size) -> bool {
    let [x, y, width, height] = rect;
    width > 0.0
        && height > 0.0
        && x + width > 0.0
        && y + height > 0.0
        && x < viewport.width
        && y < viewport.height
}

impl AccessibilitySnapshot {
    pub(crate) fn from_host(host: &EngineHost) -> Option<Self> {
        let tree = host.browser.accessibility_tree()?;
        let title = host.title().unwrap_or_default();
        let viewport = host.viewport;
        let scale = host.scale;
        let mut layout_map = HashMap::default();
        if let Ok(layout) = host.browser.layout(viewport) {
            collect_layout(&layout.root, (0.0, 0.0), &mut layout_map);
        }
        let document = host.browser.document().ok();
        let mut nodes = Vec::new();
        let mut focused = None;

        #[allow(clippy::too_many_arguments)]
        fn push_node(
            source: &AccessibilityNode,
            parent: Option<usize>,
            host: &EngineHost,
            document: Option<&Document>,
            layout_map: &HashMap<NodeId, LayoutMeta>,
            title: &str,
            viewport: Size,
            nodes: &mut Vec<A11yNode>,
            focused: &mut Option<usize>,
        ) -> usize {
            let index = nodes.len();
            let is_root = parent.is_none();
            let meta = layout_map.get(&source.node);
            let element = document.and_then(|doc| doc.element(source.node));
            let automation_id = element
                .and_then(|el| el.attr("id"))
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_string);
            let password = element.is_some_and(|el| {
                el.name == "input"
                    && el
                        .attr("type")
                        .is_some_and(|kind| kind.eq_ignore_ascii_case("password"))
            });
            let read_only = element.is_some_and(|el| {
                el.attr("readonly").is_some()
                    || el
                        .attr("aria-readonly")
                        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
            });
            let attr_toggle = element.and_then(|el| {
                el.attr("aria-checked")
                    .or_else(|| el.attr("aria-pressed"))
                    .and_then(parse_toggle_attr)
                    .or_else(|| {
                        matches!(source.role.as_str(), "checkbox" | "switch").then_some(
                            if el.attr("checked").is_some() {
                                Toggle::On
                            } else {
                                Toggle::Off
                            },
                        )
                    })
            });
            let toggle = meta.and_then(|m| m.toggle).or(attr_toggle);
            let live_value = host
                .browser
                .control_value(source.node)
                .or_else(|| meta.and_then(|m| m.control_value.clone()))
                .or_else(|| source.value.clone())
                .or_else(|| {
                    meta.and_then(|m| m.range_fraction)
                        .map(|fraction| format!("{:.0}%", (fraction * 100.0).clamp(0.0, 100.0)))
                });
            let bounds = if is_root {
                Some([0.0, 0.0, viewport.width, viewport.height])
            } else {
                meta.map(|m| [m.rect.x, m.rect.y, m.rect.width, m.rect.height])
            };
            let visible = if is_root {
                viewport.width > 0.0 && viewport.height > 0.0
            } else {
                bounds.is_some_and(|rect| rect_visible_in_viewport(rect, viewport))
            };
            let node_focused = meta.is_some_and(|m| m.focused);
            if node_focused {
                *focused = Some(index);
            }
            let name = if is_root && source.name.is_empty() {
                title.to_string()
            } else {
                source.name.clone()
            };
            let heading = document.and_then(|doc| heading_level(doc, source.node, &source.role));

            nodes.push(A11yNode {
                node: source.node,
                parent,
                first_child: None,
                last_child: None,
                prev_sibling: None,
                next_sibling: None,
                role: source.role.clone(),
                name,
                value: live_value,
                automation_id,
                disabled: source.disabled,
                focusable: source.focusable,
                focused: node_focused,
                read_only,
                password,
                toggle,
                heading_level: heading,
                bounds,
                visible,
            });

            let mut prev: Option<usize> = None;
            for child in &source.children {
                let child_index = push_node(
                    child,
                    Some(index),
                    host,
                    document,
                    layout_map,
                    title,
                    viewport,
                    nodes,
                    focused,
                );
                if nodes[index].first_child.is_none() {
                    nodes[index].first_child = Some(child_index);
                }
                if let Some(prev_index) = prev {
                    nodes[prev_index].next_sibling = Some(child_index);
                    nodes[child_index].prev_sibling = Some(prev_index);
                }
                prev = Some(child_index);
            }
            nodes[index].last_child = prev;
            index
        }

        let borrowed = document.as_ref().map(|doc| doc.borrow());
        push_node(
            &tree,
            None,
            host,
            borrowed.as_deref(),
            &layout_map,
            &title,
            viewport,
            &mut nodes,
            &mut focused,
        );

        Some(Self {
            title,
            viewport,
            scale,
            focused,
            nodes,
        })
    }

    pub(crate) fn index_of(&self, raw_id: u64) -> Option<usize> {
        self.nodes
            .iter()
            .position(|entry| entry.node.raw() == raw_id)
    }

    pub(crate) fn center_of(&self, index: usize) -> Option<(f32, f32)> {
        let [x, y, width, height] = self.nodes.get(index)?.bounds?;
        if width <= 0.0 || height <= 0.0 {
            return None;
        }
        Some((x + width * 0.5, y + height * 0.5))
    }

    // deepest visible descendant containing the viewport css point, later siblings on top
    #[cfg_attr(
        not(all(target_os = "windows", feature = "windowed")),
        allow(dead_code)
    )]
    pub(crate) fn hit_test(&self, css_x: f32, css_y: f32) -> usize {
        fn walk(snapshot: &AccessibilitySnapshot, index: usize, x: f32, y: f32) -> Option<usize> {
            let mut child = snapshot.nodes[index].last_child;
            while let Some(child_index) = child {
                if let Some(hit) = walk(snapshot, child_index, x, y) {
                    return Some(hit);
                }
                child = snapshot.nodes[child_index].prev_sibling;
            }
            let node = &snapshot.nodes[index];
            let [left, top, width, height] = node.bounds?;
            (width > 0.0
                && height > 0.0
                && x >= left
                && x < left + width
                && y >= top
                && y < top + height)
                .then_some(index)
        }

        if self.nodes.is_empty() {
            return 0;
        }
        walk(self, 0, css_x, css_y).unwrap_or(0)
    }

    pub(crate) fn supports_invoke(&self, index: usize) -> bool {
        let Some(node) = self.nodes.get(index) else {
            return false;
        };
        !node.disabled
            && node.toggle.is_none()
            && matches!(
                node.role.as_str(),
                "button" | "link" | "menuitem" | "tab" | "option"
            )
    }

    pub(crate) fn supports_toggle(&self, index: usize) -> bool {
        let Some(node) = self.nodes.get(index) else {
            return false;
        };
        !node.disabled
            && (node.toggle.is_some() || matches!(node.role.as_str(), "checkbox" | "switch"))
    }

    pub(crate) fn supports_value(&self, index: usize) -> bool {
        let Some(node) = self.nodes.get(index) else {
            return false;
        };
        matches!(
            node.role.as_str(),
            "textbox" | "searchbox" | "combobox" | "spinbutton" | "slider"
        )
    }
}

impl EngineHost {
    pub(crate) fn accessibility_snapshot(&self) -> Option<AccessibilitySnapshot> {
        AccessibilitySnapshot::from_host(self)
    }

    pub(crate) fn perform_accessibility_action(
        &mut self,
        action: AccessibilityAction,
    ) -> Result<bool, NavigationError> {
        let Some(snapshot) = self.accessibility_snapshot() else {
            return Ok(false);
        };
        match action {
            AccessibilityAction::Focus(raw_id) => {
                let Some(index) = snapshot.index_of(raw_id) else {
                    return Ok(false);
                };
                let node = &snapshot.nodes[index];
                if node.disabled || !node.focusable {
                    return Ok(false);
                }
                let Some((x, y)) = snapshot.center_of(index) else {
                    return Ok(false);
                };
                let down = self.input(InputEvent::PointerDown {
                    x,
                    y,
                    button: PointerButton::Primary,
                })?;
                let up = self.input(InputEvent::PointerUp {
                    x,
                    y,
                    button: PointerButton::Primary,
                })?;
                Ok(down.handled || up.handled)
            }
            AccessibilityAction::Invoke(raw_id) => {
                let Some(index) = snapshot.index_of(raw_id) else {
                    return Ok(false);
                };
                if !snapshot.supports_invoke(index) {
                    return Ok(false);
                }
                if let Some((x, y)) = snapshot.center_of(index) {
                    let down = self.input(InputEvent::PointerDown {
                        x,
                        y,
                        button: PointerButton::Primary,
                    })?;
                    let up = self.input(InputEvent::PointerUp {
                        x,
                        y,
                        button: PointerButton::Primary,
                    })?;
                    let click = self.input(InputEvent::Click { x, y })?;
                    return Ok(down.handled || up.handled || click.handled);
                }
                if let Some(id) = snapshot.nodes[index].automation_id.as_deref() {
                    return self.click_id(id);
                }
                Ok(false)
            }
            AccessibilityAction::Toggle(raw_id) => {
                let Some(index) = snapshot.index_of(raw_id) else {
                    return Ok(false);
                };
                if !snapshot.supports_toggle(index) {
                    return Ok(false);
                }
                if let Some((x, y)) = snapshot.center_of(index) {
                    let down = self.input(InputEvent::PointerDown {
                        x,
                        y,
                        button: PointerButton::Primary,
                    })?;
                    let up = self.input(InputEvent::PointerUp {
                        x,
                        y,
                        button: PointerButton::Primary,
                    })?;
                    let click = self.input(InputEvent::Click { x, y })?;
                    return Ok(down.handled || up.handled || click.handled);
                }
                if let Some(id) = snapshot.nodes[index].automation_id.as_deref() {
                    return self.click_id(id);
                }
                Ok(false)
            }
            AccessibilityAction::SetValue(raw_id, text) => {
                let Some(index) = snapshot.index_of(raw_id) else {
                    return Ok(false);
                };
                let node = &snapshot.nodes[index];
                if !snapshot.supports_value(index) || node.disabled || node.read_only {
                    return Ok(false);
                }
                let Some((x, y)) = snapshot.center_of(index) else {
                    return Ok(false);
                };
                let _ = self.input(InputEvent::PointerDown {
                    x,
                    y,
                    button: PointerButton::Primary,
                })?;
                let _ = self.input(InputEvent::PointerUp {
                    x,
                    y,
                    button: PointerButton::Primary,
                })?;
                let _ = self.input(InputEvent::Click { x, y })?;
                let _ = self.input(InputEvent::Key {
                    key: "a".to_string(),
                    code: "KeyA".to_string(),
                    modifiers: KeyModifiers {
                        control: true,
                        ..KeyModifiers::default()
                    },
                    repeat: false,
                    pressed: true,
                })?;
                let edited = if text.is_empty() {
                    self.input(InputEvent::KeyDown {
                        key: "Backspace".to_string(),
                    })?
                } else {
                    self.input(InputEvent::TextInput { text })?
                };
                Ok(edited.handled)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::navigation::{Browser, ResourceBundle};

    fn host_for(html: &str) -> EngineHost {
        let mut resources = ResourceBundle::default();
        resources
            .insert("/", "text/html; charset=utf-8", html.as_bytes().to_vec())
            .expect("html resource");
        let browser = Browser::new(resources);
        let mut host = EngineHost::new(
            browser,
            Size {
                width: 640.0,
                height: 480.0,
            },
        );
        host.navigate("https://app.ceri.invalid/")
            .expect("page should load");
        host
    }

    #[test]
    fn snapshot_builds_navigation_bounds_states_and_hit_testing() {
        let host = host_for(
            r#"<!doctype html><title>Settings</title><body style="margin:0">
                <main>
                    <h2 id="hdr" style="margin:0;height:30px">Audio</h2>
                    <button id="go" style="display:block;width:120px;height:32px">Apply</button>
                    <input id="chk" type="checkbox" checked style="display:block;width:20px;height:20px">
                    <input id="pw" type="password" value="secret" readonly style="display:block;width:160px;height:24px">
                    <button id="off" disabled style="display:block;width:80px;height:24px">Locked</button>
                </main>
            </body>"#,
        );
        let snapshot = host.accessibility_snapshot().expect("snapshot");
        assert_eq!(snapshot.title, "Settings");
        assert_eq!(snapshot.nodes[0].role, "document");
        assert_eq!(snapshot.nodes[0].name, "Settings");
        let main_idx = snapshot.nodes[0].first_child.expect("main child");
        assert_eq!(snapshot.nodes[main_idx].role, "main");
        assert_eq!(snapshot.nodes[main_idx].parent, Some(0));

        let hdr_idx = snapshot.nodes[main_idx].first_child.expect("heading");
        assert_eq!(snapshot.nodes[hdr_idx].role, "heading");
        assert_eq!(snapshot.nodes[hdr_idx].heading_level, Some(2));
        assert_eq!(
            snapshot.nodes[hdr_idx].automation_id.as_deref(),
            Some("hdr")
        );

        let btn_idx = snapshot.nodes[hdr_idx].next_sibling.expect("button");
        assert_eq!(snapshot.nodes[btn_idx].role, "button");
        assert_eq!(snapshot.nodes[btn_idx].name, "Apply");
        assert_eq!(snapshot.nodes[btn_idx].prev_sibling, Some(hdr_idx));
        assert!(snapshot.supports_invoke(btn_idx));
        assert!(!snapshot.supports_toggle(btn_idx));

        let chk_idx = snapshot.nodes[btn_idx].next_sibling.expect("checkbox");
        assert_eq!(snapshot.nodes[chk_idx].toggle, Some(Toggle::On));
        assert!(snapshot.supports_toggle(chk_idx));
        assert!(!snapshot.supports_invoke(chk_idx));

        let pw_idx = snapshot.nodes[chk_idx].next_sibling.expect("password");
        assert!(snapshot.nodes[pw_idx].password);
        assert!(snapshot.nodes[pw_idx].read_only);
        assert!(snapshot.supports_value(pw_idx));

        let off_idx = snapshot.nodes[pw_idx].next_sibling.expect("disabled");
        assert!(snapshot.nodes[off_idx].disabled);
        assert!(!snapshot.supports_invoke(off_idx));
        assert_eq!(snapshot.nodes[main_idx].last_child, Some(off_idx));

        let (bx, by) = snapshot.center_of(btn_idx).expect("button center");
        assert_eq!(snapshot.hit_test(bx, by), btn_idx);
        let (cx, cy) = snapshot.center_of(chk_idx).expect("checkbox center");
        assert_eq!(snapshot.hit_test(cx, cy), chk_idx);
    }

    #[test]
    fn accessibility_actions_invoke_toggle_and_set_live_values() {
        let mut host = host_for(
            r#"<!doctype html><body style="margin:0">
                <button id="btn" style="display:block;width:120px;height:32px">Go</button>
                <input id="chk" type="checkbox" style="display:block;width:24px;height:24px">
                <input id="field" type="text" value="old" style="display:block;width:160px;height:28px">
                <input id="ro" type="text" value="fixed" readonly style="display:block;width:160px;height:28px">
                <p id="out">none</p>
                <script>
                    document.getElementById('btn').onclick = () => {
                        document.getElementById('out').textContent = 'clicked';
                    };
                </script>
            </body>"#,
        );
        let snap = host.accessibility_snapshot().expect("snapshot");
        let find = |id: &str| {
            snap.nodes
                .iter()
                .find(|n| n.automation_id.as_deref() == Some(id))
                .expect("node by id")
                .node
                .raw()
        };
        let btn = find("btn");
        let chk = find("chk");
        let field = find("field");
        let ro = find("ro");

        assert!(
            host.perform_accessibility_action(AccessibilityAction::Invoke(btn))
                .expect("invoke")
        );
        let after_invoke = host.accessibility_snapshot().expect("after invoke");
        let out = after_invoke
            .nodes
            .iter()
            .find(|n| n.automation_id.as_deref() == Some("out"))
            .expect("out");
        assert_eq!(out.name, "clicked");

        assert!(
            host.perform_accessibility_action(AccessibilityAction::Toggle(chk))
                .expect("toggle")
        );
        let after_toggle = host.accessibility_snapshot().expect("after toggle");
        assert_eq!(
            after_toggle.nodes[after_toggle.index_of(chk).expect("chk")].toggle,
            Some(Toggle::On)
        );

        assert!(
            host.perform_accessibility_action(AccessibilityAction::Focus(field))
                .expect("focus")
        );
        let after_focus = host.accessibility_snapshot().expect("after focus");
        assert!(after_focus.nodes[after_focus.index_of(field).expect("field")].focused);

        assert!(
            host.perform_accessibility_action(AccessibilityAction::SetValue(
                field,
                "updated".to_string()
            ))
            .expect("set value")
        );
        let after_edit = host.accessibility_snapshot().expect("after edit");
        let field_node = &after_edit.nodes[after_edit.index_of(field).expect("field")];
        assert_eq!(field_node.value.as_deref(), Some("updated"));
        assert!(field_node.focused);
        assert_eq!(after_edit.focused, after_edit.index_of(field));

        // readonly text controls reject set value without mutating
        assert!(
            !host
                .perform_accessibility_action(AccessibilityAction::SetValue(ro, "nope".to_string()))
                .expect("readonly set value")
        );
        let after_ro = host.accessibility_snapshot().expect("after readonly");
        assert_eq!(
            after_ro.nodes[after_ro.index_of(ro).expect("ro")]
                .value
                .as_deref(),
            Some("fixed")
        );
    }
}
