//! The GLFW library LWJGL loads, and the detours on it.
//!
//! LWJGL resolves GLFW's exports once with `GetProcAddress` and calls through the saved
//! pointers, so patching the functions' first instructions (inline detour) is the only
//! way to see those calls; import-table hooks would not.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::{Mutex, OnceLock};

use reminedog_core::{
    InputId, Naming, glfw_button_of, glfw_key_of, input_from_glfw_button, input_from_glfw_key,
};
use windows_sys::Win32::Foundation::{HMODULE, HWND};

use crate::ffi;
use crate::frame::{self, WindowSystem};
use crate::hook::{self, export};

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

impl WindowSystem for Glfw {
    fn framebuffer_size(&self, window: *mut c_void) -> (i32, i32) {
        let (mut width, mut height) = (0, 0);
        // SAFETY: `window` is the handle GLFW itself passed to glfwSwapBuffers.
        unsafe { (self.get_framebuffer_size)(window, &mut width, &mut height) };
        (width, height)
    }

    /// 1.0 when GLFW is too old to report it.
    fn content_scale(&self, window: *mut c_void) -> f32 {
        let Some(f) = self.get_window_content_scale else {
            return 1.0;
        };
        let (mut x, mut y) = (1.0, 1.0);
        // SAFETY: as above.
        unsafe { f(window, &mut x, &mut y) };
        if x.is_finite() && x > 0.0 { x } else { 1.0 }
    }

    /// `None` if this GLFW does not export native access.
    fn hwnd(&self, window: *mut c_void) -> Option<HWND> {
        let f = self.get_win32_window?;
        // SAFETY: as above.
        let hwnd = unsafe { f(window) };
        (!hwnd.is_null()).then_some(hwnd)
    }

    fn is_visible(&self, window: *mut c_void) -> bool {
        // SAFETY: as above.
        unsafe { (self.get_window_attrib)(window, GLFW_VISIBLE) != 0 }
    }

    fn raw_motion(&self, window: *mut c_void) -> bool {
        crate::glfw_input::raw_motion(window)
    }

    fn captured(&self, window: *mut c_void) -> bool {
        crate::glfw_input::captured(window)
    }

    fn naming(&self) -> Naming {
        Naming::Glfw
    }

    fn key_state_spoofed(&self) -> bool {
        crate::glfw_input::key_state_spoofed()
    }

    fn can_send(&self, id: InputId) -> bool {
        match id {
            InputId::Key(_) => glfw_key_of(id).is_some(),
            InputId::Mouse(_) => glfw_button_of(id).is_some(),
        }
    }

    /// The keys and buttons whose GLFW codes come back as themselves.
    fn can_receive(&self, id: InputId) -> bool {
        match id {
            InputId::Key(_) => glfw_key_of(id)
                .is_some_and(|(key, scancode)| input_from_glfw_key(key, scancode) == Some(id)),
            InputId::Mouse(_) => glfw_button_of(id).and_then(input_from_glfw_button) == Some(id),
        }
    }

    fn describe(&self) -> String {
        match self.get_version_string {
            // SAFETY: returns a static NUL-terminated string; safe to call any time.
            Some(f) => format!("GLFW {}", unsafe { CStr::from_ptr(f()) }.to_string_lossy()),
            None => "GLFW".into(),
        }
    }
}

static GLFW: OnceLock<Glfw> = OnceLock::new();
/// Trampoline to the original glfwSwapBuffers.
static SWAP_BUFFERS: OnceLock<SwapBuffersFn> = OnceLock::new();
/// Trampoline to the original glfwGetFramebufferSize (the overlay needs the real size).
static GET_FRAMEBUFFER_SIZE: OnceLock<GetFramebufferSizeFn> = OnceLock::new();
/// Serializes `attach`; the loader lock already does, but don't rely on it.
static ATTACH: Mutex<()> = Mutex::new(());

/// Identifies GLFW by its exports rather than its file name, so a renamed or
/// launcher-provided GLFW (Prism's "use system GLFW") is recognized as well.
pub fn is_glfw(module: HMODULE) -> bool {
    export(module, c"glfwInit").is_some() && export(module, c"glfwSwapBuffers").is_some()
}

/// Whether a GLFW library was hooked.
pub fn attached() -> bool {
    GLFW.get().is_some()
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
    let get_framebuffer_size = resolve(c"glfwGetFramebufferSize")?;
    // SAFETY: the export and the detour share glfwGetFramebufferSize's signature. Without
    // it the zoom can only enlarge pixels, so a failure is not fatal.
    if let Err(e) = unsafe {
        hook::install(
            "glfwGetFramebufferSize",
            get_framebuffer_size,
            get_framebuffer_size_detour as *const c_void,
            &GET_FRAMEBUFFER_SIZE,
        )
    } {
        log::warn!("no high-resolution zoom: {e}");
    }
    // SAFETY: exported GLFW functions with these C signatures.
    let glfw = unsafe {
        Glfw {
            path: path.to_owned(),
            get_framebuffer_size: match GET_FRAMEBUFFER_SIZE.get() {
                Some(&original) => original,
                None => {
                    std::mem::transmute::<*const c_void, GetFramebufferSizeFn>(get_framebuffer_size)
                }
            },
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
    unsafe {
        hook::install(
            "glfwSwapBuffers",
            swap_buffers,
            swap_buffers_detour as *const c_void,
            &SWAP_BUFFERS,
        )?;
    }
    crate::glfw_input::install(module);
    Ok(glfw)
}

unsafe extern "C" fn swap_buffers_detour(window: *mut c_void) {
    ffi::catch("glfwSwapBuffers detour", || {
        if let Some(glfw) = GLFW.get() {
            frame::before_swap(glfw, window);
        }
    });
    if let Some(original) = SWAP_BUFFERS.get() {
        let _swapping = crate::tall::Swapping::begin();
        // SAFETY: same arguments GLFW's caller passed us.
        unsafe { original(window) };
    }
    // Starting or ending the zoom: the game resizes right away, between two frames.
    if let Some((window, [w, h])) = crate::tall::take_pending() {
        crate::glfw_input::send_framebuffer_size(window, w, h);
    }
    // A waypoint request: F3+C, now that the frame is out and no lock is held.
    ffi::catch("F3+C", || crate::glfw_input::run_f3c(window));
    ffi::catch("rebinds", || crate::glfw_input::release_lost_keys(window));
    ffi::catch("hotkeys", crate::rebind_state::forget_lost_presses);
}

/// Reports the tall size to the game while zooming.
unsafe extern "C" fn get_framebuffer_size_detour(
    window: *mut c_void,
    width: *mut c_int,
    height: *mut c_int,
) {
    if let Some(original) = GET_FRAMEBUFFER_SIZE.get() {
        // SAFETY: the caller's arguments.
        unsafe { original(window, width, height) };
    }
    if let Some([w, h]) = crate::tall::size_override(window) {
        // SAFETY: GLFW allows either pointer to be null.
        unsafe {
            if !width.is_null() {
                *width = w;
            }
            if !height.is_null() {
                *height = h;
            }
        }
    }
}
