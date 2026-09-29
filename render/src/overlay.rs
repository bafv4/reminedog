use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;

use egui::{
    Color32, FontData, FontDefinitions, FontFamily, Pos2, Rect, RichText, ViewportId, vec2,
};
use glow::HasContext as _;

/// A font file added as a fallback for glyphs egui's built-in fonts lack (Japanese).
pub struct FontSource {
    pub name: String,
    pub data: Vec<u8>,
    /// Face index inside a font collection (`.ttc`); 0 for plain `.ttf`/`.otf`.
    pub index: u32,
}

/// One row of diagnostic text in the status window.
#[derive(Debug, Clone)]
pub struct StatusLine {
    pub label: String,
    pub value: String,
}

impl StatusLine {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
        }
    }
}

/// Per-frame inputs from the platform hook.
pub struct FrameParams<'a> {
    /// Size of the default framebuffer in physical pixels.
    pub framebuffer_size: [u32; 2],
    /// Physical pixels per egui point (window content scale times the user's UI scale).
    pub pixels_per_point: f32,
    /// Monotonic time in seconds.
    pub time: f64,
    pub status: &'a [StatusLine],
}

#[derive(Debug)]
pub struct OverlayError(String);

impl fmt::Display for OverlayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for OverlayError {}

/// egui running on the agent's own GL context, drawing into the default framebuffer.
pub struct Overlay {
    ctx: egui::Context,
    painter: egui_glow::Painter,
    last_time: Option<f64>,
    frames: u64,
    fps: Fps,
}

impl Overlay {
    /// Must be called with the agent's GL context current.
    pub fn new(gl: Arc<glow::Context>, fonts: Vec<FontSource>) -> Result<Self, OverlayError> {
        let painter = egui_glow::Painter::new(gl, "", None, false)
            .map_err(|e| OverlayError(format!("egui_glow painter: {e}")))?;
        let ctx = egui::Context::default();
        ctx.set_theme(egui::Theme::Dark);
        ctx.set_fonts(font_definitions(fonts));
        Ok(Self {
            ctx,
            painter,
            last_time: None,
            frames: 0,
            fps: Fps::default(),
        })
    }

    /// Number of frames rendered so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Draws one frame into the default framebuffer. Must be called with the agent's GL
    /// context current on the game's drawable, right before the buffer swap.
    pub fn render(&mut self, params: &FrameParams<'_>) {
        let [width, height] = params.framebuffer_size;
        if width == 0 || height == 0 {
            return;
        }
        let ppp = if params.pixels_per_point.is_finite() {
            params.pixels_per_point.clamp(0.25, 8.0)
        } else {
            1.0
        };
        let dt = self
            .last_time
            .map_or(1.0 / 60.0, |t| (params.time - t) as f32)
            .clamp(0.001, 0.25);
        self.last_time = Some(params.time);
        self.frames += 1;
        self.fps.tick(params.time);

        let mut raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                vec2(width as f32 / ppp, height as f32 / ppp),
            )),
            max_texture_side: Some(self.painter.max_texture_side()),
            time: Some(params.time),
            predicted_dt: dt,
            ..Default::default()
        };
        raw.viewports
            .entry(ViewportId::ROOT)
            .or_default()
            .native_pixels_per_point = Some(ppp);

        let info = FrameInfo {
            frames: self.frames,
            fps: self.fps.value,
            size: [width, height],
            ppp,
        };
        let mut output = self
            .ctx
            .run_ui(raw, |ui| status_window(ui.ctx(), &info, params.status));
        let primitives = self
            .ctx
            .tessellate(std::mem::take(&mut output.shapes), output.pixels_per_point);

        // Nothing ever binds another framebuffer in our dedicated context, so this paints
        // into the window's default framebuffer (and needs no GL 3 entry point).
        self.painter.paint_and_update_textures(
            [width, height],
            output.pixels_per_point,
            &primitives,
            &mut output.textures_delta,
        );
    }

    /// Frees GL resources. Must be called with the agent's GL context current.
    pub fn destroy(&mut self) {
        self.painter.destroy();
    }
}

/// "version | renderer | vendor" of the current context, for diagnostics.
pub fn gl_summary(gl: &glow::Context) -> String {
    // SAFETY: plain queries on the current context.
    unsafe {
        format!(
            "{} | {} | {}",
            gl.get_parameter_string(glow::VERSION),
            gl.get_parameter_string(glow::RENDERER),
            gl.get_parameter_string(glow::VENDOR)
        )
    }
}

fn font_definitions(fonts: Vec<FontSource>) -> FontDefinitions {
    let mut defs = FontDefinitions::default();
    for font in fonts {
        defs.font_data.insert(
            font.name.clone(),
            Arc::new(FontData {
                font: Cow::Owned(font.data),
                index: font.index,
                tweak: Default::default(),
            }),
        );
        // Appended after egui's own fonts: Latin text keeps egui's look, and anything
        // those fonts lack (kana, kanji) falls back to this one.
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            defs.families
                .entry(family)
                .or_default()
                .push(font.name.clone());
        }
    }
    defs
}

struct FrameInfo {
    frames: u64,
    fps: f32,
    size: [u32; 2],
    ppp: f32,
}

fn status_window(ctx: &egui::Context, info: &FrameInfo, status: &[StatusLine]) {
    let frame = egui::Frame::window(&ctx.global_style()).fill(Color32::from_black_alpha(200));
    egui::Window::new("reminedog")
        .id(egui::Id::new("reminedog-status"))
        .fixed_pos([12.0, 12.0])
        .resizable(false)
        .collapsible(false)
        .movable(false)
        .interactable(false)
        .frame(frame)
        .show(ctx, |ui| {
            ui.label(RichText::new("プロトタイプ 1：オーバーレイの表示テスト").strong());
            ui.label("日本語の表示テスト：あいうえお・カタカナ・漢字");
            ui.separator();
            egui::Grid::new("reminedog-status-grid")
                .num_columns(2)
                .spacing([12.0, 2.0])
                .show(ui, |ui| {
                    let mut row = |label: &str, value: String| {
                        ui.label(label);
                        ui.label(value);
                        ui.end_row();
                    };
                    row("フレーム", info.frames.to_string());
                    row("FPS", format!("{:.0}", info.fps));
                    row("解像度", format!("{}×{}", info.size[0], info.size[1]));
                    row("スケール", format!("{:.2}", info.ppp));
                    for line in status {
                        row(&line.label, line.value.clone());
                    }
                });
        });
}

/// Frames per second, averaged over half-second windows.
#[derive(Default)]
struct Fps {
    window_start: Option<f64>,
    frames: u32,
    value: f32,
}

impl Fps {
    fn tick(&mut self, now: f64) {
        // The tick that opens a window marks its start and is not counted in it.
        let Some(start) = self.window_start else {
            self.window_start = Some(now);
            return;
        };
        self.frames += 1;
        let elapsed = now - start;
        if elapsed >= 0.5 {
            self.value = (f64::from(self.frames) / elapsed) as f32;
            self.window_start = Some(now);
            self.frames = 0;
        } else if elapsed < 0.0 {
            // Clock went backwards; start over.
            self.window_start = Some(now);
            self.frames = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fps_averages_over_half_second() {
        let mut fps = Fps::default();
        for i in 0..=60 {
            fps.tick(f64::from(i) / 120.0);
        }
        assert!((fps.value - 120.0).abs() < 1.0, "{}", fps.value);
    }

    #[test]
    fn fonts_are_appended_as_fallbacks() {
        let defs = font_definitions(vec![FontSource {
            name: "jp".into(),
            data: vec![],
            index: 1,
        }]);
        assert_eq!(defs.font_data["jp"].index, 1);
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            let names = &defs.families[&family];
            assert_eq!(names.last().map(String::as_str), Some("jp"));
            assert!(names.len() > 1, "egui's own fonts stay first");
        }
    }
}
