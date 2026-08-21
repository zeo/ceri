use crate::host::{
    EngineWorker, EngineWorkerEvent, Frame, InputEvent, KeyModifiers, PointerButton,
};
use crate::layout::Size;
use crate::navigation::{APP_ORIGIN, Browser};
use softbuffer::{Context, Surface};
use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, Ime, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, OwnedDisplayHandle};
use winit::keyboard::{Key, ModifiersState};
use winit::window::{Window, WindowId};

type WindowSurface = Surface<OwnedDisplayHandle, Rc<Window>>;

#[derive(Clone, Debug)]
pub struct WindowConfig {
    pub title: String,
    pub width: u32,
    pub height: u32,
    pub start_url: String,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            title: "Ceri".to_string(),
            width: 1120,
            height: 720,
            start_url: format!("{APP_ORIGIN}/"),
        }
    }
}

pub fn run<F>(config: WindowConfig, factory: F) -> Result<(), String>
where
    F: FnOnce() -> Result<Browser, String> + Send + 'static,
{
    let viewport = Size {
        width: config.width.max(1) as f32,
        height: config.height.max(1) as f32,
    };
    let mut worker = EngineWorker::spawn(viewport, factory)?;
    worker.navigate(config.start_url.clone())?;
    let event_loop = EventLoop::new().map_err(|error| error.to_string())?;
    let context =
        Context::new(event_loop.owned_display_handle()).map_err(|error| error.to_string())?;
    let mut app = App {
        config,
        context,
        worker,
        navigation_pending: true,
        time_request: None,
        render_request: None,
        latest_frame: None,
        applied_title: None,
        viewport,
        window: None,
        surface: None,
        cursor: (0.0, 0.0),
        modifiers: ModifiersState::empty(),
        last_tick: Instant::now(),
    };
    event_loop
        .run_app(&mut app)
        .map_err(|error| error.to_string())
}

struct App {
    config: WindowConfig,
    context: Context<OwnedDisplayHandle>,
    worker: EngineWorker,
    navigation_pending: bool,
    time_request: Option<u64>,
    render_request: Option<u64>,
    latest_frame: Option<Frame>,
    applied_title: Option<String>,
    viewport: Size,
    window: Option<Rc<Window>>,
    surface: Option<WindowSurface>,
    cursor: (f32, f32),
    modifiers: ModifiersState,
    last_tick: Instant,
}

impl App {
    fn request_redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn submit(&mut self, event_loop: &ActiveEventLoop, event: InputEvent) -> bool {
        let is_clock_tick = matches!(event, InputEvent::AdvanceTime(_));
        if is_clock_tick && self.time_request.is_some() {
            return true;
        }
        let resize = match &event {
            InputEvent::Resize(viewport) => Some(*viewport),
            _ => None,
        };
        match self.worker.input(event) {
            Ok(request_id) => {
                if is_clock_tick {
                    self.time_request = Some(request_id);
                }
                if let Some(viewport) = resize {
                    self.viewport = viewport;
                }
                self.request_redraw();
                true
            }
            Err(error) if error == "engine worker queue is full" => true,
            Err(error) => {
                eprintln!("ceri: engine input failed: {error}");
                event_loop.exit();
                false
            }
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
                        Err(error) => {
                            eprintln!("ceri: navigation failed: {error}");
                            event_loop.exit();
                            return false;
                        }
                    }
                }
                EngineWorkerEvent::Input { request_id, result } => {
                    if self.time_request == Some(request_id) {
                        self.time_request = None;
                    }
                    match result {
                        Ok(_) => request_redraw = true,
                        Err(error) => {
                            eprintln!("ceri: engine input failed: {error}");
                            event_loop.exit();
                            return false;
                        }
                    }
                }
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
                        Err(error) => {
                            eprintln!("ceri: engine render failed: {error}");
                            event_loop.exit();
                            return false;
                        }
                    }
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
        let viewport = Size {
            width: size.width as f32,
            height: size.height as f32,
        };
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
                    eprintln!("ceri: engine render request failed: {error}");
                    event_loop.exit();
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
        for (pixel, rgba) in buffer
            .iter_mut()
            .zip(frame.canvas.rgba_pixels().chunks_exact(4))
        {
            *pixel = u32::from(rgba[0]) << 16 | u32::from(rgba[1]) << 8 | u32::from(rgba[2]);
        }
        if let Err(error) = buffer.present() {
            eprintln!("ceri: window surface present failed: {error}");
            event_loop.exit();
        }
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

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            self.request_redraw();
            return;
        }
        let attributes = Window::default_attributes()
            .with_title(self.config.title.clone())
            .with_inner_size(winit::dpi::PhysicalSize::new(
                self.config.width,
                self.config.height,
            ));
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
        self.last_tick = Instant::now();
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
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Focused(false) => {
                self.submit(event_loop, InputEvent::Blur);
            }
            WindowEvent::Resized(size) => {
                self.submit(
                    event_loop,
                    InputEvent::Resize(Size {
                        width: size.width as f32,
                        height: size.height as f32,
                    }),
                );
                self.request_redraw();
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x as f32, position.y as f32);
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
                        (position.x as f32, -position.y as f32)
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
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                let key = match event.logical_key {
                    Key::Character(character) => character.to_string(),
                    Key::Named(named) => format!("{named:?}"),
                    other => format!("{other:?}"),
                };
                self.submit(
                    event_loop,
                    InputEvent::KeyDownWithModifiers {
                        key,
                        modifiers: KeyModifiers {
                            shift: self.modifiers.shift_key(),
                            control: self.modifiers.control_key(),
                            alt: self.modifiers.alt_key(),
                            meta: self.modifiers.super_key(),
                        },
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

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if !self.poll_worker(event_loop) {
            return;
        }
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(self.last_tick);
        self.last_tick = now;
        let milliseconds = elapsed.as_millis().min(u128::from(u64::MAX)) as u64;
        if milliseconds > 0 && !self.navigation_pending {
            self.submit(event_loop, InputEvent::AdvanceTime(milliseconds));
        }
        event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
            now + Duration::from_millis(16),
        ));
    }
}
