use ceri_engine_lab::{
    APP_ORIGIN, Browser, EngineHost, EngineWorker, EngineWorkerEvent, Frame, InputEvent,
    PointerButton, ResourceBundle, Size,
};
#[cfg(feature = "native-http")]
use ceri_engine_lab::NativeHttpProvider;
use softbuffer::{Context, Surface};
use std::env;
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
    backend: WindowBackend,
    context: Context<OwnedDisplayHandle>,
    window: Option<Rc<Window>>,
    surface: Option<WindowSurface>,
    cursor: (f32, f32),
    last_tick: Instant,
}

#[cfg_attr(not(feature = "native-http"), allow(dead_code))]
enum WindowBackend {
    Direct(Box<EngineHost>),
    Worker(WorkerBackend),
}

struct WorkerBackend {
    worker: EngineWorker,
    navigation_pending: bool,
    time_request: Option<u64>,
    render_request: Option<u64>,
    latest_frame: Option<Frame>,
    viewport: Size,
}

impl App {
    fn new(context: Context<OwnedDisplayHandle>, backend: WindowBackend) -> Self {
        Self {
            backend,
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
        let is_clock_tick = matches!(event, InputEvent::AdvanceTime(_));
        let resize = match &event {
            InputEvent::Resize(viewport) => Some(*viewport),
            _ => None,
        };
        let mut request_redraw = false;
        let accepted = match &mut self.backend {
            WindowBackend::Direct(host) => match host.input(event) {
                Ok(result) => {
                    request_redraw = result.frame_requested;
                    true
                }
                Err(error) => {
                    eprintln!("engine input failed: {error}");
                    event_loop.exit();
                    false
                }
            },
            WindowBackend::Worker(worker) => {
                if is_clock_tick && worker.time_request.is_some() {
                    return true;
                }
                match worker.worker.input(event) {
                    Ok(request_id) => {
                        if is_clock_tick {
                            worker.time_request = Some(request_id);
                        }
                        if let Some(viewport) = resize {
                            worker.viewport = viewport;
                        }
                        request_redraw = true;
                        true
                    }
                    Err(error) if error == "engine worker queue is full" => {
                        eprintln!("engine input dropped: {error}");
                        true
                    }
                    Err(error) => {
                        eprintln!("engine input failed: {error}");
                        event_loop.exit();
                        false
                    }
                }
            }
        };
        if request_redraw {
            self.request_redraw();
        }
        accepted
    }

    fn poll_worker(&mut self, event_loop: &ActiveEventLoop) -> bool {
        let mut request_redraw = false;
        let mut failed = false;
        if let WindowBackend::Worker(worker) = &mut self.backend {
            for event in worker.worker.poll() {
                match event {
                    EngineWorkerEvent::Navigated { result, .. } => {
                        worker.navigation_pending = false;
                        if let Err(error) = result {
                            eprintln!("engine navigation failed: {error}");
                            failed = true;
                        } else {
                            request_redraw = true;
                        }
                    }
                    EngineWorkerEvent::Input {
                        request_id,
                        result,
                    } => {
                        if worker.time_request == Some(request_id) {
                            worker.time_request = None;
                        }
                        if let Err(error) = result {
                            eprintln!("engine input failed: {error}");
                            failed = true;
                        } else {
                            request_redraw = true;
                        }
                    }
                    EngineWorkerEvent::Frame {
                        request_id,
                        result,
                    } => {
                        if worker.render_request == Some(request_id) {
                            worker.render_request = None;
                        }
                        match result {
                            Ok(frame) => {
                                worker.latest_frame = frame;
                                request_redraw = worker.latest_frame.is_some();
                            }
                            Err(error) => {
                                eprintln!("engine render failed: {error}");
                                failed = true;
                            }
                        }
                    }
                    EngineWorkerEvent::Accessibility { .. }
                    | EngineWorkerEvent::NavigationFailure { .. } => {}
                }
            }
        }
        if failed {
            event_loop.exit();
            return false;
        }
        if request_redraw {
            self.request_redraw();
        }
        true
    }

    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        if !self.poll_worker(event_loop) {
            return;
        }
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
        match &self.backend {
            WindowBackend::Direct(host) if host.viewport() != viewport => {
                if !self.submit(event_loop, InputEvent::Resize(viewport)) {
                    return;
                }
            }
            WindowBackend::Worker(worker) if worker.viewport != viewport => {
                if !self.submit(event_loop, InputEvent::Resize(viewport)) {
                    return;
                }
            }
            _ => {}
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
        let frame = match &mut self.backend {
            WindowBackend::Direct(host) => match host.render() {
                Ok(Some(frame)) => frame,
                Ok(None) => return,
                Err(error) => {
                    eprintln!("engine render failed: {error}");
                    event_loop.exit();
                    return;
                }
            },
            WindowBackend::Worker(worker) => {
                if worker.navigation_pending {
                    return;
                }
                if worker.render_request.is_none() {
                    match worker.worker.render() {
                        Ok(request_id) => worker.render_request = Some(request_id),
                        Err(error) => {
                            eprintln!("engine render queue failed: {error}");
                            event_loop.exit();
                            return;
                        }
                    }
                }
                let Some(frame) = worker.latest_frame.take() else {
                    return;
                };
                if frame.viewport != viewport {
                    return;
                }
                frame
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
        if !self.poll_worker(event_loop) {
            return;
        }
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(self.last_tick);
        self.last_tick = now;
        let milliseconds = elapsed.as_millis().min(u128::from(u64::MAX)) as u64;
        let can_tick = match &self.backend {
            WindowBackend::Direct(_) => true,
            WindowBackend::Worker(worker) => {
                !worker.navigation_pending && worker.time_request.is_none()
            }
        };
        if milliseconds > 0 && can_tick {
            self.submit(event_loop, InputEvent::AdvanceTime(milliseconds));
        }
        event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
            now + Duration::from_millis(16),
        ));
    }
}

fn main() -> Result<(), String> {
    let native_base = parse_native_base(env::args().skip(1))?;
    let viewport = Size {
        width: 800.0,
        height: 520.0,
    };
    let backend = if let Some(base) = native_base {
        #[cfg(feature = "native-http")]
        {
            let mut worker = EngineWorker::spawn(viewport, move || {
                let provider =
                    NativeHttpProvider::new(base).map_err(|error| error.to_string())?;
                Ok(Browser::with_resource_provider(
                    ResourceBundle::default(),
                    Rc::new(provider),
                ))
            })?;
            worker.navigate(format!("{APP_ORIGIN}/"))?;
            WindowBackend::Worker(WorkerBackend {
                worker,
                navigation_pending: true,
                time_request: None,
                render_request: None,
                latest_frame: None,
                viewport,
            })
        }
        #[cfg(not(feature = "native-http"))]
        {
            let _ = base;
            return Err(
                "--native-base requires the native-http feature: cargo run --features \"windowed native-http\""
                    .to_string(),
            );
        }
    } else {
        let mut resources = ResourceBundle::from_directory(env!("CARGO_MANIFEST_DIR"))
            .map_err(|error| format!("could not load engine-lab resources: {error}"))?;
        resources
            .insert(
                "/index.html",
                "text/html; charset=utf-8",
                include_bytes!("../../demo.html").to_vec(),
            )
            .map_err(|error| error.to_string())?;
        let mut host = EngineHost::new(Browser::new(resources), viewport);
        host.navigate(&format!("{APP_ORIGIN}/"))
            .map_err(|error| error.to_string())?;
        WindowBackend::Direct(Box::new(host))
    };

    let event_loop = EventLoop::new().map_err(|error| error.to_string())?;
    let context =
        Context::new(event_loop.owned_display_handle()).map_err(|error| error.to_string())?;
    let mut app = App::new(context, backend);
    event_loop
        .run_app(&mut app)
        .map_err(|error| error.to_string())
}

fn parse_native_base<I>(arguments: I) -> Result<Option<String>, String>
where
    I: IntoIterator<Item = String>,
{
    let mut arguments = arguments.into_iter();
    let mut native_base = None;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--native-base" => {
                if native_base.is_some() {
                    return Err("--native-base may only be provided once".to_string());
                }
                native_base = Some(
                    arguments
                        .next()
                        .ok_or_else(|| "--native-base requires an HTTPS URL".to_string())?,
                );
            }
            "--help" | "-h" => {
                println!(
                    "usage: ceri-engine-window [--native-base HTTPS_URL]\n\nWithout --native-base, the packaged engine-lab demo is shown. With it, EngineWorker loads the app origin through the optional native HTTPS provider."
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(native_base)
}

#[cfg(test)]
mod tests {
    use super::parse_native_base;

    #[test]
    fn parses_optional_native_base() {
        assert_eq!(
            parse_native_base(vec!["--native-base".to_string(), "https://example.test".to_string()])
                .expect("native base should parse")
                .as_deref(),
            Some("https://example.test")
        );
    }

    #[test]
    fn rejects_unknown_window_arguments() {
        assert!(parse_native_base(vec!["--unknown".to_string()]).is_err());
    }
}
