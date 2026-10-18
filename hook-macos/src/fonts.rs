//! A Japanese font for egui's fallbacks: Hiragino Sans, which every macOS has, else other
//! fonts with Japanese in the usual places. The agent reads it once, on a thread of its own,
//! and keeps it for the process.

use std::path::Path;

use reminedog_agent::os::FontFile;

/// Files tried in order, with the face of Japanese in collections.
const CANDIDATES: &[(&str, u32)] = &[
    ("/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc", 0),
    ("/System/Library/Fonts/Hiragino Sans GB.ttc", 0),
    ("/System/Library/Fonts/Supplemental/Arial Unicode.ttf", 0),
    ("/Library/Fonts/Arial Unicode.ttf", 0),
];

/// The first of the candidates that can be read.
pub fn read() -> Option<FontFile> {
    for &(path, index) in CANDIDATES {
        let path = Path::new(path);
        match std::fs::read(path) {
            Ok(data) => {
                log::info!("fonts: {} (face {index})", path.display());
                return Some(FontFile {
                    name: path
                        .file_name()
                        .map_or_else(|| "japanese".into(), |n| n.to_string_lossy().into_owned()),
                    data,
                    index,
                });
            }
            Err(e) => log::debug!("fonts: cannot read {}: {e}", path.display()),
        }
    }
    log::warn!("fonts: no Japanese font found; Japanese text shows as boxes");
    None
}
