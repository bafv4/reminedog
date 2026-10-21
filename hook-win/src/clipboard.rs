//! メニューの入力欄（貼り付け、コピー）のためのシステムのクリップボード。Win32 自身の関数を使う：
//! ウィンドウのライブラリのクリップボードの関数は、F3+C のためにフックしている。

use windows_sys::Win32::Foundation::{GlobalFree, HWND};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;

/// 読む UTF-16 の単位の最大の数（router は貼り付けの文字をこれより少なく残す）。
const MAX_UNITS: usize = 16 * 1024;

/// 開いたクリップボード。捨てると閉じる。
struct Open;

impl Open {
    /// ほかのプログラムがクリップボードを開いている間は失敗する。
    fn new(owner: HWND) -> Option<Self> {
        // SAFETY: ただの呼び出し。
        (unsafe { OpenClipboard(owner) } != 0).then_some(Open)
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        // SAFETY: `Open::new` が開いた。
        unsafe { CloseClipboard() };
    }
}

/// クリップボードの文字（あれば）。
pub fn read_text(owner: HWND) -> Option<String> {
    let _open = Open::new(owner)?;
    // SAFETY: クリップボードは開いているので、そのデータは有効なまま。ロックしている間だけ、
    // システムが報告する大きさの中で読む。
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

/// `text` を、`owner`（ゲームのウィンドウ）が持つものとしてクリップボードに置く。置けなければ false。
pub fn write_text(owner: HWND, text: &str) -> bool {
    let units: Vec<u16> = text.encode_utf16().chain([0]).collect();
    let Some(_open) = Open::new(owner) else {
        return false;
    };
    // SAFETY: クリップボードは開いている。メモリはロックしている間だけ、その大きさの中で書く。
    // `SetClipboardData` が受け取ったらクリップボードが持ち、そうでなければここで解放する。
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
