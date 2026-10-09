//! The system clipboard for the menu's text fields (paste, copy). Win32's own functions are
//! used: the window libraries' clipboard functions are hooked for F3+C.

use windows_sys::Win32::Foundation::{GlobalFree, HWND};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;

/// The most UTF-16 units read (the router keeps fewer characters of a paste).
const MAX_UNITS: usize = 16 * 1024;

/// The clipboard, open; closed when dropped.
struct Open;

impl Open {
    /// Fails while another program has the clipboard open.
    fn new(owner: HWND) -> Option<Self> {
        // SAFETY: plain call.
        (unsafe { OpenClipboard(owner) } != 0).then_some(Open)
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        // SAFETY: opened by `Open::new`.
        unsafe { CloseClipboard() };
    }
}

/// The clipboard's text, if it has any.
pub fn read_text(owner: HWND) -> Option<String> {
    let _open = Open::new(owner)?;
    // SAFETY: the clipboard is open, so its data stays valid; it is read only while locked,
    // within the size the system reports.
    unsafe {
        let handle = GetClipboardData(u32::from(CF_UNICODETEXT));
        if handle.is_null() {
            return None;
        }
        let data = GlobalLock(handle).cast::<u16>();
        if data.is_null() {
            return None;
        }
        let units = (GlobalSize(handle) / 2).min(MAX_UNITS);
        let all = std::slice::from_raw_parts(data, units);
        let len = all.iter().position(|&unit| unit == 0).unwrap_or(units);
        let text = String::from_utf16_lossy(&all[..len]);
        GlobalUnlock(handle);
        Some(text)
    }
}

/// Puts `text` on the clipboard, owned by `owner` (the game's window). False if it could not.
pub fn write_text(owner: HWND, text: &str) -> bool {
    let units: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let Some(_open) = Open::new(owner) else {
        return false;
    };
    // SAFETY: the clipboard is open. The memory is written only while locked, within its
    // size; the clipboard owns it once `SetClipboardData` took it, else it is freed here.
    unsafe {
        if EmptyClipboard() == 0 {
            return false;
        }
        let handle = GlobalAlloc(GMEM_MOVEABLE, units.len() * 2);
        if handle.is_null() {
            return false;
        }
        let data = GlobalLock(handle).cast::<u16>();
        if data.is_null() {
            GlobalFree(handle);
            return false;
        }
        std::ptr::copy_nonoverlapping(units.as_ptr(), data, units.len());
        GlobalUnlock(handle);
        if SetClipboardData(u32::from(CF_UNICODETEXT), handle).is_null() {
            GlobalFree(handle);
            return false;
        }
    }
    true
}
