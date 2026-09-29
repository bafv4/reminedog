//! Vertical placement of the Japanese font's glyphs.
//!
//! egui places each glyph at the font's ascent below the top of a row that is
//! `ascent - descent + line gap` tall, and centres rows on widgets such as check boxes. Fonts
//! whose ascent and descent are not balanced around the glyphs' middle (Yu Gothic and
//! Meiryo have a deep descent and a large line gap for Japanese typesetting) therefore draw
//! their text higher than the check box or radio button beside it. A [`FontTweak`] shifting
//! the glyphs by the difference puts them back in the middle of the row.

use egui::FontTweak;
use skrifa::MetadataProvider as _;
use skrifa::instance::{LocationRef, Size};

/// Height of the middle of the glyphs above the baseline, as a fraction of the font size.
/// Kana and kanji fill most of the em square, which sits about 0.12 em below the baseline
/// (0.88 em above it), so their middle is 0.38 em up; digits and capitals are close to that.
const GLYPH_MIDDLE: f32 = 0.38;

/// Largest shift applied, as a fraction of the font size; a font with metrics this odd is
/// more likely broken than misaligned. Yu Gothic needs about a third of this.
const MAX_SHIFT: f32 = 0.5;

/// A tweak that moves the glyphs of font `index` in `data` to the middle of egui's rows,
/// or the default tweak if the font cannot be read.
pub(crate) fn centering_tweak(data: &[u8], index: u32) -> FontTweak {
    FontTweak {
        y_offset_factor: centering_shift(data, index).unwrap_or(0.0),
        ..Default::default()
    }
}

/// How far down (as a fraction of the font size) the glyphs must move to be centred in a
/// row, from the font's vertical metrics.
pub(crate) fn centering_shift(data: &[u8], index: u32) -> Option<f32> {
    let font = skrifa::FontRef::from_index(data, index).ok()?;
    let metrics = font.metrics(Size::unscaled(), LocationRef::default());
    let em = f32::from(metrics.units_per_em);
    if em <= 0.0 {
        return None;
    }
    // Same metrics egui uses (skrifa, unscaled; descent is negative).
    shift_for(
        metrics.ascent / em,
        metrics.descent / em,
        metrics.leading / em,
    )
}

/// The shift for vertical metrics in ems.
fn shift_for(ascent: f32, descent: f32, line_gap: f32) -> Option<f32> {
    let row_height = ascent - descent + line_gap;
    // Row middle minus glyph middle, both measured from the top of the row.
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
        // Row 1.0 em, middle at 0.5; glyph middle at 0.88 - 0.38 = 0.5.
        assert!(shift.abs() < 1e-6, "{shift}");
    }

    #[test]
    fn deep_descent_and_line_gap_move_glyphs_down() {
        // Text sits at the top of a row padded below it: shift down.
        let shift = shift_for(0.88, -0.3, 0.5).unwrap();
        assert!((shift - 0.34).abs() < 1e-6, "{shift}");
        // A tall ascent puts the glyphs low: shift up.
        assert!(shift_for(1.3, -0.12, 0.0).unwrap() < 0.0);
    }

    #[test]
    fn absurd_metrics_are_clamped() {
        assert_eq!(shift_for(0.5, -10.0, 0.0), Some(MAX_SHIFT));
        assert_eq!(shift_for(f32::NAN, 0.0, 0.0), None);
    }

    #[test]
    fn egui_default_font_is_already_centred() {
        // egui's own font looks aligned with its check boxes; the rule agrees.
        let defs = egui::FontDefinitions::default();
        let font = &defs.font_data["Ubuntu-Light"];
        let shift = centering_shift(font.font.as_ref(), font.index).unwrap();
        assert!(shift.abs() < 0.05, "{shift}");
    }
}
