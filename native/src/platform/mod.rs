//! Window backends.
//!
//! One winit event loop drives four borderless, always-on-top windows — the
//! pill, the edit button, the word widget and the waterfall — exactly like the
//! four X11 windows of the Linux app. Presentation is per platform:
//!
//! * **Windows**: `UpdateLayeredWindow` on a premultiplied BGRA DIB gives true
//!   per-pixel alpha, and fully transparent pixels are click-through.
//! * **macOS**: the window's `CALayer.contents` is set to a `CGImage` of the
//!   frame; the window itself is non-opaque, floating and shadow-less.
//! * **Linux** (development build of this crate): `softbuffer`, so the same
//!   renderer can be run and profiled on X11 without the Cairo app.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tiny_skia::Pixmap;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalPosition, LogicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId, WindowLevel};

use crate::app::{App, Mods, Overlay, Surface};
use crate::media::MediaSource;
use crate::spectrum::SpectrumTracker;

#[cfg(windows)]
mod layered;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod soft;

pub struct Options {
    pub mock: bool,
    pub start_edit: bool,
    pub layout: island_core::Layout,
}

struct Win {
    window: Arc<Window>,
    cursor: (f64, f64),
    placed: Option<(i32, i32, i32, i32)>,
    visible: bool,
    #[cfg(target_os = "linux")]
    surface: soft::Soft,
    #[cfg(windows)]
    layered: layered::Layered,
    #[cfg(target_os = "macos")]
    layer: macos::MacLayer,
}

impl Win {
    /// Cursor position in screen coordinates (logical points).
    fn global(&self) -> (f64, f64) {
        let pos = self.window.outer_position().unwrap_or_default();
        let scale = self.window.scale_factor();
        (
            pos.x as f64 / scale + self.cursor.0,
            pos.y as f64 / scale + self.cursor.1,
        )
    }
}

pub struct Platform {
    windows: HashMap<Surface, Win>,
    screen: (i32, i32),
    scale: f32,
    mods: Mods,
}

impl Platform {
    fn new() -> Platform {
        Platform {
            windows: HashMap::new(),
            screen: (1920, 1080),
            scale: 1.0,
            mods: Mods::default(),
        }
    }

    fn create(&mut self, el: &ActiveEventLoop, s: Surface) -> Option<()> {
        let attrs = Window::default_attributes()
            .with_title("gabes-island")
            .with_decorations(false)
            .with_resizable(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_active(false)
            .with_inner_size(LogicalSize::new(48.0, 8.0))
            .with_position(LogicalPosition::new(0.0, 0.0))
            // Windows uses WS_EX_LAYERED + UpdateLayeredWindow instead, which
            // conflicts with winit's own transparent handling (SetLayeredWindow
            // Attributes and UpdateLayeredWindow are mutually exclusive).
            .with_transparent(!cfg!(windows));
        let window = Arc::new(el.create_window(attrs).ok()?);
        window.set_visible(false);
        #[cfg(windows)]
        layered::prepare(&window);
        let win = Win {
            #[cfg(target_os = "linux")]
            surface: soft::Soft::new(&window)?,
            #[cfg(windows)]
            layered: layered::Layered::new(&window)?,
            #[cfg(target_os = "macos")]
            layer: macos::MacLayer::new(&window)?,
            window,
            cursor: (0.0, 0.0),
            placed: None,
            visible: false,
        };
        self.windows.insert(s, win);
        self.refresh_screen();
        Some(())
    }

    fn refresh_screen(&mut self) {
        if let Some(w) = self.windows.values().next() {
            self.scale = w.window.scale_factor() as f32;
            if let Some(m) = w.window.current_monitor() {
                let size = m.size().to_logical::<f64>(m.scale_factor());
                self.screen = (size.width.round() as i32, size.height.round() as i32);
            }
        }
    }

    fn surface_of(&self, id: WindowId) -> Option<Surface> {
        self.windows
            .iter()
            .find(|(_, w)| w.window.id() == id)
            .map(|(s, _)| *s)
    }
}

impl Overlay for Platform {
    fn place(&mut self, s: Surface, x: i32, y: i32, w: i32, h: i32) {
        let Some(win) = self.windows.get_mut(&s) else {
            return;
        };
        let rect = (x, y, w.max(1), h.max(1));
        if win.placed == Some(rect) {
            return;
        }
        win.placed = Some(rect);
        let _ = win
            .window
            .request_inner_size(LogicalSize::new(rect.2 as f64, rect.3 as f64));
        win.window
            .set_outer_position(LogicalPosition::new(x as f64, y as f64));
    }

    fn present(&mut self, s: Surface, pm: &Pixmap) {
        let Some(win) = self.windows.get_mut(&s) else {
            return;
        };
        #[cfg(target_os = "linux")]
        win.surface.present(pm);
        #[cfg(windows)]
        win.layered.present(pm);
        #[cfg(target_os = "macos")]
        win.layer.present(pm);
    }

    fn show(&mut self, s: Surface) {
        if let Some(win) = self.windows.get_mut(&s) {
            if !win.visible {
                win.window.set_visible(true);
                win.visible = true;
            }
            win.window.set_window_level(WindowLevel::AlwaysOnTop);
        }
    }

    fn hide(&mut self, s: Surface) {
        if let Some(win) = self.windows.get_mut(&s) {
            if win.visible {
                win.window.set_visible(false);
                win.visible = false;
            }
        }
    }

    fn screen(&self) -> (i32, i32) {
        self.screen
    }

    fn scale(&self) -> f32 {
        self.scale
    }

    fn fullscreen(&mut self) -> bool {
        #[cfg(windows)]
        {
            layered::fullscreen_foreground()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }
}

/// The winit application: owns the state machine, the windows and the pollers.
pub struct Run {
    app: App,
    platform: Platform,
    media: Box<dyn MediaSource>,
    spectrum: SpectrumTracker,
    start_edit: bool,
    started: Instant,
    created: bool,
}

impl Run {
    pub fn new(opts: Options) -> Run {
        let app = App::new(opts.layout.clone(), 1.0, (1920, 1080), opts.mock);
        let media = crate::media::create(opts.layout.capture_app_list(), opts.mock);
        let spectrum = SpectrumTracker::new(crate::spectrum::create(opts.mock));
        Run {
            app,
            platform: Platform::new(),
            media,
            spectrum,
            start_edit: opts.start_edit,
            started: Instant::now(),
            created: false,
        }
    }

    fn now_ms(&self) -> u128 {
        self.started.elapsed().as_millis()
    }
}

impl ApplicationHandler for Run {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.created {
            return;
        }
        self.created = true;
        for s in [
            Surface::Pill,
            Surface::EditButton,
            Surface::Widget,
            Surface::Waterfall,
        ] {
            if self.platform.create(el, s).is_none() {
                eprintln!("gabes-island: cannot create window for {s:?}");
            }
        }
        self.platform.refresh_screen();
        let (sw, sh) = self.platform.screen();
        let scale = self.platform.scale();
        self.app.set_screen(sw, sh, scale);
        if self.start_edit {
            self.app.set_edit_mode(true);
            self.start_edit = false;
        }
        println!(
            "gabes-island-native: {}x{} @{:.2}x, media={}, spectrum={}",
            sw,
            sh,
            scale,
            self.media.backend(),
            self.spectrum.backend()
        );
        el.set_control_flow(ControlFlow::WaitUntil(
            Instant::now() + Duration::from_millis(16),
        ));
    }

    fn window_event(&mut self, el: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(surface) = self.platform.surface_of(id) else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => {
                self.platform.refresh_screen();
                let (sw, sh) = self.platform.screen();
                self.app.set_screen(sw, sh, self.platform.scale());
            }
            WindowEvent::CursorMoved { position, .. } => {
                let p = position.to_logical::<f64>(self.platform.scale as f64);
                let global = if let Some(win) = self.platform.windows.get_mut(&surface) {
                    win.cursor = (p.x, p.y);
                    win.global()
                } else {
                    (p.x, p.y)
                };
                self.app.on_cursor_move(surface, p.x, p.y, global);
            }
            WindowEvent::CursorLeft { .. } => {
                self.app.on_cursor_leave(surface);
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let (local, global) = match self.platform.windows.get(&surface) {
                    Some(win) => (win.cursor, win.global()),
                    None => ((0.0, 0.0), (0.0, 0.0)),
                };
                let b = match button {
                    MouseButton::Left => 1,
                    MouseButton::Middle => 2,
                    MouseButton::Right => 3,
                    _ => 0,
                };
                self.app.on_button(
                    surface,
                    b,
                    state == ElementState::Pressed,
                    self.platform.mods,
                    local,
                    global,
                    self.media.as_mut(),
                );
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let up = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y > 0.0,
                    MouseScrollDelta::PixelDelta(p) => p.y > 0.0,
                };
                self.app.on_wheel(surface, up, self.platform.mods);
            }
            WindowEvent::ModifiersChanged(m) => {
                let st = m.state();
                self.platform.mods = Mods {
                    shift: st.shift_key(),
                    ctrl: st.control_key(),
                    alt: st.alt_key(),
                };
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        let now = self.now_ms();
        let fullscreen = self.platform.fullscreen();
        self.app
            .update(now, self.media.as_mut(), &mut self.spectrum, fullscreen);
        self.app.draw(now, &mut self.platform);
        let wake = self.app.next_wake_ms();
        el.set_control_flow(ControlFlow::WaitUntil(
            Instant::now() + Duration::from_millis(wake),
        ));
    }
}

pub fn run(opts: Options) -> Result<(), String> {
    let event_loop = EventLoop::new().map_err(|e| e.to_string())?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut run = Run::new(opts);
    event_loop.run_app(&mut run).map_err(|e| e.to_string())
}
