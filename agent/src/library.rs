//! What LWJGL is handed ([`crate::jvmti`]): the hooks of the game's window library, the first
//! GLFW or SDL3 that LWJGL looks a function up in (only that one is hooked: a mod's SDL, for
//! controller support, in a GLFW game is left alone), and the zoom's replacements of LWJGL's
//! GL natives.

use std::ffi::{CStr, c_void};
use std::sync::Mutex;

use crate::hook::Hook;
use crate::os;

/// Libraries LWJGL looked functions up in that are not hooked (checked once each). Held while
/// a library is checked, so two threads never attach at once.
static PASSED: Mutex<Vec<usize>> = Mutex::new(Vec::new());

/// The hook LWJGL is handed for `name`, looked up in `library`.
pub fn hook(library: usize, name: &CStr) -> Option<&'static Hook> {
    identify(library);
    let hooks: &[&[Hook]] = if crate::glfw::is_hooked(library) {
        &[crate::glfw::hooks(), crate::glfw_input::hooks()]
    } else if crate::sdl::is_hooked(library) {
        &[crate::sdl::hooks(), crate::sdl_input::hooks()]
    } else {
        return None;
    };
    hooks
        .iter()
        .flat_map(|hooks| hooks.iter())
        .find(|hook| hook.name == name)
}

/// The native in place of LWJGL's `name` of `class`: the zoom's GL functions.
pub fn native(
    class: &str,
    name: &str,
    signature: &str,
    address: *const c_void,
) -> Option<*const c_void> {
    if class.starts_with("Lorg/lwjgl/opengl/") {
        crate::tall::wrap_native(name, signature, address)
    } else {
        None
    }
}

/// The address of an exported function of `library`, a library LWJGL loaded.
pub fn export(library: usize, name: &CStr) -> Option<*const c_void> {
    os::get().symbol(library, name)
}

/// Hooks `library` if it is the first GLFW or SDL3 LWJGL looks a function up in.
fn identify(library: usize) {
    if library == 0 || crate::glfw::is_hooked(library) || crate::sdl::is_hooked(library) {
        return;
    }
    let mut passed = PASSED.lock().unwrap_or_else(|e| e.into_inner());
    if passed.contains(&library) {
        return;
    }
    let path = || os::get().library_path(library);
    if crate::glfw::is_glfw(library) {
        if crate::sdl::attached() {
            log::warn!("a GLFW library used after SDL3 is ignored: {}", path());
        } else {
            crate::glfw::attach(library, &path());
        }
    } else if crate::sdl::is_sdl3(library) {
        if crate::glfw::attached() {
            log::warn!(
                "an SDL3 library used after GLFW (a mod's) is ignored: {}",
                path()
            );
        } else {
            crate::sdl::attach(library, &path());
        }
    }
    if !crate::glfw::is_hooked(library) && !crate::sdl::is_hooked(library) {
        passed.push(library);
    }
}
