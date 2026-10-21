//! メニューの貼り付けとコピーのための一般のペーストボード（`NSPasteboard`）。GLFW と SDL3 の
//! クリップボードの関数は使わない：エージェントが F3+C のためにフックしている。

use std::ffi::{CStr, c_char, c_void};

use crate::objc::{self, Id, Selector};

/// `NSUTF8StringEncoding`。
const UTF8: usize = 4;

static GENERAL_PASTEBOARD: Selector = Selector::new(c"generalPasteboard");
static STRING_FOR_TYPE: Selector = Selector::new(c"stringForType:");
static UTF8_STRING: Selector = Selector::new(c"UTF8String");
static CLEAR_CONTENTS: Selector = Selector::new(c"clearContents");
static SET_STRING: Selector = Selector::new(c"setString:forType:");
static ALLOC: Selector = Selector::new(c"alloc");
static INIT_WITH_BYTES: Selector = Selector::new(c"initWithBytes:length:encoding:");
static RELEASE: Selector = Selector::new(c"release");

/// 一般のペーストボードと、ただの文字の型（`NSPasteboardTypeString`）。AppKit が読み込まれてから。
fn pasteboard() -> Option<(Id, Id)> {
    let class = objc::class(c"NSPasteboard");
    if class.is_null() {
        return None;
    }
    // SAFETY: AppKit が export する NSString の定数（へのポインタ）。読み込み済みのイメージから
    // 引く。
    let kind = unsafe {
        let p = libc::dlsym(libc::RTLD_DEFAULT, c"NSPasteboardTypeString".as_ptr());
        if p.is_null() {
            return None;
        }
        *p.cast::<Id>()
    };
    // SAFETY: 共有のペーストボードを答えるクラスメソッド。
    let board = unsafe { objc::send::<Id>(class, &GENERAL_PASTEBOARD) };
    (!board.is_null() && !kind.is_null()).then_some((board, kind))
}

/// ペーストボードのただの文字。
pub fn read() -> Option<String> {
    objc::autoreleased(|| {
        let (board, kind) = pasteboard()?;
        // SAFETY: NSPasteboard と NSString のメソッド。UTF-8 の写しはプールと同じだけ生きる。
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

/// `text` をただの文字としてペーストボードに置く。
pub fn write(text: &str) -> bool {
    objc::autoreleased(|| {
        let Some((board, kind)) = pasteboard() else {
            return false;
        };
        let class = objc::class(c"NSString");
        if class.is_null() {
            return false;
        }
        // SAFETY: NSString と NSPasteboard のメソッド。文字列はペーストボードが受け取った後で
        // 解放する。
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
