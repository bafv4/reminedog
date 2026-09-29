//! The agent's own OpenGL context on the game's device context.
//!
//! The context is created on the same HDC as the game's, so both render to the same
//! default framebuffer, but each keeps its own GL state: switching to ours inside the swap
//! detour and back afterwards leaves Minecraft's state untouched, whether the game uses a
//! core or a compatibility profile.
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
type GlGetStringFn = unsafe extern "system" fn(u32) -> *const u8;
type GlGetIntegervFn = unsafe extern "system" fn(u32, *mut i32);

pub struct Wgl {
    module: HMODULE,
    create_context: CreateContextFn,
    delete_context: DeleteContextFn,
    make_current: MakeCurrentFn,
    get_current_context: GetCurrentContextFn,
    get_current_dc: GetCurrentDcFn,
    get_proc_address: GetProcAddressFn,
    gl_get_string: GlGetStringFn,
    gl_get_integerv: GlGetIntegervFn,
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

const GL_VENDOR: u32 = 0x1F00;
const GL_RENDERER: u32 = 0x1F01;
const GL_VERSION: u32 = 0x1F02;
const GL_CONTEXT_PROFILE_MASK: u32 = 0x9126;

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
                gl_get_string: std::mem::transmute::<*const c_void, GlGetStringFn>(proc(
                    c"glGetString",
                )?),
                gl_get_integerv: std::mem::transmute::<*const c_void, GlGetIntegervFn>(proc(
                    c"glGetIntegerv",
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
    /// the driver via wglGetProcAddress, GL 1.1 only from opengl32's exports. Always the
    /// real functions, never the wrappers handed to the game (see `tall`).
    pub fn load(&self, name: &CStr) -> *const c_void {
        let get_proc_address =
            crate::tall::original_get_proc_address().unwrap_or(self.get_proc_address);
        // SAFETY: NUL-terminated name.
        let p = unsafe { get_proc_address(name.as_ptr()) };
        // Some drivers return small sentinel values instead of null on failure.
        if matches!(p as isize, -1..=3) {
            ffi::proc_address(self.module, name).unwrap_or(std::ptr::null())
        } else {
            p
        }
    }

    /// An export of opengl32.dll (the GL 1.1 functions), or null.
    pub fn export(&self, name: &CStr) -> *const c_void {
        ffi::proc_address(self.module, name).unwrap_or(std::ptr::null())
    }

    /// "version | renderer | vendor | profile" of the current context, using only GL 1.1
    /// entry points so it works on the game's context without loading anything.
    pub fn describe_current(&self) -> String {
        let string = |name| {
            // SAFETY: glGetString on the current context; null means an error.
            let s = unsafe { (self.gl_get_string)(name) };
            if s.is_null() {
                "?".to_owned()
            } else {
                // SAFETY: GL returns a static NUL-terminated string.
                unsafe { CStr::from_ptr(s.cast()) }
                    .to_string_lossy()
                    .into_owned()
            }
        };
        let version = string(GL_VERSION);
        // The profile query only exists since GL 3.2; on older contexts it would leave a
        // GL error behind in the game's context.
        let profile = if parse_gl_version(&version) >= Some((3, 2)) {
            let mut mask = 0;
            // SAFETY: valid query on a 3.2+ context.
            unsafe { (self.gl_get_integerv)(GL_CONTEXT_PROFILE_MASK, &mut mask) };
            match mask {
                m if m & 1 != 0 => "core",
                m if m & 2 != 0 => "compatibility",
                _ => "unknown profile",
            }
        } else {
            "legacy"
        };
        format!(
            "{version} | {} | {} | {profile}",
            string(GL_RENDERER),
            string(GL_VENDOR)
        )
    }
}

/// (major, minor) from a desktop GL_VERSION string such as "4.6.0 NVIDIA 555.85".
fn parse_gl_version(version: &str) -> Option<(u32, u32)> {
    let mut parts = version.split(|c: char| !c.is_ascii_digit());
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
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

/// The agent's context. Deleted explicitly with [`OwnContext::delete`]; never in `Drop`,
/// which could run during thread teardown.
pub struct OwnContext {
    hdc: HDC,
    hglrc: HGLRC,
}

impl OwnContext {
    /// Creates a context with the pixel format already set on `hdc` by GLFW.
    pub fn create(wgl: &Wgl, hdc: HDC) -> Result<Self, String> {
        // SAFETY: `hdc` is the DC of the game's current context.
        let hglrc = unsafe { (wgl.create_context)(hdc) };
        if hglrc.is_null() {
            // SAFETY: no preconditions.
            let error = unsafe { GetLastError() };
            return Err(format!("wglCreateContext failed (error {error})"));
        }
        Ok(Self { hdc, hglrc })
    }

    pub fn hdc(&self) -> HDC {
        self.hdc
    }

    /// Makes this context current; the previous one comes back when the guard drops.
    pub fn make_current<'a>(&self, wgl: &'a Wgl) -> Result<CurrentGuard<'a>, String> {
        let (prev_dc, prev_ctx) = wgl.current();
        // SAFETY: our context was created for this DC.
        if unsafe { (wgl.make_current)(self.hdc, self.hglrc) } == 0 {
            // SAFETY: no preconditions.
            let error = unsafe { GetLastError() };
            // A failed wglMakeCurrent may leave no context current: restore the game's.
            // SAFETY: restoring what was current a moment ago.
            unsafe { (wgl.make_current)(prev_dc, prev_ctx) };
            return Err(format!("wglMakeCurrent failed (error {error})"));
        }
        Ok(CurrentGuard {
            wgl,
            prev_dc,
            prev_ctx,
        })
    }

    /// Deletes the context. It must not be current.
    pub fn delete(self, wgl: &Wgl) {
        // SAFETY: we own the context and it is not current (guards restore on drop).
        if unsafe { (wgl.delete_context)(self.hglrc) } == 0 {
            log::warn!("wglDeleteContext failed");
        }
    }
}

/// Restores the previously current context (the game's) when dropped, also on panic.
pub struct CurrentGuard<'a> {
    wgl: &'a Wgl,
    prev_dc: HDC,
    prev_ctx: HGLRC,
}

impl Drop for CurrentGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: restoring the context that was current before the switch.
        if unsafe { (self.wgl.make_current)(self.prev_dc, self.prev_ctx) } == 0 {
            // SAFETY: no preconditions.
            let error = unsafe { GetLastError() };
            log::error!("cannot restore the game's GL context (error {error})");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_gl_version;

    #[test]
    fn gl_version_strings() {
        assert_eq!(parse_gl_version("4.6.0 NVIDIA 555.85"), Some((4, 6)));
        assert_eq!(
            parse_gl_version("3.2.0 Core Profile Context 24.10.1"),
            Some((3, 2))
        );
        assert_eq!(
            parse_gl_version("4.5 (Compatibility Profile) Mesa 25.2.8"),
            Some((4, 5))
        );
        assert_eq!(parse_gl_version("1.1.0"), Some((1, 1)));
        assert_eq!(parse_gl_version("garbage"), None);
        assert!(parse_gl_version("3.1.0") < Some((3, 2)));
    }
}
