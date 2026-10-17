//! What LWJGL is handed on Windows ([`crate::jvmti::Platform`]): the hooks of the game's window
//! library, the first GLFW or SDL3 that LWJGL looks a function up in (only that one is hooked:
//! a mod's SDL, for controller support, in a GLFW game is left alone), and the zoom's
//! replacements of LWJGL's GL natives.

use std::ffi::{CStr, c_void};
use std::sync::Mutex;

use windows_sys::Win32::Foundation::HMODULE;

use crate::ffi;
use crate::hook::Hook;

/// Modules LWJGL looked functions up in that are not hooked (checked once each). Held while
/// a module is checked, so two threads never attach at once.
static PASSED: Mutex<Vec<usize>> = Mutex::new(Vec::new());

/// The hook LWJGL is handed for `name`, looked up in `library`.
pub fn hook(library: usize, name: &CStr) -> Option<&'static Hook> {
    identify(library as HMODULE);
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

/// The address of an exported function of `module`, a loaded module (read from its PE
/// headers).
pub fn export(module: HMODULE, name: &CStr) -> Option<*const c_void> {
    // SAFETY: every caller passes a module LWJGL loaded and looks functions up in.
    unsafe { ffi::export_address(module, name) }
}

/// Hooks `module` if it is the first GLFW or SDL3 LWJGL looks a function up in.
fn identify(module: HMODULE) {
    let key = module as usize;
    if module.is_null() || crate::glfw::is_hooked(key) || crate::sdl::is_hooked(key) {
        return;
    }
    let mut passed = PASSED.lock().unwrap_or_else(|e| e.into_inner());
    if passed.contains(&key) {
        return;
    }
    let path = || {
        ffi::module_path(Some(module))
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    };
    if crate::glfw::is_glfw(module) {
        if crate::sdl::attached() {
            log::warn!("a GLFW library used after SDL3 is ignored: {}", path());
        } else {
            crate::glfw::attach(module, &path());
        }
    } else if crate::sdl::is_sdl3(module) {
        if crate::glfw::attached() {
            log::warn!(
                "an SDL3 library used after GLFW (a mod's) is ignored: {}",
                path()
            );
        } else {
            crate::sdl::attach(module, &path());
        }
    }
    if !crate::glfw::is_hooked(key) && !crate::sdl::is_hooked(key) {
        passed.push(key);
    }
}
