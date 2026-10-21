//! モジュールが共有する小さな FFI の補助。

use std::ffi::{CStr, c_void};
use std::path::PathBuf;

use windows_sys::Win32::Foundation::HMODULE;
use windows_sys::Win32::Globalization::{CP_ACP, MultiByteToWideChar};
use windows_sys::Win32::System::LibraryLoader::{GetModuleFileNameW, GetProcAddress};

pub use reminedog_agent::catch;

/// 読み込んだモジュールの export の関数のアドレス。モジュールが export していなければ `None`。
pub fn proc_address(module: HMODULE, name: &CStr) -> Option<*const c_void> {
    // SAFETY: GetProcAddress はモジュールの export の表を読むだけ。
    unsafe { GetProcAddress(module, name.as_ptr().cast()) }.map(|f| f as *const c_void)
}

/// JVM がコマンドラインから取った文字（エージェントのオプションなど）をデコードする。Windows の
/// `java` のランチャーは引数を ANSI のコードページ（日本語の Windows では CP932）で読むので、
/// 有効な UTF-8 でないバイトはそのコードページからデコードする。有効な UTF-8（ただの ASCII、
/// または UTF-8 を使うように設定した機械）はそのまま受け取る。
pub fn decode_command_line_text(bytes: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_owned();
    }
    let Ok(len) = i32::try_from(bytes.len()) else {
        return String::from_utf8_lossy(bytes).into_owned();
    };
    // SAFETY: 入力は `len` バイト有効。出力が null なら大きさを聞く。
    let wide_len =
        unsafe { MultiByteToWideChar(CP_ACP, 0, bytes.as_ptr(), len, std::ptr::null_mut(), 0) };
    if wide_len <= 0 {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut wide = vec![0u16; wide_len as usize];
    // SAFETY: 出力のバッファは `wide_len` 個の u16 を持つ。
    let written =
        unsafe { MultiByteToWideChar(CP_ACP, 0, bytes.as_ptr(), len, wide.as_mut_ptr(), wide_len) };
    wide.truncate(written.max(0) as usize);
    String::from_utf16_lossy(&wide)
}

/// 読み込んだモジュールのフルパス（`None` = プロセスの実行ファイル）。
pub fn module_path(module: Option<HMODULE>) -> Option<PathBuf> {
    let module = module.unwrap_or(std::ptr::null_mut());
    let mut buf = vec![0u16; 260];
    loop {
        // SAFETY: バッファは `buf.len()` 個の u16 分有効。
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

/// NUL で終わる `s` の UTF-16 の写し。
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
        // SAFETY: 前提はない。
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
