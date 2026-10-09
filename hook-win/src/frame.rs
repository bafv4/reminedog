//! Per-frame work inside the buffer-swap detours (`glfwSwapBuffers` for Minecraft up to
//! 1.21, `SDL_GL_SwapWindow` for 26.x).

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};

use glow::HasContext as _;
use reminedog_core::{InputId, Naming, Settings, input_by_name, input_name, settings_path};
use reminedog_render::{
    BrowserState, FrameInput, FrameParams, Hotkeys, Notice, Overlay, PointerSpeed, StatusLine,
    ZoomView, gl_summary, hotkeys, hotkeys_quiet, resolve,
};

use crate::agent::{self, Globals};
use crate::browser;
use crate::clipboard;
use crate::fonts;
use crate::input;
use crate::pointer;
use crate::saver;
use crate::tall::TallZoom;
use crate::waypoints;
use crate::wgl::{self, OwnContext, Wgl};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Gdi::{GetDC, HDC};
use windows_sys::Win32::UI::WindowsAndMessaging::IsIconic;

/// Settings are saved after this long without further changes (or, changed in the menu, when
/// it closes), so dragging a slider does not write the file each time.
const SAVE_DELAY: Duration = Duration::from_secs(1);
/// A failed save is tried again after this long.
const SAVE_RETRY: Duration = Duration::from_secs(10);
/// How long the overlay going away waits for the settings to be saved.
const SAVE_WAIT: Duration = Duration::from_secs(2);
/// How long a failed save's notice shows, in seconds.
const NOTICE_SECONDS: f64 = 6.0;

/// The menu's note when keys cannot be rebound.
const NO_KEY_REBINDS: &str = "ゲームがキーの状態を読む関数をフックできなかったので、キーボードのキーは置き換えられない（マウスのボタンは置き換えられる）";

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
    /// How the game's options.txt names keys under this library.
    fn naming(&self) -> Naming;
    /// Whether the game's reads of the key state go through the key rebinding. Without it keys
    /// cannot be rebound: the game would read a held source as held (when a screen closes).
    fn key_state_spoofed(&self) -> bool;
    /// Whether the game can be given `id` (as a key or mouse button event) under this library.
    fn can_send(&self, id: InputId) -> bool;
    /// Whether this library reports presses of `id` (a rule's source must be one).
    fn can_receive(&self, id: InputId) -> bool;
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
    // every call instead of the window's own one; skip the frame and keep the overlay. A zoom
    // ends: the game would go on drawing its tall frames unseen.
    // SAFETY: a valid window handle.
    if ws
        .hwnd(window)
        .is_some_and(|hwnd| unsafe { IsIconic(hwnd) } != 0)
    {
        if let Ok(mut state) = STATE.try_lock()
            && let State::Ready(runtime) = &mut *state
            && runtime.window == window as usize
        {
            runtime.tall.release();
        }
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
    /// When the settings are to be saved, if not saved since they changed, and whether they
    /// changed in the menu (then they are saved when it closes).
    unsaved: Option<(Instant, bool)>,
    /// A save on the saving thread.
    saving: Option<saver::Pending>,
    /// What was wrong with the settings file when it was read ([`settings_problem`]).
    load_problem: Option<String>,
    /// Why the last save failed.
    save_problem: Option<String>,
    /// For the next frame.
    notices: Vec<Notice>,
    /// The browser's picture last taken ([`browser::frame_if_new`]).
    page_version: u64,
    tall: TallZoom,
    /// The router's hotkeys, from `settings`.
    hotkeys: Hotkeys,
    /// Keys may be rebound ([`WindowSystem::key_state_spoofed`]), not only mouse buttons.
    key_rebinds: bool,
    /// Why some rebinding rules cannot work, for the menu.
    rebind_note: Option<String>,
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
        let load_problem = settings_problem(&settings);
        log::info!(
            "settings: {} (menu {}, zoom {}, waypoint {}, navigate {}, {})",
            settings_path.display(),
            settings.menu_key,
            settings.zoom_key,
            settings.waypoint_key,
            settings.navigate_key,
            rebinds_summary(&settings)
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
        let keys = hotkeys(&settings);
        let key_rebinds = ws.key_state_spoofed();
        if !key_rebinds {
            log::warn!("rebinds: the game's key state reads are not hooked; keys are not rebound");
        }
        let rules = rebind_rules(ws, &settings, &keys);
        let active = {
            let mut router = input::router();
            router.set_hotkeys(keys);
            router.set_rebinds(rules, key_rebinds);
            router.set_pointer_speed(pointer_speed(ws, window));
            router.rebinds().to_vec()
        };
        log_active_rebinds(&active);

        let status = vec![
            StatusLine::new(
                "ビルド",
                format!("{} ({})", agent::VERSION, agent::BUILD_ID),
            ),
            StatusLine::new("ウィンドウ", ws.describe()),
            StatusLine::new("ゲームのGL", game_gl),
            StatusLine::new("オーバーレイのGL", overlay_gl),
            StatusLine::new("ゲームフォルダ", screen_path(&agent.game_dir)),
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
            saving: None,
            load_problem,
            save_problem: None,
            notices: Vec::new(),
            page_version: browser::NO_PICTURE,
            tall: TallZoom::default(),
            hotkeys: keys,
            key_rebinds,
            rebind_note: (!key_rebinds).then(|| NO_KEY_REBINDS.to_owned()),
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
        // Ctrl+V in a text field: the clipboard is read with the router unlocked, and its
        // text goes in with this frame's events.
        if input::router().take_paste_request()
            && let Some(text) = ws.hwnd(window).and_then(clipboard::read_text)
        {
            input::router().paste(&text);
        }

        // The zoom works in the game's context, which is current at the swap. It ends when
        // the game shows a menu (the inventory opened with the zoom key held).
        let playing = ws.captured(window);
        let want_zoom = input::router().zoom_active() && playing;
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
        let (waypoint_commands, browser_commands, browser_actions) = {
            let _current = self.context.make_current(wgl).map_err(FrameError::Failed)?;
            // Takes the router's hotkey actions, so before the router is locked below.
            let marks = waypoints::before_frame(&agent.game_dir, self.window, playing, ws.naming());
            let (browser, browser_notices) = browser::before_frame();
            let browser_shown = browser.shown;
            // The browser's keys only while there is a page to act on.
            let browser_keys = browser_shown && browser.state == BrowserState::Ready;
            // Only the menu shows them.
            let (game_bindings, unsupported_inputs) = if ui_open {
                (
                    waypoints::game_bindings(),
                    unsupported_inputs(ws, &self.settings),
                )
            } else {
                (Vec::new(), Vec::new())
            };
            let (input, browser_actions) = {
                let mut router = input::router();
                router.set_enabled(true);
                router.set_screen([width as u32, height as u32], scale);
                router.set_browser_shown(browser_keys);
                let input = FrameInput {
                    events: router.take_events(),
                    modifiers: router.modifiers(),
                    ui_open: router.ui_open(),
                    zoom,
                    software_cursor: router.software_cursor(),
                    captured: router.take_captured(),
                    high_res_note,
                    waypoints: marks.view,
                    notices: marks
                        .notices
                        .into_iter()
                        .chain(browser_notices)
                        .chain(self.notices.drain(..))
                        .collect(),
                    reserved_keys: marks.reserved_keys,
                    game_bindings,
                    rebind_note: self.rebind_note.clone(),
                    settings_note: self.settings_note(),
                    unsupported_inputs,
                    browser,
                };
                (input, router.take_browser_actions())
            };
            // The browser thread waits only while a new picture is uploaded; a picture it is
            // writing right now waits for the next frame. Hidden, the last one stays.
            if browser_shown {
                let frame = browser::frame_if_new(&mut self.page_version);
                // SAFETY: our context is current.
                unsafe {
                    self.overlay
                        .upload_page(frame.as_deref().map(browser::pixels))
                };
            }
            let output = self.overlay.render(FrameParams {
                framebuffer_size: [width as u32, height as u32],
                pixels_per_point: scale,
                time: agent.start.elapsed().as_secs_f64(),
                status: &self.status,
                input,
            });
            // The rules again only when they or the hotkeys changed: resolving logs them (the
            // zoom's slider changes the settings every frame while dragged). The hotkeys'
            // problems were logged when the settings were read.
            let changed = output.settings.as_ref().map(|settings| {
                let keys = hotkeys_quiet(settings);
                let rules = (settings.rebinds_enabled != self.settings.rebinds_enabled
                    || settings.rebinds != self.settings.rebinds
                    || keys != self.hotkeys)
                    .then(|| rebind_rules(ws, settings, &keys));
                (keys, rules)
            });
            let active = {
                let mut router = input::router();
                router.set_text_focus(output.text_focus);
                if output.close_ui {
                    router.set_ui_open(false);
                }
                if output.start_capture {
                    router.start_capture();
                }
                if output.start_input_capture {
                    router.start_input_capture();
                }
                if output.cancel_capture {
                    router.cancel_capture();
                }
                match &changed {
                    Some((keys, rules)) => {
                        router.set_hotkeys(*keys);
                        rules.as_ref().map(|rules| {
                            router.set_rebinds(rules.clone(), self.key_rebinds);
                            router.rebinds().to_vec()
                        })
                    }
                    None => None,
                }
            };
            if let Some(active) = active {
                log_active_rebinds(&active);
            }
            if let Some((keys, _)) = changed {
                self.hotkeys = keys;
            }
            if let Some(settings) = output.settings {
                if settings.zoom_factor != self.settings.zoom_factor
                    || settings.zoom_high_res != self.settings.zoom_high_res
                {
                    self.tall.retry();
                }
                self.settings = settings;
                self.unsaved = Some((Instant::now() + SAVE_DELAY, ui_open));
            }
            if let Some(text) = &output.copied_text
                && !ws
                    .hwnd(window)
                    .is_some_and(|hwnd| clipboard::write_text(hwnd, text))
            {
                log::debug!("cannot put the menu's copied text on the clipboard");
            }
            self.log_gl_errors();
            (
                output.waypoint_commands,
                output.browser_commands,
                browser_actions,
            )
        };
        waypoints::after_frame(waypoint_commands, self.window);
        browser::after_frame(
            browser_commands,
            browser_actions,
            &self.settings,
            &agent.game_dir,
        );
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

    /// Hands the settings to the saving thread once they are due, and takes in how the last
    /// save went. A failed save is told once and tried again later.
    fn save_settings_when_due(&mut self) {
        if let Some(saving) = &self.saving {
            let Some(result) = saving.poll() else {
                return;
            };
            self.saving = None;
            self.saved(result);
        }
        let Some((due, in_menu)) = self.unsaved else {
            return;
        };
        let menu_closed = in_menu && !input::router().ui_open();
        if Instant::now() < due && !menu_closed {
            return;
        }
        self.unsaved = None;
        if self.settings.unreadable {
            log::debug!("settings: not saved; the file could not be read");
            return;
        }
        match self.settings.to_json() {
            Ok(bytes) => self.saving = Some(saver::write(self.settings_path.clone(), bytes)),
            Err(e) => self.saved(Err(e)),
        }
    }

    fn saved(&mut self, result: std::io::Result<()>) {
        match result {
            Ok(()) => {
                if self.save_problem.take().is_some() {
                    log::info!("settings saved after an earlier failure");
                }
                log::info!(
                    "settings saved (menu {}, zoom {}, waypoint {}, navigate {}, ×{:.1}, high resolution {}, {})",
                    self.settings.menu_key,
                    self.settings.zoom_key,
                    self.settings.waypoint_key,
                    self.settings.navigate_key,
                    self.settings.zoom_factor,
                    self.settings.zoom_high_res,
                    rebinds_summary(&self.settings)
                );
            }
            Err(e) => {
                let problem = format!("設定を保存できなかった：{e}（あとで保存し直す）");
                if self.save_problem.as_ref() != Some(&problem) {
                    log::error!(
                        "cannot save settings to {}: {e}",
                        self.settings_path.display()
                    );
                    self.notices
                        .push(Notice::new(problem.clone(), true, NOTICE_SECONDS));
                    self.save_problem = Some(problem);
                }
                // A newer change is saved at its own time; else these again, later.
                self.unsaved
                    .get_or_insert((Instant::now() + SAVE_RETRY, false));
            }
        }
    }

    /// What the menu says about the settings file.
    fn settings_note(&self) -> Option<String> {
        let notes: Vec<&str> = [self.load_problem.as_deref(), self.save_problem.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        (!notes.is_empty()).then(|| notes.join("\n"))
    }

    /// Saves what is not saved yet before the overlay goes (it is made again from the file),
    /// waiting a little for it.
    fn flush_settings(&mut self) {
        if let Some(saving) = self.saving.take()
            && let Some(result) = saving.wait(SAVE_WAIT)
        {
            self.saved(result);
        }
        if self.unsaved.take().is_none() || self.settings.unreadable {
            return;
        }
        let result = self
            .settings
            .to_json()
            .map(|bytes| saver::write(self.settings_path.clone(), bytes))
            .map(|saving| saving.wait(SAVE_WAIT));
        match result {
            Ok(Some(result)) => self.saved(result),
            Ok(None) => log::warn!("settings: still saving as the overlay goes"),
            Err(e) => self.saved(Err(e)),
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

    /// Saves the settings, ends the zoom (with the game's context current, as in the swap),
    /// frees GL resources and deletes the context. Best effort: the DC may be gone.
    fn teardown(mut self: Box<Self>) {
        self.flush_settings();
        self.tall.release();
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

/// `path` for the screen, with the user's folders as `%APPDATA%` and the like: the status
/// can be on the screen while streaming, and the full path has the user's name.
fn screen_path(path: &std::path::Path) -> String {
    let text = path.display().to_string();
    for var in ["APPDATA", "LOCALAPPDATA", "USERPROFILE"] {
        let Some(dir) = std::env::var_os(var).filter(|dir| !dir.is_empty()) else {
            continue;
        };
        let dir = dir.to_string_lossy();
        let dir = dir.trim_end_matches('\\');
        if let Some(head) = text.get(..dir.len())
            && head.eq_ignore_ascii_case(dir)
            && (text.len() == dir.len() || text[dir.len()..].starts_with('\\'))
        {
            return format!("%{var}%{}", &text[dir.len()..]);
        }
    }
    text
}

/// The menu's note about a settings file that was not read as it is.
fn settings_problem(settings: &Settings) -> Option<String> {
    if let Some(moved) = &settings.moved_aside_to {
        let name = moved
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        return Some(format!(
            "settings.json の書き方に誤りがあったので reminedog/{name} に移し、既定の設定で起動した（直して settings.json に戻すと、次の起動で読み込む）"
        ));
    }
    settings.unreadable.then(|| {
        "settings.json を読み込めなかったので既定の設定で動いている（上書きしないよう、この起動中は設定を保存しない）"
            .to_owned()
    })
}

/// The key rebinding's rules in `settings` the router uses ([`resolve`]): less those the
/// window library cannot report the source of or give the game the output of.
fn rebind_rules(
    ws: &dyn WindowSystem,
    settings: &Settings,
    keys: &Hotkeys,
) -> Vec<(InputId, InputId)> {
    resolve(settings, keys, &unsupported_inputs(ws, settings))
}

/// The keys and buttons of the rules in `settings` that the window library cannot report (as
/// a source) or give the game (as an output).
fn unsupported_inputs(ws: &dyn WindowSystem, settings: &Settings) -> Vec<InputId> {
    let mut ids = Vec::new();
    for rule in &settings.rebinds {
        let from = input_by_name(&rule.from, Naming::Modern).filter(|&id| !ws.can_receive(id));
        let to = input_by_name(&rule.to, Naming::Modern).filter(|&id| !ws.can_send(id));
        for id in from.into_iter().chain(to) {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    ids
}

/// The rules the router uses in the end (keys may be left out there, see
/// [`WindowSystem::key_state_spoofed`]).
fn log_active_rebinds(rules: &[(InputId, InputId)]) {
    let list: Vec<String> = rules
        .iter()
        .map(|&(from, to)| format!("{} -> {}", input_name(from), input_name(to)))
        .collect();
    log::info!("rebinds: {} active ({})", rules.len(), list.join(", "));
}

/// `rebinds 2` or `rebinds off (2)`, for the settings' log lines.
fn rebinds_summary(settings: &Settings) -> String {
    if settings.rebinds_enabled {
        format!("rebinds {}", settings.rebinds.len())
    } else {
        format!("rebinds off ({})", settings.rebinds.len())
    }
}
