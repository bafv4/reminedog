//! Small FFI helpers shared by the modules.

use std::ffi::{CStr, c_void};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

use windows_sys::Win32::Foundation::HMODULE;
use windows_sys::Win32::System::Diagnostics::Debug::{
    IMAGE_DIRECTORY_ENTRY_EXPORT, IMAGE_NT_HEADERS64, IMAGE_NT_OPTIONAL_HDR64_MAGIC,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleFileNameW, GetProcAddress};
use windows_sys::Win32::System::SystemServices::{
    IMAGE_DOS_HEADER, IMAGE_DOS_SIGNATURE, IMAGE_EXPORT_DIRECTORY, IMAGE_NT_SIGNATURE,
};

/// Runs `f`, turning a panic into a log entry. Every function the JVM, the loader or a
/// detour calls into must go through this: a panic unwinding into foreign frames aborts
/// the process, which here means crashing the game.
pub fn catch<R>(what: &str, f: impl FnOnce() -> R) -> Option<R> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => Some(value),
        Err(_) => {
            // The panic hook already logged the message and location.
            log::error!("recovered from a panic in {what}");
            None
        }
    }
}

/// Address of an exported function of a fully loaded module, or `None` if the module
/// doesn't export it.
pub fn proc_address(module: HMODULE, name: &CStr) -> Option<*const c_void> {
    // SAFETY: GetProcAddress only reads the module's export table.
    unsafe { GetProcAddress(module, name.as_ptr().cast()) }.map(|f| f as *const c_void)
}

/// Looks up an export by reading the PE export table directly. Unlike `GetProcAddress`
/// it never enters the loader, so it is safe on a module that is still being loaded
/// (inside the DLL notification, before its `DllMain` ran). Forwarded exports count as
/// missing.
///
/// # Safety
/// `module` must be the base address of a mapped 64-bit PE image.
pub unsafe fn export_address(module: HMODULE, name: &CStr) -> Option<*const c_void> {
    let base = module as *const u8;
    if base.is_null() {
        return None;
    }
    let read_u32 = |offset: usize| {
        // SAFETY: offsets come from the image's own headers (caller guarantees a PE).
        unsafe { base.add(offset).cast::<u32>().read_unaligned() }
    };
    // SAFETY (whole block): every offset is taken from the image's headers and checked
    // against the export directory's declared counts.
    unsafe {
        let dos = base.cast::<IMAGE_DOS_HEADER>().read_unaligned();
        if dos.e_magic != IMAGE_DOS_SIGNATURE || dos.e_lfanew <= 0 {
            return None;
        }
        let nt = base
            .add(dos.e_lfanew as usize)
            .cast::<IMAGE_NT_HEADERS64>()
            .read_unaligned();
        if nt.Signature != IMAGE_NT_SIGNATURE
            || nt.OptionalHeader.Magic != IMAGE_NT_OPTIONAL_HDR64_MAGIC
        {
            return None;
        }
        let dir = nt.OptionalHeader.DataDirectory[usize::from(IMAGE_DIRECTORY_ENTRY_EXPORT)];
        if dir.VirtualAddress == 0 || dir.Size == 0 {
            return None;
        }
        let exports = base
            .add(dir.VirtualAddress as usize)
            .cast::<IMAGE_EXPORT_DIRECTORY>()
            .read_unaligned();
        let wanted = name.to_bytes();
        for i in 0..exports.NumberOfNames as usize {
            let name_rva = read_u32(exports.AddressOfNames as usize + i * 4) as usize;
            if CStr::from_ptr(base.add(name_rva).cast()).to_bytes() != wanted {
                continue;
            }
            let ordinal = base
                .add(exports.AddressOfNameOrdinals as usize + i * 2)
                .cast::<u16>()
                .read_unaligned() as usize;
            if ordinal >= exports.NumberOfFunctions as usize {
                return None;
            }
            let rva = read_u32(exports.AddressOfFunctions as usize + ordinal * 4) as usize;
            let dir_start = dir.VirtualAddress as usize;
            let is_forwarder = (dir_start..dir_start + dir.Size as usize).contains(&rva);
            return (rva != 0 && !is_forwarder).then(|| base.add(rva).cast());
        }
        None
    }
}

/// Full path of a loaded module (`None` = the process executable).
pub fn module_path(module: Option<HMODULE>) -> Option<PathBuf> {
    let module = module.unwrap_or(std::ptr::null_mut());
    let mut buf = vec![0u16; 260];
    loop {
        // SAFETY: the buffer is valid for `buf.len()` u16s.
        let len = unsafe { GetModuleFileNameW(module, buf.as_mut_ptr(), buf.len() as u32) };
        if len == 0 {
            return None;
        }
        if (len as usize) < buf.len() {
            return Some(PathBuf::from(String::from_utf16_lossy(
                &buf[..len as usize],
            )));
        }
        if buf.len() >= 32 * 1024 {
            return None;
        }
        buf.resize(buf.len() * 2, 0);
    }
}

/// NUL-terminated UTF-16 copy of `s`.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Lossy conversion of a (pointer, length in u16 units) UTF-16 buffer.
///
/// # Safety
/// `ptr` must be null or valid for `len` reads.
pub unsafe fn from_wide(ptr: *const u16, len: usize) -> String {
    if ptr.is_null() || len == 0 {
        return String::new();
    }
    // SAFETY: guaranteed by the caller.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(ptr, len) })
}

/// First bytes of a function, hex-encoded, for diagnosing detours.
///
/// # Safety
/// `addr` must point to at least `len` readable bytes (a function's code is).
pub unsafe fn hex_bytes(addr: *const c_void, len: usize) -> String {
    // SAFETY: guaranteed by the caller.
    let bytes = unsafe { std::slice::from_raw_parts(addr.cast::<u8>(), len) };
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;

    fn ntdll() -> HMODULE {
        // SAFETY: NUL-terminated name; ntdll is always loaded.
        unsafe { GetModuleHandleW(wide("ntdll.dll").as_ptr()) }
    }

    #[test]
    fn export_table_lookup_matches_get_proc_address() {
        let module = ntdll();
        assert!(!module.is_null());
        for name in [c"LdrRegisterDllNotification", c"NtClose", c"RtlGetVersion"] {
            // SAFETY: ntdll is a mapped PE image.
            let ours = unsafe { export_address(module, name) };
            assert_eq!(ours, proc_address(module, name), "{name:?}");
            assert!(ours.is_some(), "{name:?}");
        }
    }

    #[test]
    fn export_table_lookup_misses_unknown_names() {
        // SAFETY: ntdll is a mapped PE image.
        assert_eq!(
            unsafe { export_address(ntdll(), c"reminedogNoSuchExport") },
            None
        );
        // SAFETY: null is rejected before any read.
        assert_eq!(
            unsafe { export_address(std::ptr::null_mut(), c"NtClose") },
            None
        );
    }

    #[test]
    fn wide_round_trip() {
        let w = wide("日本語 path");
        assert_eq!(w.last(), Some(&0));
        // SAFETY: `w` holds len - 1 code units before the terminator.
        assert_eq!(unsafe { from_wide(w.as_ptr(), w.len() - 1) }, "日本語 path");
        // SAFETY: null is handled.
        assert_eq!(unsafe { from_wide(std::ptr::null(), 3) }, "");
    }
}
