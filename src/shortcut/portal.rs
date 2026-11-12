// shortcuts on a wayland desktop, where the compositor keeps the keyboard to itself: the desktop
// portal binds them by id, with the user's say, and reports when each fires

use super::{ALT, ALT_GR, CONTROL, Events, Key, SHIFT, SUPER, Shortcut, ShortcutState};
use crate::dbus::{Bus, Message, SIGNAL, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const SHORTCUTS: &str = "org.freedesktop.portal.GlobalShortcuts";

static TOKENS: AtomicU32 = AtomicU32::new(0);

// the xkbcommon names of the named keys' keysyms, as the shortcuts specification spells triggers
const KEYSYM_NAMES: &[(u32, &str)] = &[
    (0x60, "grave"),
    (0x5c, "backslash"),
    (0x5b, "bracketleft"),
    (0x5d, "bracketright"),
    (0x2c, "comma"),
    (0x3d, "equal"),
    (0x2d, "minus"),
    (0x2e, "period"),
    (0x27, "apostrophe"),
    (0x3b, "semicolon"),
    (0x2f, "slash"),
    (0x20, "space"),
    (0xff09, "Tab"),
    (0xffe5, "Caps_Lock"),
    (0xff7f, "Num_Lock"),
    (0xff14, "Scroll_Lock"),
    (0xff08, "BackSpace"),
    (0xffff, "Delete"),
    (0xff63, "Insert"),
    (0xff0d, "Return"),
    (0xff52, "Up"),
    (0xff54, "Down"),
    (0xff51, "Left"),
    (0xff53, "Right"),
    (0xff50, "Home"),
    (0xff57, "End"),
    (0xff55, "Page_Up"),
    (0xff56, "Page_Down"),
    (0xff1b, "Escape"),
    (0xff13, "Pause"),
    (0xff61, "Print"),
    (0xff67, "Menu"),
    (0x1008_ff13, "XF86AudioRaiseVolume"),
    (0x1008_ff11, "XF86AudioLowerVolume"),
    (0x1008_ff12, "XF86AudioMute"),
    (0x1008_ff17, "XF86AudioNext"),
    (0x1008_ff16, "XF86AudioPrev"),
    (0x1008_ff15, "XF86AudioStop"),
    (0x1008_ff14, "XF86AudioPlay"),
    (0xffab, "KP_Add"),
    (0xffad, "KP_Subtract"),
    (0xffaa, "KP_Multiply"),
    (0xffaf, "KP_Divide"),
    (0xffae, "KP_Decimal"),
];

// the trigger the app would like, which the user may change; altgr has no name there
fn trigger(shortcut: Shortcut) -> Option<String> {
    if shortcut.modifiers & ALT_GR != 0 {
        return None;
    }
    let key = match shortcut.key {
        Key::Letter(letter) => (letter.to_ascii_lowercase() as char).to_string(),
        Key::Digit(digit) => digit.to_string(),
        Key::Function(number) => format!("F{number}"),
        Key::Numpad(digit) => format!("KP_{digit}"),
        Key::Named(named) => KEYSYM_NAMES
            .iter()
            .find(|(keysym, _)| *keysym == named.keysym)?
            .1
            .to_string(),
    };
    let mut trigger = String::new();
    for (bit, name) in [
        (CONTROL, "CTRL"),
        (ALT, "ALT"),
        (SHIFT, "SHIFT"),
        (SUPER, "LOGO"),
    ] {
        if shortcut.modifiers & bit != 0 {
            trigger.push_str(name);
            trigger.push('+');
        }
    }
    Some(trigger + &key)
}

fn token() -> String {
    format!(
        "ceri_{}_{}",
        std::process::id(),
        TOKENS.fetch_add(1, Ordering::Relaxed)
    )
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

// the session the shortcuts are bound in, which a change replaces, as a session binds only once
#[derive(Default)]
struct Bound {
    session: Option<String>,
    shortcuts: Vec<Shortcut>,
}

pub(super) struct Portal {
    bus: Arc<Bus>,
    responses: Mutex<Receiver<Message>>,
    bound: Arc<Mutex<Bound>>,
}

impl Portal {
    pub(super) fn start(events: Events, app: Option<&(String, String)>) -> Result<Self, String> {
        let address = crate::dbus::session_address()?;
        probe(&address)?;
        if let Some((identifier, name)) = app {
            ensure_entry(identifier, name);
        }
        Self::start_at(&address, events, app)
    }

    fn start_at(
        address: &str,
        events: Events,
        app: Option<&(String, String)>,
    ) -> Result<Self, String> {
        let (responses, received) = channel();
        let bound = Arc::new(Mutex::new(Bound::default()));
        let read = bound.clone();
        let bus = Bus::at(address, move |_, message| {
            if message.kind != SIGNAL {
                return;
            }
            let state = match message.member.as_deref() {
                Some("Activated") => ShortcutState::Pressed,
                Some("Deactivated") => ShortcutState::Released,
                Some("Response" | "NameOwnerChanged") => {
                    let _ = responses.send(message);
                    return;
                }
                _ => return,
            };
            let (Some(session), Some(id)) = (
                message.body.first().and_then(Value::as_str),
                message.body.get(1).and_then(Value::as_str),
            ) else {
                return;
            };
            let bound = read.lock().unwrap_or_else(PoisonError::into_inner);
            if bound.session.as_deref() != Some(session) {
                return;
            }
            if let Some(shortcut) = bound.shortcuts.iter().find(|held| held.to_string() == id) {
                let _ = events.send((*shortcut, state));
            }
        })?;
        let portal = Self {
            bus,
            responses: Mutex::new(received),
            bound,
        };
        let opened = portal.open(app);
        if opened.is_err() {
            portal.bus.close();
        }
        opened.map(|()| portal)
    }

    fn open(&self, app: Option<&(String, String)>) -> Result<(), String> {
        for rule in [
            "type='signal',interface='org.freedesktop.portal.Request',member='Response'"
                .to_string(),
            format!("type='signal',interface='{SHORTCUTS}'"),
            format!("type='signal',member='NameOwnerChanged',arg0='{PORTAL}'"),
        ] {
            self.bus
                .call(bus_call("AddMatch", vec![Value::Str(rule)]))?;
        }
        // an app outside a sandbox names itself before any other portal call, or newer portals
        // refuse it a session; older ones have no registry and go by the process instead
        if let Some((identifier, _)) = app
            && let Err(error) = self.bus.call(Message::call(
                PORTAL,
                PORTAL_PATH,
                "org.freedesktop.host.portal.Registry",
                "Register",
                vec![Value::str(identifier), Value::dict([])],
            ))
        {
            eprintln!("ceri: the desktop portal does not know this app: {error}");
        }
        Ok(())
    }

    pub(super) fn register(&self, shortcut: Shortcut) -> Result<(), String> {
        let mut wanted = self.held();
        wanted.push(shortcut);
        let bound = self.bind(&wanted)?;
        if bound.contains(&shortcut) {
            return Ok(());
        }
        // a refused bind leaves nothing bound, so the shortcuts held before come back
        let held = &wanted[..wanted.len() - 1];
        if held.iter().any(|held| !bound.contains(held)) {
            let _ = self.bind(held);
        }
        Err(format!("the desktop did not bind {shortcut}"))
    }

    pub(super) fn unregister(&self, shortcut: Shortcut) {
        let mut wanted = self.held();
        wanted.retain(|held| *held != shortcut);
        let _ = self.bind(&wanted);
    }

    fn held(&self) -> Vec<Shortcut> {
        self.bound
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .shortcuts
            .clone()
    }

    // the old session goes first, as the portal ties shortcut ids to the app across sessions
    fn bind(&self, wanted: &[Shortcut]) -> Result<Vec<Shortcut>, String> {
        let old = {
            let mut bound = self.bound.lock().unwrap_or_else(PoisonError::into_inner);
            bound.shortcuts.clear();
            bound.session.take()
        };
        if let Some(old) = old {
            let _ = self.bus.call(Message::call(
                PORTAL,
                &old,
                "org.freedesktop.portal.Session",
                "Close",
                Vec::new(),
            ));
        }
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        let created = self.request(
            "CreateSession",
            vec![Value::dict([
                ("handle_token", Value::str(token())),
                ("session_handle_token", Value::str(token())),
            ])],
        )?;
        let session = created
            .get("session_handle")
            .and_then(Value::as_str)
            .ok_or("the portal made no session")?
            .to_string();
        let shortcuts = wanted
            .iter()
            .map(|shortcut| {
                let mut options = vec![("description", Value::str(shortcut.to_string()))];
                if let Some(trigger) = trigger(*shortcut) {
                    options.push(("preferred_trigger", Value::str(trigger)));
                }
                Value::Struct(vec![Value::str(shortcut.to_string()), Value::dict(options)])
            })
            .collect();
        let answer = self.request(
            "BindShortcuts",
            vec![
                Value::Path(session.clone()),
                Value::Array("(sa{sv})".into(), shortcuts),
                Value::str(""),
                Value::dict([("handle_token", Value::str(token()))]),
            ],
        );
        let results = match answer {
            Ok(results) => results,
            Err(error) => {
                let _ = self.bus.call(Message::call(
                    PORTAL,
                    &session,
                    "org.freedesktop.portal.Session",
                    "Close",
                    Vec::new(),
                ));
                return Err(error);
            }
        };
        let ids: Vec<&str> = results
            .get("shortcuts")
            .map(Value::items)
            .unwrap_or_default()
            .iter()
            .filter_map(|entry| entry.items().first().and_then(Value::as_str))
            .collect();
        let held: Vec<Shortcut> = wanted
            .iter()
            .copied()
            .filter(|shortcut| ids.contains(&shortcut.to_string().as_str()))
            .collect();
        *self.bound.lock().unwrap_or_else(PoisonError::into_inner) = Bound {
            session: Some(session),
            shortcuts: held.clone(),
        };
        Ok(held)
    }

    // a portal request, answered by a Response on the request's own path once the user is done
    fn request(&self, member: &str, body: Vec<Value>) -> Result<Value, String> {
        let received = self
            .responses
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // what came while nothing was asked belongs to no request
        while received.try_recv().is_ok() {}
        let reply = self
            .bus
            .call(Message::call(PORTAL, PORTAL_PATH, SHORTCUTS, member, body))?;
        let request = reply
            .first()
            .and_then(Value::as_str)
            .ok_or("the portal answered without a request")?
            .to_string();
        loop {
            let signal = match received.recv_timeout(Duration::from_secs(30)) {
                Ok(signal) => signal,
                Err(RecvTimeoutError::Timeout) => {
                    self.bus.call(bus_call("GetId", Vec::new()))?;
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err("the session bus closed".to_string());
                }
            };
            match signal.member.as_deref() {
                Some("Response") if signal.path.as_deref() == Some(request.as_str()) => {
                    return match signal.body.first().and_then(Value::as_i64) {
                        Some(0) => Ok(signal
                            .body
                            .get(1)
                            .cloned()
                            .unwrap_or_else(|| Value::dict([]))),
                        Some(1) => Err("the user declined the shortcuts".to_string()),
                        _ => Err(format!("the desktop portal refused {member}")),
                    };
                }
                Some("NameOwnerChanged") => {
                    return Err("the desktop portal went away".to_string());
                }
                _ => {}
            }
        }
    }
}

// asked on a connection of its own, as one that has called the portal may no longer register
fn probe(address: &str) -> Result<(), String> {
    let bus = Bus::at(address, |_, _| {})?;
    let version = bus.call(Message::call(
        PORTAL,
        PORTAL_PATH,
        "org.freedesktop.DBus.Properties",
        "Get",
        vec![Value::str(SHORTCUTS), Value::str("version")],
    ));
    bus.close();
    match version {
        Ok(version) if version.first().and_then(Value::as_i64).unwrap_or_default() >= 1 => Ok(()),
        Ok(_) => Err("the desktop portal has no global shortcuts".to_string()),
        Err(error) => Err(format!(
            "the desktop portal has no global shortcuts: {error}"
        )),
    }
}

// the portal knows an app by the desktop entry named for it, so one is written where none is
fn ensure_entry(identifier: &str, name: &str) {
    let id = format!("{identifier}.desktop");
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|dirs| !dirs.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
    let Some(data) = crate::dirs::data() else {
        return;
    };
    let installed = std::iter::once(data.clone())
        .chain(data_dirs.split(':').map(PathBuf::from))
        .any(|dir| dir.join("applications").join(&id).is_file());
    if installed {
        return;
    }
    let Ok(program) = crate::autostart::program() else {
        return;
    };
    let applications = data.join("applications");
    let written = std::fs::create_dir_all(&applications)
        .and_then(|()| std::fs::write(applications.join(&id), entry(name, &program)));
    if let Err(error) = written {
        eprintln!("ceri: could not write the desktop entry {id}: {error}");
    }
}

fn entry(name: &str, program: &std::path::Path) -> String {
    let name: String = name
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let exec = crate::autostart::exec_argument(&program.to_string_lossy());
    format!(
        "[Desktop Entry]\nType=Application\nName={name}\nExec={exec}\nTerminal=false\nNoDisplay=true\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, Stdio};

    fn shortcut(accelerator: &str) -> Shortcut {
        Shortcut::parse(accelerator).expect("a shortcut")
    }

    #[test]
    fn triggers_follow_the_shortcuts_specification() {
        for (accelerator, expected) in [
            ("CmdOrCtrl+Shift+K", Some("CTRL+SHIFT+k")),
            ("Alt+F9", Some("ALT+F9")),
            ("Super+Space", Some("LOGO+space")),
            (
                "Control+Alt+Shift+Super+PageUp",
                Some("CTRL+ALT+SHIFT+LOGO+Page_Up"),
            ),
            ("Ctrl+num5", Some("CTRL+KP_5")),
            ("Ctrl+Digit0", Some("CTRL+0")),
            ("MediaPlayPause", Some("XF86AudioPlay")),
            ("Ctrl+~", Some("CTRL+SHIFT+grave")),
            ("AltGr+E", None),
        ] {
            assert_eq!(
                trigger(shortcut(accelerator)).as_deref(),
                expected,
                "{accelerator}"
            );
        }
        for named in super::super::NAMED {
            assert!(
                KEYSYM_NAMES
                    .iter()
                    .any(|(keysym, _)| *keysym == named.keysym),
                "{} has a trigger name",
                named.name
            );
        }
    }

    #[test]
    fn the_entry_runs_the_program_out_of_sight() {
        assert_eq!(
            entry("Clip\nApp", std::path::Path::new("/opt/clip app/clip")),
            "[Desktop Entry]\nType=Application\nName=Clip App\nExec=\"/opt/clip app/clip\"\nTerminal=false\nNoDisplay=true\n"
        );
    }

    struct Daemon(Child);

    impl Drop for Daemon {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    // a private bus with a stand-in portal that binds every shortcut but F12, the way a user
    // turning one down would leave it
    fn fake_portal() -> (Daemon, String, Arc<Bus>, Arc<Mutex<Vec<String>>>) {
        let mut child = Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("dbus-daemon should start");
        let mut address = String::new();
        BufReader::new(child.stdout.take().expect("the daemon's output"))
            .read_line(&mut address)
            .expect("the daemon prints its address");
        let daemon = Daemon(child);
        let address = address.trim().to_string();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let log = calls.clone();
        let sessions = AtomicU32::new(0);
        let portal = Bus::at(&address, move |bus, call| {
            if call.kind != crate::dbus::METHOD_CALL {
                return;
            }
            let member = call.member.clone().unwrap_or_default();
            let token = |body: &Value| {
                body.get("handle_token")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            let respond = |request: &str, results: Value| {
                let _ = bus.send(&call.reply(vec![Value::Path(request.to_string())]));
                let _ = bus.send(&Message::signal(
                    request,
                    "org.freedesktop.portal.Request",
                    "Response",
                    vec![Value::Uint32(0), results],
                ));
            };
            match member.as_str() {
                "Get" => {
                    let _ = bus.send(&call.reply(vec![Value::variant(Value::Uint32(2))]));
                }
                "Register" => {
                    log.lock().unwrap().push(format!(
                        "Register {}",
                        call.body[0].as_str().unwrap_or_default()
                    ));
                    let _ = bus.send(&call.reply(Vec::new()));
                }
                "Close" => {
                    log.lock()
                        .unwrap()
                        .push(format!("Close {}", call.path.clone().unwrap_or_default()));
                    let _ = bus.send(&call.reply(Vec::new()));
                }
                "CreateSession" => {
                    let session = format!(
                        "/org/freedesktop/portal/desktop/session/{}",
                        sessions.fetch_add(1, Ordering::Relaxed)
                    );
                    log.lock().unwrap().push(format!("CreateSession {session}"));
                    let request = format!(
                        "/org/freedesktop/portal/desktop/request/{}",
                        token(&call.body[0])
                    );
                    respond(
                        &request,
                        Value::dict([("session_handle", Value::str(session))]),
                    );
                }
                "BindShortcuts" => {
                    let wanted: Vec<String> = call.body[1]
                        .items()
                        .iter()
                        .map(|entry| {
                            let id = entry.items()[0].as_str().unwrap_or_default();
                            match entry.items()[1].get("preferred_trigger") {
                                Some(trigger) => {
                                    format!("{id}={}", trigger.as_str().unwrap_or_default())
                                }
                                None => id.to_string(),
                            }
                        })
                        .collect();
                    log.lock().unwrap().push(format!(
                        "BindShortcuts {} [{}]",
                        call.body[0].as_str().unwrap_or_default(),
                        wanted.join(" ")
                    ));
                    let bound = call.body[1]
                        .items()
                        .iter()
                        .filter(|entry| entry.items()[0].as_str() != Some("F12"))
                        .map(|entry| {
                            Value::Struct(vec![
                                entry.items()[0].clone(),
                                Value::dict([("trigger_description", Value::str("Press it"))]),
                            ])
                        })
                        .collect();
                    let request = format!(
                        "/org/freedesktop/portal/desktop/request/{}",
                        token(&call.body[3])
                    );
                    respond(
                        &request,
                        Value::dict([("shortcuts", Value::Array("(sa{sv})".into(), bound))]),
                    );
                }
                _ => {
                    let _ = bus.send(&call.error("org.freedesktop.DBus.Error.UnknownMethod", ""));
                }
            }
        })
        .expect("the stand-in portal connects");
        portal
            .call(bus_call(
                "RequestName",
                vec![Value::str(PORTAL), Value::Uint32(4)],
            ))
            .expect("the stand-in portal takes the portal's name");
        (daemon, address, portal, calls)
    }

    #[test]
    fn shortcuts_bind_through_the_portal_and_fire_from_its_signals() {
        let (_daemon, address, fake, calls) = fake_portal();
        let (events, fired) = channel();
        let app = ("dev.ceri.shortcuts".to_string(), "Shortcuts".to_string());
        probe(&address).expect("the stand-in portal has global shortcuts");
        let portal = Portal::start_at(&address, events, Some(&app)).expect("the portal opens");
        let (first, second) = (shortcut("CmdOrCtrl+Shift+K"), shortcut("Alt+F9"));
        portal.register(first).expect("the first binds");
        portal.register(second).expect("the second binds");
        assert_eq!(
            portal.register(shortcut("F12")),
            Err("the desktop did not bind F12".to_string())
        );
        assert_eq!(portal.held(), [first, second]);
        let signal = |session: &str, member: &str, id: &str| {
            fake.send(&Message::signal(
                PORTAL_PATH,
                SHORTCUTS,
                member,
                vec![
                    Value::Path(session.to_string()),
                    Value::str(id),
                    Value::Uint64(7),
                    Value::dict([]),
                ],
            ))
            .expect("the signal goes out");
        };
        let current = "/org/freedesktop/portal/desktop/session/2";
        signal(
            "/org/freedesktop/portal/desktop/session/0",
            "Activated",
            "Alt+F9",
        );
        signal(current, "Activated", "Alt+F9");
        signal(current, "Deactivated", "Alt+F9");
        signal(current, "Activated", "Control+Shift+KeyK");
        let wait = || {
            fired
                .recv_timeout(Duration::from_secs(5))
                .expect("an event")
        };
        assert_eq!(wait(), (second, ShortcutState::Pressed));
        assert_eq!(wait(), (second, ShortcutState::Released));
        assert_eq!(wait(), (first, ShortcutState::Pressed));
        portal.unregister(first);
        assert_eq!(portal.held(), [second]);
        portal.unregister(second);
        assert!(portal.held().is_empty());
        let session = |n: u32| format!("/org/freedesktop/portal/desktop/session/{n}");
        assert_eq!(
            *calls.lock().unwrap(),
            [
                "Register dev.ceri.shortcuts".to_string(),
                format!("CreateSession {}", session(0)),
                format!(
                    "BindShortcuts {} [Control+Shift+KeyK=CTRL+SHIFT+k]",
                    session(0)
                ),
                format!("Close {}", session(0)),
                format!("CreateSession {}", session(1)),
                format!(
                    "BindShortcuts {} [Control+Shift+KeyK=CTRL+SHIFT+k Alt+F9=ALT+F9]",
                    session(1)
                ),
                format!("Close {}", session(1)),
                format!("CreateSession {}", session(2)),
                format!(
                    "BindShortcuts {} [Control+Shift+KeyK=CTRL+SHIFT+k Alt+F9=ALT+F9 F12=F12]",
                    session(2)
                ),
                format!("Close {}", session(2)),
                format!("CreateSession {}", session(3)),
                format!("BindShortcuts {} [Alt+F9=ALT+F9]", session(3)),
                format!("Close {}", session(3)),
            ]
        );
        portal.bus.close();
        fake.close();
    }
}
