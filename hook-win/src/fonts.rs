//! Japanese fonts from the Windows installation, used as egui fallbacks.
//!
//! The file (Yu Gothic is 14 MB) is read once, on a thread of its own right after the agent
//! loads ([`preload`]), and kept for the process: the overlay, made again when the game's
//! device context changes, borrows it.

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::OnceLock;

use reminedog_render::FontSource;
use windows_sys::Win32::System::SystemInformation::GetWindowsDirectoryW;

use crate::ffi;

/// Font collections tried in order; the first readable one wins. Face 0 of each is the
/// regular (non-UI) variant.
const CANDIDATES: &[&str] = &["YuGothM.ttc", "YuGothR.ttc", "meiryo.ttc", "msgothic.ttc"];

/// The font read: its file name and contents.
static FONT: OnceLock<Option<(&'static str, &'static [u8])>> = OnceLock::new();

/// Starts reading the font, so the first frame does not wait for the disk.
pub fn preload() {
    let spawned = std::thread::Builder::new()
        .name("reminedog-fonts".into())
        .spawn(|| {
            ffi::catch("reading the fonts", || {
                FONT.get_or_init(read);
            });
        });
    if let Err(e) = spawned {
        log::debug!("fonts: no thread to read them ahead ({e})");
    }
}

/// The Japanese fallback font (waiting for [`preload`] if it is still reading).
pub fn load_japanese() -> Vec<FontSource> {
    match FONT.get_or_init(read) {
        Some((file, data)) => vec![FontSource {
            name: format!("system:{file}"),
            data: Cow::Borrowed(data),
            index: 0,
        }],
        None => Vec::new(),
    }
}

fn read() -> Option<(&'static str, &'static [u8])> {
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
                // Kept for the process.
                return Some((file, Vec::leak(data)));
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
    // SAFETY: the buffer holds `buf.len()` u16s.
    let len = unsafe { GetWindowsDirectoryW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
    let windows = if len > 0 && len < buf.len() {
        PathBuf::from(String::from_utf16_lossy(&buf[..len]))
    } else {
        PathBuf::from(r"C:\Windows")
    };
    windows.join("Fonts")
}
