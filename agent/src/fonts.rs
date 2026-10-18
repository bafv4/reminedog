//! The Japanese font for egui's fallbacks, read once ([`crate::os::Os::japanese_font`]) on a
//! thread of its own right after the agent loads ([`preload`]) and kept for the process: the
//! overlay, made again when the game's drawable changes, borrows it.

use std::borrow::Cow;
use std::sync::OnceLock;

use reminedog_render::FontSource;

/// The font read: its name, contents and face.
static FONT: OnceLock<Option<(String, &'static [u8], u32)>> = OnceLock::new();

fn read() -> Option<(String, &'static [u8], u32)> {
    let font = crate::os::get().japanese_font()?;
    // Kept for the process.
    Some((font.name, Vec::leak(font.data), font.index))
}

/// Starts reading the font, so the first frame does not wait for the disk.
pub fn preload() {
    let spawned = std::thread::Builder::new()
        .name("reminedog-fonts".into())
        .spawn(|| {
            crate::catch("reading the fonts", || {
                FONT.get_or_init(read);
            });
        });
    if let Err(e) = spawned {
        log::debug!("fonts: no thread to read them ahead ({e})");
    }
}

/// The Japanese fallback font (waiting for [`preload`] if it is still reading).
pub fn japanese() -> Vec<FontSource> {
    match FONT.get_or_init(read) {
        Some((name, data, index)) => vec![FontSource {
            name: name.clone(),
            data: Cow::Borrowed(data),
            index: *index,
        }],
        None => Vec::new(),
    }
}
