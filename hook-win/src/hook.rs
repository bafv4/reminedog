//! Inline detours (MinHook) and export lookup for freshly loaded modules.

use std::ffi::{CStr, c_void};
use std::sync::OnceLock;

use minhook::MinHook;
use windows_sys::Win32::Foundation::HMODULE;

use crate::ffi;

/// Export lookup that is safe while the module is still being loaded. `module` must come
/// from the loader (DLL notification or module list), i.e. be the base of a mapped image.
pub fn export(module: HMODULE, name: &CStr) -> Option<*const c_void> {
    // SAFETY: see above; every caller passes a module handed over by the loader.
    unsafe { ffi::export_address(module, name) }
}

/// Patches `target` to jump to `detour`. The trampoline that runs the original goes into
/// `original` before the patch goes live, so the detour can always reach it.
///
/// # Safety
/// `target` must be a function whose signature `detour` and `F` share, and nobody may be
/// executing its first instructions (true while its DLL is still being loaded).
pub unsafe fn install<F: Copy>(
    name: &str,
    target: *const c_void,
    detour: *const c_void,
    original: &OnceLock<F>,
) -> Result<(), String> {
    assert_eq!(
        size_of::<F>(),
        size_of::<*const c_void>(),
        "F must be a fn pointer"
    );
    // SAFETY: guaranteed by the caller; the hex dumps only read code bytes.
    unsafe {
        log::debug!("{name} @ {target:p}: {}", ffi::hex_bytes(target, 16));
        let trampoline = MinHook::create_hook(target.cast_mut(), detour.cast_mut())
            .map_err(|e| format!("create {name} hook: {e:?}"))?;
        log::debug!(
            "{name} trampoline @ {trampoline:p}: {}",
            ffi::hex_bytes(trampoline, 32)
        );
        let trampoline = trampoline.cast_const();
        if original
            .set(std::mem::transmute_copy::<*const c_void, F>(&trampoline))
            .is_err()
        {
            return Err(format!("{name} is already detoured"));
        }
        MinHook::enable_hook(target.cast_mut())
            .map_err(|e| format!("enable {name} hook: {e:?}"))?;
        log::debug!("{name} patched: {}", ffi::hex_bytes(target, 16));
    }
    Ok(())
}
