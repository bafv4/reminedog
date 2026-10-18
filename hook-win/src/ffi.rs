//! Small FFI helpers shared by the modules.

use std::ffi::{CStr, c_void};
use std::path::PathBuf;

use windows_sys::Win32::Foundation::HMODULE;
use windows_sys::Win32::Globalization::{CP_ACP, MultiByteToWideChar};
use windows_sys::Win32::System::LibraryLoader::{GetModuleFileNameW, GetProcAddress};

pub use reminedog_agent::catch;

/// Address of an exported function of a loaded module, or `None` if the module doesn't
/// export it.
pub fn proc_address(module: HMODULE, name: &CStr) -> Option<*const c_void> {
    // SAFETY: GetProcAddress only reads the module's export table.
    unsafe { GetProcAddress(module, name.as_ptr().cast()) }.map(|f| f as *const c_void)
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn wide_is_nul_terminated() {
        let w = wide("日本語 path");
        assert_eq!(w.last(), Some(&0));
        assert_eq!(String::from_utf16_lossy(&w[..w.len() - 1]), "日本語 path");
    }
}
