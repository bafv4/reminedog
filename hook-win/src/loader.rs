//! Watches DLL loads so GLFW, SDL3 and opengl32 can be detoured the moment they load.
//!
//! `LdrRegisterDllNotification` reports every load, whichever API triggered it
//! (`LoadLibraryA/W/ExA/ExW`, static imports), after the image is mapped and before
//! `LoadLibrary` returns, so the detours are in place before LWJGL calls any GLFW
//! function. The callback runs under the loader lock: keep it short and never load a DLL
//! from it.

use std::ffi::c_void;

use windows_sys::Win32::Foundation::HMODULE;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::ProcessStatus::K32EnumProcessModules;
use windows_sys::Win32::System::Threading::GetCurrentProcess;

use crate::ffi;

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *const u16,
}

/// `LDR_DLL_LOADED_NOTIFICATION_DATA` (the unloaded variant has the same layout).
#[repr(C)]
struct DllNotificationData {
    flags: u32,
    full_dll_name: *const UnicodeString,
    base_dll_name: *const UnicodeString,
    dll_base: *mut c_void,
    size_of_image: u32,
}

const REASON_LOADED: u32 = 1;

type DllNotificationFn =
    unsafe extern "system" fn(reason: u32, data: *const DllNotificationData, context: *mut c_void);
type LdrRegisterDllNotificationFn = unsafe extern "system" fn(
    flags: u32,
    callback: DllNotificationFn,
    context: *mut c_void,
    cookie: *mut *mut c_void,
) -> i32;

/// Starts watching DLL loads, then checks modules that are already loaded.
pub fn watch() -> Result<(), String> {
    let ntdll_name = ffi::wide("ntdll.dll");
    // SAFETY: NUL-terminated wide string; ntdll is always loaded.
    let ntdll = unsafe { GetModuleHandleW(ntdll_name.as_ptr()) };
    if ntdll.is_null() {
        return Err("ntdll.dll not found".into());
    }
    let register = ffi::proc_address(ntdll, c"LdrRegisterDllNotification")
        .ok_or("LdrRegisterDllNotification not exported by ntdll")?;
    // SAFETY: documented signature of LdrRegisterDllNotification.
    let register: LdrRegisterDllNotificationFn = unsafe { std::mem::transmute(register) };
    let mut cookie = std::ptr::null_mut();
    // SAFETY: valid callback and out pointer. The registration is never removed: the
    // callback lives as long as this DLL, which the JVM never unloads.
    let status = unsafe { register(0, on_dll_notification, std::ptr::null_mut(), &mut cookie) };
    if status < 0 {
        return Err(format!(
            "LdrRegisterDllNotification failed: NTSTATUS 0x{status:08X}"
        ));
    }
    log::info!("watching DLL loads");
    scan_loaded_modules();
    Ok(())
}

unsafe extern "system" fn on_dll_notification(
    reason: u32,
    data: *const DllNotificationData,
    _context: *mut c_void,
) {
    ffi::catch("DLL load notification", || {
        if reason != REASON_LOADED || data.is_null() {
            return;
        }
        // SAFETY: the loader passes valid notification data for the call's duration.
        let data = unsafe { &*data };
        let path = if data.full_dll_name.is_null() {
            String::new()
        } else {
            // SAFETY: the loader's UNICODE_STRING is valid; Length is in bytes.
            unsafe {
                let name = &*data.full_dll_name;
                ffi::from_wide(name.buffer, usize::from(name.length) / 2)
            }
        };
        log::info!("DLL loaded: {path}");
        on_module(data.dll_base as HMODULE, &path);
    });
}

/// GLFW may already be loaded if the agent was attached late; handle that too.
fn scan_loaded_modules() {
    let mut modules: Vec<HMODULE> = vec![std::ptr::null_mut(); 1024];
    let mut needed = 0u32;
    // SAFETY: the buffer holds `modules.len()` handles; `needed` receives the byte count.
    let ok = unsafe {
        K32EnumProcessModules(
            GetCurrentProcess(),
            modules.as_mut_ptr(),
            (modules.len() * size_of::<HMODULE>()) as u32,
            &mut needed,
        )
    };
    if ok == 0 {
        log::warn!("cannot list loaded modules");
        return;
    }
    let count = (needed as usize / size_of::<HMODULE>()).min(modules.len());
    log::debug!("{count} modules already loaded");
    for &module in &modules[..count] {
        let path = ffi::module_path(Some(module))
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        on_module(module, &path);
    }
}

fn on_module(module: HMODULE, path: &str) {
    if crate::tall::is_opengl32(path) {
        crate::tall::attach(module);
    } else if crate::glfw::is_glfw(module) {
        // One window library only: the game's is loaded first.
        if crate::sdl::attached() {
            log::warn!("a GLFW library was loaded after SDL3 and is ignored: {path}");
        } else {
            crate::glfw::attach(module, path);
        }
    } else if crate::sdl::is_sdl3(module) {
        if crate::glfw::attached() {
            log::warn!("an SDL3 library was loaded after GLFW (a mod's) and is ignored: {path}");
        } else {
            crate::sdl::attach(module, path);
        }
    }
}
