//! Japanese fonts from the Windows installation, used as egui fallbacks.

use std::path::PathBuf;

use reminedog_render::FontSource;
use windows_sys::Win32::System::SystemInformation::GetWindowsDirectoryW;

/// Font collections tried in order; the first readable one wins. Face 0 of each is the
/// regular (non-UI) variant.
const CANDIDATES: &[&str] = &["YuGothM.ttc", "YuGothR.ttc", "meiryo.ttc", "msgothic.ttc"];

pub fn load_japanese() -> Vec<FontSource> {
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
                return vec![FontSource {
                    name: format!("system:{file}"),
                    data,
                    index: 0,
                }];
            }
            Err(e) => log::debug!("font {}: {e}", path.display()),
        }
    }
    log::warn!(
        "no Japanese font found in {}; Japanese text will not render",
        dir.display()
    );
    Vec::new()
}

fn fonts_dir() -> PathBuf {
    let mut buf = [0u16; 260];
    // SAFETY: the buffer holds `buf.len()` u16s.
    let len = unsafe { GetWindowsDirectoryW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
    let windows = if len > 0 && len < buf.len() {
        PathBuf::from(String::from_utf16_lossy(&buf[..len]))
    } else {
        PathBuf::from(r"C:\Windows")
    };
    windows.join("Fonts")
}
