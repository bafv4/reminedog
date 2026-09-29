//! Small FFI helpers shared by the modules.

use std::ffi::{CStr, c_void};
use std::mem::offset_of;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

use windows_sys::Win32::Foundation::HMODULE;
use windows_sys::Win32::Globalization::{CP_ACP, MultiByteToWideChar};
use windows_sys::Win32::System::Diagnostics::Debug::{
    IMAGE_DATA_DIRECTORY, IMAGE_DIRECTORY_ENTRY_EXPORT, IMAGE_NT_HEADERS64,
    IMAGE_NT_OPTIONAL_HDR64_MAGIC, IMAGE_OPTIONAL_HEADER64,
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
/// missing. It runs on every DLL the process loads, so every read is bounds-checked and
/// anything malformed yields `None`.
///
/// # Safety
/// `module` must be null or the base address of a mapped image.
pub unsafe fn export_address(module: HMODULE, name: &CStr) -> Option<*const c_void> {
    // SAFETY: guaranteed by the caller.
    let (image, export_dir) = unsafe { Image::open(module)? };
    let (dir_start, dir_size) = (export_dir.VirtualAddress as usize, export_dir.Size as usize);
    let exports: IMAGE_EXPORT_DIRECTORY = image.read(dir_start)?;
    let wanted = name.to_bytes();
    for i in 0..exports.NumberOfNames as usize {
        let name_rva: u32 = image.read(image_offset(exports.AddressOfNames, i, 4)?)?;
        let Some(candidate) = image.bytes(name_rva as usize, wanted.len() + 1) else {
            continue;
        };
        if &candidate[..wanted.len()] != wanted || candidate[wanted.len()] != 0 {
            continue;
        }
        let ordinal: u16 = image.read(image_offset(exports.AddressOfNameOrdinals, i, 2)?)?;
        if u32::from(ordinal) >= exports.NumberOfFunctions {
            return None;
        }
        let rva: u32 = image.read(image_offset(
            exports.AddressOfFunctions,
            usize::from(ordinal),
            4,
        )?)?;
        let rva = rva as usize;
        let is_forwarder = (dir_start..dir_start.saturating_add(dir_size)).contains(&rva);
        return (rva != 0 && rva < image.size && !is_forwarder)
            // SAFETY: `rva` is inside the image.
            .then(|| unsafe { image.base.add(rva) }.cast());
    }
    None
}

/// `table + index * stride`, without overflow.
fn image_offset(table: u32, index: usize, stride: usize) -> Option<usize> {
    index.checked_mul(stride)?.checked_add(table as usize)
}

/// A mapped PE32+ image whose reads are all checked against its `SizeOfImage`.
struct Image {
    base: *const u8,
    size: usize,
}

impl Image {
    /// Validates the headers and returns the image with its export data directory.
    ///
    /// # Safety
    /// `module` must be null or the base address of a mapped image.
    unsafe fn open(module: HMODULE) -> Option<(Image, IMAGE_DATA_DIRECTORY)> {
        let base = module as *const u8;
        if base.is_null() {
            return None;
        }
        // The headers of a mapped image always lie in its first page.
        let headers = Image { base, size: 4096 };
        let dos: IMAGE_DOS_HEADER = headers.read(0)?;
        if dos.e_magic != IMAGE_DOS_SIGNATURE {
            return None;
        }
        let nt: IMAGE_NT_HEADERS64 = headers.read(usize::try_from(dos.e_lfanew).ok()?)?;
        let optional = &nt.OptionalHeader;
        // Images may declare fewer than 16 data directories; then the bytes after them
        // belong to the section table.
        let directories_end =
            offset_of!(IMAGE_OPTIONAL_HEADER64, DataDirectory) + size_of::<IMAGE_DATA_DIRECTORY>();
        if nt.Signature != IMAGE_NT_SIGNATURE
            || optional.Magic != IMAGE_NT_OPTIONAL_HDR64_MAGIC
            || optional.NumberOfRvaAndSizes <= u32::from(IMAGE_DIRECTORY_ENTRY_EXPORT)
            || usize::from(nt.FileHeader.SizeOfOptionalHeader) < directories_end
        {
            return None;
        }
        let dir = optional.DataDirectory[usize::from(IMAGE_DIRECTORY_ENTRY_EXPORT)];
        if dir.VirtualAddress == 0 || dir.Size == 0 {
            return None;
        }
        let image = Image {
            base,
            size: optional.SizeOfImage as usize,
        };
        Some((image, dir))
    }

    fn read<T: Copy>(&self, offset: usize) -> Option<T> {
        let end = offset.checked_add(size_of::<T>())?;
        // SAFETY: [offset, end) lies inside the mapped image.
        (end <= self.size).then(|| unsafe { self.base.add(offset).cast::<T>().read_unaligned() })
    }

    fn bytes(&self, offset: usize, len: usize) -> Option<&[u8]> {
        let end = offset.checked_add(len)?;
        // SAFETY: [offset, end) lies inside the mapped image, which outlives `self`.
        (end <= self.size)
            .then(|| unsafe { std::slice::from_raw_parts(self.base.add(offset), len) })
    }
}

/// Decodes text the JVM took from its command line, such as the agent options. The Windows
/// `java` launcher reads its arguments in the ANSI code page (CP932 on Japanese Windows),
/// so bytes that are not valid UTF-8 are decoded from that code page. Valid UTF-8 (plain
/// ASCII, or a system set to use UTF-8) is taken as is.
pub fn decode_command_line_text(bytes: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_owned();
    }
    let Ok(len) = i32::try_from(bytes.len()) else {
        return String::from_utf8_lossy(bytes).into_owned();
    };
    // SAFETY: the input is valid for `len` bytes; a null output asks for the size.
    let wide_len =
        unsafe { MultiByteToWideChar(CP_ACP, 0, bytes.as_ptr(), len, std::ptr::null_mut(), 0) };
    if wide_len <= 0 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut wide = vec![0u16; wide_len as usize];
    // SAFETY: the output buffer holds `wide_len` u16s.
    let written =
        unsafe { MultiByteToWideChar(CP_ACP, 0, bytes.as_ptr(), len, wide.as_mut_ptr(), wide_len) };
    wide.truncate(written.max(0) as usize);
    String::from_utf16_lossy(&wide)
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
    fn export_table_lookup_rejects_non_images() {
        // A buffer that is not a PE image (no MZ signature).
        let junk = [0u8; 8192];
        // SAFETY: the buffer is readable for its whole length, which covers every read
        // the function may do before it rejects the signature.
        assert_eq!(
            unsafe { export_address(junk.as_ptr() as HMODULE, c"NtClose") },
            None
        );
    }

    #[test]
    fn command_line_text_decoding() {
        assert_eq!(
            decode_command_line_text(b"gamedir=C:\\x,log=debug"),
            "gamedir=C:\\x,log=debug"
        );
        assert_eq!(decode_command_line_text("日本語".as_bytes()), "日本語");
        // SAFETY: no preconditions.
        match unsafe { windows_sys::Win32::Globalization::GetACP() } {
            1252 => assert_eq!(decode_command_line_text(&[b'x', 0xE9]), "xé"),
            // "マイクラ" in Shift_JIS.
            932 => assert_eq!(
                decode_command_line_text(&[0x83, 0x7D, 0x83, 0x43, 0x83, 0x4E, 0x83, 0x89]),
                "マイクラ"
            ),
            _ => {}
        }
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
