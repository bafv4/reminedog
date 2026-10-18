//! The agent's own OpenGL context on the game's surface, and queries on the current context,
//! through the OS's context functions ([`crate::os::Os`]).
//!
//! The context is created on the same surface as the game's, so both render to the same
//! default framebuffer, but each keeps its own GL state: switching to ours inside the swap
//! hook and back afterwards leaves Minecraft's state untouched, whether the game uses a
//! core or a compatibility profile.

use std::ffi::{CStr, c_void};
use std::sync::OnceLock;

use crate::os::{self, Os};

const GL_VENDOR: u32 = 0x1F00;
const GL_RENDERER: u32 = 0x1F01;
const GL_VERSION: u32 = 0x1F02;
const GL_CONTEXT_PROFILE_MASK: u32 = 0x9126;

type GlGetStringFn = unsafe extern "system" fn(u32) -> *const u8;
type GlGetIntegervFn = unsafe extern "system" fn(u32, *mut i32);

/// `glGetIntegerv`, once found (a GL 1.1 function: the same for every context of a driver).
static GET_INTEGERV: OnceLock<GlGetIntegervFn> = OnceLock::new();

/// The current context, or 0.
pub fn current_context() -> usize {
    os::get().current_context()
}

/// A GL function of the current context as `F` (its fn pointer type).
///
/// # Safety
/// `F` is the type of the GL function `name`.
pub unsafe fn function<F: Copy>(name: &CStr) -> Option<F> {
    const { assert!(size_of::<F>() == size_of::<*const c_void>()) };
    let p = os::get().gl_function(name);
    // SAFETY: guaranteed by the caller.
    (!p.is_null()).then(|| unsafe { std::mem::transmute_copy::<*const c_void, F>(&p) })
}

/// `glGetIntegerv(name)` on the current context (0 if it cannot be asked).
pub fn integer(name: u32) -> i32 {
    let get = match GET_INTEGERV.get() {
        Some(&get) => Some(get),
        // SAFETY: glGetIntegerv's type. Kept only once found (no context current: not yet).
        None => unsafe { function::<GlGetIntegervFn>(c"glGetIntegerv") }
            .map(|get| *GET_INTEGERV.get_or_init(|| get)),
    };
    let mut value = 0;
    if let Some(get) = get {
        // SAFETY: a single-value query writes one integer.
        unsafe { get(name, &mut value) };
    }
    value
}

/// A glow context on the current GL context.
///
/// # Safety
/// A context is current on this thread, and the glow context is used only while it is.
pub unsafe fn glow_context() -> glow::Context {
    let os = os::get();
    // SAFETY: guaranteed by the caller; the loader resolves functions for the current context.
    unsafe { glow::Context::from_loader_function_cstr(|name| os.gl_function(name)) }
}

/// "version | renderer | vendor | profile" of the current context, using only GL 1.1
/// entry points so it works on the game's context without loading anything.
pub fn describe_current() -> String {
    // SAFETY: glGetString's type.
    let get_string = unsafe { function::<GlGetStringFn>(c"glGetString") };
    let string = |name| {
        let Some(get_string) = get_string else {
            return "?".to_owned();
        };
        // SAFETY: glGetString on the current context; null means an error.
        let s = unsafe { get_string(name) };
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
        match integer(GL_CONTEXT_PROFILE_MASK) {
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

/// (major, minor) from a desktop GL_VERSION string such as "4.6.0 NVIDIA 555.85".
fn parse_gl_version(version: &str) -> Option<(u32, u32)> {
    let mut parts = version.split(|c: char| !c.is_ascii_digit());
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// The agent's context. Deleted explicitly with [`OwnContext::delete`]; never in `Drop`,
/// which could run during thread teardown.
pub struct OwnContext {
    surface: usize,
    context: usize,
}

impl OwnContext {
    /// Creates a context with the pixel format already set on `surface` by the window
    /// library.
    pub fn create(surface: usize) -> Result<Self, String> {
        let context = os::get().create_context(surface)?;
        Ok(Self { surface, context })
    }

    pub fn surface(&self) -> usize {
        self.surface
    }

    /// Makes this context current; the previous one comes back when the guard drops.
    pub fn make_current(&self) -> Result<CurrentGuard, String> {
        let os = os::get();
        let (surface, context) = os.current_gl();
        if let Err(e) = os.make_current(self.surface, self.context) {
            // A failed switch may leave no context current: restore the game's.
            let _ = os.make_current(surface, context);
            return Err(e);
        }
        Ok(CurrentGuard {
            os,
            surface,
            context,
        })
    }

    /// Deletes the context. It must not be current.
    pub fn delete(self) {
        if let Err(e) = os::get().delete_context(self.context) {
            log::warn!("cannot delete the overlay's context: {e}");
        }
    }
}

/// Restores the previously current context (the game's) when dropped, also on panic.
pub struct CurrentGuard {
    os: &'static dyn Os,
    surface: usize,
    context: usize,
}

impl Drop for CurrentGuard {
    fn drop(&mut self) {
        if let Err(e) = self.os.make_current(self.surface, self.context) {
            log::error!("cannot restore the game's GL context: {e}");
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
