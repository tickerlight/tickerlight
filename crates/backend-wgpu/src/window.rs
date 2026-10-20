use std::cell::RefCell;
use std::error::Error;
use std::rc::Rc;
use std::sync::Arc;

use tickerlight_core::touch::{Phase as TouchPhase, Touches};
use tickerlight_core::{Event, Key, PointerShape, Scene, Surface};
use web_time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key as WinitKey, KeyCode, NamedKey, PhysicalKey};
use winit::window::{CursorIcon, Window, WindowId};

use crate::Renderer;

/// Two presses this close in time and place are a double click.
const DOUBLE_CLICK: Duration = Duration::from_millis(500);
const DOUBLE_CLICK_PIXELS: f32 = 4.0;

/// Opens a window (or, in a browser, a canvas in the page) with the chart and runs until it is closed.
///
/// On the desktop this blocks until the window is closed. In a browser it returns at once and the
/// page keeps the chart running.
///
/// Everything here is translation: winit events become chart [`Event`]s, and the chart's
/// scene is drawn by [`Renderer`]. All logic lives in `tickerlight-core`, where it is tested
/// without a window.
pub fn run(surface: impl Surface + 'static) -> Result<(), Box<dyn Error>> {
    run_app(Box::new(surface), None)
}

/// Like [`run`], in a browser, but draws into a canvas that the page already has instead of adding
/// one. The canvas is sized by the page (CSS), and the chart follows it.
#[cfg(target_arch = "wasm32")]
pub fn run_on_canvas(
    surface: impl Surface + 'static,
    canvas: web_sys::HtmlCanvasElement,
) -> Result<(), Box<dyn Error>> {
    run_app(Box::new(surface), Some(canvas))
}

#[cfg(target_arch = "wasm32")]
type Canvas = Option<web_sys::HtmlCanvasElement>;
#[cfg(not(target_arch = "wasm32"))]
type Canvas = Option<std::convert::Infallible>;

fn run_app(surface: Box<dyn Surface>, canvas: Canvas) -> Result<(), Box<dyn Error>> {
    let event_loop = EventLoop::<Ready>::with_user_event().build()?;
    #[allow(unused_mut)]
    let mut app = App {
        surface,
        scene: Scene::new(),
        cursor: (0.0, 0.0),
        last_press: None,
        double: false,
        moved: None,
        shortcut: false,
        command: false,
        alt: false,
        shift: false,
        clipboard: Clipboard::default(),
        title: String::new(),
        shape: PointerShape::Default,
        touches: Touches::new(),
        last_tick: None,
        gfx: None,
        pending: Rc::new(RefCell::new(None)),
        proxy: event_loop.create_proxy(),
        error: None,
        canvas,
        frames: FrameLog::from_env(),
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        event_loop.run_app(&mut app)?;
        match app.error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        use winit::platform::web::EventLoopExtWebSys;
        event_loop.spawn_app(app);
        Ok(())
    }
}

/// Sent to the event loop when the GPU has been set up in the background (browsers cannot block on it).
struct Ready;

type Pending = Rc<RefCell<Option<Result<Gfx, Box<dyn Error>>>>>;

/// With `TICKERLIGHT_FRAMES` set, a line a second on stderr: frames drawn, the slowest, and the longest wait from
/// an input to the frame that showed it.
struct FrameLog {
    since: Instant,
    frames: u32,
    slowest: Duration,
    /// When the first input not yet shown arrived.
    input: Option<Instant>,
    lag: Duration,
}

impl FrameLog {
    fn from_env() -> Option<Self> {
        std::env::var_os("TICKERLIGHT_FRAMES").map(|_| Self {
            since: Instant::now(),
            frames: 0,
            slowest: Duration::ZERO,
            input: None,
            lag: Duration::ZERO,
        })
    }

    fn drawn(&mut self, started: Instant) {
        let now = Instant::now();
        self.frames += 1;
        self.slowest = self.slowest.max(now - started);
        if let Some(input) = self.input.take() {
            self.lag = self.lag.max(now - input);
        }
        if now - self.since >= Duration::from_secs(1) {
            eprintln!(
                "{} frames, slowest {:.1} ms, input shown after at most {:.1} ms",
                self.frames,
                self.slowest.as_secs_f64() * 1000.0,
                self.lag.as_secs_f64() * 1000.0
            );
            *self = Self {
                since: now,
                frames: 0,
                slowest: Duration::ZERO,
                input: None,
                lag: Duration::ZERO,
            };
        }
    }
}

struct App {
    surface: Box<dyn Surface>,
    scene: Scene,
    /// Where the pointer is, as the window last told.
    cursor: (f32, f32),
    /// When and where the left button last went down, to tell a double click.
    last_press: Option<(Instant, (f32, f32))>,
    /// The press that went down was the second of a double click: its release sends `Event::DoubleClick`.
    double: bool,
    /// The pointer's latest position not yet given to the surface: a mouse reports many moves a frame, and only
    /// the last one shows.
    moved: Option<(f32, f32)>,
    /// Ctrl, Alt or the system key held: letters are then not typed.
    shortcut: bool,
    /// Ctrl or the system key (Cmd) held: with A, C, X and V they edit text.
    command: bool,
    /// Alt (Option) held: with a letter it is a shortcut.
    alt: bool,
    shift: bool,
    clipboard: Clipboard,
    /// The window's title as last set.
    title: String,
    /// The pointer's shape as last set.
    shape: PointerShape,
    touches: Touches,
    /// When the last `Tick` was sent; `None` while nothing animates.
    last_tick: Option<Instant>,
    gfx: Option<Gfx>,
    /// The GPU setup that finished in the background, waiting to be picked up.
    pending: Pending,
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    proxy: EventLoopProxy<Ready>,
    error: Option<Box<dyn Error>>,
    /// The page's canvas to draw into, if it gave one (browser only).
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    canvas: Canvas,
    frames: Option<FrameLog>,
}

struct Gfx {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: Renderer,
}

/// The graphics APIs to try, in turn. `WGPU_BACKEND` (`vulkan`, `gl`, `dx12`, `metal`) picks one.
///
/// On Linux OpenGL comes first: it draws on the GPU the screen is on, and the chart needs no more than it gives.
/// Vulkan opens every GPU to list them, which on a laptop wakes a sleeping discrete one (seconds), and may then
/// draw on it with a driver that cannot yet (NVK on older NVIDIA chips: a minute to start, then a lost device).
fn backend_order() -> Vec<wgpu::Backends> {
    if let Some(chosen) = wgpu::Backends::from_env() {
        return vec![chosen];
    }
    if cfg!(all(
        unix,
        not(target_os = "macos"),
        not(target_os = "ios"),
        not(target_os = "android"),
        not(target_arch = "wasm32")
    )) {
        vec![wgpu::Backends::GL, wgpu::Backends::PRIMARY]
    } else {
        vec![wgpu::Backends::all()]
    }
}

impl Gfx {
    /// Sets up the GPU for `window` with the first graphics API that works. Async because in a browser
    /// requesting the adapter cannot block.
    async fn new(window: Arc<Window>, display: winit::event_loop::OwnedDisplayHandle) -> Result<Self, Box<dyn Error>> {
        let mut failure: Box<dyn Error> = "no graphics API to try".into();
        for backends in backend_order() {
            match Self::with(backends, window.clone(), display.clone()).await {
                Ok(gfx) => return Ok(gfx),
                Err(e) => failure = e,
            }
        }
        Err(failure)
    }

    async fn with(
        backends: wgpu::Backends,
        window: Arc<Window>,
        display: winit::event_loop::OwnedDisplayHandle,
    ) -> Result<Self, Box<dyn Error>> {
        let mut descriptor = wgpu::InstanceDescriptor::new_with_display_handle(Box::new(display));
        descriptor.backends = backends;
        // In a browser this picks WebGPU if the browser can really provide it, else WebGL2.
        let instance = wgpu::util::new_instance_with_webgpu_detection(descriptor).await;
        let surface = instance.create_surface(window.clone())?;
        // A chart is no game: the GPU that keeps the battery is enough.
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await?;
        let info = adapter.get_info();
        eprintln!("drawing with {} ({:?})", info.name, info.backend);
        // The renderer needs very little: ask only for what WebGL2 can give too, so that any GPU will do.
        let required_limits = wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits());
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                required_limits,
                ..Default::default()
            })
            .await?;

        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or("the surface is not supported by the GPU adapter")?;
        // Colors are written as given, so avoid sRGB targets (see `Renderer`).
        if let Some(&format) = surface.get_capabilities(&adapter).formats.iter().find(|f| !f.is_srgb()) {
            config.format = format;
        }
        // Show each frame as soon as it is drawn: one frame queued, not two, so the chart follows the pointer.
        config.desired_maximum_frame_latency = 1;
        if surface
            .get_capabilities(&adapter)
            .present_modes
            .contains(&wgpu::PresentMode::Mailbox)
        {
            config.present_mode = wgpu::PresentMode::Mailbox;
        }
        surface.configure(&device, &config);
        let renderer = Renderer::new(&device, config.format);
        Ok(Self {
            window,
            surface,
            config,
            device,
            queue,
            renderer,
        })
    }

    fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    fn draw(&mut self, scene: &Scene) {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Outdated => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            // Occluded, timed out and the like: skip this frame, the next redraw retries.
            _ => return,
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let size = (self.config.width, self.config.height);
        self.renderer.render(&self.device, &self.queue, &view, size, scene);
        self.window.pre_present_notify();
        self.queue.present(frame);
    }
}

impl App {
    /// Takes over a finished GPU setup: a failure ends the program, success shows the first frame.
    fn finish_setup(&mut self, event_loop: &ActiveEventLoop, result: Result<Gfx, Box<dyn Error>>) {
        match result {
            Ok(gfx) => {
                let size = gfx.window.inner_size();
                self.surface.set_scale(gfx.window.scale_factor() as f32);
                self.surface.resize(size.width as f32, size.height as f32);
                gfx.window.request_redraw();
                self.gfx = Some(gfx);
            }
            Err(e) => {
                // A page has nowhere to return the error to: put it where the developer looks.
                #[cfg(target_arch = "wasm32")]
                web_sys::console::error_1(&format!("chart: could not start the GPU renderer: {e}").into());
                self.error = Some(e);
                event_loop.exit();
            }
        }
    }
}

impl ApplicationHandler<Ready> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gfx.is_some() || self.pending.borrow().is_some() {
            return;
        }
        #[allow(unused_mut)]
        let mut attributes = Window::default_attributes().with_title("Tickerlight");
        #[cfg(not(target_arch = "wasm32"))]
        {
            attributes = attributes.with_inner_size(winit::dpi::LogicalSize::new(1200.0, 700.0));
        }
        #[cfg(target_arch = "wasm32")]
        {
            // The page's CSS decides the size of the canvas; winit follows it.
            use winit::platform::web::WindowAttributesExtWebSys;
            attributes = match self.canvas.take() {
                Some(canvas) => attributes.with_canvas(Some(canvas)),
                None => attributes.with_append(true),
            };
        }
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(e) => {
                self.error = Some(e.into());
                event_loop.exit();
                return;
            }
        };
        #[cfg(target_arch = "wasm32")]
        {
            // Without this the browser takes fingers for scrolling and zooming the page, and cancels the touches.
            use winit::platform::web::WindowExtWebSys;
            if let Some(canvas) = window.canvas() {
                let _ = canvas.style().set_property("touch-action", "none");
            }
        }
        let setup = Gfx::new(window, event_loop.owned_display_handle());

        #[cfg(not(target_arch = "wasm32"))]
        {
            let result = pollster::block_on(setup);
            self.finish_setup(event_loop, result);
        }
        #[cfg(target_arch = "wasm32")]
        {
            let (pending, proxy) = (self.pending.clone(), self.proxy.clone());
            wasm_bindgen_futures::spawn_local(async move {
                *pending.borrow_mut() = Some(setup.await);
                let _ = proxy.send_event(Ready);
            });
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, _: Ready) {
        let result = self.pending.borrow_mut().take();
        if let Some(result) = result {
            self.finish_setup(event_loop, result);
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        let Some(gfx) = self.gfx.as_mut() else { return };
        if let Some(log) = &mut self.frames
            && matches!(
                event,
                WindowEvent::CursorMoved { .. }
                    | WindowEvent::MouseInput { .. }
                    | WindowEvent::MouseWheel { .. }
                    | WindowEvent::KeyboardInput { .. }
            )
        {
            log.input.get_or_insert_with(Instant::now);
        }
        // A pending move goes first, so the surface sees events in their order.
        if !matches!(event, WindowEvent::CursorMoved { .. })
            && let Some((x, y)) = self.moved.take()
            && self.surface.handle(Event::PointerMoved { x, y })
            && !matches!(event, WindowEvent::RedrawRequested)
        {
            gfx.window.request_redraw();
        }
        let chart_event = match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
                return;
            }
            WindowEvent::Resized(size) => {
                gfx.resize(size.width, size.height);
                self.surface.resize(size.width as f32, size.height as f32);
                gfx.window.request_redraw();
                return;
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                // The size arrives in a following `Resized`.
                self.surface.set_scale(scale_factor as f32);
                gfx.window.request_redraw();
                return;
            }
            WindowEvent::RedrawRequested => {
                let started = Instant::now();
                self.surface.build_scene(&mut self.scene);
                gfx.draw(&self.scene);
                if let Some(log) = &mut self.frames {
                    log.drawn(started);
                }
                let title = self.surface.title();
                if !title.is_empty() && title != self.title {
                    gfx.window.set_title(&title);
                    self.title = title;
                }
                return;
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x as f32, position.y as f32);
                self.moved = Some((position.x as f32, position.y as f32));
                return;
            }
            WindowEvent::CursorLeft { .. } => Event::PointerLeft,
            WindowEvent::MouseInput {
                button: MouseButton::Left,
                state,
                ..
            } => match state {
                ElementState::Pressed => {
                    let now = Instant::now();
                    let (x, y) = self.cursor;
                    let near = DOUBLE_CLICK_PIXELS * gfx.window.scale_factor() as f32;
                    self.double = self.last_press.is_some_and(|(at, (px, py))| {
                        now - at <= DOUBLE_CLICK && (x - px).abs() <= near && (y - py).abs() <= near
                    });
                    // A third press starts over.
                    self.last_press = (!self.double).then_some((now, (x, y)));
                    Event::PointerDown
                }
                ElementState::Released => {
                    let mut redraw = self.surface.handle(Event::PointerUp);
                    if std::mem::take(&mut self.double) {
                        redraw |= self.surface.handle(Event::DoubleClick);
                    }
                    if redraw {
                        gfx.window.request_redraw();
                    }
                    return;
                }
            },
            WindowEvent::MouseInput {
                button: MouseButton::Right,
                state: ElementState::Pressed,
                ..
            } => Event::SecondaryDown,
            WindowEvent::Touch(touch) => {
                let phase = match touch.phase {
                    winit::event::TouchPhase::Started => TouchPhase::Start,
                    winit::event::TouchPhase::Moved => TouchPhase::Move,
                    winit::event::TouchPhase::Ended | winit::event::TouchPhase::Cancelled => TouchPhase::End,
                };
                let events = self
                    .touches
                    .handle(touch.id, phase, touch.location.x as f32, touch.location.y as f32);
                let mut redraw = false;
                for event in events {
                    redraw |= self.surface.handle(event);
                }
                if redraw {
                    gfx.window.request_redraw();
                }
                return;
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                let state = modifiers.state();
                self.shortcut = state.control_key() || state.alt_key() || state.super_key();
                self.command = state.control_key() || state.super_key();
                self.alt = state.alt_key();
                self.shift = state.shift_key();
                Event::Modifiers {
                    adding: self.command || self.shift,
                }
            }
            WindowEvent::KeyboardInput { event: key_event, .. } if key_event.state == ElementState::Pressed => {
                let alt_letter = letter_of(key_event.physical_key).filter(|_| self.alt && !self.command);
                let keys: Vec<Key> = match &key_event.logical_key {
                    // Alt with a letter is a shortcut by the key's place, so it works in any layout.
                    _ if let Some(letter) = alt_letter => {
                        vec![Key::Alt(if self.shift {
                            letter.to_ascii_uppercase()
                        } else {
                            letter
                        })]
                    }
                    WinitKey::Character(text) if self.shortcut => {
                        match (self.command, text.to_lowercase().as_str()) {
                            (true, "a") => vec![Key::SelectAll],
                            (true, "c") => vec![Key::Copy],
                            (true, "x") => vec![Key::Cut],
                            (true, "z") if self.shift => vec![Key::Redo],
                            (true, "z") => vec![Key::Undo],
                            (true, "y") => vec![Key::Redo],
                            // Over the chart with no text typed, it pastes what was copied there.
                            (true, "v") if !self.surface.takes_text() => vec![Key::Paste],
                            // Pasted text is typed in; a line break would submit, so it becomes a space.
                            (true, "v") => self
                                .clipboard
                                .paste()
                                .chars()
                                .filter_map(|c| match c {
                                    '\r' => None,
                                    '\n' | '\t' => Some(Key::Char(' ')),
                                    c if c.is_control() => None,
                                    c => Some(Key::Char(c)),
                                })
                                .collect(),
                            _ => return,
                        }
                    }
                    // All of what a key typed (a composed character can be more than one).
                    WinitKey::Character(text) => text.chars().map(Key::Char).collect(),
                    key => match translate_key(key, self.shift, self.command) {
                        Some(key) => vec![key],
                        None => return,
                    },
                };
                let mut redraw = false;
                for key in keys {
                    redraw |= self.surface.handle(Event::Key(key));
                }
                if let Some(text) = self.surface.take_copied() {
                    self.clipboard.copy(text);
                }
                if redraw {
                    gfx.window.request_redraw();
                }
                return;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let delta = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 50.0,
                };
                Event::Wheel {
                    x: self.cursor.0,
                    delta,
                }
            }
            _ => return,
        };
        if self.surface.handle(chart_event) {
            gfx.window.request_redraw();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // The moves that came since the last frame, as one.
        if let Some((x, y)) = self.moved.take()
            && self.surface.handle(Event::PointerMoved { x, y })
            && let Some(gfx) = &self.gfx
        {
            gfx.window.request_redraw();
        }
        // The pointer shows what a press would do where it is.
        let shape = self.surface.pointer_shape();
        if shape != self.shape
            && let Some(gfx) = &self.gfx
        {
            self.shape = shape;
            gfx.window.set_cursor(match shape {
                PointerShape::Default => CursorIcon::Default,
                PointerShape::Crosshair => CursorIcon::Crosshair,
                PointerShape::Hand => CursorIcon::Pointer,
                PointerShape::Text => CursorIcon::Text,
                PointerShape::ResizeVertical => CursorIcon::NsResize,
                PointerShape::ResizeHorizontal => CursorIcon::EwResize,
                PointerShape::Grabbing => CursorIcon::Grabbing,
                PointerShape::Move => CursorIcon::Move,
            });
        }
        // While something animates (a replay playing), send a tick per frame; while only waiting for data, fewer;
        // otherwise sleep.
        if self.gfx.is_none() || !self.surface.needs_tick() {
            self.last_tick = None;
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        }
        let now = Instant::now();
        let dt = self
            .last_tick
            .map_or(0.0, |t| now.duration_since(t).as_secs_f32().min(0.25));
        self.last_tick = Some(now);
        if self.surface.handle(Event::Tick { dt })
            && let Some(gfx) = &self.gfx
        {
            gfx.window.request_redraw();
        }
        // A frame's time while something moves; live data and the like do with a tenth of a second.
        let wait = if self.surface.animating() { 16 } else { 100 };
        event_loop.set_control_flow(ControlFlow::WaitUntil(now + Duration::from_millis(wait)));
    }
}

/// The system clipboard, made when first used: on Linux it serves what was copied for as long as it lives. The
/// browser build has none.
#[derive(Default)]
struct Clipboard {
    #[cfg(not(target_arch = "wasm32"))]
    system: Option<arboard::Clipboard>,
}

impl Clipboard {
    #[cfg(not(target_arch = "wasm32"))]
    fn system(&mut self) -> Option<&mut arboard::Clipboard> {
        if self.system.is_none() {
            self.system = arboard::Clipboard::new().ok();
        }
        self.system.as_mut()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn paste(&mut self) -> String {
        self.system().and_then(|c| c.get_text().ok()).unwrap_or_default()
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn copy(&mut self, text: String) {
        if let Some(c) = self.system() {
            let _ = c.set_text(text);
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn paste(&mut self) -> String {
        String::new()
    }

    #[cfg(target_arch = "wasm32")]
    fn copy(&mut self, _: String) {}
}

/// The letter of a key by where it is on the keyboard, as on a US layout, whatever the layout in use types there.
fn letter_of(key: PhysicalKey) -> Option<char> {
    const LETTERS: [KeyCode; 26] = [
        KeyCode::KeyA,
        KeyCode::KeyB,
        KeyCode::KeyC,
        KeyCode::KeyD,
        KeyCode::KeyE,
        KeyCode::KeyF,
        KeyCode::KeyG,
        KeyCode::KeyH,
        KeyCode::KeyI,
        KeyCode::KeyJ,
        KeyCode::KeyK,
        KeyCode::KeyL,
        KeyCode::KeyM,
        KeyCode::KeyN,
        KeyCode::KeyO,
        KeyCode::KeyP,
        KeyCode::KeyQ,
        KeyCode::KeyR,
        KeyCode::KeyS,
        KeyCode::KeyT,
        KeyCode::KeyU,
        KeyCode::KeyV,
        KeyCode::KeyW,
        KeyCode::KeyX,
        KeyCode::KeyY,
        KeyCode::KeyZ,
    ];
    let PhysicalKey::Code(code) = key else {
        return None;
    };
    LETTERS
        .iter()
        .position(|&c| c == code)
        .map(|i| (b'a' + i as u8) as char)
}

fn translate_key(key: &WinitKey, shift: bool, command: bool) -> Option<Key> {
    Some(match key {
        WinitKey::Named(NamedKey::ArrowLeft) if command => Key::CtrlLeft,
        WinitKey::Named(NamedKey::ArrowRight) if command => Key::CtrlRight,
        WinitKey::Named(NamedKey::ArrowUp) if command => Key::CtrlUp,
        WinitKey::Named(NamedKey::ArrowDown) if command => Key::CtrlDown,
        WinitKey::Named(NamedKey::ArrowLeft) if shift => Key::SelectLeft,
        WinitKey::Named(NamedKey::ArrowRight) if shift => Key::SelectRight,
        WinitKey::Named(NamedKey::Home) if shift => Key::SelectHome,
        WinitKey::Named(NamedKey::End) if shift => Key::SelectEnd,
        WinitKey::Named(NamedKey::Space) => Key::Space,
        WinitKey::Named(NamedKey::Enter) => Key::Enter,
        WinitKey::Named(NamedKey::Escape) => Key::Escape,
        WinitKey::Named(NamedKey::Delete) => Key::Delete,
        WinitKey::Named(NamedKey::Backspace) => Key::Backspace,
        WinitKey::Named(NamedKey::ArrowLeft) => Key::Left,
        WinitKey::Named(NamedKey::ArrowRight) => Key::Right,
        WinitKey::Named(NamedKey::ArrowUp) => Key::Up,
        WinitKey::Named(NamedKey::ArrowDown) => Key::Down,
        WinitKey::Named(NamedKey::Home) => Key::Home,
        WinitKey::Named(NamedKey::End) => Key::End,
        _ => return None,
    })
}
