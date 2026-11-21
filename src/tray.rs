// the notification area icon with its native menu: the shell's on Windows, a
// StatusNotifierItem with a dbusmenu elsewhere

use std::sync::{Arc, Mutex, MutexGuard};

#[cfg(target_os = "windows")]
#[path = "tray/windows.rs"]
mod platform;

#[cfg(not(target_os = "windows"))]
#[path = "tray/linux.rs"]
mod platform;

#[derive(Clone, Debug, PartialEq)]
pub enum MenuItem {
    Item {
        id: String,
        text: String,
        enabled: bool,
        // some for an item with a check mark, which flips when chosen
        checked: Option<bool>,
    },
    // an entry that opens a menu of its own
    Submenu {
        text: String,
        enabled: bool,
        items: Vec<MenuItem>,
    },
    Separator,
}

impl MenuItem {
    pub fn new(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self::Item {
            id: id.into(),
            text: text.into(),
            enabled: true,
            checked: None,
        }
    }

    pub fn check(id: impl Into<String>, text: impl Into<String>, checked: bool) -> Self {
        Self::Item {
            id: id.into(),
            text: text.into(),
            enabled: true,
            checked: Some(checked),
        }
    }

    pub fn submenu(text: impl Into<String>, items: Vec<MenuItem>) -> Self {
        Self::Submenu {
            text: text.into(),
            enabled: true,
            items,
        }
    }

    pub fn disabled(self) -> Self {
        match self {
            Self::Submenu { text, items, .. } => Self::Submenu {
                text,
                enabled: false,
                items,
            },
            Self::Item {
                id, text, checked, ..
            } => Self::Item {
                id,
                text,
                enabled: false,
                checked,
            },
            separator => separator,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct TrayConfig {
    // PNG, JPEG, GIF, ICO or SVG bytes; the app icon without one
    pub icon: Option<Vec<u8>>,
    pub tooltip: String,
    pub menu: Vec<MenuItem>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrayEvent {
    // the icon was clicked, or chosen from the keyboard
    Click,
    // a menu item was chosen, by its id
    Menu(String),
}

pub(crate) type TrayHandler = Box<dyn Fn(TrayEvent) + Send + Sync>;

pub(crate) struct State {
    pub(crate) icon: Option<Vec<u8>>,
    pub(crate) tooltip: String,
    pub(crate) menu: Vec<MenuItem>,
    pub(crate) revision: u32,
}

// every entry of a menu and its submenus in the order they read, each with the number of the
// entry that holds it: entries count from 1 and 0 is the menu itself
pub(crate) fn rows(menu: &[MenuItem]) -> Vec<(usize, &MenuItem)> {
    fn walk<'a>(menu: &'a [MenuItem], parent: usize, rows: &mut Vec<(usize, &'a MenuItem)>) {
        for item in menu {
            rows.push((parent, item));
            let own = rows.len();
            if let MenuItem::Submenu { items, .. } = item {
                walk(items, own, rows);
            }
        }
    }
    let mut found = Vec::new();
    walk(menu, 0, &mut found);
    found
}

// the entry at a place in that order, counted from 0
fn row_mut<'a>(menu: &'a mut [MenuItem], left: &mut usize) -> Option<&'a mut MenuItem> {
    for item in menu {
        if *left == 0 {
            return Some(item);
        }
        *left -= 1;
        if let MenuItem::Submenu { items, .. } = item
            && let Some(found) = row_mut(items, left)
        {
            return Some(found);
        }
    }
    None
}

type ItemParts<'a> = (&'a mut String, &'a mut bool, &'a mut Option<bool>);

fn item_mut<'a>(menu: &'a mut [MenuItem], wanted: &str) -> Option<ItemParts<'a>> {
    for item in menu {
        match item {
            MenuItem::Item {
                id,
                text,
                enabled,
                checked,
            } if id == wanted => return Some((text, enabled, checked)),
            MenuItem::Submenu { items, .. } => {
                if let Some(found) = item_mut(items, wanted) {
                    return Some(found);
                }
            }
            _ => {}
        }
    }
    None
}

impl State {
    // the id of the item at a place among the rows, flipping its check mark as a chosen check
    // item does
    pub(crate) fn choose(&mut self, index: usize) -> Option<String> {
        let mut left = index;
        match row_mut(&mut self.menu, &mut left)? {
            MenuItem::Item {
                id,
                enabled: true,
                checked,
                ..
            } => {
                if let Some(checked) = checked {
                    *checked = !*checked;
                    self.revision += 1;
                }
                Some(id.clone())
            }
            _ => None,
        }
    }
}

pub(crate) type Shared = Arc<Mutex<State>>;

pub(crate) fn lock(state: &Shared) -> MutexGuard<'_, State> {
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) enum Change {
    Icon,
    Tooltip,
    Menu,
}

// the app's icon in the notification area; updates from any thread show at once
pub struct Tray {
    state: Shared,
    backend: Option<platform::Backend>,
}

impl Tray {
    // a tray that cannot reach the shell still keeps its state, so a later shell shows it
    pub(crate) fn create(identifier: &str, config: TrayConfig, handler: TrayHandler) -> Self {
        let state = Arc::new(Mutex::new(State {
            icon: config.icon,
            tooltip: config.tooltip,
            menu: config.menu,
            revision: 1,
        }));
        let backend = platform::Backend::create(identifier, state.clone(), handler)
            .map_err(|error| eprintln!("ceri: the tray icon is unavailable: {error}"))
            .ok();
        Self { state, backend }
    }

    pub fn set_tooltip(&self, text: &str) {
        self.change(Change::Tooltip, |state| state.tooltip = text.to_string());
    }

    pub fn set_icon(&self, image: Vec<u8>) {
        self.change(Change::Icon, |state| state.icon = Some(image));
    }

    pub fn set_checked(&self, id: &str, value: bool) {
        self.change_item(id, |_, _, checked| {
            if checked.is_some() {
                *checked = Some(value);
            }
        });
    }

    pub fn set_text(&self, id: &str, value: &str) {
        self.change_item(id, |text, _, _| *text = value.to_string());
    }

    pub fn set_enabled(&self, id: &str, value: bool) {
        self.change_item(id, |_, enabled, _| *enabled = value);
    }

    pub fn set_menu(&self, menu: Vec<MenuItem>) {
        self.change(Change::Menu, |state| {
            state.menu = menu;
            state.revision += 1;
        });
    }

    fn change_item(
        &self,
        wanted: &str,
        update: impl FnOnce(&mut String, &mut bool, &mut Option<bool>),
    ) {
        self.change(Change::Menu, |state| {
            if let Some((text, enabled, checked)) = item_mut(&mut state.menu, wanted) {
                update(text, enabled, checked);
                state.revision += 1;
            }
        });
    }

    fn change(&self, change: Change, update: impl FnOnce(&mut State)) {
        update(&mut lock(&self.state));
        if let Some(backend) = &self.backend {
            backend.refresh(change);
        }
    }

    // takes the icon out of the notification area as the app exits
    pub(crate) fn remove(&self) {
        if let Some(backend) = &self.backend {
            backend.remove();
        }
    }
}

// square RGBA pixels of the icon at a size; none leaves the platform's generic app icon
pub(crate) fn pixels(image: Option<&[u8]>, size: u32) -> Option<Vec<u8>> {
    image
        .and_then(|bytes| ceri_engine::rasterize_icon(bytes, size))
        .filter(|(_, rasterized)| *rasterized == size)
        .map(|(rgba, _)| rgba)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choosing_a_check_item_flips_it_and_disabled_items_stay_quiet() {
        let mut state = State {
            icon: None,
            tooltip: String::new(),
            menu: vec![
                MenuItem::check("pause", "pause recording", false),
                MenuItem::Separator,
                MenuItem::new("save", "save clip").disabled(),
                MenuItem::new("quit", "quit"),
            ],
            revision: 1,
        };
        assert_eq!(state.choose(0).as_deref(), Some("pause"));
        assert_eq!(
            state.menu[0],
            MenuItem::check("pause", "pause recording", true)
        );
        assert_eq!(state.revision, 2);
        assert_eq!(state.choose(1), None);
        assert_eq!(state.choose(2), None);
        assert_eq!(state.choose(3).as_deref(), Some("quit"));
        assert_eq!(state.choose(9), None);
        assert_eq!(state.revision, 2);
        // rows count through a submenu before what follows it
        state.menu.insert(
            1,
            MenuItem::submenu(
                "quality",
                vec![
                    MenuItem::check("high", "high", true),
                    MenuItem::check("low", "low", false),
                ],
            ),
        );
        let parents: Vec<_> = rows(&state.menu)
            .iter()
            .map(|(parent, _)| *parent)
            .collect();
        assert_eq!(parents, [0, 0, 2, 2, 0, 0, 0]);
        assert_eq!(state.choose(1), None, "a submenu is not chosen itself");
        assert_eq!(state.choose(3).as_deref(), Some("low"));
        assert_eq!(state.choose(6).as_deref(), Some("quit"));
        assert!(
            item_mut(&mut state.menu, "low").is_some_and(|(_, _, checked)| *checked == Some(true))
        );
        assert_eq!(pixels(Some(b"not an image"), 16), None);
    }
}
