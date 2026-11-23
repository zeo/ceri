use crate::bridge::Waker;
use crate::css::ColorScheme;
use crate::host::{
    EngineWorker, EngineWorkerEvent, Frame, InputEvent, KeyModifiers, PointerButton, RegionPress,
};
use crate::layout::Size;
use crate::navigation::{APP_ORIGIN, Browser, UserPreferences};
use softbuffer::{Context, Surface};
use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex, PoisonError};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalPosition, LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy, OwnedDisplayHandle};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};
use winit::window::{
    CursorIcon, Fullscreen, ResizeDirection, Theme, UserAttentionType, Window, WindowId,
    WindowLevel,
};

type WindowSurface = Surface<OwnedDisplayHandle, Rc<Window>>;

// where a window's frames go: a composition swapchain, or a software surface where one cannot
// be made, with the canvas revision and surface size it shows
enum Output {
    #[cfg(target_os = "windows")]
    Composition(crate::presenter::Presenter),
    Software(WindowSurface, Option<(u64, (u32, u32))>),
}

impl Output {
    fn composed(&self) -> bool {
        match self {
            #[cfg(target_os = "windows")]
            Self::Composition(_) => true,
            Self::Software(..) => false,
        }
    }

    fn open(context: &Context<OwnedDisplayHandle>, window: &Rc<Window>) -> Result<Self, String> {
        #[cfg(target_os = "windows")]
        {
            use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
            let size = window.inner_size();
            let composed = window
                .window_handle()
                .map_err(|error| error.to_string())
                .and_then(|handle| match handle.as_raw() {
                    RawWindowHandle::Win32(handle) => {
                        crate::presenter::Presenter::new(handle.hwnd.get(), size.width, size.height)
                    }
                    _ => Err("the window has no Win32 handle".to_string()),
                });
            match composed {
                Ok(presenter) => return Ok(Self::Composition(presenter)),
                Err(error) => eprintln!("ceri: drawing in software, {error}"),
            }
        }
        Surface::new(context, window.clone())
            .map(|surface| Self::Software(surface, None))
            .map_err(|error| format!("window surface creation failed: {error}"))
    }

    fn present(
        &mut self,
        frame: &Frame,
        width: NonZeroU32,
        height: NonZeroU32,
    ) -> Result<(), String> {
        match self {
            #[cfg(target_os = "windows")]
            Self::Composition(presenter) => presenter.present(
                frame.canvas.rgba_pixels(),
                (frame.canvas.width(), frame.canvas.height()),
                (width.get(), height.get()),
                Some(frame.canvas.revision()),
                &frame.planes,
            ),
            Self::Software(surface, shown) => {
                let revision = frame.canvas.revision();
                let size = (width.get(), height.get());
                if *shown == Some((revision.id, size)) {
                    return Ok(());
                }
                surface
                    .resize(width, height)
                    .map_err(|error| format!("window surface resize failed: {error}"))?;
                let mut buffer = surface
                    .buffer_mut()
                    .map_err(|error| format!("window surface buffer failed: {error}"))?;
                let changed = match revision.changed {
                    // a buffer presented last holds the shown revision outside the box
                    Some((base, boxes)) if buffer.age() == 1 && *shown == Some((base, size)) => {
                        let [left, top, right, bottom] = boxes.bounds();
                        [left, top, right.min(size.0), bottom.min(size.1)]
                    }
                    _ => [0, 0, size.0, size.1],
                };
                *shown = None;
                present_frame(frame, size, changed, &mut buffer);
                let [left, top, right, bottom] = changed;
                if let (Some(columns), Some(rows)) = (
                    NonZeroU32::new(right.saturating_sub(left)),
                    NonZeroU32::new(bottom.saturating_sub(top)),
                ) {
                    buffer
                        .present_with_damage(&[softbuffer::Rect {
                            x: left,
                            y: top,
                            width: columns,
                            height: rows,
                        }])
                        .map_err(|error| format!("window surface present failed: {error}"))?;
                }
                *shown = Some((revision.id, size));
                Ok(())
            }
        }
    }

    // a frame laid out for an earlier size, shown while the new size's frame is made: its
    // pixels where they fall inside the window, and past its edges the colour of its bottom
    // right corner, which is the page's own background far more often than not
    fn present_stale(
        &mut self,
        frame: &Frame,
        width: NonZeroU32,
        height: NonZeroU32,
    ) -> Result<(), String> {
        let (columns, rows) = (width.get() as usize, height.get() as usize);
        let canvas = &frame.canvas;
        let pixels = canvas.rgba_pixels();
        let stride = canvas.bytes_per_row();
        let (canvas_columns, canvas_rows) = (canvas.width() as usize, canvas.height() as usize);
        let corner = match (canvas_columns, canvas_rows) {
            (0, _) | (_, 0) => [0, 0, 0, 255],
            _ => {
                let at = (canvas_rows - 1) * stride + (canvas_columns - 1) * 4;
                [pixels[at], pixels[at + 1], pixels[at + 2], 255]
            }
        };
        let mut padded = vec![0u8; columns * rows * 4];
        for (row, target) in padded.chunks_exact_mut(columns * 4).enumerate() {
            let shared = if row < canvas_rows {
                columns.min(canvas_columns)
            } else {
                0
            };
            if shared > 0 {
                let source = &pixels[row * stride..row * stride + shared * 4];
                target[..shared * 4].copy_from_slice(source);
            }
            for pixel in target[shared * 4..].as_chunks_mut::<4>().0 {
                *pixel = corner;
            }
        }
        match self {
            #[cfg(target_os = "windows")]
            Self::Composition(presenter) => presenter.present(
                &padded,
                (width.get(), height.get()),
                (width.get(), height.get()),
                None,
                &frame.planes,
            ),
            Self::Software(surface, shown) => {
                *shown = None;
                surface
                    .resize(width, height)
                    .map_err(|error| format!("window surface resize failed: {error}"))?;
                let mut buffer = surface
                    .buffer_mut()
                    .map_err(|error| format!("window surface buffer failed: {error}"))?;
                for (pixel, rgba) in buffer.iter_mut().zip(padded.as_chunks::<4>().0) {
                    *pixel =
                        u32::from(rgba[0]) << 16 | u32::from(rgba[1]) << 8 | u32::from(rgba[2]);
                }
                buffer
                    .present()
                    .map_err(|error| format!("window surface present failed: {error}"))
            }
        }
    }

    // covers a window grown past its last frame until a frame of the new size comes; a
    // software surface has nothing kept to extend
    #[cfg(target_os = "windows")]
    fn extend(&mut self, width: u32, height: u32) -> Result<(), String> {
        match self {
            Self::Composition(presenter) => presenter.extend((width, height)),
            Self::Software(..) => Ok(()),
        }
    }

    // one colour over the whole window until its page paints
    fn fill(&mut self, rgb: [u8; 3], width: NonZeroU32, height: NonZeroU32) -> Result<(), String> {
        match self {
            // the presenter repeats a short canvas's last pixel to the surface edge
            #[cfg(target_os = "windows")]
            Self::Composition(presenter) => presenter.present(
                &[rgb[0], rgb[1], rgb[2], 255],
                (1, 1),
                (width.get(), height.get()),
                None,
                &[],
            ),
            Self::Software(surface, shown) => {
                *shown = None;
                surface
                    .resize(width, height)
                    .map_err(|error| format!("window surface resize failed: {error}"))?;
                let mut buffer = surface
                    .buffer_mut()
                    .map_err(|error| format!("window surface buffer failed: {error}"))?;
                buffer.fill(u32::from(rgb[0]) << 16 | u32::from(rgb[1]) << 8 | u32::from(rgb[2]));
                buffer
                    .present()
                    .map_err(|error| format!("window surface present failed: {error}"))
            }
        }
    }
}

// sizes and positions are logical pixels unless a field says otherwise
#[derive(Clone, Debug)]
pub struct WindowConfig {
    // names the window to commands, capabilities and events; unique among open windows
    pub label: String,
    pub title: String,
    pub width: u32,
    pub height: u32,
    pub minimum_width: u32,
    pub minimum_height: u32,
    pub maximum_size: Option<(u32, u32)>,
    // from the desktop origin; the window is centred on the primary monitor without one
    pub position: Option<(i32, i32)>,
    pub start_url: String,
    // square RGBA pixels for the window and taskbar icon
    pub icon: Option<(Vec<u8>, u32)>,
    // where the window position, size and maximized state are remembered between launches
    pub geometry_path: Option<PathBuf>,
    // without decorations the page draws its own title bar, with app-region areas to drag it
    pub decorations: bool,
    pub resizable: bool,
    pub visible: bool,
    pub maximized: bool,
    pub fullscreen: bool,
    pub always_on_top: bool,
    // only Windows has a taskbar a window can leave
    pub skip_taskbar: bool,
    // the desktop shows wherever the page paints nothing, once the page clears the
    // backgrounds of its html and body
    pub transparent: bool,
    // the drop shadow Windows gives a window without decorations
    pub shadow: bool,
    // whether the window takes the keyboard when it opens
    pub focused: bool,
    // pointer input goes to whatever is beneath the window
    pub click_through: bool,
    // screen capture shows the window blank, on Windows and macOS
    pub content_protected: bool,
    // shown from the moment the window opens until its page paints
    pub background: [u8; 3],
    // the desktop entry the window belongs to on linux, as its wayland app_id and x11 class
    pub app_id: Option<String>,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            label: "main".to_string(),
            title: "Ceri".to_string(),
            width: 1120,
            height: 720,
            minimum_width: 480,
            minimum_height: 320,
            maximum_size: None,
            position: None,
            start_url: format!("{APP_ORIGIN}/"),
            icon: None,
            geometry_path: None,
            decorations: true,
            resizable: true,
            visible: true,
            maximized: false,
            fullscreen: false,
            always_on_top: false,
            skip_taskbar: false,
            transparent: false,
            shadow: true,
            focused: true,
            click_through: false,
            content_protected: false,
            background: [255, 255, 255],
            app_id: None,
        }
    }
}

// physical pixels, the normal bounds even while the window is maximized
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WindowGeometry {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    maximized: bool,
}

impl WindowGeometry {
    fn read(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        let mut parts = text.split_whitespace();
        let mut next = || parts.next()?.parse::<i64>().ok();
        let (x, y, width, height) = (next()?, next()?, next()?, next()?);
        let maximized = next() == Some(1);
        if !(64..=32_768).contains(&width) || !(64..=32_768).contains(&height) {
            return None;
        }
        Some(Self {
            x: x.clamp(-32_768, 32_768) as i32,
            y: y.clamp(-32_768, 32_768) as i32,
            width: width as u32,
            height: height as u32,
            maximized,
        })
    }

    fn write(&self, path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(
            path,
            format!(
                "{} {} {} {} {}",
                self.x,
                self.y,
                self.width,
                self.height,
                u8::from(self.maximized)
            ),
        );
    }

    // a window is only put back where some monitor still shows the start of its title bar
    fn visible_on(&self, monitors: &[Monitor]) -> bool {
        monitors.iter().any(|monitor| {
            let (x, y) = (
                i64::from(self.x) + (60.0 * monitor.scale) as i64,
                i64::from(self.y) + (10.0 * monitor.scale) as i64,
            );
            let (left, top) = (i64::from(monitor.position.0), i64::from(monitor.position.1));
            (left..left + i64::from(monitor.size.0)).contains(&x)
                && (top..top + i64::from(monitor.size.1)).contains(&y)
        })
    }
}

// a display, in physical pixels
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Monitor {
    pub name: Option<String>,
    pub position: (i32, i32),
    pub size: (u32, u32),
    pub scale: f64,
    // the display new windows open on
    pub primary: bool,
    // the position and size of what a taskbar or dock leaves free, the whole display where
    // the platform does not say
    pub work_area: ((i32, i32), (u32, u32)),
}

// the platform's own handle for a window, for native code an app runs beside the page
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeHandle {
    Win32 { hwnd: isize },
    Xlib { window: u64 },
    Xcb { window: u32 },
    Wayland { surface: usize },
}

// what an open window looks like right now; position and size are physical pixels
#[derive(Clone, Debug, PartialEq)]
pub struct WindowState {
    pub position: (i32, i32),
    pub size: (u32, u32),
    pub scale: f64,
    pub zoom: f32,
    pub maximized: bool,
    pub minimized: bool,
    pub fullscreen: bool,
    pub focused: bool,
    pub visible: bool,
    pub handle: Option<NativeHandle>,
}

// sizes and positions are logical pixels
#[derive(Clone, Debug, PartialEq)]
pub enum WindowAction {
    Show,
    Hide,
    // shows, restores from the taskbar and brings the window to the front
    Focus,
    Minimize,
    Unminimize,
    Maximize,
    Unmaximize,
    ToggleMaximize,
    Fullscreen(bool),
    Title(String),
    Size(u32, u32),
    // the same in the display's own pixels, for a host that measured the page in them
    PhysicalSize(u32, u32),
    // the least the user can shrink the window to, as minimum_width and minimum_height
    MinimumSize(u32, u32),
    Position(i32, i32),
    Center,
    AlwaysOnTop(bool),
    SkipTaskbar(bool),
    ClickThrough(bool),
    ContentProtected(bool),
    // scales the page like a browser's zoom, from a quarter to five times
    Zoom(f32),
    RequestAttention,
    // moves or resizes the window with the pointer from a press the page received; a press
    // already over when the loop gets here ends at once
    Drag,
    ResizeDrag(ResizeDirection),
    // a close the user or app asks for goes to the page, which closes with Destroy unless
    // one of its listeners refuses
    InterceptClose(bool),
    Destroy,
}

// what the loop publishes for other threads to read without waiting on it
#[derive(Default)]
struct Published {
    windows: BTreeMap<String, WindowState>,
    monitors: Vec<Monitor>,
    // windows asked for that the loop has not opened yet
    opening: usize,
}

// opens and closes windows and passes events between them, from any thread
#[derive(Clone)]
pub struct WindowControl {
    proxy: EventLoopProxy<Request>,
    published: Arc<Mutex<Published>>,
}

enum Request {
    // a worker has events to poll
    Wake,
    Open(Box<WindowConfig>),
    Close(String),
    Act(String, WindowAction),
    // an event for every window's listeners, or for one window's
    Emit {
        target: Option<String>,
        name: String,
        payload: String,
    },
    Run(Box<dyn FnOnce() + Send>),
    Exit,
}

impl WindowControl {
    // opens a window, or focuses the open one with the same label
    pub fn open(&self, config: WindowConfig) {
        self.published().opening += 1;
        if self
            .proxy
            .send_event(Request::Open(Box::new(config)))
            .is_err()
        {
            self.published().opening -= 1;
        }
    }

    // asks the window to close, as its close button does
    pub fn close(&self, label: &str) {
        let _ = self.proxy.send_event(Request::Close(label.to_string()));
    }

    pub fn act(&self, label: &str, action: WindowAction) {
        let _ = self
            .proxy
            .send_event(Request::Act(label.to_string(), action));
    }

    pub fn emit(&self, target: Option<&str>, name: &str, payload: &str) {
        let _ = self.proxy.send_event(Request::Emit {
            target: target.map(str::to_string),
            name: name.to_string(),
            payload: payload.to_string(),
        });
    }

    // runs a task on the loop's thread, which owns the native windows, for platform calls that
    // have to come from it, such as an OLE drag on Windows
    pub fn run_on_loop(&self, task: impl FnOnce() + Send + 'static) {
        let _ = self.proxy.send_event(Request::Run(Box::new(task)));
    }

    // closes every window without asking and ends the loop
    pub fn exit(&self) {
        let _ = self.proxy.send_event(Request::Exit);
    }

    pub fn state(&self, label: &str) -> Option<WindowState> {
        self.published().windows.get(label).cloned()
    }

    pub fn labels(&self) -> Vec<String> {
        self.published().windows.keys().cloned().collect()
    }

    pub fn monitors(&self) -> Vec<Monitor> {
        self.published().monitors.clone()
    }

    fn published(&self) -> std::sync::MutexGuard<'_, Published> {
        self.published
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn waker(&self) -> Waker {
        let proxy = self.proxy.clone();
        Arc::new(move || {
            let _ = proxy.send_event(Request::Wake);
        })
    }
}

// what a window's browser is built with
pub struct WindowContext {
    pub label: String,
    pub control: WindowControl,
}

pub type StartedHook = Box<dyn FnOnce(&WindowControl)>;
pub type CloseHook = Box<dyn FnMut(&WindowControl, &str) -> bool>;
pub type ClosedHook = Box<dyn FnMut(&WindowControl, &str)>;
pub type DropHook = Box<dyn FnMut(&WindowControl, &str, &[PathBuf])>;

// the app's part in the loop; every hook runs on the loop's thread
#[derive(Default)]
pub struct Lifecycle {
    // keeps the loop running with no window open, for an app that lives in the tray
    pub keep_running: bool,
    // once the loop runs and the first windows are open
    pub started: Option<StartedHook>,
    // whether a window its user or app asked to close may close
    pub close_requested: Option<CloseHook>,
    pub closed: Option<ClosedHook>,
    // a window is open and its state, native handle included, can be read by its label
    pub opened: Option<ClosedHook>,
    // files let go over a window, before its page hears of them
    pub dropped: Option<DropHook>,
    // after the last window has gone, before run returns
    pub exiting: Option<Box<dyn FnOnce()>>,
    // fills in what a window's config leaves to the app, for launch windows and later ones alike
    pub prepare: Option<Box<dyn Fn(WindowConfig) -> WindowConfig>>,
}

// runs windows until the last one closes; the factory builds each window's browser on that
// window's own engine thread
pub fn run<F>(windows: Vec<WindowConfig>, factory: F) -> Result<(), String>
where
    F: Fn(&WindowContext) -> Result<Browser, String> + Send + Clone + 'static,
{
    run_with(windows, factory, Lifecycle::default())
}

pub fn run_with<F>(
    windows: Vec<WindowConfig>,
    factory: F,
    lifecycle: Lifecycle,
) -> Result<(), String>
where
    F: Fn(&WindowContext) -> Result<Browser, String> + Send + Clone + 'static,
{
    if windows.is_empty() && !lifecycle.keep_running && lifecycle.started.is_none() {
        return Err("an app needs at least one window".to_string());
    }
    let event_loop = EventLoop::<Request>::with_user_event()
        .build()
        .map_err(|error| error.to_string())?;
    let control = WindowControl {
        proxy: event_loop.create_proxy(),
        published: Arc::default(),
    };
    let context =
        Context::new(event_loop.owned_display_handle()).map_err(|error| error.to_string())?;
    let mut app = App {
        context,
        factory,
        control,
        startup: Some(windows),
        windows: HashMap::new(),
        lifecycle,
    };
    event_loop
        .run_app(&mut app)
        .map_err(|error| error.to_string())
}

struct App<F> {
    context: Context<OwnedDisplayHandle>,
    factory: F,
    control: WindowControl,
    // the windows to open once the loop starts
    startup: Option<Vec<WindowConfig>>,
    windows: HashMap<WindowId, AppWindow>,
    lifecycle: Lifecycle,
}

type Spawn = Rc<dyn Fn(Size, Option<String>) -> Result<EngineWorker, String>>;

struct AppWindow {
    config: WindowConfig,
    // starts this window's engine, showing the recovery document with a reason
    spawn: Spawn,
    restarts: u8,
    worker: EngineWorker,
    navigation_pending: bool,
    render_request: Option<u64>,
    latest_frame: Option<Frame>,
    applied_title: Option<String>,
    // the fullscreen the page last asked for, so only its changes move the window
    page_fullscreen: bool,
    applied_cursor: Option<crate::css::Cursor>,
    viewport: Size,
    // the monitor's scale times the page zoom
    scale: f32,
    // the display's refresh rate in millihertz, once the engine was told
    refresh_rate: Option<u32>,
    // the last display the page was told of
    display: Option<InputEvent>,
    page_shown: bool,
    zoom: f32,
    window: Option<Rc<Window>>,
    output: Option<Output>,
    handle: Option<NativeHandle>,
    cursor: (f32, f32),
    // buttons the page saw go down and not yet up
    pressed: Vec<PointerButton>,
    // the resize edge under the pointer of a window without decorations
    edge: Option<ResizeDirection>,
    normal: Option<WindowGeometry>,
    modifiers: ModifiersState,
    preferences: UserPreferences,
    // files are being held over the window, and the ones let go since the page last heard
    hovering: bool,
    dropped: Vec<PathBuf>,
    published: Arc<Mutex<Published>>,
    closing: bool,
    intercept_close: bool,
}

impl<F> App<F>
where
    F: Fn(&WindowContext) -> Result<Browser, String> + Send + Clone + 'static,
{
    fn open(&mut self, event_loop: &ActiveEventLoop, config: WindowConfig) {
        let config = match &self.lifecycle.prepare {
            Some(prepare) => prepare(config),
            None => config,
        };
        if let Some(open) = self
            .windows
            .values_mut()
            .find(|window| window.config.label == config.label)
        {
            open.act(WindowAction::Focus);
            return;
        }
        let viewport = Size {
            width: config.width.max(1) as f32,
            height: config.height.max(1) as f32,
        };
        let factory = self.factory.clone();
        let label = config.label.clone();
        let see_through = config.transparent;
        let control = self.control.clone();
        let notify = control.waker();
        let spawn: Spawn = Rc::new(move |viewport, recovery: Option<String>| {
            let factory = factory.clone();
            let context = WindowContext {
                label: label.clone(),
                control: control.clone(),
            };
            EngineWorker::spawn_live(
                viewport,
                move || {
                    let mut browser = factory(&context)?;
                    browser.set_see_through(see_through);
                    if let Some(reason) = recovery {
                        browser.show_recovery(500, &reason);
                    }
                    Ok(browser)
                },
                notify.clone(),
            )
        });
        let worker = match spawn(viewport, None) {
            Ok(worker) => worker,
            Err(error) => {
                eprintln!(
                    "ceri: the {} window's engine did not start: {error}",
                    config.label
                );
                return;
            }
        };
        let mut this = AppWindow {
            config,
            spawn,
            restarts: 0,
            worker,
            navigation_pending: true,
            render_request: None,
            latest_frame: None,
            applied_title: None,
            page_fullscreen: false,
            applied_cursor: None,
            viewport,
            scale: 1.0,
            refresh_rate: None,
            display: None,
            page_shown: true,
            zoom: 1.0,
            window: None,
            output: None,
            handle: None,
            cursor: (0.0, 0.0),
            pressed: Vec::new(),
            edge: None,
            normal: None,
            modifiers: ModifiersState::empty(),
            preferences: UserPreferences::default(),
            hovering: false,
            dropped: Vec::new(),
            published: self.control.published.clone(),
            closing: false,
            intercept_close: false,
        };
        let monitors = self.control.monitors();
        let Some(id) = this.create(event_loop, &self.context, &monitors) else {
            this.worker.shutdown();
            return;
        };
        // the page starts once the window has published the state it may ask for
        if let Err(error) = this.worker.navigate(this.config.start_url.clone()) {
            eprintln!(
                "ceri: the {} window's engine did not start: {error}",
                this.config.label
            );
            this.worker.shutdown();
            self.control.published().windows.remove(&this.config.label);
            return;
        }
        let label = this.config.label.clone();
        self.windows.insert(id, this);
        if let Some(hook) = &mut self.lifecycle.opened {
            hook(&self.control, &label);
        }
    }

    // a close the user or app asked for, which the app may refuse
    fn request_close(&mut self, id: WindowId) {
        let Some(label) = self
            .windows
            .get(&id)
            .map(|window| window.config.label.clone())
        else {
            return;
        };
        let allowed = match &mut self.lifecycle.close_requested {
            Some(hook) => hook(&self.control, &label),
            None => true,
        };
        if allowed && let Some(window) = self.windows.get_mut(&id) {
            if window.intercept_close {
                window.deliver("window:close-requested", "{}");
            } else {
                window.remember_geometry();
                window.closing = true;
            }
        }
    }

    fn refresh_monitors(&self, event_loop: &ActiveEventLoop) {
        let primary = event_loop.primary_monitor();
        let monitors = event_loop.available_monitors().map(|monitor| Monitor {
            primary: primary.as_ref() == Some(&monitor),
            ..monitor_of(&monitor)
        });
        self.control.published().monitors = monitors.collect();
    }

    // drops closed windows, and ends the loop with the last one
    fn sweep(&mut self, event_loop: &ActiveEventLoop) {
        let closed = self
            .windows
            .iter()
            .filter(|(_, window)| window.closing)
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        for id in closed {
            if let Some(window) = self.windows.remove(&id) {
                window.worker.shutdown();
                self.control
                    .published()
                    .windows
                    .remove(&window.config.label);
                if let Some(hook) = &mut self.lifecycle.closed {
                    hook(&self.control, &window.config.label);
                }
                // the windows still open hear which one went
                let payload = format!(
                    r#"{{"label":{}}}"#,
                    crate::script::json_quote(&window.config.label)
                );
                for open in self.windows.values_mut() {
                    open.deliver("window:closed", &payload);
                }
            }
        }
        let opening = self.control.published().opening;
        if self.windows.is_empty()
            && self.startup.is_none()
            && opening == 0
            && !self.lifecycle.keep_running
        {
            event_loop.exit();
        }
    }
}

impl AppWindow {
    fn create(
        &mut self,
        event_loop: &ActiveEventLoop,
        context: &Context<OwnedDisplayHandle>,
        monitors: &[Monitor],
    ) -> Option<WindowId> {
        let background = background_mode();
        if background {
            // the off-display place a run opens at is no place to remember
            self.config.geometry_path = None;
        }
        let config = &self.config;
        let remembered = config
            .geometry_path
            .as_deref()
            .and_then(WindowGeometry::read);
        let mut attributes = Window::default_attributes()
            .with_title(config.title.clone())
            .with_min_inner_size(LogicalSize::new(
                config.minimum_width.max(1),
                config.minimum_height.max(1),
            ))
            .with_inner_size(match remembered {
                Some(geometry) => PhysicalSize::new(geometry.width, geometry.height).into(),
                None => winit::dpi::Size::Logical(LogicalSize::new(
                    config.width as f64,
                    config.height as f64,
                )),
            })
            .with_decorations(config.decorations)
            .with_resizable(config.resizable)
            .with_visible(config.visible)
            .with_active(config.focused && !background)
            .with_transparent(config.transparent)
            .with_maximized(
                !background
                    && (config.maximized || remembered.is_some_and(|known| known.maximized)),
            )
            .with_content_protected(config.content_protected);
        if let Some((width, height)) = config.maximum_size {
            attributes = attributes.with_max_inner_size(LogicalSize::new(width, height));
        }
        if config.fullscreen && !background {
            attributes = attributes.with_fullscreen(Some(Fullscreen::Borderless(None)));
        }
        if config.always_on_top && !background {
            attributes = attributes.with_window_level(WindowLevel::AlwaysOnTop);
        }
        #[cfg(target_os = "windows")]
        {
            use winit::platform::windows::WindowAttributesExtWindows;
            attributes = attributes
                .with_skip_taskbar(config.skip_taskbar || background)
                .with_undecorated_shadow(config.shadow)
                // with no surface of its own behind the composition, the window is clear
                // where the frame is
                .with_no_redirection_bitmap(config.transparent);
        }
        #[cfg(all(unix, not(target_os = "macos")))]
        if let Some(id) = &config.app_id {
            use winit::platform::wayland::WindowAttributesExtWayland;
            attributes = attributes.with_name(id, id);
        }
        let primary = event_loop
            .primary_monitor()
            .or_else(|| event_loop.available_monitors().next());
        let position = match (remembered, config.position) {
            (Some(geometry), _) if geometry.visible_on(monitors) => {
                Some(PhysicalPosition::new(geometry.x, geometry.y).into())
            }
            (_, Some((x, y))) => Some(LogicalPosition::new(x, y).into()),
            _ => primary.map(|monitor| {
                let size = match remembered {
                    Some(geometry) => (geometry.width, geometry.height),
                    None => {
                        let scale = monitor.scale_factor();
                        (
                            (f64::from(config.width) * scale) as u32,
                            (f64::from(config.height) * scale) as u32,
                        )
                    }
                };
                centred(&monitor_of(&monitor), size).into()
            }),
        };
        let position = if background {
            Some(PhysicalPosition::new(BACKGROUND_ORIGIN.0, BACKGROUND_ORIGIN.1).into())
        } else {
            position
        };
        if let Some(position) = position {
            attributes = attributes.with_position::<winit::dpi::Position>(position);
        }
        if let Some((rgba, size)) = config.icon.clone()
            && let Ok(icon) = winit::window::Icon::from_rgba(rgba, size, size)
        {
            attributes = attributes.with_window_icon(Some(icon));
        }
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Rc::new(window),
            Err(error) => {
                eprintln!("ceri: window creation failed: {error}");
                return None;
            }
        };
        let mut output = match Output::open(context, &window) {
            Ok(output) => output,
            Err(error) => {
                eprintln!("ceri: {error}");
                return None;
            }
        };
        let size = window.inner_size();
        if !self.config.transparent
            && let (Some(width), Some(height)) =
                (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
            && let Err(error) = output.fill(self.config.background, width, height)
        {
            eprintln!("ceri: {error}");
        }
        if self.config.click_through {
            let _ = window.set_cursor_hittest(false);
        }
        window.set_ime_allowed(true);
        self.handle = native_handle(&window);
        self.normal = remembered.map(|known| WindowGeometry {
            maximized: false,
            ..known
        });
        self.applied_title = Some(self.config.title.clone());
        self.window = Some(window);
        self.output = Some(output);
        self.announce_planes();
        let scale = self.window_scale();
        if scale != self.scale {
            self.submit(InputEvent::ScaleChanged(scale));
        }
        self.follow_refresh_rate();
        let scheme = self.window_color_scheme();
        self.apply_preferences(scheme);
        self.publish();
        self.request_redraw();
        self.window.as_ref().map(|window| window.id())
    }

    fn act(&mut self, action: WindowAction) {
        let Some(window) = self.window.clone() else {
            return;
        };
        if background_mode() {
            match action {
                WindowAction::Show | WindowAction::Focus | WindowAction::Unminimize => {
                    show_inactive(&window);
                    self.show_page(true);
                    return;
                }
                // each of these would bring the window onto a display or into the user's way
                WindowAction::Maximize
                | WindowAction::ToggleMaximize
                | WindowAction::Fullscreen(_)
                | WindowAction::Position(..)
                | WindowAction::Center
                | WindowAction::AlwaysOnTop(_)
                | WindowAction::RequestAttention
                | WindowAction::Drag
                | WindowAction::ResizeDrag(_) => return,
                _ => {}
            }
        }
        match action {
            WindowAction::Show => {
                window.set_visible(true);
                self.show_page(true);
            }
            WindowAction::Hide => {
                window.set_visible(false);
                self.show_page(false);
            }
            WindowAction::Focus => {
                window.set_visible(true);
                window.set_minimized(false);
                window.focus_window();
            }
            WindowAction::Minimize => window.set_minimized(true),
            WindowAction::Unminimize => window.set_minimized(false),
            WindowAction::Maximize => window.set_maximized(true),
            WindowAction::Unmaximize => window.set_maximized(false),
            WindowAction::ToggleMaximize => window.set_maximized(!window.is_maximized()),
            WindowAction::Fullscreen(on) => {
                window.set_fullscreen(on.then_some(Fullscreen::Borderless(None)));
            }
            WindowAction::Title(title) => {
                self.config.title = title;
                self.applied_title = None;
                if let Some(frame) = &self.latest_frame
                    && frame.title.is_some()
                {
                    return;
                }
                window.set_title(&self.config.title);
                self.applied_title = Some(self.config.title.clone());
            }
            WindowAction::Size(width, height) => {
                let _ = window.request_inner_size(LogicalSize::new(width, height));
            }
            WindowAction::PhysicalSize(width, height) => {
                let _ = window.request_inner_size(PhysicalSize::new(width, height));
            }
            WindowAction::MinimumSize(width, height) => {
                window.set_min_inner_size(Some(LogicalSize::new(width.max(1), height.max(1))));
            }
            WindowAction::Position(x, y) => window.set_outer_position(LogicalPosition::new(x, y)),
            WindowAction::Center => {
                if let Some(monitor) = window
                    .current_monitor()
                    .or_else(|| window.primary_monitor())
                {
                    let size = window.outer_size();
                    window.set_outer_position(centred(
                        &monitor_of(&monitor),
                        (size.width, size.height),
                    ));
                }
            }
            WindowAction::AlwaysOnTop(on) => window.set_window_level(if on {
                WindowLevel::AlwaysOnTop
            } else {
                WindowLevel::Normal
            }),
            WindowAction::SkipTaskbar(skip) => {
                #[cfg(target_os = "windows")]
                {
                    use winit::platform::windows::WindowExtWindows;
                    window.set_skip_taskbar(skip);
                }
                #[cfg(not(target_os = "windows"))]
                let _ = skip;
            }
            WindowAction::ClickThrough(on) => {
                let _ = window.set_cursor_hittest(!on);
            }
            WindowAction::ContentProtected(on) => window.set_content_protected(on),
            WindowAction::Zoom(zoom) => {
                if zoom.is_finite() {
                    self.zoom = zoom.clamp(0.25, 5.0);
                    let scale = self.window_scale();
                    self.submit(InputEvent::ScaleChanged(scale));
                    self.request_redraw();
                }
            }
            WindowAction::RequestAttention => {
                window.request_user_attention(Some(UserAttentionType::Informational));
            }
            WindowAction::Drag => {
                let _ = window.drag_window();
            }
            WindowAction::ResizeDrag(direction) => {
                let _ = window.drag_resize_window(direction);
            }
            WindowAction::InterceptClose(on) => self.intercept_close = on,
            WindowAction::Destroy => {
                self.remember_geometry();
                self.closing = true;
            }
        }
        self.publish();
    }

    // keeps the state other threads read in step with the window
    fn publish(&mut self) {
        let Some(window) = &self.window else {
            return;
        };
        let size = window.inner_size();
        let position = window
            .outer_position()
            .unwrap_or(PhysicalPosition::new(0, 0));
        let minimized = window.is_minimized().unwrap_or(false);
        // a minimized window reports what it returns to
        let maximized = match minimized {
            true => self.published_state().is_some_and(|state| state.maximized),
            false => window.is_maximized(),
        };
        let state = WindowState {
            position: (position.x, position.y),
            size: (size.width, size.height),
            scale: window.scale_factor(),
            zoom: self.zoom,
            maximized,
            minimized,
            fullscreen: window.fullscreen().is_some(),
            focused: window.has_focus(),
            visible: window.is_visible().unwrap_or(true),
            handle: self.handle,
        };
        if !state.maximized && !state.minimized && !state.fullscreen && size.width > 0 {
            self.normal = Some(WindowGeometry {
                x: position.x,
                y: position.y,
                width: size.width,
                height: size.height,
                maximized: false,
            });
        }
        self.published
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .windows
            .insert(self.config.label.clone(), state);
    }

    // a window without decorations is resized from a thin band inside its edges
    fn resize_edge(&self, x: f64, y: f64) -> Option<ResizeDirection> {
        let window = self.window.as_ref()?;
        if self.config.decorations
            || !self.config.resizable
            || window.is_maximized()
            || window.fullscreen().is_some()
        {
            return None;
        }
        let size = window.inner_size();
        resize_direction(
            (f64::from(size.width), f64::from(size.height)),
            (x, y),
            window.scale_factor(),
        )
    }

    fn window_event(&mut self, event: WindowEvent) {
        match event {
            WindowEvent::Focused(focused) => {
                if focused {
                    // forced colours can come and go without the theme changing, and the
                    // window is told of neither while it is out of focus
                    let scheme = self.window_color_scheme();
                    self.apply_preferences(scheme);
                } else {
                    self.submit(InputEvent::Blur);
                }
                self.publish();
            }
            WindowEvent::Moved(_) => {
                self.follow_refresh_rate();
                self.publish();
            }
            // one event comes for each file held or let go, so the page hears once of the hold
            WindowEvent::HoveredFile(_) => {
                if !self.hovering {
                    self.hovering = true;
                    self.deliver("window:file-hover", r#"{"over":true}"#);
                }
            }
            WindowEvent::HoveredFileCancelled => {
                if self.hovering {
                    self.hovering = false;
                    self.deliver("window:file-hover", r#"{"over":false}"#);
                }
            }
            WindowEvent::DroppedFile(path) => {
                self.hovering = false;
                self.dropped.push(path);
            }
            // an off-display window reads as covered, and a run wants its page live
            WindowEvent::Occluded(occluded) if !background_mode() => {
                self.show_page(!occluded);
                self.publish();
            }
            WindowEvent::CursorLeft { .. } => {
                if self.edge.take().is_some() {
                    self.show_cursor();
                }
            }
            WindowEvent::Resized(size) => {
                #[cfg(target_os = "windows")]
                if let Some(output) = &mut self.output
                    && let Err(error) = output.extend(size.width, size.height)
                {
                    eprintln!("ceri: {error}");
                }
                let viewport = layout_viewport(size.width, size.height, self.window_scale());
                self.submit(InputEvent::Resize(viewport));
                self.publish();
                if let Some(state) = self.published_state() {
                    let payload = format!(
                        "{{\"width\":{},\"height\":{},\"maximized\":{},\"minimized\":{},\"fullscreen\":{}}}",
                        viewport.width,
                        viewport.height,
                        state.maximized,
                        state.minimized,
                        state.fullscreen
                    );
                    self.deliver("window:resized", &payload);
                }
                self.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                let scale = self.window_scale();
                self.submit(InputEvent::ScaleChanged(scale));
                self.publish();
                self.request_redraw();
            }
            WindowEvent::ThemeChanged(theme) => {
                let scheme = match theme {
                    Theme::Dark => ColorScheme::Dark,
                    Theme::Light => ColorScheme::Light,
                };
                self.apply_preferences(scheme);
                self.request_redraw();
            }
            WindowEvent::CursorMoved { position, .. } => {
                let edge = self.resize_edge(position.x, position.y);
                if edge != self.edge {
                    self.edge = edge;
                    self.show_cursor();
                }
                if edge.is_some() {
                    return;
                }
                let scale = f64::from(self.scale);
                self.cursor = ((position.x / scale) as f32, (position.y / scale) as f32);
                self.submit(InputEvent::PointerMove {
                    x: self.cursor.0,
                    y: self.cursor.1,
                });
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (delta_x, delta_y) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x * 40.0, -y * 40.0),
                    MouseScrollDelta::PixelDelta(position) => {
                        let scale = f64::from(self.scale);
                        ((position.x / scale) as f32, -(position.y / scale) as f32)
                    }
                };
                let (x, y) = self.cursor;
                self.submit(InputEvent::Scroll {
                    x,
                    y,
                    delta_x,
                    delta_y,
                });
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let Some(button) = Self::pointer_button(button) else {
                    return;
                };
                if let (Some(edge), Some(window)) = (self.edge, &self.window) {
                    if state == ElementState::Pressed && button == PointerButton::Primary {
                        let _ = window.drag_resize_window(edge);
                    }
                    return;
                }
                let (x, y) = self.cursor;
                let event = match state {
                    ElementState::Pressed => InputEvent::PointerDown { x, y, button },
                    ElementState::Released => InputEvent::PointerUp { x, y, button },
                };
                if !self.submit(event) {
                    return;
                }
                self.pressed.retain(|&held| held != button);
                if state == ElementState::Pressed {
                    self.pressed.push(button);
                }
                if state == ElementState::Released && button == PointerButton::Primary {
                    self.submit(InputEvent::Click { x, y });
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let key = match event.logical_key {
                    Key::Character(character) => character.to_string(),
                    Key::Named(NamedKey::Space) => " ".to_string(),
                    Key::Named(named) => format!("{named:?}"),
                    other => format!("{other:?}"),
                };
                // winit names codes after KeyboardEvent.code, except Meta, which it calls Super
                let code = match event.physical_key {
                    PhysicalKey::Code(KeyCode::SuperLeft) => "MetaLeft".to_string(),
                    PhysicalKey::Code(KeyCode::SuperRight) => "MetaRight".to_string(),
                    PhysicalKey::Code(code) => format!("{code:?}"),
                    PhysicalKey::Unidentified(_) => String::new(),
                };
                self.submit(InputEvent::Key {
                    key,
                    code,
                    modifiers: KeyModifiers {
                        shift: self.modifiers.shift_key(),
                        control: self.modifiers.control_key(),
                        alt: self.modifiers.alt_key(),
                        meta: self.modifiers.super_key(),
                    },
                    repeat: event.repeat,
                    pressed: event.state == ElementState::Pressed,
                });
            }
            WindowEvent::Ime(Ime::Preedit(text, cursor)) => {
                self.submit(InputEvent::CompositionUpdate {
                    text: text.to_string(),
                    cursor,
                });
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                self.submit(InputEvent::CompositionCommit {
                    text: text.to_string(),
                });
            }
            WindowEvent::Ime(Ime::Disabled) => {
                self.submit(InputEvent::CompositionCancel);
            }
            WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }

    // a composition presenter shows videos under the canvas, so the engine stops painting them
    fn announce_planes(&mut self) {
        if self.output.as_ref().is_some_and(Output::composed)
            && let Err(error) = self.worker.show_planes()
        {
            eprintln!("ceri: videos stay painted into the page, {error}");
        }
    }

    // one restart, then the engine's own recovery document
    fn recover(&mut self, detail: &str) -> bool {
        self.restarts = self.restarts.saturating_add(1);
        if self.restarts > 2 {
            eprintln!("ceri: the engine stopped again, closing the window");
            self.closing = true;
            return false;
        }
        let recovering = self.restarts > 1;
        match (self.spawn)(self.viewport, recovering.then(|| detail.to_string())) {
            Ok(mut worker) => {
                if !recovering && worker.navigate(self.config.start_url.clone()).is_err() {
                    eprintln!("ceri: the restarted engine refused the app URL");
                    self.closing = true;
                    return false;
                }
                eprintln!("ceri: the engine restarted after {detail}");
                self.worker = worker;
                self.announce_planes();
                self.navigation_pending = !recovering;
                self.render_request = None;
                self.latest_frame = None;
                self.request_redraw();
                true
            }
            Err(error) => {
                eprintln!("ceri: the engine could not restart: {error}");
                self.closing = true;
                false
            }
        }
    }

    fn deliver(&mut self, name: &str, payload: &str) {
        match self.worker.emit(name, payload) {
            Ok(_) => {}
            Err(error) if error == "engine worker queue is full" => {
                eprintln!(
                    "ceri: the {} window dropped the {name} event",
                    self.config.label
                );
            }
            Err(error) => {
                self.recover(&error);
            }
        }
    }

    fn request_redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    // a modal loop a task ran, such as an OLE drag, reads the release itself, so the page gets
    // it here without a click
    fn release_lost_buttons(&mut self) {
        let (x, y) = self.cursor;
        let released: Vec<_> = self
            .pressed
            .iter()
            .copied()
            .filter(|&button| !button_held(button))
            .collect();
        for button in released {
            self.pressed.retain(|&held| held != button);
            self.submit(InputEvent::PointerUp { x, y, button });
        }
    }

    fn submit(&mut self, event: InputEvent) -> bool {
        let resize = match &event {
            InputEvent::Resize(viewport) => Some(*viewport),
            _ => None,
        };
        let rescale = match &event {
            InputEvent::ScaleChanged(scale) => Some(*scale),
            _ => None,
        };
        match self.worker.input(event) {
            Ok(_) => {
                if let Some(viewport) = resize {
                    self.viewport = viewport;
                }
                if let Some(scale) = rescale {
                    self.scale = scale;
                }
                self.request_redraw();
                true
            }
            Err(error) if error == "engine worker queue is full" => true,
            Err(error) => self.recover(&error),
        }
    }

    fn poll_worker(&mut self) -> bool {
        let mut request_redraw = false;
        for event in self.worker.poll() {
            match event {
                EngineWorkerEvent::Navigated { result, .. } => {
                    self.navigation_pending = false;
                    match result {
                        Ok(_) => request_redraw = true,
                        Err(error) => return self.recover(&error.to_string()),
                    }
                }
                EngineWorkerEvent::Input { result, .. } => match result {
                    Ok(result) => {
                        if let Some(cursor) = result.cursor {
                            self.apply_cursor(cursor);
                        }
                        if let Some(region) = result.region {
                            self.answer_region(region);
                        }
                        request_redraw = true;
                    }
                    Err(error) => return self.recover(&error.to_string()),
                },
                EngineWorkerEvent::Frame { request_id, result } => {
                    if self.render_request == Some(request_id) {
                        self.render_request = None;
                    }
                    match result {
                        Ok(frame) => {
                            if frame.is_some() {
                                self.latest_frame = frame;
                                request_redraw = true;
                            }
                        }
                        Err(error) => return self.recover(&error.to_string()),
                    }
                }
                EngineWorkerEvent::Invalidated => request_redraw = true,
                EngineWorkerEvent::Failed(error) => {
                    return self.recover(&error.to_string());
                }
                EngineWorkerEvent::Accessibility { .. }
                | EngineWorkerEvent::NavigationFailure { .. } => {}
            }
        }
        if request_redraw {
            self.request_redraw();
        }
        true
    }

    fn apply_cursor(&mut self, cursor: crate::css::Cursor) {
        if self.applied_cursor == Some(cursor) {
            return;
        }
        self.applied_cursor = Some(cursor);
        self.show_cursor();
    }

    // a resize edge's arrows win over the page's cursor
    fn show_cursor(&self) {
        let Some(window) = &self.window else {
            return;
        };
        let page = self.applied_cursor.unwrap_or_default();
        let icon = match self.edge {
            Some(ResizeDirection::North | ResizeDirection::South) => CursorIcon::NsResize,
            Some(ResizeDirection::East | ResizeDirection::West) => CursorIcon::EwResize,
            Some(ResizeDirection::NorthWest | ResizeDirection::SouthEast) => CursorIcon::NwseResize,
            Some(ResizeDirection::NorthEast | ResizeDirection::SouthWest) => CursorIcon::NeswResize,
            // the engine's names are the css keywords, which the platform cursor set shares
            None => page.name().parse::<CursorIcon>().unwrap_or_default(),
        };
        window.set_cursor_visible(self.edge.is_some() || page != crate::css::Cursor::None);
        window.set_cursor(icon);
    }

    fn published_state(&self) -> Option<WindowState> {
        self.published
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .windows
            .get(&self.config.label)
            .cloned()
    }

    fn answer_region(&self, region: RegionPress) {
        let Some(window) = &self.window else {
            return;
        };
        match region {
            // the press may already be over when the engine answers, and the platform then ends
            // the move at once
            RegionPress::Move => {
                let _ = window.drag_window();
            }
            // as on a native caption, a window that keeps its size does not maximize
            RegionPress::ToggleMaximize if self.config.resizable => {
                window.set_maximized(!window.is_maximized())
            }
            RegionPress::ToggleMaximize => {}
        }
    }

    fn apply_fullscreen(&mut self, frame: &Frame) {
        if frame.fullscreen == self.page_fullscreen {
            return;
        }
        self.page_fullscreen = frame.fullscreen;
        if let Some(window) = &self.window
            && !background_mode()
        {
            window.set_fullscreen(frame.fullscreen.then_some(Fullscreen::Borderless(None)));
        }
    }

    fn apply_title(&mut self, frame: &Frame) {
        let title = frame
            .title
            .clone()
            .unwrap_or_else(|| self.config.title.clone());
        if self.applied_title.as_deref() == Some(title.as_str()) {
            return;
        }
        if let Some(window) = &self.window {
            window.set_title(&title);
        }
        self.applied_title = Some(title);
    }

    fn redraw(&mut self) {
        if !self.poll_worker() {
            return;
        }
        let Some(size) = self.window.as_ref().map(|window| window.inner_size()) else {
            return;
        };
        let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return;
        };
        let scale = self.window_scale();
        if self.scale != scale && !self.submit(InputEvent::ScaleChanged(scale)) {
            return;
        }
        let viewport = layout_viewport(size.width, size.height, scale);
        if self.viewport != viewport && !self.submit(InputEvent::Resize(viewport)) {
            return;
        }
        if self.navigation_pending {
            return;
        }
        if self.render_request.is_none() {
            match self.worker.render() {
                Ok(request_id) => self.render_request = Some(request_id),
                Err(error) if error == "engine worker queue is full" => {}
                Err(error) => {
                    self.recover(&error);
                    return;
                }
            }
        }
        let Some(frame) = self.latest_frame.take() else {
            return;
        };
        if frame.viewport != viewport {
            if let Some(output) = &mut self.output
                && let Err(error) = output.present_stale(&frame, width, height)
            {
                eprintln!("ceri: {error}");
            }
            return;
        }
        self.apply_title(&frame);
        self.apply_fullscreen(&frame);
        let Some(output) = &mut self.output else {
            return;
        };
        if let Err(error) = output.present(&frame, width, height) {
            eprintln!("ceri: {error}");
            self.closing = true;
        }
    }

    fn remember_geometry(&mut self) {
        if self.config.geometry_path.is_none() {
            return;
        }
        self.publish();
        let maximized = self.published_state().is_some_and(|state| state.maximized);
        if let (Some(path), Some(normal)) = (&self.config.geometry_path, self.normal) {
            WindowGeometry {
                maximized,
                ..normal
            }
            .write(path);
        }
    }

    fn window_color_scheme(&self) -> ColorScheme {
        match self.window.as_ref().and_then(|window| window.theme()) {
            Some(Theme::Dark) => ColorScheme::Dark,
            Some(Theme::Light) => ColorScheme::Light,
            None => self.preferences.color_scheme,
        }
    }

    fn apply_preferences(&mut self, scheme: ColorScheme) {
        let preferences = UserPreferences {
            color_scheme: scheme,
            forced_colors: forced_palette(),
            ..self.preferences
        };
        if preferences != self.preferences
            && self.submit(InputEvent::PreferencesChanged(preferences))
        {
            self.preferences = preferences;
        }
    }

    // tells the page whether its window shows, when that changes
    fn show_page(&mut self, shown: bool) {
        if self.page_shown != shown {
            self.page_shown = shown;
            self.submit(InputEvent::Visible(shown));
        }
    }

    // tells the engine how often the display the window is on refreshes, and how big it is,
    // when either changes
    fn follow_refresh_rate(&mut self) {
        let monitor = self
            .window
            .as_ref()
            .and_then(|window| window.current_monitor());
        let rate = monitor
            .as_ref()
            .and_then(|monitor| monitor.refresh_rate_millihertz());
        if let Some(rate) = rate.filter(|rate| self.refresh_rate != Some(*rate)) {
            self.refresh_rate = Some(rate);
            self.submit(InputEvent::RefreshRate(rate));
        }
        if let Some(monitor) = monitor {
            let known = monitor_of(&monitor);
            // css pixels count from the display's own corner at its own scale
            let scale = (known.scale as f32).max(0.1);
            let ((left, top), (width, height)) = known.work_area;
            let display = InputEvent::Display {
                size: (known.size.0 as f32 / scale, known.size.1 as f32 / scale),
                available: [
                    (left - known.position.0) as f32 / scale,
                    (top - known.position.1) as f32 / scale,
                    width as f32 / scale,
                    height as f32 / scale,
                ],
            };
            if self.display.as_ref() != Some(&display) {
                self.display = Some(display.clone());
                self.submit(display);
            }
        }
    }

    fn window_scale(&self) -> f32 {
        self.window
            .as_ref()
            .map(|window| window.scale_factor() as f32 * self.zoom)
            .unwrap_or(self.scale)
    }

    fn pointer_button(button: MouseButton) -> Option<PointerButton> {
        match button {
            MouseButton::Left => Some(PointerButton::Primary),
            MouseButton::Right => Some(PointerButton::Secondary),
            MouseButton::Middle => Some(PointerButton::Auxiliary),
            MouseButton::Back | MouseButton::Forward | MouseButton::Other(_) => None,
        }
    }
}

#[cfg(target_os = "windows")]
fn button_held(button: PointerButton) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetKeyState, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON,
    };
    let key = match button {
        PointerButton::Primary => VK_LBUTTON,
        PointerButton::Secondary => VK_RBUTTON,
        PointerButton::Auxiliary => VK_MBUTTON,
    };
    // this thread's view of the button, which moves as its queue is read
    unsafe { GetKeyState(i32::from(key.0)) < 0 }
}

// winit tracks no button state to ask, so a release a task's own loop read stays unseen here
#[cfg(not(target_os = "windows"))]
fn button_held(_: PointerButton) -> bool {
    true
}

impl<F> ApplicationHandler<Request> for App<F>
where
    F: Fn(&WindowContext) -> Result<Browser, String> + Send + Clone + 'static,
{
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.refresh_monitors(event_loop);
        if let Some(startup) = self.startup.take() {
            for config in startup {
                self.open(event_loop, config);
            }
            if let Some(started) = self.lifecycle.started.take() {
                started(&self.control);
            }
        }
        for window in self.windows.values() {
            window.request_redraw();
        }
        self.sweep(event_loop);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => self.request_close(window_id),
            event => {
                let moved = matches!(
                    event,
                    WindowEvent::Moved(_) | WindowEvent::ScaleFactorChanged { .. }
                );
                if let Some(window) = self.windows.get_mut(&window_id) {
                    window.window_event(event);
                }
                if moved {
                    self.refresh_monitors(event_loop);
                }
            }
        }
        self.sweep(event_loop);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, request: Request) {
        match request {
            Request::Wake => {
                for window in self.windows.values_mut() {
                    window.poll_worker();
                }
            }
            Request::Open(config) => {
                self.refresh_monitors(event_loop);
                self.open(event_loop, *config);
                let mut published = self.control.published();
                published.opening = published.opening.saturating_sub(1);
            }
            Request::Close(label) => {
                let id = self
                    .windows
                    .iter()
                    .find(|(_, window)| window.config.label == label)
                    .map(|(id, _)| *id);
                if let Some(id) = id {
                    self.request_close(id);
                }
            }
            Request::Act(label, action) => {
                if let Some(window) = self
                    .windows
                    .values_mut()
                    .find(|window| window.config.label == label)
                {
                    window.act(action);
                }
            }
            Request::Emit {
                target,
                name,
                payload,
            } => {
                for window in self.windows.values_mut() {
                    if target
                        .as_ref()
                        .is_none_or(|label| *label == window.config.label)
                    {
                        window.deliver(&name, &payload);
                    }
                }
            }
            Request::Run(task) => {
                task();
                for window in self.windows.values_mut() {
                    window.release_lost_buttons();
                }
            }
            Request::Exit => {
                for window in self.windows.values_mut() {
                    window.remember_geometry();
                }
                event_loop.exit();
                return;
            }
        }
        self.sweep(event_loop);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        for window in self.windows.values_mut() {
            window.poll_worker();
            // the files of one drop arrive one by one, and are whole once the loop has no
            // more events
            if !window.dropped.is_empty() {
                let paths = std::mem::take(&mut window.dropped);
                if let Some(hook) = &mut self.lifecycle.dropped {
                    hook(&self.control, &window.config.label, &paths);
                }
                window.deliver("window:file-drop", &drop_payload(&paths));
            }
        }
        self.sweep(event_loop);
        // workers keep their own clocks and wake the loop when they have something to show
        event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        for (_, window) in self.windows.drain() {
            window.worker.shutdown();
        }
        if let Some(exiting) = self.lifecycle.exiting.take() {
            exiting();
        }
    }
}

fn monitor_of(handle: &winit::monitor::MonitorHandle) -> Monitor {
    let position = (handle.position().x, handle.position().y);
    let size = (handle.size().width, handle.size().height);
    Monitor {
        name: handle.name(),
        position,
        size,
        scale: handle.scale_factor(),
        primary: false,
        work_area: work_area(handle).unwrap_or((position, size)),
    }
}

#[cfg(target_os = "windows")]
fn work_area(handle: &winit::monitor::MonitorHandle) -> Option<((i32, i32), (u32, u32))> {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, HMONITOR, MONITORINFO};
    use winit::platform::windows::MonitorHandleExtWindows;
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: the structure names its own size and lives across the call
    unsafe { GetMonitorInfoW(HMONITOR(handle.hmonitor() as *mut _), &mut info) }
        .as_bool()
        .then(|| {
            let area = info.rcWork;
            (
                (area.left, area.top),
                (
                    (area.right - area.left).max(0) as u32,
                    (area.bottom - area.top).max(0) as u32,
                ),
            )
        })
}

#[cfg(not(target_os = "windows"))]
fn work_area(_handle: &winit::monitor::MonitorHandle) -> Option<((i32, i32), (u32, u32))> {
    None
}

fn centred(monitor: &Monitor, size: (u32, u32)) -> PhysicalPosition<i32> {
    let start = |origin: i32, room: u32, length: u32| {
        origin + ((i64::from(room) - i64::from(length)) / 2).max(0) as i32
    };
    PhysicalPosition::new(
        start(monitor.position.0, monitor.size.0, size.0),
        start(monitor.position.1, monitor.size.1, size.1),
    )
}

// physical pixels; the band is five logical pixels deep and a corner reaches sixteen along
// each side, as in Chromium's frameless windows
fn resize_direction(size: (f64, f64), at: (f64, f64), scale: f64) -> Option<ResizeDirection> {
    let (inset, corner) = (5.0 * scale, 16.0 * scale);
    let ((width, height), (x, y)) = (size, at);
    let (left, right, top, bottom) = (
        x < inset,
        x >= width - inset,
        y < inset,
        y >= height - inset,
    );
    if !(left || right || top || bottom) {
        return None;
    }
    let (near_left, near_right) = (x < corner, x >= width - corner);
    let (near_top, near_bottom) = (y < corner, y >= height - corner);
    Some(match () {
        _ if (top && near_left) || (left && near_top) => ResizeDirection::NorthWest,
        _ if (top && near_right) || (right && near_top) => ResizeDirection::NorthEast,
        _ if (bottom && near_left) || (left && near_bottom) => ResizeDirection::SouthWest,
        _ if (bottom && near_right) || (right && near_bottom) => ResizeDirection::SouthEast,
        _ if top => ResizeDirection::North,
        _ if bottom => ResizeDirection::South,
        _ if left => ResizeDirection::West,
        _ => ResizeDirection::East,
    })
}

// where a window opens under CERI_BACKGROUND, far past any display
const BACKGROUND_ORIGIN: (i32, i32) = (-20000, 0);

// CERI_BACKGROUND=1 is for automated runs beside whatever the user is doing: windows open off
// every display, never take focus or a taskbar button, never move onto a display, and their
// pages run as shown
fn background_mode() -> bool {
    static BACKGROUND: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *BACKGROUND
        .get_or_init(|| std::env::var_os("CERI_BACKGROUND").is_some_and(|value| value == "1"))
}

fn show_inactive(window: &Window) {
    #[cfg(target_os = "windows")]
    if let Some(NativeHandle::Win32 { hwnd }) = native_handle(window) {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::UI::WindowsAndMessaging::{SW_SHOWNOACTIVATE, ShowWindow};
        // safety: the handle belongs to a live window this thread owns
        let _ = unsafe { ShowWindow(HWND(hwnd as *mut std::ffi::c_void), SW_SHOWNOACTIVATE) };
        return;
    }
    window.set_visible(true);
}

// c_ulong is 32 bits on windows, so the xlib conversion only looks useless on linux
#[allow(clippy::useless_conversion)]
fn native_handle(window: &Window) -> Option<NativeHandle> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(NativeHandle::Win32 {
            hwnd: handle.hwnd.get(),
        }),
        RawWindowHandle::Xlib(handle) => Some(NativeHandle::Xlib {
            window: u64::from(handle.window),
        }),
        RawWindowHandle::Xcb(handle) => Some(NativeHandle::Xcb {
            window: handle.window.get(),
        }),
        RawWindowHandle::Wayland(handle) => Some(NativeHandle::Wayland {
            surface: handle.surface.as_ptr() as usize,
        }),
        _ => None,
    }
}

fn layout_viewport(width: u32, height: u32, scale: f32) -> Size {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    Size {
        width: (width.max(1) as f32 / scale).max(1.0),
        height: (height.max(1) as f32 / scale).max(1.0),
    }
}

// copies the left, top, right and bottom box of the canvas into a surface buffer
fn present_frame(
    frame: &Frame,
    (width, _): (u32, u32),
    [left, top, right, bottom]: [u32; 4],
    buffer: &mut [u32],
) {
    let pixels = frame.canvas.rgba_pixels();
    let stride = frame.canvas.bytes_per_row();
    let (left, width) = (left as usize, width as usize);
    let right = (right.min(frame.canvas.width()) as usize).max(left);
    for row in top as usize..bottom.min(frame.canvas.height()) as usize {
        let source = &pixels[row * stride + left * 4..row * stride + right * 4];
        let target = &mut buffer[row * width + left..row * width + right];
        for (pixel, rgba) in target.iter_mut().zip(source.as_chunks::<4>().0.iter()) {
            *pixel = u32::from(rgba[0]) << 16 | u32::from(rgba[1]) << 8 | u32::from(rgba[2]);
        }
    }
}

fn drop_payload(paths: &[PathBuf]) -> String {
    let quoted: Vec<_> = paths
        .iter()
        .map(|path| crate::script::json_quote(&path.to_string_lossy()))
        .collect();
    format!(r#"{{"paths":[{}]}}"#, quoted.join(","))
}

// the palette windows forces on applications while its high contrast setting is on, as
// chromium maps it: the window colours for the page and its fields, the hot light for links
#[cfg(target_os = "windows")]
fn forced_palette() -> Option<crate::css::SystemColors> {
    use windows::Win32::Graphics::Gdi::{
        COLOR_BTNFACE, COLOR_BTNTEXT, COLOR_GRAYTEXT, COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT,
        COLOR_HOTLIGHT, COLOR_WINDOW, COLOR_WINDOWTEXT, GetSysColor, SYS_COLOR_INDEX,
    };
    use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
    use windows::Win32::UI::WindowsAndMessaging::{
        SPI_GETHIGHCONTRAST, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
    };
    let mut state = HIGHCONTRASTW {
        cbSize: size_of::<HIGHCONTRASTW>() as u32,
        ..Default::default()
    };
    // SAFETY: the structure is the one the query fills, and names its own size
    unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            state.cbSize,
            Some((&raw mut state).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .ok()?;
    if !state.dwFlags.contains(HCF_HIGHCONTRASTON) {
        return None;
    }
    let color = |index: SYS_COLOR_INDEX| {
        // SAFETY: reads one entry of the system colour table
        let value = unsafe { GetSysColor(index) };
        crate::css::Color::opaque(value as u8, (value >> 8) as u8, (value >> 16) as u8)
    };
    let (link, text) = (color(COLOR_HOTLIGHT), color(COLOR_BTNTEXT));
    Some(crate::css::SystemColors {
        canvas: color(COLOR_WINDOW),
        canvas_text: color(COLOR_WINDOWTEXT),
        link_text: link,
        visited_text: link,
        active_text: link,
        gray_text: color(COLOR_GRAYTEXT),
        highlight: color(COLOR_HIGHLIGHT),
        highlight_text: color(COLOR_HIGHLIGHTTEXT),
        button_face: color(COLOR_BTNFACE),
        button_text: text,
        button_border: text,
        field: color(COLOR_WINDOW),
        field_text: color(COLOR_WINDOWTEXT),
    })
}

#[cfg(not(target_os = "windows"))]
fn forced_palette() -> Option<crate::css::SystemColors> {
    None
}

#[cfg(test)]
mod tests {
    use super::{
        Monitor, ResizeDirection, centred, layout_viewport, present_frame, resize_direction,
    };

    #[test]
    fn a_software_present_copies_only_the_box_it_is_given() {
        let frame = crate::host::Frame {
            sequence: 0,
            viewport: crate::layout::Size {
                width: 4.0,
                height: 3.0,
            },
            scale: 1.0,
            title: None,
            fullscreen: false,
            canvas: std::sync::Arc::new(crate::paint::Canvas::new(
                4,
                3,
                crate::css::Color::opaque(1, 2, 3),
            )),
            planes: Vec::new(),
        };
        let mut buffer = vec![7; 5 * 3];
        present_frame(&frame, (5, 3), [1, 1, 3, 2], &mut buffer);
        let painted = 0x01_02_03;
        assert_eq!(
            buffer,
            [7, 7, 7, 7, 7, 7, painted, painted, 7, 7, 7, 7, 7, 7, 7]
        );
        present_frame(&frame, (5, 3), [0, 0, 5, 3], &mut buffer);
        assert!(
            buffer
                .chunks(5)
                .all(|row| row[..4] == [painted; 4] && row[4] == 7),
            "the surface past the canvas is left alone"
        );
    }

    #[test]
    fn frameless_windows_resize_from_a_band_inside_their_edges() {
        let size = (800.0, 600.0);
        let at = |x, y| resize_direction(size, (x, y), 1.0);
        assert_eq!(at(400.0, 300.0), None);
        assert_eq!(at(400.0, 6.0), None);
        assert_eq!(at(400.0, 2.0), Some(ResizeDirection::North));
        assert_eq!(at(400.0, 597.0), Some(ResizeDirection::South));
        assert_eq!(at(1.0, 300.0), Some(ResizeDirection::West));
        assert_eq!(at(799.0, 300.0), Some(ResizeDirection::East));
        assert_eq!(at(12.0, 1.0), Some(ResizeDirection::NorthWest));
        assert_eq!(at(1.0, 590.0), Some(ResizeDirection::SouthWest));
        assert_eq!(at(790.0, 598.0), Some(ResizeDirection::SouthEast));
        assert_eq!(at(797.0, 10.0), Some(ResizeDirection::NorthEast));
        assert_eq!(
            resize_direction((1600.0, 1200.0), (800.0, 8.0), 2.0),
            Some(ResizeDirection::North),
            "the band is logical pixels deep"
        );
    }

    #[test]
    fn centring_splits_the_spare_room_and_keeps_big_windows_at_the_corner() {
        let monitor = Monitor {
            name: None,
            position: (1920, 0),
            size: (2560, 1440),
            scale: 1.5,
            ..Default::default()
        };
        let placed = centred(&monitor, (1620, 1050));
        assert_eq!((placed.x, placed.y), (1920 + 470, 195));
        let huge = centred(&monitor, (4000, 3000));
        assert_eq!((huge.x, huge.y), (1920, 0));
    }

    #[test]
    fn layout_viewport_divides_physical_pixels_by_the_display_scale() {
        let viewport = layout_viewport(2400, 1500, 2.0);
        assert_eq!((viewport.width, viewport.height), (1200.0, 750.0));
        let unscaled = layout_viewport(800, 600, 1.0);
        assert_eq!((unscaled.width, unscaled.height), (800.0, 600.0));
        let guarded = layout_viewport(0, 0, 0.0);
        assert_eq!((guarded.width, guarded.height), (1.0, 1.0));
    }
}

#[cfg(test)]
mod geometry_tests {
    use super::{Monitor, WindowGeometry};

    #[test]
    fn window_geometry_round_trips_through_a_file() {
        let path = std::env::temp_dir().join(format!("ceri-window-{}", std::process::id()));
        let geometry = WindowGeometry {
            x: 120,
            y: -40,
            width: 1280,
            height: 800,
            maximized: true,
        };
        geometry.write(&path);
        assert_eq!(WindowGeometry::read(&path), Some(geometry));

        std::fs::write(&path, "10 20 900 600").expect("an older geometry should be writable");
        assert_eq!(
            WindowGeometry::read(&path),
            Some(WindowGeometry {
                x: 10,
                y: 20,
                width: 900,
                height: 600,
                maximized: false,
            }),
            "a file from before the maximized flag still restores"
        );

        std::fs::write(&path, "0 0 4 4").expect("a tiny geometry should be writable");
        assert_eq!(
            WindowGeometry::read(&path),
            None,
            "an unusable size is ignored rather than applied"
        );
        std::fs::write(&path, "not a window").expect("garbage should be writable");
        assert_eq!(WindowGeometry::read(&path), None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_window_returns_only_where_a_monitor_still_shows_its_title_bar() {
        let monitors = [
            Monitor {
                name: None,
                position: (0, 0),
                size: (1920, 1080),
                scale: 1.0,
                ..Default::default()
            },
            Monitor {
                name: None,
                position: (-2560, -200),
                size: (2560, 1440),
                scale: 2.0,
                ..Default::default()
            },
        ];
        let at = |x, y| WindowGeometry {
            x,
            y,
            width: 1000,
            height: 700,
            maximized: false,
        };
        assert!(at(100, 100).visible_on(&monitors));
        assert!(at(-2400, -100).visible_on(&monitors));
        assert!(
            !at(1900, 100).visible_on(&monitors),
            "only its corner shows"
        );
        assert!(
            !at(4000, 100).visible_on(&monitors),
            "its monitor was unplugged"
        );
        assert!(
            !at(100, -40).visible_on(&monitors),
            "its title bar is above the screen"
        );
    }

    #[test]
    fn dropped_paths_reach_the_page_as_json() {
        let paths = [
            std::path::PathBuf::from(r#"C:\shots\a "b".png"#),
            std::path::PathBuf::from("/tmp/c.png"),
        ];
        assert_eq!(
            super::drop_payload(&paths),
            r#"{"paths":["C:\\shots\\a \"b\".png","/tmp/c.png"]}"#
        );
    }
}
