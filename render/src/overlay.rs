use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;

use egui::{
    Color32, FontData, FontDefinitions, FontFamily, Modifiers, Pos2, Rect, RichText, ViewportId,
    vec2,
};
use glow::HasContext as _;

use reminedog_core::Settings;
use reminedog_core::settings::ZOOM_FACTOR_RANGE;

use crate::font_metrics;
use crate::hotkey::Hotkey;
use crate::input::Captured;
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

/// How the zoom shows this frame.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum ZoomView {
    #[default]
    Off,
    /// The overlay enlarges the centre of the game's normal frame.
    Magnify,
    /// The game rendered a taller frame and the platform hook already put its centre on
    /// the screen, enlarged `factor` times.
    HighRes { factor: f32 },
}

/// This frame's input state, from [`crate::InputRouter`].
#[derive(Default)]
pub struct FrameInput {
    pub events: Vec<egui::Event>,
    pub modifiers: Modifiers,
    pub ui_open: bool,
    pub zoom: ZoomView,
    /// Where to draw the overlay's own cursor (points), when the game hides the real one.
    pub software_cursor: Option<Pos2>,
    /// The key captured after [`FrameOutput::start_capture`].
    pub captured: Option<Captured>,
    /// Why the high-resolution zoom cannot be used, shown in the menu.
    pub high_res_note: Option<String>,
}

/// What the platform hook has to act on after a frame.
#[derive(Debug, Default, PartialEq)]
pub struct FrameOutput {
    /// The UI's close button was clicked.
    pub close_ui: bool,
    /// Capture the next key for a hotkey ([`crate::InputRouter::start_capture`]).
    pub start_capture: bool,
    pub cancel_capture: bool,
    /// The settings changed: apply the hotkeys and save.
    pub settings: Option<Settings>,
}

/// The two hotkeys in `settings`; unreadable ones fall back to the defaults.
pub fn hotkeys(settings: &Settings) -> (Hotkey, Hotkey) {
    let parse = |text: &str, default: Hotkey, what: &str| {
        Hotkey::parse(text).unwrap_or_else(|| {
            log::warn!("settings: unknown {what} key {text:?}; using {default}");
            default
        })
    };
    (
        parse(&settings.menu_key, Hotkey::DEFAULT_MENU, "menu"),
        parse(&settings.zoom_key, Hotkey::DEFAULT_ZOOM, "zoom"),
    )
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Menu,
    Zoom,
}

impl Action {
    fn name(self) -> &'static str {
        match self {
            Action::Menu => "メニューを開く",
            Action::Zoom => "ズーム",
        }
    }
}

/// The settings and the menu's own state.
struct UiState {
    settings: Settings,
    menu_key: Hotkey,
    zoom_key: Hotkey,
    test_text: String,
    /// Waiting for a key for this hotkey.
    capturing: Option<Action>,
    /// Why the last captured key was not taken.
    key_note: Option<String>,
}

impl UiState {
    fn new(settings: Settings) -> Self {
        let (menu_key, zoom_key) = hotkeys(&settings);
        Self {
            settings,
            menu_key,
            zoom_key,
            test_text: String::new(),
            capturing: None,
            key_note: None,
        }
    }

    fn hotkey(&self, action: Action) -> Hotkey {
        match action {
            Action::Menu => self.menu_key,
            Action::Zoom => self.zoom_key,
        }
    }

    fn set_hotkey(&mut self, action: Action, hotkey: Hotkey) {
        match action {
            Action::Menu => {
                self.menu_key = hotkey;
                self.settings.menu_key = hotkey.to_string();
            }
            Action::Zoom => {
                self.zoom_key = hotkey;
                self.settings.zoom_key = hotkey.to_string();
            }
        }
    }

    /// Assigns a captured key unless it would make the zoom unreachable: the menu hotkey is
    /// checked first and matches with extra modifiers held.
    fn assign(&mut self, action: Action, hotkey: Hotkey) {
        let (menu, zoom) = match action {
            Action::Menu => (hotkey, self.zoom_key),
            Action::Zoom => (self.menu_key, hotkey),
        };
        let shadowed = menu.trigger == zoom.trigger
            && (!menu.ctrl || zoom.ctrl)
            && (!menu.shift || zoom.shift)
            && (!menu.alt || zoom.alt);
        if shadowed {
            let other = match action {
                Action::Menu => Action::Zoom,
                Action::Zoom => Action::Menu,
            };
            self.key_note = Some(format!(
                "{} は「{}」と重なるので使えない",
                hotkey.label(),
                other.name()
            ));
            return;
        }
        self.key_note = None;
        self.set_hotkey(action, hotkey);
    }
}

/// What the menu asked for this frame.
#[derive(Default)]
struct MenuActions {
    close: bool,
    capture: Option<Action>,
    cancel_capture: bool,
}

/// egui running on the agent's own GL context, drawing into the default framebuffer.
pub struct Overlay {
    ctx: egui::Context,
    painter: egui_glow::Painter,
    zoom: Zoom,
    state: UiState,
    first_time: Option<f64>,
    last_time: Option<f64>,
    frames: u64,
    fps: Fps,
}

impl Overlay {
    /// Must be called with the agent's GL context current.
    pub fn new(
        gl: Arc<glow::Context>,
        fonts: Vec<FontSource>,
        settings: Settings,
    ) -> Result<Self, OverlayError> {
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
            state: UiState::new(settings),
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
        let state = &mut self.state;
        let before = state.settings.clone();
        if let Some(captured) = input.captured {
            match (captured, state.capturing.take()) {
                (Captured::Hotkey(hotkey), Some(action)) => state.assign(action, hotkey),
                (Captured::Cancelled, _) | (_, None) => {}
            }
        }
        if !input.ui_open {
            state.capturing = None;
        }
        if input.zoom == ZoomView::Magnify {
            // SAFETY: the caller guarantees our context is current on the game's drawable.
            unsafe {
                self.zoom.draw(
                    self.painter.gl(),
                    [width, height],
                    state.settings.zoom_factor,
                    state.settings.zoom_smooth,
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
        let high_res_note = input.high_res_note.as_deref();
        let mut actions = MenuActions::default();
        let mut output = self.ctx.run_ui(raw, |ui| {
            if input.ui_open {
                // Dim the game so it is clear the overlay has the input.
                ui.painter()
                    .rect_filled(ui.max_rect(), 0.0, Color32::from_black_alpha(90));
                actions = main_window(ui.ctx(), state, &info, params.status, high_res_note);
            } else if state.settings.show_status {
                status_window(ui.ctx(), &info, params.status);
            }
            match input.zoom {
                ZoomView::Off => {}
                ZoomView::Magnify => zoom_badge(ui.ctx(), state.settings.zoom_factor),
                ZoomView::HighRes { factor } => zoom_badge(ui.ctx(), factor),
            }
            if show_hint {
                hint(ui.ctx(), state.menu_key);
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
        let mut out = FrameOutput {
            close_ui: actions.close,
            cancel_capture: actions.cancel_capture,
            ..Default::default()
        };
        if let Some(action) = actions.capture {
            state.capturing = Some(action);
            state.key_note = None;
            out.start_capture = true;
        }
        if state.settings != before {
            out.settings = Some(state.settings.clone());
        }
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

/// The menu.
fn main_window(
    ctx: &egui::Context,
    state: &mut UiState,
    info: &FrameInfo,
    status: &[StatusLine],
    high_res_note: Option<&str>,
) -> MenuActions {
    let mut actions = MenuActions::default();
    let mut open = true;
    egui::Window::new("reminedog")
        .id(egui::Id::new("reminedog-main"))
        .open(&mut open)
        .default_pos([40.0, 40.0])
        .resizable(false)
        .show(ctx, |ui| {
            ui.label(format!("{} か Esc で閉じる", state.menu_key.label()));
            ui.separator();

            ui.label(RichText::new("ズーム").strong());
            ui.label(format!(
                "ゲーム中に {} を押している間、画面の中央を拡大する",
                state.zoom_key.label()
            ));
            let (lo, hi) = ZOOM_FACTOR_RANGE;
            ui.add(egui::Slider::new(&mut state.settings.zoom_factor, lo..=hi).text("倍率"));
            ui.checkbox(&mut state.settings.zoom_high_res, "高精細にする")
                .on_hover_text(
                    "ゲームに縦長の解像度で描かせ、細かいところまで拡大する。\n\
                     倍率に比例して重くなる",
                );
            if let Some(note) = high_res_note.filter(|_| state.settings.zoom_high_res) {
                ui.label(RichText::new(note).weak());
            }
            // Used when high resolution is off or not available.
            ui.add_enabled_ui(
                !state.settings.zoom_high_res || high_res_note.is_some(),
                |ui| {
                    ui.checkbox(&mut state.settings.zoom_smooth, "なめらかに拡大する")
                        .on_hover_text("高精細でないときの拡大のしかた");
                },
            );
            ui.separator();

            ui.label(RichText::new("キー").strong());
            egui::Grid::new("reminedog-keys")
                .num_columns(2)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    for action in [Action::Menu, Action::Zoom] {
                        ui.label(action.name());
                        let waiting = state.capturing == Some(action);
                        let text = if waiting {
                            "キーを押す…".to_owned()
                        } else {
                            state.hotkey(action).label()
                        };
                        if ui.add(egui::Button::new(text).selected(waiting)).clicked() {
                            if waiting {
                                state.capturing = None;
                                actions.cancel_capture = true;
                            } else {
                                actions.capture = Some(action);
                            }
                        }
                        ui.end_row();
                    }
                });
            if state.capturing.is_some() {
                ui.label(
                    RichText::new("割り当てるキーかマウスのボタンを押す（Esc で取り消し）").weak(),
                );
            } else if let Some(note) = &state.key_note {
                ui.label(RichText::new(note).color(ui.visuals().warn_fg_color));
            }
            if ui.button("キーを元に戻す").clicked() {
                state.capturing = None;
                actions.cancel_capture = true;
                state.key_note = None;
                state.set_hotkey(Action::Menu, Hotkey::DEFAULT_MENU);
                state.set_hotkey(Action::Zoom, Hotkey::DEFAULT_ZOOM);
            }
            ui.separator();

            ui.label(RichText::new("入力のテスト").strong());
            ui.add(
                egui::TextEdit::singleline(&mut state.test_text)
                    .hint_text("日本語を入力できるか試す"),
            );
            ui.separator();

            egui::CollapsingHeader::new("状態")
                .id_salt("reminedog-status-section")
                .show(ui, |ui| status_grid(ui, info, status));
            ui.checkbox(
                &mut state.settings.show_status,
                "メニューを閉じても状態を表示する",
            );
        });
    actions.close = !open;
    actions
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

fn hint(ctx: &egui::Context, menu_key: Hotkey) {
    egui::Area::new(egui::Id::new("reminedog-hint"))
        .anchor(egui::Align2::LEFT_TOP, [12.0, 12.0])
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::popup(&ctx.global_style()).show(ui, |ui| {
                ui.label(format!("reminedog：{} でメニューを開く", menu_key.label()));
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

    fn state() -> UiState {
        UiState::new(Settings::default())
    }

    #[test]
    fn assigning_a_hotkey_updates_the_settings() {
        let mut state = state();
        state.assign(Action::Zoom, Hotkey::parse("C").unwrap());
        assert_eq!(state.settings.zoom_key, "C");
        assert_eq!(state.zoom_key, Hotkey::parse("C").unwrap());
        assert_eq!(state.key_note, None);
    }

    #[test]
    fn a_hotkey_that_would_hide_the_zoom_is_refused() {
        let mut state = state();
        // The menu on plain Z would swallow every zoom press.
        state.assign(Action::Menu, Hotkey::parse("Z").unwrap());
        assert_eq!(state.settings.menu_key, "Ctrl+I");
        assert!(state.key_note.is_some());
        // Same for the zoom on the menu's own key.
        state.assign(Action::Zoom, Hotkey::parse("Ctrl+I").unwrap());
        assert_eq!(state.settings.zoom_key, "Z");
        // Plain I for the zoom is fine: Ctrl+I opens the menu, I zooms.
        state.assign(Action::Zoom, Hotkey::parse("I").unwrap());
        assert_eq!(state.settings.zoom_key, "I");
        assert_eq!(state.key_note, None);
    }

    #[test]
    fn unreadable_hotkeys_fall_back_to_the_defaults() {
        let settings = Settings {
            menu_key: "Nope".into(),
            zoom_key: "Mouse4".into(),
            ..Settings::default()
        };
        let (menu, zoom) = hotkeys(&settings);
        assert_eq!(menu, Hotkey::DEFAULT_MENU);
        assert_eq!(zoom.to_string(), "Mouse4");
    }
}
