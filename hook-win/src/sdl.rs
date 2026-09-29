//! SDL3, which Minecraft 26.x opens its window with instead of GLFW (LWJGL 3.4).
//!
//! The overlay does not support SDL3 yet. For now these detours only record, in the log,
//! how the game uses it: the window flags and whether it creates an OpenGL context or a
//! Vulkan surface. That decides how SDL3 support has to be built.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use windows_sys::Win32::Foundation::HMODULE;

use crate::ffi;
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

const WINDOW_OPENGL: u64 = 0x0000_0002;
const WINDOW_HIDDEN: u64 = 0x0000_0008;
const WINDOW_VULKAN: u64 = 0x1000_0000;

static CREATE_WINDOW: OnceLock<CreateWindowFn> = OnceLock::new();
static GL_CREATE_CONTEXT: OnceLock<GlCreateContextFn> = OnceLock::new();
static GL_SWAP_WINDOW: OnceLock<GlSwapWindowFn> = OnceLock::new();
static VULKAN_CREATE_SURFACE: OnceLock<VulkanCreateSurfaceFn> = OnceLock::new();
static GET_VERSION: OnceLock<GetVersionFn> = OnceLock::new();

static ATTACHED: Mutex<bool> = Mutex::new(false);
static RENDERER_LOGGED: AtomicBool = AtomicBool::new(false);
static SWAPS: AtomicU64 = AtomicU64::new(0);

/// Identifies SDL3 by its exports, like GLFW.
pub fn is_sdl3(module: HMODULE) -> bool {
    [c"SDL_Init", c"SDL_CreateWindow", c"SDL_GL_SwapWindow"]
        .iter()
        .all(|name| export(module, name).is_some())
}

pub fn attach(module: HMODULE, path: &str) {
    let mut attached = ATTACHED.lock().unwrap_or_else(|e| e.into_inner());
    if *attached {
        log::warn!("a second SDL3 library was loaded and is ignored: {path}");
        return;
    }
    *attached = true;
    log::info!(
        "SDL3 loaded: {path}. The overlay does not support SDL3 (Minecraft 26.x) yet; \
         only recording which graphics API the game uses"
    );
    if let Some(f) = export(module, c"SDL_GetVersion") {
        // SAFETY: SDL_GetVersion has this signature.
        let _ = GET_VERSION.set(unsafe { std::mem::transmute::<*const c_void, GetVersionFn>(f) });
    }
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
            c"SDL_Vulkan_CreateSurface",
            vulkan_create_surface_detour as *const c_void,
            &VULKAN_CREATE_SURFACE,
        );
    }
    log::info!("SDL3 probes installed");
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
        let version = GET_VERSION.get().map_or_else(
            || "?".to_owned(),
            // SAFETY: SDL_GetVersion takes no arguments.
            |f| {
                let v = unsafe { f() };
                format!("{}.{}.{}", v / 1_000_000, v / 1000 % 1000, v % 1000)
            },
        );
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
    ffi::catch("SDL_GL_SwapWindow probe", || {
        let n = SWAPS.fetch_add(1, Ordering::Relaxed) + 1;
        if matches!(n, 1 | 100 | 1000) {
            log::info!("SDL_GL_SwapWindow: {n} frames");
        }
    });
    // SAFETY: same arguments the caller passed us.
    unsafe { original(window) }
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
