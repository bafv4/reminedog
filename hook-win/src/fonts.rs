//! egui の代わりの字形に使う、Windows に入っている日本語フォント。エージェントはファイル
//! （游ゴシックは 14 MB）を専用のスレッドで 1 回だけ読み、プロセスの間持つ。

use std::path::PathBuf;

use reminedog_agent::os::FontFile;
use windows_sys::Win32::System::SystemInformation::GetWindowsDirectoryW;

/// 順に試すフォントのコレクション。最初に読めたものを使う。それぞれの面 0 がふつうの
/// （UI 用でない）もの。
const CANDIDATES: &[&str] = &["YuGothM.ttc", "YuGothR.ttc", "meiryo.ttc", "msgothic.ttc"];

/// Windows に入っている、読める最初の日本語フォント。
pub fn read() -> Option<FontFile> {
    let dir = fonts_dir();
    for file in CANDIDATES {
        let path = dir.join(file);
        match std::fs::read(&path) {
            Ok(data) => {
                log::info!(
                    "Japanese font: {} ({} KiB)",
                    path.display(),
                    data.len() / 1024
                );
                return Some(FontFile {
                    name: format!("system:{file}"),
                    data,
                    index: 0,
                });
            }
            Err(e) => log::debug!("font {}: {e}", path.display()),
        }
    }
    log::warn!(
        "no Japanese font found in {}; Japanese text will not render",
        dir.display()
    );
    None
}

fn fonts_dir() -> PathBuf {
    let mut buf = [0u16; 260];
    // SAFETY: バッファは `buf.len()` 個の u16 を持つ。
    let len = unsafe { GetWindowsDirectoryW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
    let windows = if len > 0 && len < buf.len() {
        PathBuf::from(String::from_utf16_lossy(&buf[..len]))
    } else {
        PathBuf::from(r"C:\Windows")
    };
    windows.join("Fonts")
}
