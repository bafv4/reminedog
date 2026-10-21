//! egui の代わりの字形に使う日本語フォント。fontconfig（`fc-match`）で探し、なければ
//! ディストリビューションが Noto Sans CJK や IPA ゴシックを置く場所から。エージェントは専用の
//! スレッドで 1 回だけ読み、プロセスの間持つ。

use std::path::{Path, PathBuf};
use std::process::Command;

use reminedog_agent::os::FontFile;

/// fontconfig が答えないときに試すファイルと、コレクションの中の日本語の面。
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

/// fontconfig が選ぶ日本語フォント。なければよくある場所のもの。
pub fn read() -> Option<FontFile> {
    let found = fontconfig().into_iter().chain(
        CANDIDATES
            .iter()
            .map(|&(path, index)| (PathBuf::from(path), index)),
    );
    for (path, index) in found {
        match std::fs::read(&path) {
            Ok(data) => {
                log::info!("fonts: {} (face {index})", path.display());
                return Some(FontFile {
                    name: file_name(&path),
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

/// fontconfig が日本語のゴシック体に選ぶもの：そのファイルと面。
fn fontconfig() -> Option<(PathBuf, u32)> {
    let output = Command::new("fc-match")
        .args(["--format=%{file}\n%{index}\n%{lang}", "sans-serif:lang=ja"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let choice = parse_match(&String::from_utf8_lossy(&output.stdout));
    if choice.is_none() {
        log::debug!("fonts: fontconfig has no Japanese sans-serif font");
    }
    choice
}

/// fc-match のファイル・番号・言語：フォントに日本語があればファイルと面（fc-match は日本語が
/// なくても一番近いフォントを答える）。可変フォントの名前付きインスタンスの番号は、面の 16 ビットの
/// 上にインスタンスを持つ。
fn parse_match(text: &str) -> Option<(PathBuf, u32)> {
    let mut lines = text.lines();
    let file = lines.next()?.trim();
    let index = lines
        .next()
        .and_then(|i| i.trim().parse::<u32>().ok())
        .unwrap_or(0)
        & 0xFFFF;
    let japanese = lines
        .next()
        .is_some_and(|langs| langs.split('|').any(|lang| lang.trim() == "ja"));
    (!file.is_empty() && japanese).then(|| (PathBuf::from(file), index))
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map_or_else(|| "japanese".into(), |n| n.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_match_needs_japanese() {
        let noto = "/usr/share/fonts/NotoSansCJK-Regular.ttc\n2\nen|ja|ko|zh-cn\n";
        assert_eq!(
            parse_match(noto),
            Some((PathBuf::from("/usr/share/fonts/NotoSansCJK-Regular.ttc"), 2))
        );
        let dejavu = "/usr/share/fonts/DejaVuSans.ttf\n0\naa|af|en|fr\n";
        assert_eq!(parse_match(dejavu), None);
        assert_eq!(parse_match(""), None);
    }

    #[test]
    fn a_named_instance_gives_its_face() {
        let variable = "/usr/share/fonts/NotoSansCJK-VF.ttc\n131074\nja|ko\n";
        assert_eq!(
            parse_match(variable),
            Some((PathBuf::from("/usr/share/fonts/NotoSansCJK-VF.ttc"), 2))
        );
    }
}
