//! The GLFW library LWJGL loads, and the detours on it.
//!
//! LWJGL resolves GLFW's exports once with `GetProcAddress` and calls through the saved
//! pointers, so patching the functions' first instructions (inline detour) is the only
//! way to see those calls; import-table hooks would not.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::{Mutex, OnceLock};

use minhook::MinHook;
use windows_sys::Win32::Foundation::{HMODULE, HWND};

use crate::ffi;

type SwapBuffersFn = unsafe extern "C" fn(window: *mut c_void);
type GetFramebufferSizeFn =
    unsafe extern "C" fn(window: *mut c_void, width: *mut c_int, height: *mut c_int);
type GetWindowContentScaleFn =
    unsafe extern "C" fn(window: *mut c_void, xscale: *mut f32, yscale: *mut f32);
type GetVersionStringFn = unsafe extern "C" fn() -> *const c_char;
type GetWindowAttribFn = unsafe extern "C" fn(window: *mut c_void, attrib: c_int) -> c_int;
type GetWin32WindowFn = unsafe extern "C" fn(window: *mut c_void) -> HWND;

const GLFW_VISIBLE: c_int = 0x0002_0004;

/// GLFW functions the agent calls itself.
pub struct Glfw {
    pub path: String,
    get_framebuffer_size: GetFramebufferSizeFn,
    /// GLFW 3.3+.
    get_window_content_scale: Option<GetWindowContentScaleFn>,
    get_version_string: Option<GetVersionStringFn>,
    get_window_attrib: GetWindowAttribFn,
    /// Native access export; LWJGL's builds have it.
    get_win32_window: Option<GetWin32WindowFn>,
}

impl Glfw {
    /// Framebuffer size in pixels; call on the thread that owns the window.
    pub fn framebuffer_size(&self, window: *mut c_void) -> (i32, i32) {
        let (mut width, mut height) = (0, 0);
        // SAFETY: `window` is the handle GLFW itself passed to glfwSwapBuffers.
        unsafe { (self.get_framebuffer_size)(window, &mut width, &mut height) };
        (width, height)
    }

    /// Content scale (DPI scale), 1.0 when GLFW is too old to report it.
    pub fn content_scale(&self, window: *mut c_void) -> f32 {
        let Some(f) = self.get_window_content_scale else {
            return 1.0;
        };
        let (mut x, mut y) = (1.0, 1.0);
        // SAFETY: as above.
        unsafe { f(window, &mut x, &mut y) };
        if x.is_finite() && x > 0.0 { x } else { 1.0 }
    }

    /// The window's HWND, if this GLFW exports native access.
    pub fn win32_window(&self, window: *mut c_void) -> Option<HWND> {
        let f = self.get_win32_window?;
        // SAFETY: as above.
        let hwnd = unsafe { f(window) };
        (!hwnd.is_null()).then_some(hwnd)
    }

    /// Whether the window is shown (mods may create hidden helper windows).
    pub fn is_visible(&self, window: *mut c_void) -> bool {
        // SAFETY: as above.
        unsafe { (self.get_window_attrib)(window, GLFW_VISIBLE) != 0 }
    }

    pub fn version_string(&self) -> String {
        match self.get_version_string {
            // SAFETY: returns a static NUL-terminated string; safe to call any time.
            Some(f) => unsafe { CStr::from_ptr(f()) }
                .to_string_lossy()
                .into_owned(),
            None => "?".into(),
        }
    }
}

static GLFW: OnceLock<Glfw> = OnceLock::new();
/// Trampoline to the original glfwSwapBuffers.
static SWAP_BUFFERS: OnceLock<SwapBuffersFn> = OnceLock::new();
/// Serializes `attach`; the loader lock already does, but don't rely on it.
static ATTACH: Mutex<()> = Mutex::new(());

/// `None` until a GLFW library was loaded and hooked.
pub fn get() -> Option<&'static Glfw> {
    GLFW.get()
}

/// Identifies GLFW by its exports rather than its file name, so a renamed or
/// launcher-provided GLFW (Prism's "use system GLFW") is recognized as well.
pub fn is_glfw(module: HMODULE) -> bool {
    export(module, c"glfwInit").is_some() && export(module, c"glfwSwapBuffers").is_some()
}

/// Export lookup that is safe while the module is still being loaded.
fn export(module: HMODULE, name: &CStr) -> Option<*const c_void> {
    // SAFETY: `module` comes from the loader (notification or module list), so it is
    // the base of a mapped PE image.
    unsafe { ffi::export_address(module, name) }
}

pub fn attach(module: HMODULE, path: &str) {
    let _guard = ATTACH.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = GLFW.get() {
        log::warn!(
            "a second GLFW library was loaded and is ignored: {path} (hooked: {})",
            existing.path
        );
        return;
    }
    match install(module, path) {
        Ok(glfw) => {
            log::info!("GLFW hooks installed: {path}");
            let _ = GLFW.set(glfw);
        }
        Err(e) => log::error!("cannot hook GLFW ({path}): {e}"),
    }
}

fn install(module: HMODULE, path: &str) -> Result<Glfw, String> {
    let resolve = |name: &CStr| {
        export(module, name).ok_or_else(|| format!("{} not exported", name.to_string_lossy()))
    };
    let swap_buffers = resolve(c"glfwSwapBuffers")?;
    // SAFETY: exported GLFW functions with these C signatures.
    let glfw = unsafe {
        Glfw {
            path: path.to_owned(),
            get_framebuffer_size: std::mem::transmute::<*const c_void, GetFramebufferSizeFn>(
                resolve(c"glfwGetFramebufferSize")?,
            ),
            get_window_content_scale: export(module, c"glfwGetWindowContentScale")
                .map(|f| std::mem::transmute::<*const c_void, GetWindowContentScaleFn>(f)),
            get_version_string: export(module, c"glfwGetVersionString")
                .map(|f| std::mem::transmute::<*const c_void, GetVersionStringFn>(f)),
            get_window_attrib: std::mem::transmute::<*const c_void, GetWindowAttribFn>(resolve(
                c"glfwGetWindowAttrib",
            )?),
            get_win32_window: export(module, c"glfwGetWin32Window")
                .map(|f| std::mem::transmute::<*const c_void, GetWin32WindowFn>(f)),
        }
    };

    // SAFETY: the address is glfwSwapBuffers and the detour has the same signature.
    let original = unsafe {
        detour(
            "glfwSwapBuffers",
            swap_buffers,
            swap_buffers_detour as *const c_void,
        )?
    };
    // SAFETY: the trampoline behaves like the original glfwSwapBuffers.
    let original = unsafe { std::mem::transmute::<*const c_void, SwapBuffersFn>(original) };
    if SWAP_BUFFERS.set(original).is_err() {
        return Err("glfwSwapBuffers is already detoured".into());
    }
    Ok(glfw)
}

/// Patches `target` to jump to `detour` and returns the trampoline that runs the
/// original. The patch goes live immediately.
///
/// # Safety
/// `target` must be a function whose signature `detour` shares, and nobody may be
/// executing its first instructions (true while its DLL is still being loaded).
unsafe fn detour(
    name: &str,
    target: *const c_void,
    detour: *const c_void,
) -> Result<*const c_void, String> {
    // SAFETY: guaranteed by the caller; the hex dumps only read code bytes.
    unsafe {
        log::debug!("{name} @ {target:p}: {}", ffi::hex_bytes(target, 16));
        let trampoline = MinHook::create_hook(target.cast_mut(), detour.cast_mut())
            .map_err(|e| format!("create {name} hook: {e:?}"))?;
        MinHook::enable_hook(target.cast_mut())
            .map_err(|e| format!("enable {name} hook: {e:?}"))?;
        log::debug!("{name} patched: {}", ffi::hex_bytes(target, 16));
        log::debug!(
            "{name} trampoline @ {trampoline:p}: {}",
            ffi::hex_bytes(trampoline, 32)
        );
        Ok(trampoline.cast_const())
    }
}

unsafe extern "C" fn swap_buffers_detour(window: *mut c_void) {
    ffi::catch("glfwSwapBuffers detour", || {
        crate::frame::before_swap(window)
    });
    if let Some(original) = SWAP_BUFFERS.get() {
        // SAFETY: same arguments GLFW's caller passed us.
        unsafe { original(window) };
    }
}
