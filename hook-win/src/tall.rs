//! High-resolution zoom: the game renders a taller frame and the middle of it fills the
//! window.
//!
//! Minecraft's field of view is vertical, so a frame `k` times taller at the same width
//! packs `k` times more pixels into every degree; its middle, shown at the window's size,
//! is the view enlarged `k` times with real detail (the "tall" resolution speedrunners
//! use to measure ender eyes). While the zoom key is held:
//!
//! 1. The game is told its framebuffer is that tall size (GLFW: the framebuffer size
//!    callback and `glfwGetFramebufferSize`; SDL3: a pixel-size event and
//!    `SDL_GetWindowSizeInPixels`). It resizes its render targets as for any resize.
//! 2. The game's own output to the window (framebuffer 0) goes to a framebuffer of ours of
//!    that size instead: `wglGetProcAddress` is detoured to hand the game wrappers of
//!    `glBindFramebuffer(EXT)` and `glBlitNamedFramebuffer` that substitute ours for 0.
//! 3. At the buffer swap, the middle rows of our framebuffer are copied into the window.
//!
//! `glViewport` is wrapped too, only to confirm that the game really renders at the tall
//! size; if it does not, the zoom falls back to enlarging the pixels.

use std::ffi::{CStr, c_char, c_void};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use glow::HasContext as _;
use reminedog_render::{ZoomView, middle_row, tall_size};
use windows_sys::Win32::Foundation::HMODULE;

use crate::ffi;
use crate::hook::{self, export};
use crate::wgl;

type GetProcAddressFn = unsafe extern "system" fn(*const c_char) -> *const c_void;
type BindFramebufferFn = unsafe extern "system" fn(target: u32, framebuffer: u32);
type BlitNamedFramebufferFn = unsafe extern "system" fn(
    read: u32,
    draw: u32,
    src_x0: i32,
    src_y0: i32,
    src_x1: i32,
    src_y1: i32,
    dst_x0: i32,
    dst_y0: i32,
    dst_x1: i32,
    dst_y1: i32,
    mask: u32,
    filter: u32,
);
type ViewportFn = unsafe extern "system" fn(x: i32, y: i32, width: i32, height: i32);

const GL_FRAMEBUFFER: u32 = 0x8D40;
const GL_READ_FRAMEBUFFER: u32 = 0x8CA8;
const GL_DRAW_FRAMEBUFFER: u32 = 0x8CA9;

/// Trampoline to the real `wglGetProcAddress`.
static GET_PROC_ADDRESS: OnceLock<GetProcAddressFn> = OnceLock::new();
static BIND_FRAMEBUFFER: OnceLock<BindFramebufferFn> = OnceLock::new();
static BIND_FRAMEBUFFER_EXT: OnceLock<BindFramebufferFn> = OnceLock::new();
static BLIT_NAMED_FRAMEBUFFER: OnceLock<BlitNamedFramebufferFn> = OnceLock::new();
static VIEWPORT: OnceLock<ViewportFn> = OnceLock::new();
/// The game got our `glViewport`, so the tall size can be confirmed.
static VIEWPORT_WRAPPED: AtomicBool = AtomicBool::new(false);
static ATTACHED: Mutex<bool> = Mutex::new(false);

/// Our framebuffer standing in for 0 in the game's context, or 0 when not zooming.
static REDIRECT: AtomicU32 = AtomicU32::new(0);
static GAME_CONTEXT: AtomicUsize = AtomicUsize::new(0);
/// The game bound (or blitted to) framebuffer 0 since the last frame, getting ours.
static REDIRECTED: AtomicBool = AtomicBool::new(false);
/// The tall size (width << 32 | height) the game should set as its viewport, and whether
/// it did since the last frame. Only the height is compared: the game may keep a width of
/// its own (26.3 renders 2560 wide in a window SDL reports as 2561 pixels wide).
static TALL_VIEWPORT: AtomicU64 = AtomicU64::new(0);
static TALL_VIEWPORT_SEEN: AtomicBool = AtomicBool::new(false);
/// For the log while zooming: the game's `glViewport` calls since the last frame, the
/// tallest of them (height << 32 | width), and how often it asked for the window's size.
static VIEWPORT_CALLS: AtomicU32 = AtomicU32::new(0);
static VIEWPORT_TALLEST: AtomicU64 = AtomicU64::new(0);
static SIZE_QUERIES: AtomicU32 = AtomicU32::new(0);

/// The size the game is told while zooming, and the window it applies to.
static SIZE_OVERRIDE: Mutex<Option<(usize, [i32; 2])>> = Mutex::new(None);
/// A size the game still has to be told (after the next swap for GLFW, in the next
/// `SDL_PollEvent` for SDL3).
static PENDING: Mutex<Option<(usize, [i32; 2])>> = Mutex::new(None);
/// The real window was resized while the game believed the tall size.
static REAL_RESIZED: AtomicBool = AtomicBool::new(false);

pub fn is_opengl32(path: &str) -> bool {
    path.rsplit(['\\', '/'])
        .next()
        .is_some_and(|name| name.eq_ignore_ascii_case("opengl32.dll"))
}

/// Detours `wglGetProcAddress` when opengl32.dll loads, before the game looks up any GL
/// function.
pub fn attach(module: HMODULE) {
    let mut attached = ATTACHED.lock().unwrap_or_else(|e| e.into_inner());
    if *attached {
        return;
    }
    *attached = true;
    let Some(target) = export(module, c"wglGetProcAddress") else {
        log::warn!("opengl32.dll does not export wglGetProcAddress; no high-resolution zoom");
        return;
    };
    // SAFETY: the export, the detour and the slot share wglGetProcAddress's signature;
    // opengl32.dll is still being loaded, so nobody runs it yet.
    match unsafe {
        hook::install(
            "wglGetProcAddress",
            target,
            get_proc_address_detour as *const c_void,
            &GET_PROC_ADDRESS,
        )
    } {
        Ok(()) => log::info!("OpenGL hooks installed"),
        Err(e) => log::warn!("no high-resolution zoom: {e}"),
    }
}

/// The real `wglGetProcAddress`, for the overlay's own context.
pub fn original_get_proc_address() -> Option<GetProcAddressFn> {
    GET_PROC_ADDRESS.get().copied()
}

fn valid(p: *const c_void) -> bool {
    // Some drivers return small sentinel values instead of null.
    !matches!(p as isize, -1..=3)
}

unsafe extern "system" fn get_proc_address_detour(name: *const c_char) -> *const c_void {
    let Some(original) = GET_PROC_ADDRESS.get() else {
        return std::ptr::null();
    };
    // SAFETY: the caller's argument.
    let real = unsafe { original(name) };
    if name.is_null() {
        return real;
    }
    ffi::catch("wglGetProcAddress detour", || {
        // SAFETY: GL function names are NUL-terminated.
        let name = unsafe { CStr::from_ptr(name) };
        wrap(name, real)
    })
    .unwrap_or(real)
}

/// Our wrapper for `name` if we have one, with `real` stored as the function it calls.
fn wrap(name: &CStr, real: *const c_void) -> *const c_void {
    /// Stores the first real address; the ICD returns the same one for every context.
    fn keep<F: Copy>(slot: &OnceLock<F>, name: &CStr, real: *const c_void) -> bool {
        // SAFETY: `F` is the fn type of `name`, and `real` its address.
        let f = unsafe { std::mem::transmute_copy::<*const c_void, F>(&real) };
        if slot.set(f).is_err() {
            // SAFETY: as above, both are fn pointers of the same size.
            let first =
                unsafe { std::mem::transmute_copy::<F, *const c_void>(slot.get().unwrap()) };
            if first != real {
                log::warn!("{name:?} has another address in this context; still using the first");
            }
        }
        true
    }
    match name.to_bytes() {
        b"glBindFramebuffer" if valid(real) => {
            keep(&BIND_FRAMEBUFFER, name, real);
            bind_framebuffer_wrapper as *const c_void
        }
        b"glBindFramebufferEXT" if valid(real) => {
            keep(&BIND_FRAMEBUFFER_EXT, name, real);
            bind_framebuffer_ext_wrapper as *const c_void
        }
        b"glBlitNamedFramebuffer" if valid(real) => {
            keep(&BLIT_NAMED_FRAMEBUFFER, name, real);
            blit_named_framebuffer_wrapper as *const c_void
        }
        b"glViewport" => {
            // A GL 1.1 function: most drivers return null here and the caller falls back
            // to opengl32's export; hand out the wrapper either way.
            let real = if valid(real) {
                real
            } else {
                match wgl::get() {
                    Ok(wgl) => wgl.export(name),
                    Err(_) => std::ptr::null(),
                }
            };
            if !valid(real) {
                return real;
            }
            keep(&VIEWPORT, name, real);
            VIEWPORT_WRAPPED.store(true, Ordering::Relaxed);
            viewport_wrapper as *const c_void
        }
        _ => real,
    }
}

/// Our framebuffer in place of 0, while zooming and in the game's context.
fn redirected(framebuffer: u32) -> u32 {
    if framebuffer != 0 {
        return framebuffer;
    }
    let ours = REDIRECT.load(Ordering::Relaxed);
    if ours == 0 {
        return 0;
    }
    let current = wgl::get().map_or(0, |wgl| wgl.current().1 as usize);
    if current == 0 || current != GAME_CONTEXT.load(Ordering::Relaxed) {
        return 0;
    }
    REDIRECTED.store(true, Ordering::Relaxed);
    ours
}

unsafe extern "system" fn bind_framebuffer_wrapper(target: u32, framebuffer: u32) {
    let framebuffer = if matches!(
        target,
        GL_FRAMEBUFFER | GL_DRAW_FRAMEBUFFER | GL_READ_FRAMEBUFFER
    ) {
        redirected(framebuffer)
    } else {
        framebuffer
    };
    if let Some(real) = BIND_FRAMEBUFFER.get() {
        // SAFETY: the caller's arguments, with our framebuffer for 0.
        unsafe { real(target, framebuffer) }
    }
}

unsafe extern "system" fn bind_framebuffer_ext_wrapper(target: u32, framebuffer: u32) {
    let framebuffer = if target == GL_FRAMEBUFFER {
        redirected(framebuffer)
    } else {
        framebuffer
    };
    if let Some(real) = BIND_FRAMEBUFFER_EXT.get() {
        // SAFETY: as above.
        unsafe { real(target, framebuffer) }
    }
}

#[allow(clippy::too_many_arguments)]
unsafe extern "system" fn blit_named_framebuffer_wrapper(
    read: u32,
    draw: u32,
    src_x0: i32,
    src_y0: i32,
    src_x1: i32,
    src_y1: i32,
    dst_x0: i32,
    dst_y0: i32,
    dst_x1: i32,
    dst_y1: i32,
    mask: u32,
    filter: u32,
) {
    if let Some(real) = BLIT_NAMED_FRAMEBUFFER.get() {
        // SAFETY: as above.
        unsafe {
            real(
                redirected(read),
                redirected(draw),
                src_x0,
                src_y0,
                src_x1,
                src_y1,
                dst_x0,
                dst_y0,
                dst_x1,
                dst_y1,
                mask,
                filter,
            )
        }
    }
}

unsafe extern "system" fn viewport_wrapper(x: i32, y: i32, width: i32, height: i32) {
    let tall = TALL_VIEWPORT.load(Ordering::Relaxed);
    if tall != 0 {
        if tall & 0xffff_ffff == pack([0, height]) {
            TALL_VIEWPORT_SEEN.store(true, Ordering::Relaxed);
        }
        VIEWPORT_CALLS.fetch_add(1, Ordering::Relaxed);
        VIEWPORT_TALLEST.fetch_max(pack([height, width]), Ordering::Relaxed);
    }
    if let Some(real) = VIEWPORT.get() {
        // SAFETY: the caller's arguments.
        unsafe { real(x, y, width, height) }
    }
}

fn pack([w, h]: [i32; 2]) -> u64 {
    (u64::from(w as u32) << 32) | u64::from(h as u32)
}

/// The size to report to the game for `window` instead of the real one, while zooming.
pub fn size_override(window: *mut c_void) -> Option<[i32; 2]> {
    let current = *SIZE_OVERRIDE.lock().unwrap_or_else(|e| e.into_inner());
    let size = current.and_then(|(w, size)| (w == window as usize).then_some(size));
    if size.is_some() {
        SIZE_QUERIES.fetch_add(1, Ordering::Relaxed);
    }
    size
}

/// The window system reports a real resize of `window`. Returns true if the game must not
/// see it: it believes the tall size until the zoom ends (at the next swap), and is then
/// told the new real size.
pub fn real_resize(window: *mut c_void) -> bool {
    if size_override(window).is_some() {
        REAL_RESIZED.store(true, Ordering::Relaxed);
        true
    } else {
        false
    }
}

/// The size the game has to be told now, if any.
pub fn take_pending() -> Option<(*mut c_void, [i32; 2])> {
    PENDING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .map(|(window, size)| (window as *mut c_void, size))
}

fn set_size(window: *mut c_void, size: Option<[i32; 2]>, tell: [i32; 2]) {
    *SIZE_OVERRIDE.lock().unwrap_or_else(|e| e.into_inner()) =
        size.map(|size| (window as usize, size));
    *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = Some((window as usize, tell));
}

/// Frames in a row the game may fail to render at the tall size (the resize is picked up
/// with a frame of delay on some paths) before the high-resolution zoom is given up.
const MAX_MISSES: u32 = 3;
/// ...and for how long. Recreating the render targets at the tall size can stall the game
/// for several frames, so a frame count alone gives up too early on real hardware.
const MISS_TIMEOUT: Duration = Duration::from_secs(1);

/// Per-window state, used in the swap detour with the game's context current.
#[derive(Default)]
pub struct TallZoom {
    game: Option<GameGl>,
    active: Option<Active>,
    misses: u32,
    /// When the current run of misses began.
    first_miss: Option<Instant>,
    /// Why the high-resolution zoom is not available, for the menu.
    failure: Option<String>,
}

/// The overlay is going away (its drawable changed, or a frame panicked) mid-zoom: stop
/// redirecting and give the game its real size back. Without the game's context current
/// the bindings cannot be fixed up here; the game rebinds its own framebuffers every frame.
impl Drop for TallZoom {
    fn drop(&mut self) {
        if let Some(active) = self.active.take() {
            REDIRECT.store(0, Ordering::Relaxed);
            TALL_VIEWPORT.store(0, Ordering::Relaxed);
            set_size(active.window as *mut c_void, None, active.real);
            log::debug!("zoom: ended with the overlay");
        }
    }
}

/// GL objects in the game's context.
struct GameGl {
    context: usize,
    gl: glow::Context,
    max_dim: u32,
    target: Option<Target>,
}

struct Target {
    framebuffer: glow::Framebuffer,
    color: glow::Renderbuffer,
    depth: glow::Renderbuffer,
    size: [i32; 2],
}

struct Active {
    window: usize,
    real: [i32; 2],
    tall: [i32; 2],
    factor: f32,
}

impl TallZoom {
    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    /// Called in the swap detour before the overlay draws, with the game's context
    /// current. Shows the zoomed frame if the game rendered one, starts or ends the zoom,
    /// and says how the overlay should show the zoom this frame.
    pub fn frame(
        &mut self,
        window: *mut c_void,
        real: [i32; 2],
        want: bool,
        factor: f32,
    ) -> ZoomView {
        let fallback = if want {
            ZoomView::Magnify
        } else {
            ZoomView::Off
        };
        let Ok(wgl) = wgl::get() else {
            return fallback;
        };
        let context = wgl.current().1 as usize;
        if let Some(active) = &self.active {
            if context == 0 || context != GAME_CONTEXT.load(Ordering::Relaxed) {
                // Some other context is swapping; leave the zoom as it is.
                return fallback;
            }
            let redirected = REDIRECTED.swap(false, Ordering::Relaxed);
            let tall = TALL_VIEWPORT_SEEN.swap(false, Ordering::Relaxed)
                || !VIEWPORT_WRAPPED.load(Ordering::Relaxed);
            let viewport_calls = VIEWPORT_CALLS.swap(0, Ordering::Relaxed);
            let tallest = VIEWPORT_TALLEST.swap(0, Ordering::Relaxed);
            let size_queries = SIZE_QUERIES.swap(0, Ordering::Relaxed);
            let seen = || {
                let (h, w) = (tallest >> 32, tallest & 0xffff_ffff);
                let mut current = [0; 4];
                if let Some(game) = &self.game {
                    // SAFETY: the game's context is current; a query changes no state.
                    unsafe {
                        game.gl
                            .get_parameter_i32_slice(glow::VIEWPORT, &mut current)
                    };
                }
                format!(
                    "glViewport calls: {viewport_calls}, tallest {w}x{h}, viewport now {}x{}, size queries: {size_queries}",
                    current[2], current[3]
                )
            };
            let factor = active.factor;
            let view = if redirected && tall {
                self.misses = 0;
                self.first_miss = None;
                self.compose();
                ZoomView::HighRes { factor }
            } else {
                self.misses += 1;
                let first_miss = *self.first_miss.get_or_insert_with(Instant::now);
                if log::log_enabled!(log::Level::Debug) {
                    log::debug!(
                        "zoom: frame not rendered at the tall size (output redirected: {redirected}, tall viewport: {tall}, {} frames in {:.0?}; {})",
                        self.misses,
                        first_miss.elapsed(),
                        seen()
                    );
                }
                fallback
            };
            let resized = REAL_RESIZED.swap(false, Ordering::Relaxed) || real != active.real;
            if self.misses >= MAX_MISSES
                && self
                    .first_miss
                    .is_some_and(|first| first.elapsed() >= MISS_TIMEOUT)
            {
                log::warn!(
                    "zoom: the game does not render at the tall size; enlarging pixels instead (last frame: output redirected: {redirected}, tall viewport: {tall}; {} frames; {})",
                    self.misses,
                    seen()
                );
                self.failure = Some("ゲームが縦長の解像度で描かなかった".into());
            }
            if !want || resized || self.failure.is_some() {
                self.stop(real);
            }
            return view;
        }
        if !want || self.failure.is_some() {
            return fallback;
        }
        if context == 0 {
            return fallback;
        }
        if BIND_FRAMEBUFFER.get().is_none() && BIND_FRAMEBUFFER_EXT.get().is_none() {
            self.failure = Some("ゲームの OpenGL の関数をフックできなかった".into());
            log::warn!(
                "zoom: the game did not look up glBindFramebuffer through wglGetProcAddress"
            );
            return fallback;
        }
        if let Err(e) = self.start(window, context, real, factor) {
            log::warn!("zoom: no high resolution: {e}");
            self.failure = Some(e);
        }
        // This frame is still the normal one; the tall frames start with the next.
        fallback
    }

    fn start(
        &mut self,
        window: *mut c_void,
        context: usize,
        real: [i32; 2],
        factor: f32,
    ) -> Result<(), String> {
        let game = self.game_gl(context)?;
        let size = [real[0].max(0) as u32, real[1].max(0) as u32];
        let Some(([w, h], factor)) = tall_size(size, factor, game.max_dim) else {
            // Nothing to gain (factor near 1, or the window already as tall as the GPU
            // allows); enlarge pixels this time.
            return Ok(());
        };
        let tall = [w as i32, h as i32];
        let framebuffer = game.ensure_target(tall)?;
        let id = framebuffer.0.get();
        GAME_CONTEXT.store(context, Ordering::Relaxed);
        REDIRECTED.store(false, Ordering::Relaxed);
        TALL_VIEWPORT.store(pack(tall), Ordering::Relaxed);
        TALL_VIEWPORT_SEEN.store(false, Ordering::Relaxed);
        VIEWPORT_CALLS.store(0, Ordering::Relaxed);
        VIEWPORT_TALLEST.store(0, Ordering::Relaxed);
        SIZE_QUERIES.store(0, Ordering::Relaxed);
        REDIRECT.store(id, Ordering::Relaxed);
        // "0" means ours from now on; if 0 is bound right now (the game's view of it is
        // cached), bind ours in its place.
        game.swap_binding(None, Some(framebuffer));
        set_size(window, Some(tall), tall);
        self.misses = 0;
        self.first_miss = None;
        self.active = Some(Active {
            window: window as usize,
            real,
            tall,
            factor,
        });
        log::debug!(
            "zoom: game renders at {}x{} (×{factor:.2})",
            tall[0],
            tall[1]
        );
        Ok(())
    }

    fn stop(&mut self, real: [i32; 2]) {
        let Some(active) = self.active.take() else {
            return;
        };
        REDIRECT.store(0, Ordering::Relaxed);
        TALL_VIEWPORT.store(0, Ordering::Relaxed);
        if let Some(game) = &self.game
            && let Some(target) = &game.target
        {
            game.swap_binding(Some(target.framebuffer), None);
        }
        set_size(active.window as *mut c_void, None, real);
        log::debug!("zoom: game back at {}x{}", real[0], real[1]);
    }

    /// The middle of our framebuffer into the window, at 1:1.
    fn compose(&self) {
        let (Some(game), Some(active)) = (&self.game, &self.active) else {
            return;
        };
        let Some(target) = &game.target else {
            return;
        };
        let gl = &game.gl;
        let [w, h] = active.real;
        let y0 = middle_row(active.tall[1] as u32, h as u32) as i32;
        // SAFETY: the game's context is current (checked by the caller); every state
        // touched is restored.
        unsafe {
            let saved = SavedState::save(gl);
            gl.disable(glow::SCISSOR_TEST);
            gl.color_mask(true, true, true, true);
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(target.framebuffer));
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, None);
            gl.blit_framebuffer(
                0,
                y0,
                w,
                y0 + h,
                0,
                0,
                w,
                h,
                glow::COLOR_BUFFER_BIT,
                glow::NEAREST,
            );
            saved.restore(gl);
        }
    }

    fn game_gl(&mut self, context: usize) -> Result<&mut GameGl, String> {
        if self.game.as_ref().is_some_and(|g| g.context != context) {
            // A new context (the old one's objects went with it).
            self.game = None;
        }
        if self.game.is_none() {
            let wgl = wgl::get()?;
            // SAFETY: the game's context is current; the loader returns its functions.
            let gl = unsafe { glow::Context::from_loader_function_cstr(|name| wgl.load(name)) };
            let version = gl.version();
            if version.major < 3 {
                return Err(format!(
                    "OpenGL {}.{} では使えない（3.0 以降が必要）",
                    version.major, version.minor
                ));
            }
            // SAFETY: plain queries.
            let max_dim = unsafe {
                let mut viewport = [0; 2];
                gl.get_parameter_i32_slice(glow::MAX_VIEWPORT_DIMS, &mut viewport);
                [
                    gl.get_parameter_i32(glow::MAX_TEXTURE_SIZE),
                    gl.get_parameter_i32(glow::MAX_RENDERBUFFER_SIZE),
                    viewport[0].min(viewport[1]),
                ]
                .into_iter()
                .min()
                .unwrap_or(0)
                .max(0) as u32
            };
            log::info!("zoom: largest tall size {max_dim} px");
            self.game = Some(GameGl {
                context,
                gl,
                max_dim,
                target: None,
            });
        }
        Ok(self.game.as_mut().expect("just set"))
    }
}

impl GameGl {
    /// Our framebuffer at `size` (colour and depth-stencil renderbuffers).
    fn ensure_target(&mut self, size: [i32; 2]) -> Result<glow::Framebuffer, String> {
        if let Some(target) = &self.target {
            if target.size == size {
                return Ok(target.framebuffer);
            }
            let target = self.target.take().expect("checked");
            // SAFETY: our objects, in the game's context (current).
            unsafe {
                self.gl.delete_framebuffer(target.framebuffer);
                self.gl.delete_renderbuffer(target.color);
                self.gl.delete_renderbuffer(target.depth);
            }
        }
        let gl = &self.gl;
        // SAFETY: the game's context is current; its bindings are restored.
        unsafe {
            let saved = SavedState::save(gl);
            let saved_renderbuffer = gl.get_parameter_i32(glow::RENDERBUFFER_BINDING);
            let result = (|| {
                let color = gl.create_renderbuffer()?;
                gl.bind_renderbuffer(glow::RENDERBUFFER, Some(color));
                gl.renderbuffer_storage(glow::RENDERBUFFER, glow::RGBA8, size[0], size[1]);
                let depth = gl.create_renderbuffer()?;
                gl.bind_renderbuffer(glow::RENDERBUFFER, Some(depth));
                gl.renderbuffer_storage(
                    glow::RENDERBUFFER,
                    glow::DEPTH24_STENCIL8,
                    size[0],
                    size[1],
                );
                let framebuffer = gl.create_framebuffer()?;
                gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(framebuffer));
                gl.framebuffer_renderbuffer(
                    glow::DRAW_FRAMEBUFFER,
                    glow::COLOR_ATTACHMENT0,
                    glow::RENDERBUFFER,
                    Some(color),
                );
                gl.framebuffer_renderbuffer(
                    glow::DRAW_FRAMEBUFFER,
                    glow::DEPTH_STENCIL_ATTACHMENT,
                    glow::RENDERBUFFER,
                    Some(depth),
                );
                let status = gl.check_framebuffer_status(glow::DRAW_FRAMEBUFFER);
                Ok::<_, String>((framebuffer, color, depth, status))
            })();
            gl.bind_renderbuffer(
                glow::RENDERBUFFER,
                std::num::NonZeroU32::new(saved_renderbuffer as u32).map(glow::NativeRenderbuffer),
            );
            saved.restore(gl);
            let (framebuffer, color, depth, status) =
                result.map_err(|e| format!("フレームバッファを作れなかった（{e}）"))?;
            if status != glow::FRAMEBUFFER_COMPLETE {
                gl.delete_framebuffer(framebuffer);
                gl.delete_renderbuffer(color);
                gl.delete_renderbuffer(depth);
                return Err(format!(
                    "{}×{} のフレームバッファを作れなかった（0x{status:X}）",
                    size[0], size[1]
                ));
            }
            self.target = Some(Target {
                framebuffer,
                color,
                depth,
                size,
            });
            Ok(framebuffer)
        }
    }

    /// Where `from` is bound for drawing or reading, binds `to` instead (with the real
    /// function, not the game's wrapper).
    fn swap_binding(&self, from: Option<glow::Framebuffer>, to: Option<glow::Framebuffer>) {
        let from = from.map_or(0, |f| f.0.get() as i32);
        let gl = &self.gl;
        // SAFETY: the game's context is current.
        unsafe {
            for (binding, target) in [
                (glow::DRAW_FRAMEBUFFER_BINDING, glow::DRAW_FRAMEBUFFER),
                (glow::READ_FRAMEBUFFER_BINDING, glow::READ_FRAMEBUFFER),
            ] {
                if gl.get_parameter_i32(binding) == from {
                    gl.bind_framebuffer(target, to);
                }
            }
        }
    }
}

/// Game state the blits change.
struct SavedState {
    draw: i32,
    read: i32,
    scissor: bool,
    color_mask: [bool; 4],
}

impl SavedState {
    unsafe fn save(gl: &glow::Context) -> Self {
        // SAFETY: plain queries on the current context.
        unsafe {
            Self {
                draw: gl.get_parameter_i32(glow::DRAW_FRAMEBUFFER_BINDING),
                read: gl.get_parameter_i32(glow::READ_FRAMEBUFFER_BINDING),
                scissor: gl.is_enabled(glow::SCISSOR_TEST),
                color_mask: gl.get_parameter_bool_array::<4>(glow::COLOR_WRITEMASK),
            }
        }
    }

    unsafe fn restore(&self, gl: &glow::Context) {
        let framebuffer =
            |id: i32| std::num::NonZeroU32::new(id as u32).map(glow::NativeFramebuffer);
        // SAFETY: restores what `save` read, on the same context.
        unsafe {
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, framebuffer(self.draw));
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, framebuffer(self.read));
            if self.scissor {
                gl.enable(glow::SCISSOR_TEST);
            }
            let [r, g, b, a] = self.color_mask;
            gl.color_mask(r, g, b, a);
        }
    }
}
