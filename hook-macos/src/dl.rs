//! The dynamic linker: symbols of the libraries LWJGL loaded, and libraries the agent uses
//! itself (OpenGL.framework) once the game has loaded them.

use std::ffi::{CStr, c_char, c_void};

// dyld's image list (libSystem; libc's bindings are deprecated in favor of another crate).
unsafe extern "C" {
    fn _dyld_image_count() -> u32;
    fn _dyld_get_image_name(index: u32) -> *const c_char;
}

/// The address of `name` in `library`, a `dlopen` handle.
pub fn symbol(library: usize, name: &CStr) -> Option<*const c_void> {
    // SAFETY: a handle LWJGL got from dlopen; a NUL-terminated name.
    let p = unsafe { libc::dlsym(library as *mut c_void, name.as_ptr()) };
    (!p.is_null()).then_some(p.cast_const())
}

/// Where the library of a `dlopen` handle was loaded from: the loaded image whose handle it is
/// (macOS has no `dlinfo`).
pub fn path(library: usize) -> Option<String> {
    // SAFETY: dyld's image list; RTLD_NOLOAD only looks an image up, and the reference it takes
    // is dropped again.
    unsafe {
        for i in 0.._dyld_image_count() {
            let name = _dyld_get_image_name(i);
            if name.is_null() {
                continue;
            }
            let handle = libc::dlopen(name, libc::RTLD_LAZY | libc::RTLD_NOLOAD);
            if handle.is_null() {
                continue;
            }
            let same = handle as usize == library;
            libc::dlclose(handle);
            if same {
                return Some(CStr::from_ptr(name).to_string_lossy().into_owned());
            }
        }
    }
    None
}

/// A library the process has already loaded (never loads one), as a handle.
pub fn loaded(name: &CStr) -> Option<usize> {
    // SAFETY: RTLD_NOLOAD only looks the library up; the handle stays valid for the process
    // (the reference taken is never dropped).
    let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_LAZY | libc::RTLD_NOLOAD) };
    (!handle.is_null()).then_some(handle as usize)
}
