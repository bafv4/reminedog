//! 日本語フォントの字形の縦の位置。
//!
//! egui は、高さが `ascent - descent + line gap` の行の上から、フォントの ascent だけ下に各字形を
//! 置き、チェックボックスなどの部品では行を中央に置く。ascent と descent が字形の中央の周りで
//! つり合っていないフォント（游ゴシックとメイリオは、日本語の組版のために descent が深く、
//! line gap が大きい）は、そのため、隣のチェックボックスやラジオボタンより高く文字を描く。
//! 差の分だけ字形をずらす [`FontTweak`] で、行の中央に戻す。

use egui::FontTweak;
use skrifa::MetadataProvider as _;
use skrifa::instance::{LocationRef, Size};

/// ベースラインからの字形の中央の高さ（フォントの大きさに対する割合）。かなと漢字は em の
/// 四角のほとんどを埋め、その四角はベースラインの約 0.12 em 下にある（上に 0.88 em）ので、
/// 中央は 0.38 em 上。数字と大文字もそれに近い。
const GLYPH_MIDDLE: f32 = 0.38;

/// ずらす最大の量（フォントの大きさに対する割合）。これほど変な値のフォントは、ずれているより
/// 壊れている見込みが高い。游ゴシックはこれの約 3 分の 1 が要る。
const MAX_SHIFT: f32 = 0.5;

/// `data` のフォント `index` の字形を egui の行の中央に動かす調整。フォントを読めなければ
/// 既定の調整。
pub(crate) fn centering_tweak(data: &[u8], index: u32) -> FontTweak {
    FontTweak {
        y_offset_factor: centering_shift(data, index).unwrap_or(0.0),
        ..Default::default()
    }
}

/// フォントの縦の値から求めた、字形を行の中央に置くために下へ動かす量（フォントの大きさに
/// 対する割合）。
pub(crate) fn centering_shift(data: &[u8], index: u32) -> Option<f32> {
    let font = skrifa::FontRef::from_index(data, index).ok()?;
    let metrics = font.metrics(Size::unscaled(), LocationRef::default());
    let em = f32::from(metrics.units_per_em);
    if em <= 0.0 {
        return None;
    }
    // egui と同じ値（skrifa、拡大なし。descent は負）。
    shift_for(
        metrics.ascent / em,
        metrics.descent / em,
        metrics.leading / em,
    )
}

/// em での縦の値についてのずらす量。
fn shift_for(ascent: f32, descent: f32, line_gap: f32) -> Option<f32> {
    let row_height = ascent - descent + line_gap;
    // 行の中央から字形の中央を引いたもの（どちらも行の上から測る）。
    let shift = row_height / 2.0 - (ascent - GLYPH_MIDDLE);
    shift
        .is_finite()
        .then(|| shift.clamp(-MAX_SHIFT, MAX_SHIFT))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreadable_font_gets_no_shift() {
        assert_eq!(centering_shift(&[], 0), None);
        assert_eq!(centering_tweak(b"not a font", 0).y_offset_factor, 0.0);
    }

    #[test]
    fn balanced_metrics_need_no_shift() {
        let shift = shift_for(0.88, -0.12, 0.0).unwrap();
        // 行は 1.0 em、中央は 0.5。字形の中央は 0.88 - 0.38 = 0.5。
        assert!(shift.abs() < 1e-6, "{shift}");
    }

    #[test]
    fn deep_descent_and_line_gap_move_glyphs_down() {
        // 文字が、下に余白のある行の上の方にある：下にずらす。
        let shift = shift_for(0.88, -0.3, 0.5).unwrap();
        assert!((shift - 0.34).abs() < 1e-6, "{shift}");
        // 高い ascent は字形を低く置く：上にずらす。
        assert!(shift_for(1.3, -0.12, 0.0).unwrap() < 0.0);
    }

    #[test]
    fn absurd_metrics_are_clamped() {
        assert_eq!(shift_for(0.5, -10.0, 0.0), Some(MAX_SHIFT));
        assert_eq!(shift_for(f32::NAN, 0.0, 0.0), None);
    }

    #[test]
    fn egui_default_font_is_already_centred() {
        // egui 自身のフォントはチェックボックスとそろって見える。決まりもそれと合う。
        let defs = egui::FontDefinitions::default();
        let font = &defs.font_data["Ubuntu-Light"];
        let shift = centering_shift(font.font.as_ref(), font.index).unwrap();
        assert!(shift.abs() < 0.05, "{shift}");
    }
}
