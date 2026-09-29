use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;

use egui::{
    Color32, FontData, FontDefinitions, FontFamily, Modifiers, Pos2, Rect, RichText, ViewportId,
    vec2,
};
use glow::HasContext as _;

use crate::font_metrics;
use crate::zoom::Zoom;

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
    pub input: FrameInput,
}

/// This frame's input state, from [`crate::InputRouter`].
#[derive(Default)]
pub struct FrameInput {
    pub events: Vec<egui::Event>,
    pub modifiers: Modifiers,
    pub ui_open: bool,
    pub zoom: bool,
    /// Where to draw the overlay's own cursor (points), when the game hides the real one.
    pub software_cursor: Option<Pos2>,
}

/// What the platform hook has to act on after a frame.
#[derive(Debug, Default, PartialEq)]
pub struct FrameOutput {
    /// The UI's close button was clicked.
    pub close_ui: bool,
}

#[derive(Debug)]
pub struct OverlayError(String);

impl fmt::Display for OverlayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for OverlayError {}

/// How long the hotkey hint shows after the first frame, in seconds.
const HINT_SECONDS: f64 = 10.0;

/// Settings changed from the UI. Not saved yet.
struct Settings {
    zoom_factor: f32,
    zoom_smooth: bool,
    show_status: bool,
    test_text: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            zoom_factor: 4.0,
            zoom_smooth: true,
            show_status: false,
            test_text: String::new(),
        }
    }
}

/// egui running on the agent's own GL context, drawing into the default framebuffer.
pub struct Overlay {
    ctx: egui::Context,
    painter: egui_glow::Painter,
    zoom: Zoom,
    settings: Settings,
    first_time: Option<f64>,
    last_time: Option<f64>,
    frames: u64,
    fps: Fps,
}

impl Overlay {
    /// Must be called with the agent's GL context current.
    pub fn new(gl: Arc<glow::Context>, fonts: Vec<FontSource>) -> Result<Self, OverlayError> {
        let zoom = Zoom::new(&gl);
        let painter = egui_glow::Painter::new(gl, "", None, false)
            .map_err(|e| OverlayError(format!("egui_glow painter: {e}")))?;
        let ctx = egui::Context::default();
        ctx.set_theme(egui::Theme::Dark);
        ctx.set_fonts(font_definitions(fonts));
        Ok(Self {
            ctx,
            painter,
            zoom,
            settings: Settings::default(),
            first_time: None,
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
    pub fn render(&mut self, params: FrameParams<'_>) -> FrameOutput {
        let [width, height] = params.framebuffer_size;
        if width == 0 || height == 0 {
            return FrameOutput::default();
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
        let first_time = *self.first_time.get_or_insert(params.time);
        self.frames += 1;
        self.fps.tick(params.time);

        let input = params.input;
        if input.zoom {
            // SAFETY: the caller guarantees our context is current on the game's drawable.
            unsafe {
                self.zoom.draw(
                    self.painter.gl(),
                    [width, height],
                    self.settings.zoom_factor,
                    self.settings.zoom_smooth,
                );
            }
        }

        let mut raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                vec2(width as f32 / ppp, height as f32 / ppp),
            )),
            max_texture_side: Some(self.painter.max_texture_side()),
            time: Some(params.time),
            predicted_dt: dt,
            events: std::iter::once(egui::Event::ModifiersChanged(input.modifiers))
                .chain(input.events)
                .collect(),
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
        let show_hint = !input.ui_open && params.time - first_time < HINT_SECONDS;
        let settings = &mut self.settings;
        let mut out = FrameOutput::default();
        let mut output = self.ctx.run_ui(raw, |ui| {
            if input.ui_open {
                // Dim the game so it is clear the overlay has the input.
                ui.painter()
                    .rect_filled(ui.max_rect(), 0.0, Color32::from_black_alpha(90));
                out.close_ui = main_window(ui.ctx(), settings, &info, params.status);
            } else if settings.show_status {
                status_window(ui.ctx(), &info, params.status);
            }
            if input.zoom {
                zoom_badge(ui.ctx(), settings.zoom_factor);
            }
            if show_hint {
                hint(ui.ctx());
            }
            if let Some(pos) = input.software_cursor {
                draw_cursor(ui.ctx(), pos);
            }
        });
        let primitives = self
            .ctx
            .tessellate(std::mem::take(&mut output.shapes), output.pixels_per_point);

        // Nothing but the zoom (which rebinds 0 when done) binds another framebuffer in our
        // dedicated context, so this paints into the window's default framebuffer.
        self.painter.paint_and_update_textures(
            [width, height],
            output.pixels_per_point,
            &primitives,
            &mut output.textures_delta,
        );
        out
    }

    /// Frees GL resources. Must be called with the agent's GL context current.
    pub fn destroy(&mut self) {
        // SAFETY: the caller guarantees our context is current.
        unsafe { self.zoom.destroy(self.painter.gl()) };
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
        // Japanese fonts sit high in egui's rows (above the check boxes) without this.
        let tweak = font_metrics::centering_tweak(&font.data, font.index);
        log::debug!(
            "font {}: glyphs shifted down by {:.3} em",
            font.name,
            tweak.y_offset_factor
        );
        defs.font_data.insert(
            font.name.clone(),
            Arc::new(FontData {
                font: Cow::Owned(font.data),
                index: font.index,
                tweak,
            }),
        );
        // Proportional text uses this font first: mixing it with egui's own Latin font put
        // digits and kana on different baselines ("プロトタイプ 1" with a sunken "1").
        // Monospace keeps egui's font and only falls back to this one.
        defs.families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, font.name.clone());
        defs.families
            .entry(FontFamily::Monospace)
            .or_default()
            .push(font.name.clone());
    }
    defs
}

struct FrameInfo {
    frames: u64,
    fps: f32,
    size: [u32; 2],
    ppp: f32,
}

fn status_grid(ui: &mut egui::Ui, info: &FrameInfo, status: &[StatusLine]) {
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
}

/// Read-only status in the corner while the UI is closed (opt-in).
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
        .show(ctx, |ui| status_grid(ui, info, status));
}

/// The menu opened with Ctrl+I. Returns true when its close button was clicked.
fn main_window(
    ctx: &egui::Context,
    settings: &mut Settings,
    info: &FrameInfo,
    status: &[StatusLine],
) -> bool {
    let mut open = true;
    egui::Window::new("reminedog")
        .id(egui::Id::new("reminedog-main"))
        .open(&mut open)
        .default_pos([40.0, 40.0])
        .resizable(false)
        .show(ctx, |ui| {
            ui.label("Ctrl+I か Esc で閉じる");
            ui.separator();

            ui.label(RichText::new("ズーム").strong());
            ui.label("ゲーム中に Z を押している間、画面の中央を拡大する");
            ui.add(egui::Slider::new(&mut settings.zoom_factor, 1.5..=8.0).text("倍率"));
            ui.checkbox(&mut settings.zoom_smooth, "なめらかに拡大する");
            ui.separator();

            ui.label(RichText::new("入力のテスト").strong());
            ui.add(
                egui::TextEdit::singleline(&mut settings.test_text)
                    .hint_text("日本語を入力できるか試す"),
            );
            ui.separator();

            egui::CollapsingHeader::new("状態")
                .id_salt("reminedog-status-section")
                .show(ui, |ui| status_grid(ui, info, status));
            ui.checkbox(
                &mut settings.show_status,
                "メニューを閉じても状態を表示する",
            );
        });
    !open
}

fn zoom_badge(ctx: &egui::Context, factor: f32) {
    egui::Area::new(egui::Id::new("reminedog-zoom"))
        .anchor(egui::Align2::CENTER_TOP, [0.0, 12.0])
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::popup(&ctx.global_style()).show(ui, |ui| {
                ui.label(format!("ズーム ×{factor:.1}"));
            });
        });
}

fn hint(ctx: &egui::Context) {
    egui::Area::new(egui::Id::new("reminedog-hint"))
        .anchor(egui::Align2::LEFT_TOP, [12.0, 12.0])
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::popup(&ctx.global_style()).show(ui, |ui| {
                ui.label("reminedog：Ctrl+I でメニューを開く");
            });
        });
}

/// An arrow cursor, drawn on top of everything while the game hides the real cursor.
fn draw_cursor(ctx: &egui::Context, pos: Pos2) {
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Debug,
        egui::Id::new("reminedog-cursor"),
    ));
    let points = [
        pos,
        pos + vec2(0.0, 17.0),
        pos + vec2(4.5, 13.0),
        pos + vec2(12.0, 12.5),
    ];
    painter.add(egui::Shape::convex_polygon(
        points.to_vec(),
        Color32::WHITE,
        egui::Stroke::new(1.0, Color32::BLACK),
    ));
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
    fn japanese_font_leads_proportional_and_backs_up_monospace() {
        let defs = font_definitions(vec![FontSource {
            name: "jp".into(),
            data: vec![],
            index: 1,
        }]);
        assert_eq!(defs.font_data["jp"].index, 1);
        let proportional = &defs.families[&FontFamily::Proportional];
        assert_eq!(proportional.first().map(String::as_str), Some("jp"));
        assert!(proportional.len() > 1, "egui's fonts remain as fallbacks");
        let monospace = &defs.families[&FontFamily::Monospace];
        assert_eq!(monospace.last().map(String::as_str), Some("jp"));
        assert!(monospace.len() > 1, "egui's monospace font stays first");
    }
}
