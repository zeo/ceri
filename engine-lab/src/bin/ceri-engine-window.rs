use ceri_engine_lab::{
    APP_ORIGIN, Browser, EngineHost, InputEvent, PointerButton, ResourceBundle, Size,
};
use softbuffer::{Context, Surface};
use std::num::NonZeroU32;
use std::rc::Rc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, Ime, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, OwnedDisplayHandle};
use winit::keyboard::Key;
use winit::window::{Window, WindowId};

type WindowSurface = Surface<OwnedDisplayHandle, Rc<Window>>;

struct App {
    host: EngineHost,
    context: Context<OwnedDisplayHandle>,
    window: Option<Rc<Window>>,
    surface: Option<WindowSurface>,
    cursor: (f32, f32),
    last_tick: Instant,
}

impl App {
    fn new(context: Context<OwnedDisplayHandle>, host: EngineHost) -> Self {
        Self {
            host,
            context,
            window: None,
            surface: None,
            cursor: (0.0, 0.0),
            last_tick: Instant::now(),
        }
    }

    fn request_redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    fn submit(&mut self, event_loop: &ActiveEventLoop, event: InputEvent) -> bool {
        match self.host.input(event) {
            Ok(result) => {
                if result.frame_requested {
                    self.request_redraw();
                }
                true
            }
            Err(error) => {
                eprintln!("engine input failed: {error}");
                event_loop.exit();
                false
            }
        }
    }

    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        let Some(size) = self.window.as_ref().map(|window| window.inner_size()) else {
            return;
        };
        if size.width == 0 || size.height == 0 {
            return;
        }
        let viewport = Size {
            width: size.width as f32,
            height: size.height as f32,
        };
        if self.host.viewport() != viewport
            && !self.submit(event_loop, InputEvent::Resize(viewport))
        {
            return;
        }
        let Some(surface) = &mut self.surface else {
            return;
        };
        let (Some(width), Some(height)) =
            (NonZeroU32::new(size.width), NonZeroU32::new(size.height))
        else {
            return;
        };
        if let Err(error) = surface.resize(width, height) {
            eprintln!("engine surface resize failed: {error}");
            event_loop.exit();
            return;
        }
        let frame = match self.host.render() {
            Ok(Some(frame)) => frame,
            Ok(None) => return,
            Err(error) => {
                eprintln!("engine render failed: {error}");
                event_loop.exit();
                return;
            }
        };
        let mut buffer = match surface.buffer_mut() {
            Ok(buffer) => buffer,
            Err(error) => {
                eprintln!("engine surface buffer failed: {error}");
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
            eprintln!("engine surface present failed: {error}");
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
            .with_title("Ceri custom engine")
            .with_inner_size(winit::dpi::PhysicalSize::new(800_u32, 520_u32));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Rc::new(window),
            Err(error) => {
                eprintln!("engine window creation failed: {error}");
                event_loop.exit();
                return;
            }
        };
        let surface = match Surface::new(&self.context, window.clone()) {
            Ok(surface) => surface,
            Err(error) => {
                eprintln!("engine surface creation failed: {error}");
                event_loop.exit();
                return;
            }
        };
        self.window = Some(window);
        self.surface = Some(surface);
        if let Some(window) = &self.window {
            window.set_ime_allowed(true);
        }
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
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed =>
            {
                let key = match event.logical_key {
                    Key::Character(character) => character.to_string(),
                    Key::Named(named) => format!("{named:?}"),
                    other => format!("{other:?}"),
                };
                self.submit(event_loop, InputEvent::KeyDown { key });
            }
            WindowEvent::Ime(Ime::Commit(text)) => {
                self.submit(
                    event_loop,
                    InputEvent::TextInput {
                        text: text.to_string(),
                    },
                );
            }
            WindowEvent::RedrawRequested => self.redraw(event_loop),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(self.last_tick);
        self.last_tick = now;
        let milliseconds = elapsed.as_millis().min(u128::from(u64::MAX)) as u64;
        if milliseconds > 0 {
            self.submit(event_loop, InputEvent::AdvanceTime(milliseconds));
        }
        event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
            now + Duration::from_millis(16),
        ));
    }
}

fn main() -> Result<(), String> {
    let mut resources = ResourceBundle::from_directory(env!("CARGO_MANIFEST_DIR"))
        .map_err(|error| format!("could not load engine-lab resources: {error}"))?;
    resources
        .insert(
            "/index.html",
            "text/html; charset=utf-8",
            include_bytes!("../../demo.html").to_vec(),
        )
        .map_err(|error| error.to_string())?;
    let mut host = EngineHost::new(
        Browser::new(resources),
        Size {
            width: 800.0,
            height: 520.0,
        },
    );
    host.navigate(&format!("{APP_ORIGIN}/"))
        .map_err(|error| error.to_string())?;

    let event_loop = EventLoop::new().map_err(|error| error.to_string())?;
    let context =
        Context::new(event_loop.owned_display_handle()).map_err(|error| error.to_string())?;
    let mut app = App::new(context, host);
    event_loop
        .run_app(&mut app)
        .map_err(|error| error.to_string())
}
