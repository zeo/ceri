// shortcuts that reach the app while another app has the keyboard: RegisterHotKey on a thread
// of its own on Windows, passive key grabs on the X11 root window elsewhere

use serde::Serialize;
use std::fmt;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum ShortcutState {
    Pressed,
    Released,
}

type Listener = Box<dyn Fn(&str, ShortcutState) + Send + Sync>;

type Events = Sender<(Shortcut, ShortcutState)>;

// registers accelerators in electron's and tauri's grammar, such as CmdOrCtrl+Shift+KeyK or
// Alt+F9; each answers under its canonical name, the one register returns
#[derive(Clone, Default)]
pub struct GlobalShortcuts {
    shared: Arc<Shared>,
}

#[derive(Default)]
struct Shared {
    listener: OnceLock<Listener>,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    backend: Option<platform::Backend>,
    registered: Vec<Shortcut>,
}

impl GlobalShortcuts {
    pub(crate) fn listen(&self, listener: Listener) {
        let _ = self.shared.listener.set(listener);
    }

    // the canonical name of the shortcut, or why it could not be had
    pub fn register(&self, accelerator: &str) -> Result<String, String> {
        let shortcut = Shortcut::parse(accelerator)?;
        let mut state = self.state();
        if state.registered.contains(&shortcut) {
            return Err(format!("{shortcut} is already registered"));
        }
        if state.backend.is_none() {
            state.backend = Some(platform::Backend::start(self.events()?)?);
        }
        if let Some(backend) = &state.backend {
            backend.register(shortcut)?;
        }
        state.registered.push(shortcut);
        Ok(shortcut.to_string())
    }

    pub fn unregister(&self, accelerator: &str) -> Result<(), String> {
        let shortcut = Shortcut::parse(accelerator)?;
        let mut state = self.state();
        let Some(index) = state.registered.iter().position(|held| *held == shortcut) else {
            return Ok(());
        };
        state.registered.remove(index);
        if let Some(backend) = &state.backend {
            backend.unregister(shortcut);
        }
        Ok(())
    }

    pub fn unregister_all(&self) {
        let mut state = self.state();
        let registered = std::mem::take(&mut state.registered);
        if let Some(backend) = &state.backend {
            for shortcut in registered {
                backend.unregister(shortcut);
            }
        }
    }

    pub fn is_registered(&self, accelerator: &str) -> Result<bool, String> {
        let shortcut = Shortcut::parse(accelerator)?;
        Ok(self.state().registered.contains(&shortcut))
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // listeners run on a thread of their own, so one may register or unregister shortcuts
    fn events(&self) -> Result<Events, String> {
        let (events, received) = channel::<(Shortcut, ShortcutState)>();
        let shared = self.shared.clone();
        std::thread::Builder::new()
            .name("ceri shortcut events".to_string())
            .spawn(move || {
                for (shortcut, state) in received {
                    if let Some(listener) = shared.listener.get() {
                        listener(&shortcut.to_string(), state);
                    }
                }
            })
            .map_err(|error| format!("could not start the shortcut events: {error}"))?;
        Ok(events)
    }
}

const ALT: u8 = 1;
const CONTROL: u8 = 2;
const SHIFT: u8 = 4;
const SUPER: u8 = 8;
const ALT_GR: u8 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Shortcut {
    modifiers: u8,
    key: Key,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Key {
    Letter(u8),
    Digit(u8),
    Function(u8),
    Numpad(u8),
    Named(&'static Named),
}

#[derive(Debug, PartialEq, Eq)]
struct Named {
    name: &'static str,
    aliases: &'static [&'static str],
    // the windows virtual key and the x11 keysym
    virtual_key: u16,
    keysym: u32,
}

const fn named(
    name: &'static str,
    aliases: &'static [&'static str],
    virtual_key: u16,
    keysym: u32,
) -> Named {
    Named {
        name,
        aliases,
        virtual_key,
        keysym,
    }
}

static NAMED: &[Named] = &[
    named("Backquote", &["BACKQUOTE", "`"], 0xc0, 0x60),
    named("Backslash", &["BACKSLASH", "\\"], 0xdc, 0x5c),
    named("BracketLeft", &["BRACKETLEFT", "["], 0xdb, 0x5b),
    named("BracketRight", &["BRACKETRIGHT", "]"], 0xdd, 0x5d),
    named("Comma", &["COMMA", ","], 0xbc, 0x2c),
    named("Equal", &["EQUAL", "="], 0xbb, 0x3d),
    named("Minus", &["MINUS", "-"], 0xbd, 0x2d),
    named("Period", &["PERIOD", "."], 0xbe, 0x2e),
    named("Quote", &["QUOTE", "'"], 0xde, 0x27),
    named("Semicolon", &["SEMICOLON", ";"], 0xba, 0x3b),
    named("Slash", &["SLASH", "/"], 0xbf, 0x2f),
    named("Space", &["SPACE"], 0x20, 0x20),
    named("Tab", &["TAB"], 0x09, 0xff09),
    named("CapsLock", &["CAPSLOCK"], 0x14, 0xffe5),
    named("NumLock", &["NUMLOCK"], 0x90, 0xff7f),
    named("ScrollLock", &["SCROLLLOCK"], 0x91, 0xff14),
    named("Backspace", &["BACKSPACE"], 0x08, 0xff08),
    named("Delete", &["DELETE"], 0x2e, 0xffff),
    named("Insert", &["INSERT"], 0x2d, 0xff63),
    named("Enter", &["ENTER", "RETURN"], 0x0d, 0xff0d),
    named("ArrowUp", &["ARROWUP", "UP"], 0x26, 0xff52),
    named("ArrowDown", &["ARROWDOWN", "DOWN"], 0x28, 0xff54),
    named("ArrowLeft", &["ARROWLEFT", "LEFT"], 0x25, 0xff51),
    named("ArrowRight", &["ARROWRIGHT", "RIGHT"], 0x27, 0xff53),
    named("Home", &["HOME"], 0x24, 0xff50),
    named("End", &["END"], 0x23, 0xff57),
    named("PageUp", &["PAGEUP"], 0x21, 0xff55),
    named("PageDown", &["PAGEDOWN"], 0x22, 0xff56),
    named("Escape", &["ESCAPE", "ESC"], 0x1b, 0xff1b),
    named("Pause", &["PAUSE", "PAUSEBREAK"], 0x13, 0xff13),
    named("PrintScreen", &["PRINTSCREEN"], 0x2c, 0xff61),
    named("ContextMenu", &["CONTEXTMENU"], 0x5d, 0xff67),
    named(
        "AudioVolumeUp",
        &["AUDIOVOLUMEUP", "VOLUMEUP"],
        0xaf,
        0x1008_ff13,
    ),
    named(
        "AudioVolumeDown",
        &["AUDIOVOLUMEDOWN", "VOLUMEDOWN"],
        0xae,
        0x1008_ff11,
    ),
    named(
        "AudioVolumeMute",
        &["AUDIOVOLUMEMUTE", "VOLUMEMUTE"],
        0xad,
        0x1008_ff12,
    ),
    named(
        "MediaTrackNext",
        &["MEDIATRACKNEXT", "MEDIANEXTTRACK"],
        0xb0,
        0x1008_ff17,
    ),
    named(
        "MediaTrackPrevious",
        &["MEDIATRACKPREVIOUS", "MEDIAPREVIOUSTRACK"],
        0xb1,
        0x1008_ff16,
    ),
    named("MediaStop", &["MEDIASTOP"], 0xb2, 0x1008_ff15),
    named("MediaPlayPause", &["MEDIAPLAYPAUSE"], 0xb3, 0x1008_ff14),
    named(
        "NumpadAdd",
        &["NUMPADADD", "NUMADD", "NUMPADPLUS", "NUMPLUS"],
        0x6b,
        0xffab,
    ),
    named(
        "NumpadSubtract",
        &["NUMPADSUBTRACT", "NUMSUBTRACT", "NUMSUB"],
        0x6d,
        0xffad,
    ),
    named(
        "NumpadMultiply",
        &["NUMPADMULTIPLY", "NUMMULTIPLY", "NUMMULT"],
        0x6a,
        0xffaa,
    ),
    named(
        "NumpadDivide",
        &["NUMPADDIVIDE", "NUMDIVIDE", "NUMDIV"],
        0x6f,
        0xffaf,
    ),
    named(
        "NumpadDecimal",
        &["NUMPADDECIMAL", "NUMDECIMAL", "NUMDEC"],
        0x6e,
        0xffae,
    ),
];

// electron's punctuation that a us layout types with shift held, as the key under it
const SHIFTED: &[(&str, &str)] = &[
    (")", "0"),
    ("!", "1"),
    ("@", "2"),
    ("#", "3"),
    ("$", "4"),
    ("%", "5"),
    ("^", "6"),
    ("&", "7"),
    ("*", "8"),
    ("(", "9"),
    (":", ";"),
    ("+", "="),
    ("PLUS", "="),
    ("<", ","),
    ("_", "-"),
    (">", "."),
    ("?", "/"),
    ("~", "`"),
    ("{", "["),
    ("}", "]"),
    ("|", "\\"),
    ("\"", "'"),
];

impl Shortcut {
    pub(crate) fn parse(accelerator: &str) -> Result<Self, String> {
        let invalid = || format!("{accelerator:?} is not a shortcut");
        let accelerator = accelerator.trim();
        // a trailing ++ is the plus key after the modifiers
        let (modifiers, key) = match accelerator.strip_suffix("++") {
            Some(modifiers) => (modifiers, "+"),
            None if accelerator == "+" => ("", "+"),
            None => accelerator.rsplit_once('+').unwrap_or(("", accelerator)),
        };
        let mut shortcut = Self::key(&key.trim().to_ascii_uppercase()).ok_or_else(invalid)?;
        if !modifiers.is_empty() {
            for modifier in modifiers.split('+') {
                shortcut.modifiers |= match modifier.trim().to_ascii_uppercase().as_str() {
                    "ALT" | "OPTION" => ALT,
                    "ALTGR" => ALT_GR,
                    "CONTROL" | "CTRL" | "COMMANDORCONTROL" | "COMMANDORCTRL" | "CMDORCTRL"
                    | "CMDORCONTROL" => CONTROL,
                    "SHIFT" => SHIFT,
                    "SUPER" | "META" | "COMMAND" | "CMD" => SUPER,
                    _ => return Err(invalid()),
                };
            }
        }
        Ok(shortcut)
    }

    fn key(name: &str) -> Option<Self> {
        let plain = |key| Self { modifiers: 0, key };
        if let Some((_, under)) = SHIFTED.iter().find(|(typed, _)| *typed == name) {
            return Self::key(under).map(|under| Self {
                modifiers: SHIFT,
                ..under
            });
        }
        if let Some(named) = NAMED.iter().find(|named| named.aliases.contains(&name)) {
            return Some(plain(Key::Named(named)));
        }
        let single = |text: &str| match text.as_bytes() {
            [byte] => Some(*byte),
            _ => None,
        };
        let number = |text: &str, range: std::ops::RangeInclusive<u8>| {
            text.parse::<u8>()
                .ok()
                .filter(|number| range.contains(number) && !text.starts_with('0'))
        };
        let letter = single(name.strip_prefix("KEY").unwrap_or(name))
            .filter(u8::is_ascii_uppercase)
            .map(Key::Letter);
        let digit = single(name.strip_prefix("DIGIT").unwrap_or(name))
            .filter(u8::is_ascii_digit)
            .map(|digit| Key::Digit(digit - b'0'));
        let function = name
            .strip_prefix('F')
            .and_then(|number_text| number(number_text, 1..=24))
            .map(Key::Function);
        let numpad = name
            .strip_prefix("NUMPAD")
            .or_else(|| name.strip_prefix("NUM"))
            .and_then(single)
            .filter(u8::is_ascii_digit)
            .map(|digit| Key::Numpad(digit - b'0'));
        letter.or(digit).or(function).or(numpad).map(plain)
    }

    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    fn virtual_key(self) -> u16 {
        match self.key {
            Key::Letter(letter) => letter.into(),
            Key::Digit(digit) => u16::from(b'0' + digit),
            Key::Function(number) => 0x6f + u16::from(number),
            Key::Numpad(digit) => 0x60 + u16::from(digit),
            Key::Named(named) => named.virtual_key,
        }
    }

    #[cfg_attr(target_os = "windows", allow(dead_code))]
    fn keysym(self) -> u32 {
        match self.key {
            Key::Letter(letter) => letter.to_ascii_lowercase().into(),
            Key::Digit(digit) => u32::from(b'0' + digit),
            Key::Function(number) => 0xffbd + u32::from(number),
            Key::Numpad(digit) => 0xffb0 + u32::from(digit),
            Key::Named(named) => named.keysym,
        }
    }
}

impl fmt::Display for Shortcut {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (bit, name) in [
            (CONTROL, "Control"),
            (ALT, "Alt"),
            (ALT_GR, "AltGr"),
            (SHIFT, "Shift"),
            (SUPER, "Super"),
        ] {
            if self.modifiers & bit != 0 {
                write!(formatter, "{name}+")?;
            }
        }
        match self.key {
            Key::Letter(letter) => write!(formatter, "Key{}", letter as char),
            Key::Digit(digit) => write!(formatter, "Digit{digit}"),
            Key::Function(number) => write!(formatter, "F{number}"),
            Key::Numpad(digit) => write!(formatter, "Numpad{digit}"),
            Key::Named(named) => formatter.write_str(named.name),
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::{ALT, ALT_GR, CONTROL, Events, SHIFT, SUPER, Shortcut, ShortcutState};
    use std::sync::mpsc::{Receiver, Sender, channel};
    use windows::Win32::Foundation::{ERROR_HOTKEY_ALREADY_REGISTERED, LPARAM, WPARAM};
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT,
        MOD_WIN, RegisterHotKey, UnregisterHotKey,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetMessageW, KillTimer, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, SetTimer,
        WM_APP, WM_HOTKEY, WM_TIMER,
    };

    enum Request {
        Register(Shortcut, Sender<Result<(), String>>),
        Unregister(Shortcut, Sender<()>),
    }

    // a hotkey without a window belongs to the thread that registered it, so one thread does
    // all of them and answers requests posted to its queue
    pub(super) struct Backend {
        thread: u32,
        requests: Sender<Request>,
    }

    impl Backend {
        pub(super) fn start(events: Events) -> Result<Self, String> {
            let (requests, received) = channel();
            let (started, thread) = channel();
            std::thread::Builder::new()
                .name("ceri shortcuts".to_string())
                .spawn(move || {
                    let mut message = MSG::default();
                    // SAFETY: peeking makes the thread's queue, so posts from now on land
                    let _ =
                        unsafe { PeekMessageW(&mut message, None, WM_APP, WM_APP, PM_NOREMOVE) };
                    let _ = started.send(unsafe { GetCurrentThreadId() });
                    serve(&received, &events);
                })
                .map_err(|error| format!("could not start the shortcut thread: {error}"))?;
            let thread = thread
                .recv()
                .map_err(|_| "the shortcut thread stopped".to_string())?;
            Ok(Self { thread, requests })
        }

        pub(super) fn register(&self, shortcut: Shortcut) -> Result<(), String> {
            let (answer, answered) = channel();
            self.post(Request::Register(shortcut, answer))?;
            answered
                .recv()
                .map_err(|_| "the shortcut thread stopped".to_string())?
        }

        pub(super) fn unregister(&self, shortcut: Shortcut) {
            let (answer, answered) = channel();
            if self.post(Request::Unregister(shortcut, answer)).is_ok() {
                let _ = answered.recv();
            }
        }

        fn post(&self, request: Request) -> Result<(), String> {
            self.requests
                .send(request)
                .map_err(|_| "the shortcut thread stopped".to_string())?;
            // SAFETY: the thread made its queue before it gave out its id
            unsafe { PostThreadMessageW(self.thread, WM_APP, WPARAM(0), LPARAM(0)) }
                .map_err(|error| error.message())
        }
    }

    fn modifiers(shortcut: Shortcut) -> HOT_KEY_MODIFIERS {
        let mut modifiers = MOD_NOREPEAT;
        for (bit, flag) in [
            (ALT, MOD_ALT),
            (CONTROL, MOD_CONTROL),
            (SHIFT, MOD_SHIFT),
            (SUPER, MOD_WIN),
            (ALT_GR, MOD_CONTROL | MOD_ALT),
        ] {
            if shortcut.modifiers & bit != 0 {
                modifiers |= flag;
            }
        }
        modifiers
    }

    fn serve(requests: &Receiver<Request>, events: &Events) {
        let mut registered: Vec<(i32, Shortcut)> = Vec::new();
        let mut next = 1;
        // pressed shortcuts, watched for their key going up; windows sends no release
        let mut held: Vec<Shortcut> = Vec::new();
        let mut timer = 0;
        let mut message = MSG::default();
        // SAFETY: the message lives across each call
        while unsafe { GetMessageW(&mut message, None, 0, 0) }.0 > 0 {
            match message.message {
                WM_APP => {
                    for request in requests.try_iter() {
                        match request {
                            Request::Register(shortcut, answer) => {
                                let id = next;
                                next += 1;
                                // SAFETY: registers for this thread, which has no window
                                let result = unsafe {
                                    RegisterHotKey(
                                        None,
                                        id,
                                        modifiers(shortcut),
                                        shortcut.virtual_key().into(),
                                    )
                                }
                                .map(|()| registered.push((id, shortcut)))
                                .map_err(|error| {
                                    match error.code()
                                        == ERROR_HOTKEY_ALREADY_REGISTERED.to_hresult()
                                    {
                                        true => format!("{shortcut} is taken by another app"),
                                        false => format!(
                                            "could not register {shortcut}: {}",
                                            error.message()
                                        ),
                                    }
                                });
                                let _ = answer.send(result);
                            }
                            Request::Unregister(shortcut, answer) => {
                                registered.retain(|&(id, held)| {
                                    let gone = held == shortcut;
                                    if gone {
                                        // SAFETY: the id was registered on this thread
                                        let _ = unsafe { UnregisterHotKey(None, id) };
                                    }
                                    !gone
                                });
                                held.retain(|&down| down != shortcut);
                                let _ = answer.send(());
                            }
                        }
                    }
                }
                WM_HOTKEY => {
                    let pressed = registered
                        .iter()
                        .find(|(id, _)| *id as usize == message.wParam.0)
                        .map(|&(_, shortcut)| shortcut);
                    if let Some(shortcut) = pressed
                        && !held.contains(&shortcut)
                    {
                        held.push(shortcut);
                        let _ = events.send((shortcut, ShortcutState::Pressed));
                        if timer == 0 {
                            // SAFETY: a thread timer, posted to this queue
                            timer = unsafe { SetTimer(None, 0, 15, None) };
                        }
                    }
                }
                WM_TIMER if message.wParam.0 == timer => {
                    held.retain(|&shortcut| {
                        // SAFETY: reads the key's state, any thread may
                        let down = unsafe { GetAsyncKeyState(shortcut.virtual_key().into()) }
                            as u16
                            & 0x8000
                            != 0;
                        if !down {
                            let _ = events.send((shortcut, ShortcutState::Released));
                        }
                        down
                    });
                    if held.is_empty() {
                        // SAFETY: the timer was set on this thread
                        let _ = unsafe { KillTimer(None, timer) };
                        timer = 0;
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod platform {
    use super::{ALT, ALT_GR, CONTROL, Events, SHIFT, SUPER, Shortcut, ShortcutState};
    use std::sync::{Arc, Mutex, PoisonError};
    use x11rb::connection::Connection;
    use x11rb::protocol::Event;
    use x11rb::protocol::xproto::{ConnectionExt, GrabMode, ModMask};
    use x11rb::rust_connection::RustConnection;

    const MODIFIER_MASK: u16 = 0x1 | 0x4 | 0x8 | 0x40 | 0x80;
    // caps lock and num lock, which a grab would otherwise have to match too
    const LOCKS: [u16; 4] = [0, 0x2, 0x10, 0x2 | 0x10];

    type Grabs = Arc<Mutex<Vec<(u8, u16, Shortcut)>>>;

    // a connection of its own, its events read on one thread while grabs come from others
    pub(super) struct Backend {
        connection: Arc<RustConnection>,
        root: u32,
        grabs: Grabs,
    }

    impl Backend {
        pub(super) fn start(events: Events) -> Result<Self, String> {
            if std::env::var_os("DISPLAY").is_none() {
                return Err(
                    "global shortcuts need an X11 display, and this session has none".to_string(),
                );
            }
            let (connection, screen) = x11rb::connect(None)
                .map_err(|error| format!("could not reach the X11 display: {error}"))?;
            let root = connection.setup().roots[screen].root;
            let connection = Arc::new(connection);
            let grabs = Grabs::default();
            let (reader, read) = (connection.clone(), grabs.clone());
            std::thread::Builder::new()
                .name("ceri shortcuts".to_string())
                .spawn(move || serve(&reader, &read, &events))
                .map_err(|error| format!("could not start the shortcut thread: {error}"))?;
            Ok(Self {
                connection,
                root,
                grabs,
            })
        }

        fn keycode(&self, keysym: u32) -> Result<u8, String> {
            let setup = self.connection.setup();
            let (first, last) = (setup.min_keycode, setup.max_keycode);
            let mapping = self
                .connection
                .get_keyboard_mapping(first, last - first + 1)
                .map_err(|error| error.to_string())?
                .reply()
                .map_err(|error| error.to_string())?;
            let width = usize::from(mapping.keysyms_per_keycode.max(1));
            let rows = || mapping.keysyms.chunks(width);
            rows()
                .position(|row| row.first() == Some(&keysym))
                .or_else(|| rows().position(|row| row.contains(&keysym)))
                .map(|row| first + row as u8)
                .ok_or_else(|| "the keyboard has no such key".to_string())
        }

        pub(super) fn register(&self, shortcut: Shortcut) -> Result<(), String> {
            let keycode = self
                .keycode(shortcut.keysym())
                .map_err(|error| format!("could not register {shortcut}: {error}"))?;
            let modifiers = mask(shortcut);
            for (count, lock) in LOCKS.iter().enumerate() {
                let grabbed = self
                    .connection
                    .grab_key(
                        false,
                        self.root,
                        ModMask::from(modifiers | lock),
                        keycode,
                        GrabMode::ASYNC,
                        GrabMode::ASYNC,
                    )
                    .map_err(|error| error.to_string())
                    .and_then(|cookie| cookie.check().map_err(|error| error.to_string()));
                if grabbed.is_err() {
                    self.ungrab(keycode, modifiers, &LOCKS[..count]);
                    return Err(format!("{shortcut} is taken by another app"));
                }
            }
            self.grabs
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((keycode, modifiers, shortcut));
            Ok(())
        }

        pub(super) fn unregister(&self, shortcut: Shortcut) {
            let mut grabs = self.grabs.lock().unwrap_or_else(PoisonError::into_inner);
            grabs.retain(|&(keycode, modifiers, held)| {
                let gone = held == shortcut;
                if gone {
                    self.ungrab(keycode, modifiers, &LOCKS);
                }
                !gone
            });
        }

        fn ungrab(&self, keycode: u8, modifiers: u16, locks: &[u16]) {
            for lock in locks {
                let _ =
                    self.connection
                        .ungrab_key(keycode, self.root, ModMask::from(modifiers | lock));
            }
            let _ = self.connection.flush();
        }
    }

    fn mask(shortcut: Shortcut) -> u16 {
        [
            (SHIFT, 0x1),
            (CONTROL, 0x4),
            (ALT, 0x8),
            (SUPER, 0x40),
            (ALT_GR, 0x80),
        ]
        .into_iter()
        .filter(|(bit, _)| shortcut.modifiers & bit != 0)
        .fold(0, |mask, (_, flag)| mask | flag)
    }

    fn serve(connection: &RustConnection, grabs: &Grabs, events: &Events) {
        let find = |keycode: u8, state: u16| {
            grabs
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .find(|&&(code, modifiers, _)| {
                    code == keycode && modifiers == state & MODIFIER_MASK
                })
                .map(|&(_, _, shortcut)| shortcut)
        };
        let mut pending = None;
        loop {
            let event = match pending.take() {
                Some(event) => event,
                None => match connection.wait_for_event() {
                    Ok(event) => event,
                    Err(_) => return,
                },
            };
            match event {
                Event::KeyPress(press) => {
                    if let Some(shortcut) = find(press.detail, press.state.into()) {
                        let _ = events.send((shortcut, ShortcutState::Pressed));
                    }
                }
                Event::KeyRelease(release) => {
                    // a held key repeats as a release and a press with the same time
                    let next = connection.poll_for_event().ok().flatten();
                    if let Some(Event::KeyPress(press)) = &next
                        && press.detail == release.detail
                        && press.time == release.time
                    {
                        continue;
                    }
                    pending = next;
                    if let Some(shortcut) = find(release.detail, release.state.into()) {
                        let _ = events.send((shortcut, ShortcutState::Released));
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical(accelerator: &str) -> String {
        Shortcut::parse(accelerator)
            .unwrap_or_else(|error| panic!("{error}"))
            .to_string()
    }

    #[test]
    fn electron_and_tauri_accelerators_read_the_same() {
        for (accelerator, expected) in [
            ("CommandOrControl+Shift+K", "Control+Shift+KeyK"),
            ("ctrl+shift+keyk", "Control+Shift+KeyK"),
            ("Alt+F9", "Alt+F9"),
            ("F24", "F24"),
            ("Super+Space", "Super+Space"),
            ("Shift+Alt+Ctrl+Digit1", "Control+Alt+Shift+Digit1"),
            ("CmdOrCtrl+!", "Control+Shift+Digit1"),
            ("Ctrl++", "Control+Shift+Equal"),
            ("Ctrl+Plus", "Control+Shift+Equal"),
            ("+", "Shift+Equal"),
            ("Control+-", "Control+Minus"),
            ("num5", "Numpad5"),
            ("Numpad5", "Numpad5"),
            ("numadd", "NumpadAdd"),
            ("MediaPlayPause", "MediaPlayPause"),
            ("VolumeUp", "AudioVolumeUp"),
            ("Alt + Up", "Alt+ArrowUp"),
            ("AltGr+E", "AltGr+KeyE"),
            ("0", "Digit0"),
        ] {
            assert_eq!(canonical(accelerator), expected, "{accelerator}");
        }
        for accelerator in [
            "",
            "Ctrl+",
            "Ctrl+Shift",
            "Hyper+A",
            "F0",
            "F25",
            "F01",
            "KeyAB",
            "Ctrl+A+B",
            "num10",
        ] {
            assert!(Shortcut::parse(accelerator).is_err(), "{accelerator:?}");
        }
    }

    #[test]
    fn every_canonical_name_reads_back_as_itself() {
        let mut names: Vec<String> = NAMED.iter().map(|named| named.name.to_string()).collect();
        names.extend(
            [
                "KeyQ",
                "Digit7",
                "F12",
                "Numpad0",
                "Control+Alt+AltGr+Shift+Super+KeyA",
            ]
            .map(String::from),
        );
        for name in names {
            assert_eq!(canonical(&name), name);
        }
    }

    #[test]
    fn a_held_shortcut_is_refused_to_a_second_registrar() {
        if cfg!(not(target_os = "windows")) && std::env::var_os("DISPLAY").is_none() {
            return;
        }
        let accelerator = "Control+Alt+Shift+Super+F12";
        let (first, second) = (GlobalShortcuts::default(), GlobalShortcuts::default());
        assert_eq!(first.register(accelerator).expect("registers"), accelerator);
        assert!(
            first
                .is_registered("ctrl+alt+shift+meta+f12")
                .expect("parses")
        );
        let twice = first.register(accelerator).expect_err("held already");
        let taken = second.register(accelerator).expect_err("held by the first");
        first.unregister(accelerator).expect("unregisters");
        let freed = second.register(accelerator);
        second.unregister_all();
        assert!(twice.contains("already registered"), "{twice}");
        assert!(taken.contains("taken by another app"), "{taken}");
        assert!(freed.is_ok(), "{freed:?}");
        assert!(!second.is_registered(accelerator).expect("parses"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn the_virtual_keys_are_the_platform_ones() {
        use windows::Win32::UI::Input::KeyboardAndMouse::*;
        for (accelerator, key) in [
            ("`", VK_OEM_3),
            ("\\", VK_OEM_5),
            ("[", VK_OEM_4),
            ("]", VK_OEM_6),
            (",", VK_OEM_COMMA),
            ("=", VK_OEM_PLUS),
            ("-", VK_OEM_MINUS),
            (".", VK_OEM_PERIOD),
            ("'", VK_OEM_7),
            (";", VK_OEM_1),
            ("/", VK_OEM_2),
            ("Space", VK_SPACE),
            ("Tab", VK_TAB),
            ("CapsLock", VK_CAPITAL),
            ("NumLock", VK_NUMLOCK),
            ("ScrollLock", VK_SCROLL),
            ("Backspace", VK_BACK),
            ("Delete", VK_DELETE),
            ("Insert", VK_INSERT),
            ("Enter", VK_RETURN),
            ("Up", VK_UP),
            ("Down", VK_DOWN),
            ("Left", VK_LEFT),
            ("Right", VK_RIGHT),
            ("Home", VK_HOME),
            ("End", VK_END),
            ("PageUp", VK_PRIOR),
            ("PageDown", VK_NEXT),
            ("Esc", VK_ESCAPE),
            ("Pause", VK_PAUSE),
            ("PrintScreen", VK_SNAPSHOT),
            ("ContextMenu", VK_APPS),
            ("VolumeUp", VK_VOLUME_UP),
            ("VolumeDown", VK_VOLUME_DOWN),
            ("VolumeMute", VK_VOLUME_MUTE),
            ("MediaNextTrack", VK_MEDIA_NEXT_TRACK),
            ("MediaPreviousTrack", VK_MEDIA_PREV_TRACK),
            ("MediaStop", VK_MEDIA_STOP),
            ("MediaPlayPause", VK_MEDIA_PLAY_PAUSE),
            ("numadd", VK_ADD),
            ("numsub", VK_SUBTRACT),
            ("nummult", VK_MULTIPLY),
            ("numdiv", VK_DIVIDE),
            ("numdec", VK_DECIMAL),
            ("num7", VK_NUMPAD7),
            ("F1", VK_F1),
            ("F24", VK_F24),
            ("Z", VK_Z),
            ("9", VK_9),
        ] {
            let shortcut = Shortcut::parse(accelerator).expect("parses");
            assert_eq!(shortcut.virtual_key(), key.0, "{accelerator}");
        }
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn the_keysyms_are_the_x11_ones() {
        for (accelerator, keysym) in [
            ("A", 0x61),
            ("F1", 0xffbe),
            ("F24", 0xffd5),
            ("num0", 0xffb0),
            ("num9", 0xffb9),
            ("Digit4", 0x34),
        ] {
            assert_eq!(
                Shortcut::parse(accelerator).expect("parses").keysym(),
                keysym
            );
        }
    }
}
