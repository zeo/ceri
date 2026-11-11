use super::{Change, MenuItem, Shared, TrayEvent, TrayHandler, lock, pixels};
use crate::dbus::{Bus, Message, NO_REPLY_EXPECTED, SIGNAL, Value};
use std::sync::{Arc, mpsc};

const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU_PATH: &str = "/MenuBar";
const ITEM: &str = "org.kde.StatusNotifierItem";
const MENU: &str = "com.canonical.dbusmenu";
const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const ICON_SIZES: [u32; 5] = [16, 22, 32, 48, 64];

type Reply = Result<Vec<Value>, (&'static str, String)>;

pub(crate) struct Backend {
    bus: Arc<Bus>,
    state: Shared,
}

struct Item {
    state: Shared,
    events: mpsc::Sender<TrayEvent>,
    id: String,
    name: String,
}

fn bus_call(member: &str, body: Vec<Value>) -> Message {
    Message::call(
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        member,
        body,
    )
}

// a watcher that starts or restarts later learns of the item through NameOwnerChanged
fn register(bus: &Bus, name: &str) {
    let mut message = Message::call(
        WATCHER,
        "/StatusNotifierWatcher",
        WATCHER,
        "RegisterStatusNotifierItem",
        vec![Value::str(name)],
    );
    message.flags = NO_REPLY_EXPECTED;
    let _ = bus.send(&message);
}

impl Backend {
    pub(crate) fn create(
        identifier: &str,
        state: Shared,
        handler: TrayHandler,
    ) -> Result<Self, String> {
        // handlers run apart from the bus reader, so one that calls back into the bus cannot stall it
        let (events, received) = mpsc::channel();
        std::thread::Builder::new()
            .name("ceri-tray".into())
            .spawn(move || received.iter().for_each(handler))
            .map_err(|error| format!("could not start the tray thread: {error}"))?;
        let name = format!("org.kde.StatusNotifierItem-{}-1", std::process::id());
        let item = Item {
            state: state.clone(),
            events,
            id: identifier.to_string(),
            name: name.clone(),
        };
        let bus = Bus::session(move |bus, message| item.receive(bus, message))?;
        // 4 asks not to queue behind another owner
        let owned = bus.call(bus_call(
            "RequestName",
            vec![Value::str(name.as_str()), Value::Uint32(4)],
        ))?;
        if owned.first() != Some(&Value::Uint32(1)) {
            return Err(format!("the bus name {name} is taken"));
        }
        bus.call(bus_call(
            "AddMatch",
            vec![Value::str(format!(
                "type='signal',sender='org.freedesktop.DBus',interface='org.freedesktop.DBus',\
                 member='NameOwnerChanged',arg0='{WATCHER}'"
            ))],
        ))?;
        register(&bus, &name);
        Ok(Self { bus, state })
    }

    pub(crate) fn refresh(&self, change: Change) {
        let signals: &[(&str, &str, &str)] = match change {
            Change::Icon => &[(ITEM_PATH, ITEM, "NewIcon")],
            Change::Tooltip => &[
                (ITEM_PATH, ITEM, "NewToolTip"),
                (ITEM_PATH, ITEM, "NewTitle"),
            ],
            Change::Menu => &[(MENU_PATH, MENU, "LayoutUpdated")],
        };
        for (path, interface, member) in signals {
            let body = match member {
                &"LayoutUpdated" => {
                    vec![Value::Uint32(lock(&self.state).revision), Value::Int32(0)]
                }
                _ => Vec::new(),
            };
            let _ = self
                .bus
                .send(&Message::signal(path, interface, member, body));
        }
    }

    // closing the connection drops the bus name, and the watcher takes the item away
    pub(crate) fn remove(&self) {
        self.bus.close();
    }
}

fn pixmaps(image: Option<&[u8]>) -> Value {
    let pixmaps = ICON_SIZES.iter().filter_map(|size| {
        let rgba = pixels(image, *size)?;
        // ARGB32 in network byte order
        let argb = rgba
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|[red, green, blue, alpha]| [*alpha, *red, *green, *blue])
            .collect();
        let side = Value::Int32(*size as i32);
        Some(Value::Struct(vec![side.clone(), side, Value::Bytes(argb)]))
    });
    Value::Array("(iiay)".into(), pixmaps.collect())
}

fn no_pixmaps() -> Value {
    Value::Array("(iiay)".into(), Vec::new())
}

fn item_properties(item: &MenuItem) -> Vec<(&'static str, Value)> {
    match item {
        MenuItem::Separator => vec![("type", Value::str("separator"))],
        MenuItem::Item {
            text,
            enabled,
            checked,
            ..
        } => {
            // an underscore marks the access key, so a literal one is doubled
            let mut properties = vec![
                ("label", Value::str(text.replace('_', "__"))),
                ("enabled", Value::Bool(*enabled)),
                ("visible", Value::Bool(true)),
            ];
            if let Some(checked) = checked {
                properties.push(("toggle-type", Value::str("checkmark")));
                properties.push(("toggle-state", Value::Int32(i32::from(*checked))));
            }
            properties
        }
    }
}

fn layout(id: i32, properties: Vec<(&'static str, Value)>, children: Vec<Value>) -> Value {
    Value::Struct(vec![
        Value::Int32(id),
        Value::dict(properties),
        Value::Array("v".into(), children),
    ])
}

fn unknown(what: String) -> (&'static str, String) {
    ("org.freedesktop.DBus.Error.InvalidArgs", what)
}

impl Item {
    fn receive(&self, bus: &Bus, message: Message) {
        if message.kind == SIGNAL {
            let restarted = message.member.as_deref() == Some("NameOwnerChanged")
                && message.body.first().and_then(Value::as_str) == Some(WATCHER)
                && message
                    .body
                    .get(2)
                    .and_then(Value::as_str)
                    .is_some_and(|owner| !owner.is_empty());
            if restarted {
                register(bus, &self.name);
            }
            return;
        }
        let (reply, flipped) = self.answer(&message);
        if message.wants_reply() {
            let _ = bus.send(&match reply {
                Ok(body) => message.reply(body),
                Err((name, text)) => message.error(name, &text),
            });
        }
        if flipped {
            let revision = lock(&self.state).revision;
            let body = vec![Value::Uint32(revision), Value::Int32(0)];
            let _ = bus.send(&Message::signal(MENU_PATH, MENU, "LayoutUpdated", body));
        }
    }

    // the reply to a call, and whether a chosen check item changed the menu
    fn answer(&self, message: &Message) -> (Reply, bool) {
        let path = message.path.as_deref().unwrap_or_default();
        let member = message.member.as_deref().unwrap_or_default();
        let interface = message.interface.as_deref();
        let argument = |index: usize| message.body.get(index);
        let text = |index: usize| argument(index).and_then(Value::as_str).unwrap_or_default();
        let number = |index: usize| argument(index).and_then(Value::as_i64).unwrap_or(-1);
        let on = |wanted: &str| interface.is_none_or(|interface| interface == wanted);
        let reply = match (path, member) {
            (_, "Introspect") if on("org.freedesktop.DBus.Introspectable") => {
                Ok(vec![Value::str(introspection(path))])
            }
            (_, "Ping") if on("org.freedesktop.DBus.Peer") => Ok(Vec::new()),
            (ITEM_PATH | MENU_PATH, "Get" | "GetAll" | "Set") if on(PROPERTIES) => {
                let (own, properties) = match path {
                    ITEM_PATH => (ITEM, self.properties()),
                    _ => (MENU, menu_properties()),
                };
                match (member, text(0) == own) {
                    ("GetAll", true) => Ok(vec![Value::dict(properties)]),
                    ("GetAll", false) => Ok(vec![Value::dict([])]),
                    ("Get", true) => properties
                        .into_iter()
                        .find(|(name, _)| *name == text(1))
                        .map(|(_, value)| vec![Value::variant(value)])
                        .ok_or_else(|| unknown(format!("no property {}", text(1)))),
                    ("Set", _) => Err((
                        "org.freedesktop.DBus.Error.PropertyReadOnly",
                        format!("{} is read-only", text(1)),
                    )),
                    _ => Err(unknown(format!("no interface {}", text(0)))),
                }
            }
            (ITEM_PATH, "Activate") if on(ITEM) => {
                let _ = self.events.send(TrayEvent::Click);
                Ok(Vec::new())
            }
            // the menu comes from dbusmenu, and the item has nothing else to do
            (
                ITEM_PATH,
                "ContextMenu" | "SecondaryActivate" | "Scroll" | "ProvideXdgActivationToken",
            ) if on(ITEM) => Ok(Vec::new()),
            (MENU_PATH, "GetLayout") if on(MENU) => self.layout(number(0), number(1)),
            (MENU_PATH, "GetGroupProperties") if on(MENU) => {
                let wanted: Vec<i64> = argument(0)
                    .map(|ids| ids.items().iter().filter_map(Value::as_i64).collect())
                    .unwrap_or_default();
                let menu = lock(&self.state).menu.clone();
                let groups = (0..=menu.len() as i64)
                    .filter(|id| wanted.is_empty() || wanted.contains(id))
                    .filter_map(|id| {
                        let properties = self.properties_of(&menu, id)?;
                        Some(Value::Struct(vec![
                            Value::Int32(id as i32),
                            Value::dict(properties),
                        ]))
                    })
                    .collect();
                Ok(vec![Value::Array("(ia{sv})".into(), groups)])
            }
            (MENU_PATH, "GetProperty") if on(MENU) => {
                let menu = lock(&self.state).menu.clone();
                self.properties_of(&menu, number(0))
                    .and_then(|properties| {
                        properties.into_iter().find(|(name, _)| *name == text(1))
                    })
                    .map(|(_, value)| vec![Value::variant(value)])
                    .ok_or_else(|| unknown(format!("no property {} on {}", text(1), number(0))))
            }
            (MENU_PATH, "Event") if on(MENU) => {
                let flipped = text(1) == "clicked" && self.choose(number(0));
                return (Ok(Vec::new()), flipped);
            }
            (MENU_PATH, "EventGroup") if on(MENU) => {
                let mut flipped = false;
                for event in argument(0).map(Value::items).unwrap_or_default() {
                    let [id, kind, ..] = event.items() else {
                        continue;
                    };
                    if kind.as_str() == Some("clicked") {
                        flipped |= self.choose(id.as_i64().unwrap_or(-1));
                    }
                }
                return (Ok(vec![Value::Array("i".into(), Vec::new())]), flipped);
            }
            (MENU_PATH, "AboutToShow") if on(MENU) => Ok(vec![Value::Bool(false)]),
            (MENU_PATH, "AboutToShowGroup") if on(MENU) => Ok(vec![
                Value::Array("i".into(), Vec::new()),
                Value::Array("i".into(), Vec::new()),
            ]),
            _ => Err((
                "org.freedesktop.DBus.Error.UnknownMethod",
                format!("{path} has no method {member}"),
            )),
        };
        (reply, false)
    }

    fn properties(&self) -> Vec<(&'static str, Value)> {
        let state = lock(&self.state);
        let icon = pixmaps(state.icon.as_deref());
        // the icon theme's generic program icon when there is no image of our own
        let icon_name = match icon.items().is_empty() {
            true => "application-x-executable",
            false => "",
        };
        let tooltip = Value::Struct(vec![
            Value::str(""),
            no_pixmaps(),
            Value::str(state.tooltip.as_str()),
            Value::str(""),
        ]);
        vec![
            ("Category", Value::str("ApplicationStatus")),
            ("Id", Value::str(self.id.as_str())),
            ("Title", Value::str(state.tooltip.as_str())),
            ("Status", Value::str("Active")),
            ("WindowId", Value::Int32(0)),
            ("IconName", Value::str(icon_name)),
            ("IconPixmap", icon),
            ("IconThemePath", Value::str("")),
            ("OverlayIconName", Value::str("")),
            ("OverlayIconPixmap", no_pixmaps()),
            ("AttentionIconName", Value::str("")),
            ("AttentionIconPixmap", no_pixmaps()),
            ("AttentionMovieName", Value::str("")),
            ("ToolTip", tooltip),
            ("ItemIsMenu", Value::Bool(false)),
            ("Menu", Value::Path(MENU_PATH.into())),
        ]
    }

    // item ids are menu positions from 1, and 0 is the root that holds them
    fn properties_of(&self, menu: &[MenuItem], id: i64) -> Option<Vec<(&'static str, Value)>> {
        match id {
            0 => Some(vec![("children-display", Value::str("submenu"))]),
            _ => menu.get(usize::try_from(id - 1).ok()?).map(item_properties),
        }
    }

    fn layout(&self, parent: i64, depth: i64) -> Reply {
        let state = lock(&self.state);
        let entry = |index: usize, item: &MenuItem| {
            layout(index as i32 + 1, item_properties(item), Vec::new())
        };
        let tree = match parent {
            0 => {
                let children = match depth {
                    0 => Vec::new(),
                    _ => state
                        .menu
                        .iter()
                        .enumerate()
                        .map(|(index, item)| Value::variant(entry(index, item)))
                        .collect(),
                };
                layout(
                    0,
                    vec![("children-display", Value::str("submenu"))],
                    children,
                )
            }
            _ => {
                let index = usize::try_from(parent - 1).ok();
                let item = index.and_then(|index| Some((index, state.menu.get(index)?)));
                let (index, item) =
                    item.ok_or_else(|| unknown(format!("no menu item {parent}")))?;
                entry(index, item)
            }
        };
        Ok(vec![Value::Uint32(state.revision), tree])
    }

    // true when a check item flipped, so hosts reread the menu
    fn choose(&self, id: i64) -> bool {
        let Some(index) = usize::try_from(id - 1).ok() else {
            return false;
        };
        let mut state = lock(&self.state);
        let revision = state.revision;
        let chosen = state.choose(index);
        let flipped = state.revision != revision;
        drop(state);
        if let Some(chosen) = chosen {
            let _ = self.events.send(TrayEvent::Menu(chosen));
        }
        flipped
    }
}

fn menu_properties() -> Vec<(&'static str, Value)> {
    vec![
        ("Version", Value::Uint32(3)),
        ("TextDirection", Value::str("ltr")),
        ("Status", Value::str("normal")),
        ("IconThemePath", Value::Array("s".into(), Vec::new())),
    ]
}

fn introspection(path: &str) -> String {
    let body = match path {
        ITEM_PATH => {
            r#"<interface name="org.kde.StatusNotifierItem">
<property name="Category" type="s" access="read"/><property name="Id" type="s" access="read"/>
<property name="Title" type="s" access="read"/><property name="Status" type="s" access="read"/>
<property name="WindowId" type="i" access="read"/><property name="IconName" type="s" access="read"/>
<property name="IconPixmap" type="a(iiay)" access="read"/><property name="IconThemePath" type="s" access="read"/>
<property name="OverlayIconName" type="s" access="read"/><property name="OverlayIconPixmap" type="a(iiay)" access="read"/>
<property name="AttentionIconName" type="s" access="read"/><property name="AttentionIconPixmap" type="a(iiay)" access="read"/>
<property name="AttentionMovieName" type="s" access="read"/><property name="ToolTip" type="(sa(iiay)ss)" access="read"/>
<property name="ItemIsMenu" type="b" access="read"/><property name="Menu" type="o" access="read"/>
<method name="ContextMenu"><arg name="x" type="i" direction="in"/><arg name="y" type="i" direction="in"/></method>
<method name="Activate"><arg name="x" type="i" direction="in"/><arg name="y" type="i" direction="in"/></method>
<method name="SecondaryActivate"><arg name="x" type="i" direction="in"/><arg name="y" type="i" direction="in"/></method>
<method name="Scroll"><arg name="delta" type="i" direction="in"/><arg name="orientation" type="s" direction="in"/></method>
<signal name="NewTitle"/><signal name="NewIcon"/><signal name="NewAttentionIcon"/><signal name="NewOverlayIcon"/>
<signal name="NewToolTip"/><signal name="NewStatus"><arg name="status" type="s"/></signal>
</interface>"#
        }
        MENU_PATH => {
            r#"<interface name="com.canonical.dbusmenu">
<property name="Version" type="u" access="read"/><property name="TextDirection" type="s" access="read"/>
<property name="Status" type="s" access="read"/><property name="IconThemePath" type="as" access="read"/>
<method name="GetLayout"><arg type="i" name="parentId" direction="in"/><arg type="i" name="recursionDepth" direction="in"/>
<arg type="as" name="propertyNames" direction="in"/><arg type="u" name="revision" direction="out"/>
<arg type="(ia{sv}av)" name="layout" direction="out"/></method>
<method name="GetGroupProperties"><arg type="ai" name="ids" direction="in"/><arg type="as" name="propertyNames" direction="in"/>
<arg type="a(ia{sv})" name="properties" direction="out"/></method>
<method name="GetProperty"><arg type="i" name="id" direction="in"/><arg type="s" name="name" direction="in"/>
<arg type="v" name="value" direction="out"/></method>
<method name="Event"><arg type="i" name="id" direction="in"/><arg type="s" name="eventId" direction="in"/>
<arg type="v" name="data" direction="in"/><arg type="u" name="timestamp" direction="in"/></method>
<method name="EventGroup"><arg type="a(isvu)" name="events" direction="in"/><arg type="ai" name="idErrors" direction="out"/></method>
<method name="AboutToShow"><arg type="i" name="id" direction="in"/><arg type="b" name="needUpdate" direction="out"/></method>
<method name="AboutToShowGroup"><arg type="ai" name="ids" direction="in"/><arg type="ai" name="updatesNeeded" direction="out"/>
<arg type="ai" name="idErrors" direction="out"/></method>
<signal name="ItemsPropertiesUpdated"><arg type="a(ia{sv})" name="updatedProps"/><arg type="a(ias)" name="removedProps"/></signal>
<signal name="LayoutUpdated"><arg type="u" name="revision"/><arg type="i" name="parent"/></signal>
<signal name="ItemActivationRequested"><arg type="i" name="id"/><arg type="u" name="timestamp"/></signal>
</interface>"#
        }
        _ => "",
    };
    let children = match path {
        "/" => r#"<node name="StatusNotifierItem"/><node name="MenuBar"/>"#,
        _ => "",
    };
    format!(
        "<!DOCTYPE node PUBLIC \"-//freedesktop//DTD D-BUS Object Introspection 1.0//EN\" \
         \"http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd\">\n<node>{body}{children}</node>"
    )
}

#[cfg(test)]
mod tests {
    use super::super::State;
    use super::*;
    use std::sync::Mutex;

    fn item() -> (Item, mpsc::Receiver<TrayEvent>) {
        let (events, received) = mpsc::channel();
        let state = Arc::new(Mutex::new(State {
            icon: None,
            tooltip: "clip: recording".into(),
            menu: vec![
                MenuItem::new("save", "save_clip"),
                MenuItem::Separator,
                MenuItem::check("mic", "mute microphone", false),
            ],
            revision: 1,
        }));
        let item = Item {
            state,
            events,
            id: "clip".into(),
            name: "org.kde.StatusNotifierItem-1-1".into(),
        };
        (item, received)
    }

    fn call(path: &str, interface: &str, member: &str, body: Vec<Value>) -> Message {
        Message::call(
            "org.kde.StatusNotifierItem-1-1",
            path,
            interface,
            member,
            body,
        )
    }

    #[test]
    fn serves_the_item_and_its_menu_over_dbusmenu() {
        let (item, received) = item();
        let (tooltip, _) = item.answer(&call(
            ITEM_PATH,
            PROPERTIES,
            "Get",
            vec![Value::str(ITEM), Value::str("ToolTip")],
        ));
        let tooltip = tooltip.expect("the tooltip should read");
        assert_eq!(tooltip[0].items()[2], Value::str("clip: recording"));
        let (all, _) = item.answer(&call(
            ITEM_PATH,
            PROPERTIES,
            "GetAll",
            vec![Value::str(ITEM)],
        ));
        let all = all.expect("the properties should read");
        assert_eq!(
            all[0].get("IconName"),
            Some(&Value::str("application-x-executable"))
        );
        assert_eq!(all[0].get("Menu"), Some(&Value::Path(MENU_PATH.into())));

        let (layout, _) = item.answer(&call(
            MENU_PATH,
            MENU,
            "GetLayout",
            vec![
                Value::Int32(0),
                Value::Int32(-1),
                Value::Array("s".into(), Vec::new()),
            ],
        ));
        let layout = layout.expect("the layout should read");
        assert_eq!(layout[0], Value::Uint32(1));
        assert_eq!(layout[1].signature(), "(ia{sv}av)");
        let children = layout[1].items()[2].items();
        assert_eq!(children.len(), 3);
        assert_eq!(
            children[0].items()[1].get("label"),
            Some(&Value::str("save__clip"))
        );
        assert_eq!(
            children[1].items()[1].get("type"),
            Some(&Value::str("separator"))
        );
        assert_eq!(
            children[2].items()[1].get("toggle-state"),
            Some(&Value::Int32(0))
        );

        let click = |id: i32| {
            call(
                MENU_PATH,
                MENU,
                "Event",
                vec![
                    Value::Int32(id),
                    Value::str("clicked"),
                    Value::variant(Value::Int32(0)),
                    Value::Uint32(0),
                ],
            )
        };
        assert!(item.answer(&click(3)).1);
        assert_eq!(received.try_recv(), Ok(TrayEvent::Menu("mic".into())));
        assert!(!item.answer(&click(1)).1);
        assert_eq!(received.try_recv(), Ok(TrayEvent::Menu("save".into())));
        assert!(!item.answer(&click(2)).1);
        assert!(received.try_recv().is_err());
        let (changed, _) = item.answer(&call(
            MENU_PATH,
            MENU,
            "GetProperty",
            vec![Value::Int32(3), Value::str("toggle-state")],
        ));
        assert_eq!(changed, Ok(vec![Value::variant(Value::Int32(1))]));
        let (activated, _) = item.answer(&call(
            ITEM_PATH,
            ITEM,
            "Activate",
            vec![Value::Int32(0), Value::Int32(0)],
        ));
        assert_eq!(activated, Ok(Vec::new()));
        assert_eq!(received.try_recv(), Ok(TrayEvent::Click));
        let (missing, _) = item.answer(&call(MENU_PATH, MENU, "Nope", Vec::new()));
        assert!(missing.is_err());
        let (xml, _) = item.answer(&call(
            "/",
            "org.freedesktop.DBus.Introspectable",
            "Introspect",
            Vec::new(),
        ));
        assert!(
            xml.expect("introspection should answer")[0]
                .as_str()
                .is_some_and(|xml| xml.contains("MenuBar"))
        );
    }
}
