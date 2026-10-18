//! The general pasteboard (`NSPasteboard`) for the menu's paste and copy. GLFW's and SDL3's
//! clipboard functions are not used: the agent hooks them for F3+C.

use std::ffi::{CStr, c_char, c_void};

use crate::objc::{self, Id, Selector};

/// `NSUTF8StringEncoding`.
const UTF8: usize = 4;

static GENERAL_PASTEBOARD: Selector = Selector::new(c"generalPasteboard");
static STRING_FOR_TYPE: Selector = Selector::new(c"stringForType:");
static UTF8_STRING: Selector = Selector::new(c"UTF8String");
static CLEAR_CONTENTS: Selector = Selector::new(c"clearContents");
static SET_STRING: Selector = Selector::new(c"setString:forType:");
static ALLOC: Selector = Selector::new(c"alloc");
static INIT_WITH_BYTES: Selector = Selector::new(c"initWithBytes:length:encoding:");
static RELEASE: Selector = Selector::new(c"release");

/// The general pasteboard and the plain text type (`NSPasteboardTypeString`), once AppKit is
/// loaded.
fn pasteboard() -> Option<(Id, Id)> {
    let class = objc::class(c"NSPasteboard");
    if class.is_null() {
        return None;
    }
    // SAFETY: AppKit's exported NSString constant (a pointer to it), looked up in the loaded
    // images.
    let kind = unsafe {
        let p = libc::dlsym(libc::RTLD_DEFAULT, c"NSPasteboardTypeString".as_ptr());
        if p.is_null() {
            return None;
        }
        *p.cast::<Id>()
    };
    // SAFETY: a class method answering the shared pasteboard.
    let board = unsafe { objc::send::<Id>(class, &GENERAL_PASTEBOARD) };
    (!board.is_null() && !kind.is_null()).then_some((board, kind))
}

/// The pasteboard's plain text.
pub fn read() -> Option<String> {
    objc::autoreleased(|| {
        let (board, kind) = pasteboard()?;
        // SAFETY: NSPasteboard and NSString methods; the UTF-8 copy lives as long as the pool.
        unsafe {
            let text = objc::send1::<Id, Id>(board, &STRING_FOR_TYPE, kind);
            if text.is_null() {
                return None;
            }
            let utf8 = objc::send::<*const c_char>(text, &UTF8_STRING);
            (!utf8.is_null()).then(|| CStr::from_ptr(utf8).to_string_lossy().into_owned())
        }
    })
}

/// Puts `text` on the pasteboard as plain text.
pub fn write(text: &str) -> bool {
    objc::autoreleased(|| {
        let Some((board, kind)) = pasteboard() else {
            return false;
        };
        let class = objc::class(c"NSString");
        if class.is_null() {
            return false;
        }
        // SAFETY: NSString and NSPasteboard methods; the string is released after the
        // pasteboard took it.
        unsafe {
            let allocated = objc::send::<Id>(class, &ALLOC);
            let string = objc::send3::<*const c_void, usize, usize, Id>(
                allocated,
                &INIT_WITH_BYTES,
                text.as_ptr().cast(),
                text.len(),
                UTF8,
            );
            if string.is_null() {
                return false;
            }
            objc::send::<isize>(board, &CLEAR_CONTENTS);
            let written = objc::send2::<Id, Id, u8>(board, &SET_STRING, string, kind) != 0;
            objc::send::<()>(string, &RELEASE);
            written
        }
    })
}
