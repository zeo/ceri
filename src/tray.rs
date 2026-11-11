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

    pub fn disabled(self) -> Self {
        match self {
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

impl State {
    // the id of the item at an index, flipping its check mark as a chosen check item does
    pub(crate) fn choose(&mut self, index: usize) -> Option<String> {
        match self.menu.get_mut(index)? {
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
            let item = state.menu.iter_mut().find_map(|item| match item {
                MenuItem::Item {
                    id,
                    text,
                    enabled,
                    checked,
                } if id == wanted => Some((text, enabled, checked)),
                _ => None,
            });
            if let Some((text, enabled, checked)) = item {
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
        assert_eq!(pixels(Some(b"not an image"), 16), None);
    }
}
