use crate::bridge::Waker;
use crate::css::ColorScheme;
use crate::host::{
    EngineWorker, EngineWorkerEvent, Frame, InputEvent, KeyModifiers, PointerButton,
};
use crate::layout::Size;
use crate::navigation::{APP_ORIGIN, Browser, UserPreferences};
use softbuffer::{Context, Surface};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, OwnedDisplayHandle};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};
use winit::window::{CursorIcon, Theme, Window, WindowId};

type WindowSurface = Surface<OwnedDisplayHandle, Rc<Window>>;

#[derive(Clone, Debug)]
pub struct WindowConfig {
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

pub fn run<F>(config: WindowConfig, factory: F) -> Result<(), String>
where
    F: Fn() -> Result<Browser, String> + Send + Clone + 'static,
{
    let viewport = Size {
        width: config.width.max(1) as f32,
        height: config.height.max(1) as f32,
    };
    let event_loop = EventLoop::new().map_err(|error| error.to_string())?;
    let proxy = event_loop.create_proxy();
    // a worker with something to show wakes the loop, which otherwise sleeps
    let notify: Waker = Arc::new(move || {
        let _ = proxy.send_event(());
    });
    let mut worker = EngineWorker::spawn_live(viewport, factory.clone(), notify.clone())?;
    worker.navigate(config.start_url.clone())?;
    let context =
        Context::new(event_loop.owned_display_handle()).map_err(|error| error.to_string())?;
    let mut app = App {
        config,
        context,
        factory,
        notify,
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
        surface: None,
        cursor: (0.0, 0.0),
        modifiers: ModifiersState::empty(),
        preferences: UserPreferences::default(),
    };
    event_loop
        .run_app(&mut app)
        .map_err(|error| error.to_string())
}

struct App<F> {
    config: WindowConfig,
    context: Context<OwnedDisplayHandle>,
    factory: F,
    notify: Waker,
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
    surface: Option<WindowSurface>,
    cursor: (f32, f32),
    modifiers: ModifiersState,
    preferences: UserPreferences,
}

impl<F> App<F>
where
    F: Fn() -> Result<Browser, String> + Send + Clone + 'static,
{
    // one restart, then the engine's own recovery document
    fn recover(&mut self, event_loop: &ActiveEventLoop, detail: &str) -> bool {
        self.restarts = self.restarts.saturating_add(1);
        if self.restarts > 2 {
            eprintln!("ceri: the engine stopped again, closing the window");
            event_loop.exit();
            return false;
        }
        let recovering = self.restarts > 1;
        let factory = self.factory.clone();
        let detail = detail.to_string();
        let reason = detail.clone();
        let spawned = EngineWorker::spawn_live(
            self.viewport,
            move || {
                let mut browser = factory()?;
                if recovering {
                    browser.show_recovery(500, &reason);
                }
                Ok(browser)
            },
            self.notify.clone(),
        );
        match spawned {
            Ok(mut worker) => {
                if !recovering && worker.navigate(self.config.start_url.clone()).is_err() {
                    eprintln!("ceri: the restarted engine refused the app URL");
                    event_loop.exit();
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
                event_loop.exit();
                false
            }
        }
    }

    fn request_redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn submit(&mut self, event_loop: &ActiveEventLoop, event: InputEvent) -> bool {
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
            Err(error) => self.recover(event_loop, &error),
        }
    }

    fn poll_worker(&mut self, event_loop: &ActiveEventLoop) -> bool {
        let mut request_redraw = false;
        for event in self.worker.poll() {
            match event {
                EngineWorkerEvent::Navigated { result, .. } => {
                    self.navigation_pending = false;
                    match result {
                        Ok(_) => request_redraw = true,
                        Err(error) => return self.recover(event_loop, &error.to_string()),
                    }
                }
                EngineWorkerEvent::Input { result, .. } => match result {
                    Ok(result) => {
                        if let Some(cursor) = result.cursor {
                            self.apply_cursor(cursor);
                        }
                        request_redraw = true;
                    }
                    Err(error) => return self.recover(event_loop, &error.to_string()),
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
                        Err(error) => return self.recover(event_loop, &error.to_string()),
                    }
                }
                EngineWorkerEvent::Invalidated => request_redraw = true,
                EngineWorkerEvent::Failed(error) => {
                    return self.recover(event_loop, &error.to_string());
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

    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        if !self.poll_worker(event_loop) {
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
        if self.scale != scale && !self.submit(event_loop, InputEvent::ScaleChanged(scale)) {
            return;
        }
        let viewport = layout_viewport(size.width, size.height, scale);
        if self.viewport != viewport && !self.submit(event_loop, InputEvent::Resize(viewport)) {
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
                    self.recover(event_loop, &error);
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
        let Some(surface) = &mut self.surface else {
            return;
        };
        if let Err(error) = surface.resize(width, height) {
            eprintln!("ceri: window surface resize failed: {error}");
            event_loop.exit();
            return;
        }
        let mut buffer = match surface.buffer_mut() {
            Ok(buffer) => buffer,
            Err(error) => {
                eprintln!("ceri: window surface buffer failed: {error}");
                event_loop.exit();
                return;
            }
        };
        present_frame(
            &frame,
            size.width as usize,
            size.height as usize,
            &mut buffer,
        );
        if let Err(error) = buffer.present() {
            eprintln!("ceri: window surface present failed: {error}");
            event_loop.exit();
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

    fn apply_preferences(&mut self, event_loop: &ActiveEventLoop, scheme: ColorScheme) {
        let preferences = UserPreferences {
            color_scheme: scheme,
            ..self.preferences
        };
        if self.submit(event_loop, InputEvent::PreferencesChanged(preferences)) {
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

impl<F> ApplicationHandler for App<F>
where
    F: Fn() -> Result<Browser, String> + Send + Clone + 'static,
{
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            self.request_redraw();
            return;
        }
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
                event_loop.exit();
                return;
            }
        };
        let surface = match Surface::new(&self.context, window.clone()) {
            Ok(surface) => surface,
            Err(error) => {
                eprintln!("ceri: window surface creation failed: {error}");
                event_loop.exit();
                return;
            }
        };
        window.set_ime_allowed(true);
        self.applied_title = Some(self.config.title.clone());
        self.window = Some(window);
        self.surface = Some(surface);
        let scale = self.window_scale();
        if scale != self.scale {
            self.submit(event_loop, InputEvent::ScaleChanged(scale));
        }
        let scheme = self.window_color_scheme();
        if scheme != self.preferences.color_scheme {
            self.apply_preferences(event_loop, scheme);
        }
        self.request_redraw();
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if self.window.as_ref().map(|window| window.id()) != Some(window_id) {
            return;
        }
        match event {
            WindowEvent::CloseRequested => {
                self.remember_geometry();
                event_loop.exit();
            }
            WindowEvent::Focused(false) => {
                self.submit(event_loop, InputEvent::Blur);
            }
            WindowEvent::Resized(size) => {
                let viewport = layout_viewport(size.width, size.height, self.window_scale());
                self.submit(event_loop, InputEvent::Resize(viewport));
                self.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.submit(event_loop, InputEvent::ScaleChanged(scale_factor as f32));
                self.request_redraw();
            }
            WindowEvent::ThemeChanged(theme) => {
                let scheme = match theme {
                    Theme::Dark => ColorScheme::Dark,
                    Theme::Light => ColorScheme::Light,
                };
                self.apply_preferences(event_loop, scheme);
                self.request_redraw();
            }
            WindowEvent::CursorMoved { position, .. } => {
                let scale = f64::from(self.scale);
                self.cursor = ((position.x / scale) as f32, (position.y / scale) as f32);
                self.submit(
                    event_loop,
                    InputEvent::PointerMove {
                        x: self.cursor.0,
                        y: self.cursor.1,
                    },
                );
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
                self.submit(
                    event_loop,
                    InputEvent::Scroll {
                        x,
                        y,
                        delta_x,
                        delta_y,
                    },
                );
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
                if !self.submit(event_loop, event) {
                    return;
                }
                if state == ElementState::Released && button == PointerButton::Primary {
                    self.submit(event_loop, InputEvent::Click { x, y });
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
                self.submit(
                    event_loop,
                    InputEvent::Key {
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
                    },
                );
            }
            WindowEvent::Ime(Ime::Preedit(text, cursor)) => {
                self.submit(
                    event_loop,
                    InputEvent::CompositionUpdate {
                        text: text.to_string(),
                        cursor,
                    },
                );
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                self.submit(
                    event_loop,
                    InputEvent::CompositionCommit {
                        text: text.to_string(),
                    },
                );
            }
            WindowEvent::Ime(Ime::Disabled) => {
                self.submit(event_loop, InputEvent::CompositionCancel);
            }
            WindowEvent::RedrawRequested => self.redraw(event_loop),
            _ => {}
        }
    }

    // a worker woke the loop
    fn user_event(&mut self, event_loop: &ActiveEventLoop, _event: ()) {
        self.poll_worker(event_loop);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if !self.poll_worker(event_loop) {
            return;
        }
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
