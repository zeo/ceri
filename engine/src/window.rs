use crate::bridge::Waker;
use crate::css::ColorScheme;
use crate::host::{
    EngineWorker, EngineWorkerEvent, Frame, InputEvent, KeyModifiers, PointerButton, RegionPress,
};
use crate::layout::Size;
use crate::navigation::{APP_ORIGIN, Browser, UserPreferences};
use softbuffer::{Context, Surface};
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, EventLoopProxy, OwnedDisplayHandle};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};
use winit::window::{CursorIcon, Theme, Window, WindowId};

type WindowSurface = Surface<OwnedDisplayHandle, Rc<Window>>;

// where a window's frames go: a composition swapchain, or a software surface where one cannot
// be made
enum Output {
    #[cfg(target_os = "windows")]
    Composition(crate::presenter::Presenter),
    Software(WindowSurface),
}

impl Output {
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
            .map(Self::Software)
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
                frame.canvas.width(),
                frame.canvas.height(),
                (width.get(), height.get()),
            ),
            Self::Software(surface) => {
                surface
                    .resize(width, height)
                    .map_err(|error| format!("window surface resize failed: {error}"))?;
                let mut buffer = surface
                    .buffer_mut()
                    .map_err(|error| format!("window surface buffer failed: {error}"))?;
                present_frame(
                    frame,
                    width.get() as usize,
                    height.get() as usize,
                    &mut buffer,
                );
                buffer
                    .present()
                    .map_err(|error| format!("window surface present failed: {error}"))
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct WindowConfig {
    // names the window to commands, capabilities and events; unique among open windows
    pub label: String,
    pub title: String,
    pub width: u32,
    pub height: u32,
    pub minimum_width: u32,
    pub minimum_height: u32,
    pub start_url: String,
    // square RGBA pixels for the window and taskbar icon
    pub icon: Option<(Vec<u8>, u32)>,
    // where the window position and size are remembered between launches
    pub geometry_path: Option<PathBuf>,
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
            start_url: format!("{APP_ORIGIN}/"),
            icon: None,
            geometry_path: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WindowGeometry {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

impl WindowGeometry {
    fn read(path: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        let mut parts = text.split_whitespace();
        let mut next = || parts.next()?.parse::<i64>().ok();
        let (x, y, width, height) = (next()?, next()?, next()?, next()?);
        if !(64..=32_768).contains(&width) || !(64..=32_768).contains(&height) {
            return None;
        }
        Some(Self {
            x: x.clamp(-32_768, 32_768) as i32,
            y: y.clamp(-32_768, 32_768) as i32,
            width: width as u32,
            height: height as u32,
        })
    }

    fn write(&self, path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(
            path,
            format!("{} {} {} {}", self.x, self.y, self.width, self.height),
        );
    }
}

// opens and closes windows and passes events between them, from any thread
#[derive(Clone)]
pub struct WindowControl {
    proxy: EventLoopProxy<Request>,
}

enum Request {
    // a worker has events to poll
    Wake,
    Open(Box<WindowConfig>),
    Close(String),
    // an event for every window's listeners, or for one window's
    Emit {
        target: Option<String>,
        name: String,
        payload: String,
    },
}

impl WindowControl {
    pub fn open(&self, config: WindowConfig) {
        let _ = self.proxy.send_event(Request::Open(Box::new(config)));
    }

    pub fn close(&self, label: &str) {
        let _ = self.proxy.send_event(Request::Close(label.to_string()));
    }

    pub fn emit(&self, target: Option<&str>, name: &str, payload: &str) {
        let _ = self.proxy.send_event(Request::Emit {
            target: target.map(str::to_string),
            name: name.to_string(),
            payload: payload.to_string(),
        });
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

// runs windows until the last one closes; the factory builds each window's browser on that
// window's own engine thread
pub fn run<F>(windows: Vec<WindowConfig>, factory: F) -> Result<(), String>
where
    F: Fn(&WindowContext) -> Result<Browser, String> + Send + Clone + 'static,
{
    if windows.is_empty() {
        return Err("an app needs at least one window".to_string());
    }
    let event_loop = EventLoop::<Request>::with_user_event()
        .build()
        .map_err(|error| error.to_string())?;
    let control = WindowControl {
        proxy: event_loop.create_proxy(),
    };
    let context =
        Context::new(event_loop.owned_display_handle()).map_err(|error| error.to_string())?;
    let mut app = App {
        context,
        factory,
        control,
        startup: windows,
        windows: HashMap::new(),
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
    startup: Vec<WindowConfig>,
    windows: HashMap<WindowId, AppWindow>,
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
    applied_cursor: Option<crate::css::Cursor>,
    viewport: Size,
    scale: f32,
    window: Option<Rc<Window>>,
    output: Option<Output>,
    cursor: (f32, f32),
    modifiers: ModifiersState,
    preferences: UserPreferences,
    closing: bool,
}

impl<F> App<F>
where
    F: Fn(&WindowContext) -> Result<Browser, String> + Send + Clone + 'static,
{
    fn open(&mut self, event_loop: &ActiveEventLoop, config: WindowConfig) {
        if self
            .windows
            .values()
            .any(|window| window.config.label == config.label)
        {
            eprintln!("ceri: a window labelled {} is already open", config.label);
            return;
        }
        let viewport = Size {
            width: config.width.max(1) as f32,
            height: config.height.max(1) as f32,
        };
        let factory = self.factory.clone();
        let label = config.label.clone();
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
                    if let Some(reason) = recovery {
                        browser.show_recovery(500, &reason);
                    }
                    Ok(browser)
                },
                notify.clone(),
            )
        });
        let worker = match spawn(viewport, None).and_then(|mut worker| {
            worker.navigate(config.start_url.clone())?;
            Ok(worker)
        }) {
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
            applied_cursor: None,
            viewport,
            scale: 1.0,
            window: None,
            output: None,
            cursor: (0.0, 0.0),
            modifiers: ModifiersState::empty(),
            preferences: UserPreferences::default(),
            closing: false,
        };
        let Some(id) = this.create(event_loop, &self.context) else {
            this.worker.shutdown();
            return;
        };
        self.windows.insert(id, this);
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
            }
        }
        if self.windows.is_empty() && self.startup.is_empty() {
            event_loop.exit();
        }
    }
}

impl AppWindow {
    fn create(
        &mut self,
        event_loop: &ActiveEventLoop,
        context: &Context<OwnedDisplayHandle>,
    ) -> Option<WindowId> {
        let remembered = self
            .config
            .geometry_path
            .as_deref()
            .and_then(WindowGeometry::read);
        let mut attributes = Window::default_attributes()
            .with_title(self.config.title.clone())
            .with_min_inner_size(winit::dpi::LogicalSize::new(
                self.config.minimum_width.max(1),
                self.config.minimum_height.max(1),
            ))
            .with_inner_size(match remembered {
                Some(geometry) => {
                    winit::dpi::PhysicalSize::new(geometry.width, geometry.height).into()
                }
                None => winit::dpi::Size::Logical(winit::dpi::LogicalSize::new(
                    self.config.width as f64,
                    self.config.height as f64,
                )),
            });
        if let Some(geometry) = remembered {
            attributes =
                attributes.with_position(winit::dpi::PhysicalPosition::new(geometry.x, geometry.y));
        }
        if let Some((rgba, size)) = self.config.icon.clone()
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
        let output = match Output::open(context, &window) {
            Ok(output) => output,
            Err(error) => {
                eprintln!("ceri: {error}");
                return None;
            }
        };
        window.set_ime_allowed(true);
        self.applied_title = Some(self.config.title.clone());
        self.window = Some(window);
        self.output = Some(output);
        let scale = self.window_scale();
        if scale != self.scale {
            self.submit(InputEvent::ScaleChanged(scale));
        }
        let scheme = self.window_color_scheme();
        if scheme != self.preferences.color_scheme {
            self.apply_preferences(scheme);
        }
        self.request_redraw();
        self.window.as_ref().map(|window| window.id())
    }

    fn window_event(&mut self, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.remember_geometry();
                self.closing = true;
            }
            WindowEvent::Focused(false) => {
                self.submit(InputEvent::Blur);
            }
            WindowEvent::Resized(size) => {
                let viewport = layout_viewport(size.width, size.height, self.window_scale());
                self.submit(InputEvent::Resize(viewport));
                self.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.submit(InputEvent::ScaleChanged(scale_factor as f32));
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
                let (x, y) = self.cursor;
                let event = match state {
                    ElementState::Pressed => InputEvent::PointerDown { x, y, button },
                    ElementState::Released => InputEvent::PointerUp { x, y, button },
                };
                if !self.submit(event) {
                    return;
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
        if let Some(window) = &self.window {
            window.set_cursor_visible(cursor != crate::css::Cursor::None);
            // the engine's names are the css keywords, which the platform cursor set shares
            let icon = cursor.name().parse::<CursorIcon>().unwrap_or_default();
            window.set_cursor(icon);
        }
        self.applied_cursor = Some(cursor);
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
            RegionPress::ToggleMaximize => window.set_maximized(!window.is_maximized()),
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
            return;
        }
        self.apply_title(&frame);
        let Some(output) = &mut self.output else {
            return;
        };
        if let Err(error) = output.present(&frame, width, height) {
            eprintln!("ceri: {error}");
            self.closing = true;
        }
    }

    fn remember_geometry(&self) {
        let Some(path) = self.config.geometry_path.as_deref() else {
            return;
        };
        let Some(window) = self.window.as_ref() else {
            return;
        };
        let size = window.inner_size();
        let position = window
            .outer_position()
            .unwrap_or(winit::dpi::PhysicalPosition::new(0, 0));
        WindowGeometry {
            x: position.x,
            y: position.y,
            width: size.width,
            height: size.height,
        }
        .write(path);
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
            ..self.preferences
        };
        if self.submit(InputEvent::PreferencesChanged(preferences)) {
            self.preferences = preferences;
        }
    }

    fn window_scale(&self) -> f32 {
        self.window
            .as_ref()
            .map(|window| window.scale_factor() as f32)
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

impl<F> ApplicationHandler<Request> for App<F>
where
    F: Fn(&WindowContext) -> Result<Browser, String> + Send + Clone + 'static,
{
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        for config in std::mem::take(&mut self.startup) {
            self.open(event_loop, config);
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
        if let Some(window) = self.windows.get_mut(&window_id) {
            window.window_event(event);
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
            Request::Open(config) => self.open(event_loop, *config),
            Request::Close(label) => {
                for window in self.windows.values_mut() {
                    if window.config.label == label {
                        window.remember_geometry();
                        window.closing = true;
                    }
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
        }
        self.sweep(event_loop);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        for window in self.windows.values_mut() {
            window.poll_worker();
        }
        self.sweep(event_loop);
        // workers keep their own clocks and wake the loop when they have something to show
        event_loop.set_control_flow(winit::event_loop::ControlFlow::Wait);
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

fn present_frame(frame: &Frame, width: usize, height: usize, buffer: &mut [u32]) {
    let canvas_width = frame.canvas.width() as usize;
    let canvas_height = frame.canvas.height() as usize;
    let pixels = frame.canvas.rgba_pixels();
    let stride = frame.canvas.bytes_per_row();
    let copy_width = width.min(canvas_width);
    let copy_height = height.min(canvas_height);
    for row in 0..copy_height {
        let source = &pixels[row * stride..row * stride + canvas_width * 4];
        let target = &mut buffer[row * width..row * width + copy_width];
        for (pixel, rgba) in target.iter_mut().zip(source.chunks_exact(4)) {
            *pixel = u32::from(rgba[0]) << 16 | u32::from(rgba[1]) << 8 | u32::from(rgba[2]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::layout_viewport;

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
    use super::WindowGeometry;

    #[test]
    fn window_geometry_round_trips_through_a_file() {
        let path = std::env::temp_dir().join(format!("ceri-window-{}", std::process::id()));
        let geometry = WindowGeometry {
            x: 120,
            y: -40,
            width: 1280,
            height: 800,
        };
        geometry.write(&path);
        assert_eq!(WindowGeometry::read(&path), Some(geometry));

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
}
