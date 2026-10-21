//! egui の代わりの字形に使う日本語フォント。エージェントの読み込みの直後に、専用のスレッドで
//! 1 回だけ読み（[`crate::os::Os::japanese_font`]、[`preload`]）、プロセスの間持つ。
//! ゲームの描画先が変わるたびに作り直すオーバーレイは、これを借りる。

use std::borrow::Cow;
use std::sync::OnceLock;

use reminedog_render::FontSource;

/// 読んだフォント：名前、中身、面の番号。
static FONT: OnceLock<Option<(String, &'static [u8], u32)>> = OnceLock::new();

fn read() -> Option<(String, &'static [u8], u32)> {
    let font = crate::os::get().japanese_font()?;
    // プロセスの間持つ。
    Some((font.name, Vec::leak(font.data), font.index))
}

/// フォントを読み始める（最初のフレームがディスクを待たないように）。
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

/// 日本語の代わりのフォント（[`preload`] がまだ読んでいれば待つ）。
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
