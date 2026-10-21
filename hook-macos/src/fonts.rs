//! egui の代わりの字形に使う日本語フォント：どの macOS にもあるヒラギノ角ゴシック、なければ
//! よくある場所の日本語を持つほかのフォント。エージェントは専用のスレッドで 1 回だけ読み、
//! プロセスの間持つ。

use std::path::Path;

use reminedog_agent::os::FontFile;

/// 順に試すファイルと、コレクションの中の日本語の面。
const CANDIDATES: &[(&str, u32)] = &[
    ("/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc", 0),
    ("/System/Library/Fonts/ヒラギノ角ゴシック W4.ttc", 0),
    ("/System/Library/Fonts/ヒラギノ丸ゴ ProN W4.ttc", 0),
    ("/System/Library/Fonts/Supplemental/Arial Unicode.ttf", 0),
    ("/Library/Fonts/Arial Unicode.ttf", 0),
];

/// 候補のうち、読める最初のもの。
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
