//! Which library is the game's window library: the first GLFW or SDL3 that LWJGL looks a
//! function up in ([`crate::jvmti`]). Only that one is hooked: a mod's SDL (controller
//! support) in a GLFW game is left alone.

use std::sync::Mutex;

use windows_sys::Win32::Foundation::HMODULE;

use crate::ffi;

/// Modules LWJGL looked functions up in that are not hooked (checked once each). Held while
/// a module is checked, so two threads never attach at once.
static PASSED: Mutex<Vec<usize>> = Mutex::new(Vec::new());

/// Hooks `module` if it is the first GLFW or SDL3 LWJGL looks a function up in.
pub fn identify(module: HMODULE) {
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
