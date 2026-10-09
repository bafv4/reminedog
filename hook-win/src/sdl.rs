//! SDL3, which Minecraft 26.x opens its window with instead of GLFW (LWJGL 3.4).
//!
//! 26.x renders with OpenGL through SDL3 (its "RenderPearl OpenGL" backend), so the overlay
//! works as with GLFW: `SDL_GL_SwapWindow` is detoured and hands each frame to
//! [`frame::before_swap`]. `SDL_GetWindowSizeInPixels` reports the tall size during the
//! high-resolution zoom ([`crate::tall`]), and `SDL_WarpMouseInWindow` maps the game's warps
//! from that size back into the real window. The other detours only log how the game uses SDL3
//! (window flags, OpenGL context or Vulkan surface), for diagnosing future versions.
//!
//! Only one window library is hooked, the first loaded: a mod's SDL (controller support) in
//! a GLFW game is left alone.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use reminedog_core::{InputId, Naming, SCANCODE_COUNT};
use windows_sys::Win32::Foundation::{HMODULE, HWND};

use crate::ffi;
use crate::frame::{self, WindowSystem};
use crate::hook::{self, export};

type CreateWindowFn =
    unsafe extern "C" fn(title: *const c_char, w: c_int, h: c_int, flags: u64) -> *mut c_void;
type GlCreateContextFn = unsafe extern "C" fn(window: *mut c_void) -> *mut c_void;
type GlSwapWindowFn = unsafe extern "C" fn(window: *mut c_void) -> bool;
type VulkanCreateSurfaceFn = unsafe extern "C" fn(
    window: *mut c_void,
    instance: *mut c_void,
    allocator: *const c_void,
    surface: *mut u64,
) -> bool;
type GetVersionFn = unsafe extern "C" fn() -> c_int;
type GetWindowFlagsFn = unsafe extern "C" fn(window: *mut c_void) -> u64;
type GetWindowSizeInPixelsFn =
    unsafe extern "C" fn(window: *mut c_void, w: *mut c_int, h: *mut c_int) -> bool;
type GetWindowDisplayScaleFn = unsafe extern "C" fn(window: *mut c_void) -> f32;
type GetWindowPropertiesFn = unsafe extern "C" fn(window: *mut c_void) -> u32;
type GetPointerPropertyFn =
    unsafe extern "C" fn(props: u32, name: *const c_char, default: *mut c_void) -> *mut c_void;
type GetHintBooleanFn = unsafe extern "C" fn(name: *const c_char, default: bool) -> bool;
type WarpMouseInWindowFn = unsafe extern "C" fn(window: *mut c_void, x: f32, y: f32);

const WINDOW_OPENGL: u64 = 0x0000_0002;
const WINDOW_HIDDEN: u64 = 0x0000_0008;
const WINDOW_VULKAN: u64 = 0x1000_0000;

static CREATE_WINDOW: OnceLock<CreateWindowFn> = OnceLock::new();
static GL_CREATE_CONTEXT: OnceLock<GlCreateContextFn> = OnceLock::new();
static GL_SWAP_WINDOW: OnceLock<GlSwapWindowFn> = OnceLock::new();
/// Trampoline to the original SDL_GetWindowSizeInPixels (the overlay needs the real size).
static GET_WINDOW_SIZE_IN_PIXELS: OnceLock<GetWindowSizeInPixelsFn> = OnceLock::new();
static VULKAN_CREATE_SURFACE: OnceLock<VulkanCreateSurfaceFn> = OnceLock::new();
static WARP_MOUSE_IN_WINDOW: OnceLock<WarpMouseInWindowFn> = OnceLock::new();
static SDL: OnceLock<Sdl> = OnceLock::new();

static ATTACHED: Mutex<bool> = Mutex::new(false);
static RENDERER_LOGGED: AtomicBool = AtomicBool::new(false);
static SWAPS: AtomicU64 = AtomicU64::new(0);

/// SDL3 functions the agent calls itself.
struct Sdl {
    get_version: GetVersionFn,
    get_window_flags: GetWindowFlagsFn,
    get_window_size_in_pixels: GetWindowSizeInPixelsFn,
    get_window_display_scale: GetWindowDisplayScaleFn,
    get_window_properties: GetWindowPropertiesFn,
    get_pointer_property: GetPointerPropertyFn,
    get_hint_boolean: Option<GetHintBooleanFn>,
}

impl Sdl {
    fn resolve(module: HMODULE) -> Result<Sdl, String> {
        let get = |name: &CStr| {
            export(module, name)
                .ok_or_else(|| format!("SDL3 does not export {}", name.to_string_lossy()))
        };
        // SAFETY: SDL3 exports with these C signatures.
        unsafe {
            Ok(Sdl {
                get_version: std::mem::transmute::<*const c_void, GetVersionFn>(get(
                    c"SDL_GetVersion",
                )?),
                get_window_flags: std::mem::transmute::<*const c_void, GetWindowFlagsFn>(get(
                    c"SDL_GetWindowFlags",
                )?),
                get_window_size_in_pixels: std::mem::transmute::<
                    *const c_void,
                    GetWindowSizeInPixelsFn,
                >(get(c"SDL_GetWindowSizeInPixels")?),
                get_window_display_scale: std::mem::transmute::<
                    *const c_void,
                    GetWindowDisplayScaleFn,
                >(get(c"SDL_GetWindowDisplayScale")?),
                get_window_properties: std::mem::transmute::<*const c_void, GetWindowPropertiesFn>(
                    get(c"SDL_GetWindowProperties")?,
                ),
                get_pointer_property: std::mem::transmute::<*const c_void, GetPointerPropertyFn>(
                    get(c"SDL_GetPointerProperty")?,
                ),
                get_hint_boolean: export(module, c"SDL_GetHintBoolean")
                    .map(|f| std::mem::transmute::<*const c_void, GetHintBooleanFn>(f)),
            })
        }
    }

    fn version(&self) -> String {
        // SAFETY: SDL_GetVersion takes no arguments.
        let v = unsafe { (self.get_version)() };
        format!("{}.{}.{}", v / 1_000_000, v / 1000 % 1000, v % 1000)
    }
}

// Every call below gets the SDL_Window* SDL itself passed to SDL_GL_SwapWindow, on the
// thread that swaps it.
impl WindowSystem for Sdl {
    fn describe(&self) -> String {
        format!("SDL {}", self.version())
    }

    fn is_visible(&self, window: *mut c_void) -> bool {
        // SAFETY: see above.
        unsafe { (self.get_window_flags)(window) & WINDOW_HIDDEN == 0 }
    }

    /// SDL3 reads raw input in relative mouse mode unless the game asked it to warp the
    /// system pointer instead or to apply the system's pointer speed itself.
    fn raw_motion(&self, _window: *mut c_void) -> bool {
        let Some(get) = self.get_hint_boolean else {
            return true;
        };
        // SAFETY: NUL-terminated hint names; hints may be read from any thread.
        unsafe {
            !get(c"SDL_MOUSE_RELATIVE_MODE_WARP".as_ptr(), false)
                && !get(c"SDL_MOUSE_RELATIVE_SYSTEM_SCALE".as_ptr(), false)
        }
    }

    fn captured(&self, window: *mut c_void) -> bool {
        crate::sdl_input::captured(window)
    }

    fn naming(&self) -> Naming {
        Naming::Modern
    }

    fn key_state_spoofed(&self) -> bool {
        crate::sdl_input::key_state_spoofed()
    }

    /// Any scancode SDL3 knows, and the five buttons it reports on Windows.
    fn can_send(&self, id: InputId) -> bool {
        match id {
            InputId::Key(scancode) => (4..SCANCODE_COUNT).contains(&usize::from(scancode)),
            InputId::Mouse(button) => (1..=5).contains(&button),
        }
    }

    /// The same as [`can_send`](Self::can_send): the hooks take SDL3's scancodes and buttons
    /// as they are.
    fn can_receive(&self, id: InputId) -> bool {
        self.can_send(id)
    }

    fn framebuffer_size(&self, window: *mut c_void) -> (i32, i32) {
        let (mut w, mut h) = (0, 0);
        let get = GET_WINDOW_SIZE_IN_PIXELS
            .get()
            .copied()
            .unwrap_or(self.get_window_size_in_pixels);
        // SAFETY: see above.
        if unsafe { get(window, &mut w, &mut h) } {
            (w, h)
        } else {
            (0, 0)
        }
    }

    fn content_scale(&self, window: *mut c_void) -> f32 {
        // SAFETY: see above; 0 means failure.
        let scale = unsafe { (self.get_window_display_scale)(window) };
        if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        }
    }

    fn hwnd(&self, window: *mut c_void) -> Option<HWND> {
        // SAFETY: see above; the property name is NUL-terminated.
        let hwnd = unsafe {
            let props = (self.get_window_properties)(window);
            if props == 0 {
                return None;
            }
            (self.get_pointer_property)(
                props,
                c"SDL.window.win32.hwnd".as_ptr(),
                std::ptr::null_mut(),
            )
        };
        (!hwnd.is_null()).then_some(hwnd)
    }
}

/// Identifies SDL3 by its exports, like GLFW: those SDL2 has too, and two only SDL3 has.
pub fn is_sdl3(module: HMODULE) -> bool {
    [
        c"SDL_Init",
        c"SDL_CreateWindow",
        c"SDL_GL_SwapWindow",
        c"SDL_GetWindowProperties",
        c"SDL_GetPointerProperty",
    ]
    .iter()
    .all(|name| export(module, name).is_some())
}

/// Whether an SDL3 library was hooked.
pub fn attached() -> bool {
    SDL.get().is_some()
}

pub fn attach(module: HMODULE, path: &str) {
    let mut attached = ATTACHED.lock().unwrap_or_else(|e| e.into_inner());
    if *attached {
        log::warn!("a second SDL3 library was loaded and is ignored: {path}");
        return;
    }
    *attached = true;
    match Sdl::resolve(module) {
        Ok(sdl) => {
            let _ = SDL.set(sdl);
        }
        Err(e) => {
            // Not the SDL3 the game opens its window with: no detours at all.
            log::error!("SDL3 loaded ({path}), but the overlay cannot use it: {e}");
            return;
        }
    }
    log::info!("SDL3 loaded: {path}");
    // SAFETY (each call): the SDL3 export, its detour and the slot's fn type share one
    // signature, and nothing runs the functions while SDL3 is still being loaded.
    unsafe {
        probe(
            module,
            c"SDL_CreateWindow",
            create_window_detour as *const c_void,
            &CREATE_WINDOW,
        );
        probe(
            module,
            c"SDL_GL_CreateContext",
            gl_create_context_detour as *const c_void,
            &GL_CREATE_CONTEXT,
        );
        probe(
            module,
            c"SDL_GL_SwapWindow",
            gl_swap_window_detour as *const c_void,
            &GL_SWAP_WINDOW,
        );
        probe(
            module,
            c"SDL_GetWindowSizeInPixels",
            get_window_size_in_pixels_detour as *const c_void,
            &GET_WINDOW_SIZE_IN_PIXELS,
        );
        probe(
            module,
            c"SDL_WarpMouseInWindow",
            warp_mouse_in_window_detour as *const c_void,
            &WARP_MOUSE_IN_WINDOW,
        );
        probe(
            module,
            c"SDL_Vulkan_CreateSurface",
            vulkan_create_surface_detour as *const c_void,
            &VULKAN_CREATE_SURFACE,
        );
    }
    crate::sdl_input::install(module);
    log::info!("SDL3 hooks installed");
}

/// Detours one SDL3 export; failures are logged, the other probes still go in.
///
/// # Safety
/// As for [`hook::install`].
unsafe fn probe<F: Copy>(
    module: HMODULE,
    name: &CStr,
    detour: *const c_void,
    original: &OnceLock<F>,
) {
    let target = export(module, name);
    let name = name.to_string_lossy();
    let Some(target) = target else {
        log::warn!("SDL3 does not export {name}");
        return;
    };
    // SAFETY: guaranteed by the caller.
    if let Err(e) = unsafe { hook::install(&name, target, detour, original) } {
        log::warn!("cannot probe {name}: {e}");
    }
}

fn log_renderer(api: &str, how: &str) {
    if !RENDERER_LOGGED.swap(true, Ordering::Relaxed) {
        log::info!("renderer: {api} via SDL3 ({how})");
    } else {
        log::info!("{how} called again ({api})");
    }
}

unsafe extern "C" fn create_window_detour(
    title: *const c_char,
    w: c_int,
    h: c_int,
    flags: u64,
) -> *mut c_void {
    let Some(original) = CREATE_WINDOW.get() else {
        return std::ptr::null_mut();
    };
    // SAFETY: same arguments the caller passed us.
    let window = unsafe { original(title, w, h, flags) };
    ffi::catch("SDL_CreateWindow probe", || {
        let title = if title.is_null() {
            String::new()
        } else {
            // SAFETY: SDL window titles are NUL-terminated UTF-8.
            unsafe { CStr::from_ptr(title) }
                .to_string_lossy()
                .into_owned()
        };
        let mut kinds = Vec::new();
        for (bit, name) in [
            (WINDOW_OPENGL, "OpenGL"),
            (WINDOW_VULKAN, "Vulkan"),
            (WINDOW_HIDDEN, "hidden"),
        ] {
            if flags & bit != 0 {
                kinds.push(name);
            }
        }
        let version = SDL.get().map_or_else(|| "?".to_owned(), Sdl::version);
        log::info!(
            "SDL_CreateWindow({title:?}, {w}x{h}, flags 0x{flags:X} [{}]) -> {window:p} (SDL {version})",
            kinds.join(", ")
        );
    });
    window
}

unsafe extern "C" fn gl_create_context_detour(window: *mut c_void) -> *mut c_void {
    let Some(original) = GL_CREATE_CONTEXT.get() else {
        return std::ptr::null_mut();
    };
    // SAFETY: same arguments the caller passed us.
    let context = unsafe { original(window) };
    ffi::catch("SDL_GL_CreateContext probe", || {
        log_renderer("OpenGL", "SDL_GL_CreateContext");
        if context.is_null() {
            log::warn!("SDL_GL_CreateContext failed");
        }
    });
    context
}

unsafe extern "C" fn gl_swap_window_detour(window: *mut c_void) -> bool {
    let Some(original) = GL_SWAP_WINDOW.get() else {
        return false;
    };
    ffi::catch("SDL_GL_SwapWindow detour", || {
        let n = SWAPS.fetch_add(1, Ordering::Relaxed) + 1;
        if n == 1 {
            log::info!("SDL_GL_SwapWindow: first frame");
        }
        if let Some(sdl) = SDL.get() {
            frame::before_swap(sdl, window);
        }
    });
    // SAFETY: same arguments the caller passed us.
    let swapped = unsafe { original(window) };
    ffi::catch("rebinds", || crate::sdl_input::release_lost_keys(window));
    ffi::catch("hotkeys", crate::rebind_state::forget_lost_presses);
    ffi::catch("SDL input", || crate::sdl_input::after_swap(window));
    swapped
}

/// Reports the zoom's tall size to the game while zooming.
unsafe extern "C" fn get_window_size_in_pixels_detour(
    window: *mut c_void,
    w: *mut c_int,
    h: *mut c_int,
) -> bool {
    let Some(original) = GET_WINDOW_SIZE_IN_PIXELS.get() else {
        return false;
    };
    // SAFETY: the caller's arguments.
    let ok = unsafe { original(window, w, h) };
    if ok && let Some([tall_w, tall_h]) = crate::tall::size_override(window) {
        // SAFETY: SDL allows either pointer to be null.
        unsafe {
            if !w.is_null() {
                *w = tall_w;
            }
            if !h.is_null() {
                *h = tall_h;
            }
        }
    }
    ok
}

/// While the zoom tells the game a taller window, the game's warps (26.3 re-grabs the mouse
/// at the centre of the window it believes in whenever the window is resized) are scaled
/// back into the real window. Else the system's cursor lands below the window, and a click
/// there goes to another window: the game loses the focus and the mouse.
unsafe extern "C" fn warp_mouse_in_window_detour(window: *mut c_void, x: f32, y: f32) {
    let Some(original) = WARP_MOUSE_IN_WINDOW.get() else {
        return;
    };
    let y = ffi::catch("SDL_WarpMouseInWindow detour", || real_warp_y(window, y)).unwrap_or(y);
    // SAFETY: the caller's window; the coordinates are within it.
    unsafe { original(window, x, y) };
}

/// `y` in the real window, for a warp the game made in the tall one (as it is otherwise).
fn real_warp_y(window: *mut c_void, y: f32) -> f32 {
    let Some([_, tall_h]) = crate::tall::own_query(|| crate::tall::size_override(window)) else {
        return y;
    };
    let Some(real_size) = GET_WINDOW_SIZE_IN_PIXELS.get() else {
        return y;
    };
    let (mut w, mut h) = (0, 0);
    // SAFETY: the window SDL passed the caller; valid out parameters. The trampoline gives the
    // real size.
    if !unsafe { real_size(window, &mut w, &mut h) } || h <= 0 || tall_h <= 0 {
        return y;
    }
    // The tall size is k times the real one in pixels; the window units scale alike.
    let mapped = y * h as f32 / tall_h as f32;
    log::debug!("zoom: the game warped the mouse to y {y:.0}; warping to {mapped:.0}");
    mapped
}

unsafe extern "C" fn vulkan_create_surface_detour(
    window: *mut c_void,
    instance: *mut c_void,
    allocator: *const c_void,
    surface: *mut u64,
) -> bool {
    let Some(original) = VULKAN_CREATE_SURFACE.get() else {
        return false;
    };
    // SAFETY: same arguments the caller passed us.
    let ok = unsafe { original(window, instance, allocator, surface) };
    ffi::catch("SDL_Vulkan_CreateSurface probe", || {
        log_renderer("Vulkan", "SDL_Vulkan_CreateSurface");
        if !ok {
            log::warn!("SDL_Vulkan_CreateSurface failed");
        }
    });
    ok
}
