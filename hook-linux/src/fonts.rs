//! A Japanese font for egui's fallbacks, found with fontconfig (`fc-match`), else in the
//! places distributions put Noto Sans CJK or IPA Gothic.
//!
//! The file is read once, on a thread of its own right after the agent loads ([`preload`]),
//! and kept for the process: the overlay, made again when the game's drawable changes,
//! borrows it.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use reminedog_render::FontSource;

/// Files tried when fontconfig gives none, with the face of Japanese in collections.
const CANDIDATES: &[(&str, u32)] = &[
    ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 0),
    ("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc", 0),
    (
        "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
        0,
    ),
    ("/usr/share/fonts/opentype/ipafont-gothic/ipag.ttf", 0),
    ("/usr/share/fonts/ipa-gothic/ipag.ttf", 0),
];

/// The font read: its file name, contents and face.
static FONT: OnceLock<Option<(String, &'static [u8], u32)>> = OnceLock::new();

/// Starts reading the font, so the first frame does not wait for the disk.
pub fn preload() {
    let spawned = std::thread::Builder::new()
        .name("reminedog-fonts".into())
        .spawn(|| {
            reminedog_agent::catch("reading the fonts", || {
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
        Some((name, data, index)) => vec![FontSource {
            name: name.clone(),
            data: Cow::Borrowed(data),
            index: *index,
        }],
        None => Vec::new(),
    }
}

fn read() -> Option<(String, &'static [u8], u32)> {
    let found = fontconfig().into_iter().chain(
        CANDIDATES
            .iter()
            .map(|&(path, index)| (PathBuf::from(path), index)),
    );
    for (path, index) in found {
        match std::fs::read(&path) {
            Ok(data) => {
                log::info!("fonts: {} (face {index})", path.display());
                let name = file_name(&path);
                return Some((name, Vec::leak(data), index));
            }
            Err(e) => log::debug!("fonts: cannot read {}: {e}", path.display()),
        }
    }
    log::warn!("fonts: no Japanese font found; Japanese text shows as boxes");
    None
}

/// fontconfig's choice for Japanese sans-serif text: its file and face.
fn fontconfig() -> Option<(PathBuf, u32)> {
    let output = Command::new("fc-match")
        .args(["--format=%{file}\n%{index}", "sans-serif:lang=ja"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut lines = text.lines();
    let file = lines.next()?.trim();
    let index = lines
        .next()
        .and_then(|i| i.trim().parse().ok())
        .unwrap_or(0);
    (!file.is_empty()).then(|| (PathBuf::from(file), index))
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(|| "japanese".into(), |n| n.to_string_lossy().into_owned())
}
