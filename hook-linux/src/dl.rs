//! The dynamic linker: symbols of the libraries LWJGL loaded, and libraries the agent uses
//! itself (libGL, libX11) once the game has loaded them.

use std::ffi::{CStr, CString, c_char, c_int, c_void};

/// The address of `name` in `library`, a `dlopen` handle.
pub fn symbol(library: usize, name: &CStr) -> Option<*const c_void> {
    // SAFETY: a handle LWJGL got from dlopen; a NUL-terminated name.
    let p = unsafe { libc::dlsym(library as *mut c_void, name.as_ptr()) };
    (!p.is_null()).then_some(p.cast_const())
}

/// glibc's `struct link_map`, as far as it is read.
#[repr(C)]
struct LinkMap {
    l_addr: usize,
    l_name: *const c_char,
}

/// Where the library of a `dlopen` handle was loaded from.
pub fn path(library: usize) -> Option<String> {
    let mut map: *mut LinkMap = std::ptr::null_mut();
    // SAFETY: RTLD_DI_LINKMAP writes a pointer to the library's link map.
    let ok = unsafe {
        libc::dlinfo(
            library as *mut c_void,
            libc::RTLD_DI_LINKMAP,
            (&raw mut map).cast(),
        )
    } == 0;
    if !ok || map.is_null() {
        return None;
    }
    // SAFETY: the dynamic linker's link map, alive while the library is loaded.
    let name = unsafe { (*map).l_name };
    // SAFETY: as above, a NUL-terminated path.
    (!name.is_null()).then(|| {
        unsafe { CStr::from_ptr(name) }
            .to_string_lossy()
            .into_owned()
    })
}

/// Whether a loaded library other than this one exports `symbol` (another copy of the agent,
/// from another folder: the dynamic linker loads each path as its own library).
pub fn loaded_elsewhere(symbol: &CStr, ours: *const c_void) -> bool {
    unsafe extern "C" fn collect(
        info: *mut libc::dl_phdr_info,
        _size: usize,
        names: *mut c_void,
    ) -> c_int {
        // SAFETY: the linker's entry for one object, during the call; `names` is ours.
        unsafe {
            let name = (*info).dlpi_name;
            if !name.is_null() && *name != 0 {
                (*names.cast::<Vec<CString>>()).push(CStr::from_ptr(name).to_owned());
            }
        }
        0
    }
    let mut names: Vec<CString> = Vec::new();
    // SAFETY: the callback only reads each entry and adds to `names`.
    unsafe { libc::dl_iterate_phdr(Some(collect), (&raw mut names).cast()) };
    names.iter().any(|name| {
        // SAFETY: RTLD_NOLOAD only looks the library up; the reference is dropped again.
        unsafe {
            let handle = libc::dlopen(name.as_ptr(), libc::RTLD_LAZY | libc::RTLD_NOLOAD);
            if handle.is_null() {
                return false;
            }
            let found = libc::dlsym(handle, symbol.as_ptr());
            libc::dlclose(handle);
            !found.is_null() && found.cast_const() != ours
        }
    })
}

/// A library the process has already loaded (never loads one), as a handle.
pub fn loaded(name: &CStr) -> Option<usize> {
    // SAFETY: RTLD_NOLOAD only looks the library up; the handle stays valid for the process
    // (the reference taken is never dropped).
    let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_LAZY | libc::RTLD_NOLOAD) };
    (!handle.is_null()).then_some(handle as usize)
}
