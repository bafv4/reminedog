//! Per-frame work inside the buffer-swap detours (`glfwSwapBuffers` for Minecraft up to
//! 1.21, `SDL_GL_SwapWindow` for 26.x).

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};

use glow::HasContext as _;
use reminedog_core::{Settings, settings_path};
use reminedog_render::{
    FrameInput, FrameParams, Overlay, PointerSpeed, StatusLine, ZoomView, gl_summary, hotkeys,
};

use crate::agent::{self, Globals};
use crate::fonts;
use crate::input;
use crate::pointer;
use crate::tall::TallZoom;
use crate::wgl::{self, OwnContext, Wgl};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Gdi::{GetDC, HDC};
use windows_sys::Win32::UI::WindowsAndMessaging::IsIconic;

/// Settings changed in the menu are saved after this long without further changes (or
/// when the menu closes), so dragging a slider does not write the file every frame.
const SAVE_DELAY: Duration = Duration::from_secs(1);

/// Consecutive failed frames after which the overlay gives up for the session.
const MAX_FAILURES: u32 = 30;
/// GL errors reported before going quiet.
const MAX_GL_ERRORS_LOGGED: u32 = 10;

enum State {
    Uninit,
    Ready(Box<Runtime>),
    Disabled,
}

/// One overlay for the process, following its window across threads: Forge and NeoForge
/// swap the same window from their early loading-screen thread before Minecraft's render
/// thread takes it over. Being a static, it is never dropped, so nothing runs at exit.
static STATE: Mutex<State> = Mutex::new(State::Uninit);

/// What the overlay needs from the library that owns the game's window (GLFW or SDL3).
pub trait WindowSystem: Sync {
    /// Name and version, for the log and the status window.
    fn describe(&self) -> String;
    /// Hidden helper windows (mods, Minecraft 26.x's renderer) never get the overlay.
    fn is_visible(&self, window: *mut c_void) -> bool;
    /// Drawable size in pixels; call on the thread that swaps the window.
    fn framebuffer_size(&self, window: *mut c_void) -> (i32, i32);
    /// DPI scale of the window.
    fn content_scale(&self, window: *mut c_void) -> f32;
    fn hwnd(&self, window: *mut c_void) -> Option<HWND>;
    /// Whether the game gets raw mouse counts while it captures the cursor, untouched by
    /// the system's pointer speed and acceleration (the overlay then applies them to its
    /// own cursor), rather than the motion of the system's pointer.
    fn raw_motion(&self, window: *mut c_void) -> bool;
    /// Whether the game has grabbed the cursor (it is being played, not showing a menu).
    fn captured(&self, window: *mut c_void) -> bool;
}

/// Draws the overlay into the back buffer right before the window system presents it.
pub fn before_swap(ws: &dyn WindowSystem, window: *mut c_void) {
    let Some(agent) = agent::globals() else {
        return;
    };
    if !agent.options.overlay || window.is_null() {
        return;
    }
    // A minimized window has nothing to draw on, and GetDC hands out a new temporary DC on
    // every call instead of the window's own one; skip the frame and keep the overlay.
    // SAFETY: a valid window handle.
    if ws
        .hwnd(window)
        .is_some_and(|hwnd| unsafe { IsIconic(hwnd) } != 0)
    {
        return;
    }
    // Contended means another thread is swapping another window; poisoned means a panic
    // hit mid-frame (the state was set to Disabled first). Skip the frame either way.
    let mut state = match STATE.try_lock() {
        Ok(state) => state,
        Err(TryLockError::WouldBlock) => return,
        Err(TryLockError::Poisoned(_)) => {
            // Stay off, and make sure no invisible UI keeps taking the game's input.
            input::router().set_enabled(false);
            return;
        }
    };
    // Disabled while we work: if anything below panics, the overlay stays off instead of
    // failing again on every frame.
    let next = match std::mem::replace(&mut *state, State::Disabled) {
        State::Disabled => State::Disabled,
        // A hidden helper window (some mods create one) never gets the overlay; wait for a
        // visible one.
        State::Uninit if !ws.is_visible(window) => State::Uninit,
        State::Uninit => match Runtime::create(ws, window, agent) {
            Ok(runtime) => run_frame(runtime, ws, window, agent),
            Err(e) => {
                log::error!("overlay disabled: {e}");
                State::Disabled
            }
        },
        State::Ready(runtime) => run_frame(runtime, ws, window, agent),
    };
    if matches!(next, State::Disabled) {
        input::router().set_enabled(false);
    }
    *state = next;
}

/// How relative motion should move the overlay's cursor while the game captures the mouse.
fn pointer_speed(ws: &dyn WindowSystem, window: *mut c_void) -> PointerSpeed {
    if ws.raw_motion(window) {
        pointer::system_speed()
    } else {
        log::debug!("the game reads the system pointer's motion; no pointer speed applied");
        PointerSpeed::RAW
    }
}

/// The window's device context. GLFW's and SDL's window classes have CS_OWNDC, so this is
/// the very DC the game's context renders to and the swap presents, whichever context is
/// current right now (neither swap function requires the window's own context to be).
fn window_dc(ws: &dyn WindowSystem, wgl: &Wgl, window: *mut c_void) -> HDC {
    if let Some(hwnd) = ws.hwnd(window) {
        // SAFETY: a valid window handle; the DC of a CS_OWNDC window needs no release.
        let dc = unsafe { GetDC(hwnd) };
        if !dc.is_null() {
            return dc;
        }
    }
    wgl.current().0
}

fn run_frame(
    mut runtime: Box<Runtime>,
    ws: &dyn WindowSystem,
    window: *mut c_void,
    agent: &Globals,
) -> State {
    match runtime.render(ws, window, agent) {
        Ok(()) => {
            runtime.failures = 0;
            State::Ready(runtime)
        }
        Err(FrameError::DrawableChanged) => {
            log::warn!("the game's device context changed; recreating the overlay context");
            runtime.teardown();
            State::Uninit
        }
        Err(FrameError::Failed(e)) => {
            runtime.failures += 1;
            if runtime.failures == 1 {
                log::warn!("overlay frame failed: {e}");
            }
            if runtime.failures >= MAX_FAILURES {
                log::error!("overlay disabled after {MAX_FAILURES} failed frames in a row: {e}");
                runtime.teardown();
                State::Disabled
            } else {
                State::Ready(runtime)
            }
        }
    }
}

enum FrameError {
    DrawableChanged,
    Failed(String),
}

struct Runtime {
    /// The window the overlay is drawn on; swaps of other windows are left alone.
    window: usize,
    context: OwnContext,
    gl: Arc<glow::Context>,
    overlay: Overlay,
    /// Static diagnostics plus, last, the per-frame cost.
    status: Vec<StatusLine>,
    /// Smoothed CPU time of one overlay frame (context switches included), in ms.
    cost_ms: f64,
    failures: u32,
    gl_errors_logged: u32,
    /// The UI was open in the last frame.
    ui_was_open: bool,
    settings: Settings,
    settings_path: PathBuf,
    /// When the settings last changed, if not saved since.
    unsaved: Option<Instant>,
    tall: TallZoom,
}

// SAFETY: the GL handles and objects are only used inside the swap detour while STATE is
// locked, with our context made current on the calling thread and released before the
// lock is; a WGL context may be made current on any thread.
unsafe impl Send for Runtime {}

impl Runtime {
    fn create(
        ws: &dyn WindowSystem,
        window: *mut c_void,
        agent: &Globals,
    ) -> Result<Box<Self>, String> {
        let wgl = wgl::get()?;
        let hdc = window_dc(ws, wgl, window);
        if hdc.is_null() {
            return Err("cannot get the window's device context".into());
        }
        let (current_dc, game_context) = wgl.current();
        let (width, height) = ws.framebuffer_size(window);
        // Describe the game's context only if it is the one current on this window.
        let game_gl = if current_dc == hdc && !game_context.is_null() {
            wgl.describe_current()
        } else {
            "? (the window's context was not current)".to_owned()
        };
        log::info!("window system: {}", ws.describe());
        log::info!(
            "window framebuffer {width}x{height}, content scale {:.2}",
            ws.content_scale(window)
        );
        log::info!("game GL: {game_gl}");
        log::info!("pixel format {}", wgl::describe_pixel_format(hdc));

        let settings_path = settings_path(&agent.game_dir);
        let settings = Settings::load(&settings_path);
        log::info!(
            "settings: {} (menu {}, zoom {})",
            settings_path.display(),
            settings.menu_key,
            settings.zoom_key
        );

        let context = OwnContext::create(wgl, hdc)?;
        let created = (|| {
            let _current = context.make_current(wgl)?;
            // SAFETY: our context is current; the loader resolves functions for it.
            let gl = Arc::new(unsafe {
                glow::Context::from_loader_function_cstr(|name| wgl.load(name))
            });
            let overlay_gl = gl_summary(&gl);
            log::info!("overlay GL: {overlay_gl}");
            let overlay = Overlay::new(gl.clone(), fonts::load_japanese(), settings.clone())
                .map_err(|e| e.to_string())?;
            Ok::<_, String>((gl, overlay, overlay_gl))
        })();
        let (gl, overlay, overlay_gl) = match created {
            Ok(created) => created,
            Err(e) => {
                context.delete(wgl);
                return Err(e);
            }
        };
        log::info!("overlay: initialized");
        {
            let mut router = input::router();
            router.set_hotkeys(hotkeys(&settings));
            router.set_pointer_speed(pointer_speed(ws, window));
        }

        let status = vec![
            StatusLine::new(
                "ビルド",
                format!("{} ({})", env!("CARGO_PKG_VERSION"), agent::BUILD_ID),
            ),
            StatusLine::new("ウィンドウ", ws.describe()),
            StatusLine::new("ゲームのGL", game_gl),
            StatusLine::new("オーバーレイのGL", overlay_gl),
            StatusLine::new("ゲームフォルダ", agent.game_dir.display().to_string()),
            StatusLine::new("処理時間", "-"),
        ];
        Ok(Box::new(Self {
            window: window as usize,
            context,
            gl,
            overlay,
            status,
            cost_ms: 0.0,
            failures: 0,
            gl_errors_logged: 0,
            ui_was_open: false,
            settings,
            settings_path,
            unsaved: None,
            tall: TallZoom::default(),
        }))
    }

    fn render(
        &mut self,
        ws: &dyn WindowSystem,
        window: *mut c_void,
        agent: &Globals,
    ) -> Result<(), FrameError> {
        if window as usize != self.window {
            return Ok(());
        }
        let wgl = wgl::get().map_err(FrameError::Failed)?;
        let hdc = window_dc(ws, wgl, window);
        if hdc.is_null() {
            return Err(FrameError::Failed(
                "cannot get the window's device context".into(),
            ));
        }
        if hdc != self.context.hdc() {
            log::debug!(
                "device context {:?} -> {hdc:?} (hwnd {:?}, current {:?})",
                self.context.hdc(),
                ws.hwnd(window),
                wgl.current()
            );
            return Err(FrameError::DrawableChanged);
        }
        let (width, height) = ws.framebuffer_size(window);
        if width <= 0 || height <= 0 {
            // Minimized.
            return Ok(());
        }
        let scale = ws.content_scale(window) * agent.options.ui_scale.unwrap_or(1.0);

        let started = Instant::now();
        // The pointer settings may change while the game runs; read them whenever the UI
        // opens (a few events right after Ctrl+I may still use the previous ones).
        let ui_open = input::router().ui_open();
        if ui_open && !self.ui_was_open {
            input::router().set_pointer_speed(pointer_speed(ws, window));
        }
        self.ui_was_open = ui_open;

        // The zoom works in the game's context, which is current at the swap. It ends when
        // the game shows a menu (the inventory opened with the zoom key held).
        let want_zoom = input::router().zoom_active() && ws.captured(window);
        let zoom = self.tall.frame(
            window,
            [width, height],
            want_zoom && self.settings.zoom_high_res,
            self.settings.zoom_factor,
        );
        let zoom = if want_zoom && zoom == ZoomView::Off {
            ZoomView::Magnify
        } else {
            zoom
        };
        let high_res_note = self
            .tall
            .failure()
            .map(|reason| format!("高精細は使えない：{reason}"));
        {
            let _current = self.context.make_current(wgl).map_err(FrameError::Failed)?;
            let input = {
                let mut router = input::router();
                router.set_enabled(true);
                router.set_screen([width as u32, height as u32], scale);
                FrameInput {
                    events: router.take_events(),
                    modifiers: router.modifiers(),
                    ui_open: router.ui_open(),
                    zoom,
                    software_cursor: router.software_cursor(),
                    captured: router.take_captured(),
                    high_res_note,
                    ..Default::default()
                }
            };
            let output = self.overlay.render(FrameParams {
                framebuffer_size: [width as u32, height as u32],
                pixels_per_point: scale,
                time: agent.start.elapsed().as_secs_f64(),
                status: &self.status,
                input,
            });
            {
                let mut router = input::router();
                if output.close_ui {
                    router.set_ui_open(false);
                }
                if output.start_capture {
                    router.start_capture();
                }
                if output.cancel_capture {
                    router.cancel_capture();
                }
                if let Some(settings) = &output.settings {
                    router.set_hotkeys(hotkeys(settings));
                }
            }
            if let Some(settings) = output.settings {
                self.settings = settings;
                self.unsaved = Some(Instant::now());
            }
            self.log_gl_errors();
        }
        self.save_settings_when_due();
        let cost_ms = started.elapsed().as_secs_f64() * 1000.0;
        self.cost_ms = if self.overlay.frames() <= 1 {
            cost_ms
        } else {
            self.cost_ms * 0.95 + cost_ms * 0.05
        };
        if let Some(line) = self.status.last_mut() {
            line.value = format!("{:.2} ms", self.cost_ms);
        }

        match self.overlay.frames() {
            1 => log::info!("overlay: first frame rendered ({width}x{height}, scale {scale:.2})"),
            n @ (100 | 1000 | 10_000) => log::info!(
                "overlay: {n} frames rendered, {:.2} ms per frame",
                self.cost_ms
            ),
            _ => {}
        }
        Ok(())
    }

    fn save_settings_when_due(&mut self) {
        let Some(changed) = self.unsaved else {
            return;
        };
        if changed.elapsed() < SAVE_DELAY && input::router().ui_open() {
            return;
        }
        self.unsaved = None;
        match self.settings.save(&self.settings_path) {
            Ok(()) => log::info!(
                "settings saved (menu {}, zoom {}, ×{:.1}, high resolution {})",
                self.settings.menu_key,
                self.settings.zoom_key,
                self.settings.zoom_factor,
                self.settings.zoom_high_res
            ),
            Err(e) => log::error!(
                "cannot save settings to {}: {e}",
                self.settings_path.display()
            ),
        }
    }

    /// Drains our context's GL error queue (never the game's).
    fn log_gl_errors(&mut self) {
        for _ in 0..8 {
            // SAFETY: our context is current.
            let error = unsafe { self.gl.get_error() };
            if error == glow::NO_ERROR {
                break;
            }
            self.gl_errors_logged += 1;
            if self.gl_errors_logged <= MAX_GL_ERRORS_LOGGED {
                log::warn!("GL error 0x{error:04X} in the overlay context");
            }
        }
    }

    /// Frees GL resources and deletes the context. Best effort: the DC may be gone.
    fn teardown(self: Box<Self>) {
        let Ok(wgl) = wgl::get() else {
            return;
        };
        let Runtime {
            context,
            mut overlay,
            ..
        } = *self;
        match context.make_current(wgl) {
            Ok(_current) => overlay.destroy(),
            Err(e) => log::warn!("cannot free overlay resources: {e}"),
        }
        context.delete(wgl);
    }
}
