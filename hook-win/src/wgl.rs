//! WGL: OpenGL contexts on Windows, for [`crate::windows`] (the agent keeps its own context on
//! the game's device context with them).
//!
//! opengl32.dll is resolved at runtime (GLFW loads it) instead of being imported, so
//! loading the agent does not change when or from where it gets loaded.

use std::ffi::{CStr, c_char, c_void};
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{GetLastError, HMODULE};
use windows_sys::Win32::Graphics::Gdi::HDC;
use windows_sys::Win32::Graphics::OpenGL::{
    DescribePixelFormat, GetPixelFormat, HGLRC, PFD_DOUBLEBUFFER, PFD_GENERIC_ACCELERATED,
    PFD_GENERIC_FORMAT, PIXELFORMATDESCRIPTOR,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;

use crate::ffi;

type Bool = i32;

type CreateContextFn = unsafe extern "system" fn(HDC) -> HGLRC;
type DeleteContextFn = unsafe extern "system" fn(HGLRC) -> Bool;
type MakeCurrentFn = unsafe extern "system" fn(HDC, HGLRC) -> Bool;
type GetCurrentContextFn = unsafe extern "system" fn() -> HGLRC;
type GetCurrentDcFn = unsafe extern "system" fn() -> HDC;
type GetProcAddressFn = unsafe extern "system" fn(*const c_char) -> *const c_void;

pub struct Wgl {
    module: HMODULE,
    create_context: CreateContextFn,
    delete_context: DeleteContextFn,
    make_current: MakeCurrentFn,
    get_current_context: GetCurrentContextFn,
    get_current_dc: GetCurrentDcFn,
    get_proc_address: GetProcAddressFn,
}

// SAFETY: plain function pointers and a module handle.
unsafe impl Send for Wgl {}
// SAFETY: as above; nothing is mutated after construction.
unsafe impl Sync for Wgl {}

static WGL: OnceLock<Result<Wgl, String>> = OnceLock::new();

/// opengl32.dll's entry points; fails if the process has not loaded it.
pub fn get() -> Result<&'static Wgl, String> {
    WGL.get_or_init(Wgl::resolve).as_ref().map_err(Clone::clone)
}

impl Wgl {
    fn resolve() -> Result<Wgl, String> {
        let name = ffi::wide("opengl32.dll");
        // SAFETY: NUL-terminated wide string.
        let module = unsafe { GetModuleHandleW(name.as_ptr()) };
        if module.is_null() {
            return Err("opengl32.dll is not loaded".into());
        }
        let proc = |name: &CStr| {
            ffi::proc_address(module, name)
                .ok_or_else(|| format!("opengl32.dll does not export {}", name.to_string_lossy()))
        };
        // SAFETY: documented signatures of these opengl32 exports.
        unsafe {
            Ok(Wgl {
                module,
                create_context: std::mem::transmute::<*const c_void, CreateContextFn>(proc(
                    c"wglCreateContext",
                )?),
                delete_context: std::mem::transmute::<*const c_void, DeleteContextFn>(proc(
                    c"wglDeleteContext",
                )?),
                make_current: std::mem::transmute::<*const c_void, MakeCurrentFn>(proc(
                    c"wglMakeCurrent",
                )?),
                get_current_context: std::mem::transmute::<*const c_void, GetCurrentContextFn>(
                    proc(c"wglGetCurrentContext")?,
                ),
                get_current_dc: std::mem::transmute::<*const c_void, GetCurrentDcFn>(proc(
                    c"wglGetCurrentDC",
                )?),
                get_proc_address: std::mem::transmute::<*const c_void, GetProcAddressFn>(proc(
                    c"wglGetProcAddress",
                )?),
            })
        }
    }

    /// The context and device context current on this thread (null if none).
    pub fn current(&self) -> (HDC, HGLRC) {
        // SAFETY: no preconditions.
        unsafe { ((self.get_current_dc)(), (self.get_current_context)()) }
    }

    /// Looks up a GL function for the current context: extensions and GL > 1.1 come from
    /// the driver via wglGetProcAddress, GL 1.1 only from opengl32's exports.
    pub fn load(&self, name: &CStr) -> *const c_void {
        // SAFETY: NUL-terminated name.
        let p = unsafe { (self.get_proc_address)(name.as_ptr()) };
        // Some drivers return small sentinel values instead of null on failure.
        if matches!(p as isize, -1..=3) {
            ffi::proc_address(self.module, name).unwrap_or(std::ptr::null())
        } else {
            p
        }
    }

    /// A context with the pixel format already set on `hdc` by the window library.
    pub fn create_context(&self, hdc: HDC) -> Result<HGLRC, String> {
        // SAFETY: `hdc` is the DC of the game's current context.
        let hglrc = unsafe { (self.create_context)(hdc) };
        if hglrc.is_null() {
            // SAFETY: no preconditions.
            let error = unsafe { GetLastError() };
            return Err(format!("wglCreateContext failed (error {error})"));
        }
        Ok(hglrc)
    }

    /// Makes `hglrc` current on `hdc` (null and null: none).
    pub fn make_current(&self, hdc: HDC, hglrc: HGLRC) -> Result<(), String> {
        // SAFETY: a context made for this DC, or none.
        if unsafe { (self.make_current)(hdc, hglrc) } == 0 {
            // SAFETY: no preconditions.
            let error = unsafe { GetLastError() };
            return Err(format!("wglMakeCurrent failed (error {error})"));
        }
        Ok(())
    }

    /// Deletes a context that is not current.
    pub fn delete_context(&self, hglrc: HGLRC) -> Result<(), String> {
        // SAFETY: the caller's context, not current.
        if unsafe { (self.delete_context)(hglrc) } == 0 {
            return Err("wglDeleteContext failed".into());
        }
        Ok(())
    }
}

/// Pixel format of the window's DC, for diagnosing driver/context problems.
pub fn describe_pixel_format(hdc: HDC) -> String {
    // SAFETY: plain GDI queries on a valid DC.
    unsafe {
        let index = GetPixelFormat(hdc);
        if index == 0 {
            return "none".into();
        }
        let mut pfd: PIXELFORMATDESCRIPTOR = std::mem::zeroed();
        if DescribePixelFormat(
            hdc,
            index,
            size_of::<PIXELFORMATDESCRIPTOR>() as u32,
            &mut pfd,
        ) == 0
        {
            return format!("#{index} (DescribePixelFormat failed)");
        }
        let acceleration = if pfd.dwFlags & PFD_GENERIC_FORMAT == 0 {
            "ICD"
        } else if pfd.dwFlags & PFD_GENERIC_ACCELERATED != 0 {
            "MCD"
        } else {
            "software (GDI generic)"
        };
        format!(
            "#{index}: color {} alpha {} depth {} stencil {}, double-buffered {}, {acceleration}",
            pfd.cColorBits,
            pfd.cAlphaBits,
            pfd.cDepthBits,
            pfd.cStencilBits,
            pfd.dwFlags & PFD_DOUBLEBUFFER != 0
        )
    }
}
