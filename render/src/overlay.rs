use std::borrow::Cow;
use std::fmt;
use std::sync::Arc;

use egui::{
    Color32, FontData, FontDefinitions, FontFamily, Modifiers, Pos2, Rect, RichText, ViewportId,
    vec2,
};
use glow::HasContext as _;

use reminedog_core::settings::ZOOM_FACTOR_RANGE;
use reminedog_core::{InputId, Settings, input_label};

use crate::font_metrics;
use crate::hotkey::{Hotkey, Trigger, input_trigger};
use crate::input::Captured;
use crate::rebinds_ui::{RebindMenu, rebind_section, rule_sources};
use crate::waypoints::{
    Notice, NoticeList, WaypointCommand, WaypointMenu, WaypointView, draw_destination,
    draw_notices, waypoint_section,
};
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
    pub waypoints: WaypointView,
    /// New notices this frame; the overlay keeps them until they expire.
    pub notices: Vec<Notice>,
    /// Minecraft's debug modifier, copy-location and crash keys, which the waypoint and
    /// navigate hotkeys must not take.
    pub reserved_keys: Vec<egui::Key>,
    /// The game's mappings on each key, as `reminedog_core::bindings_by_key` reads them from
    /// options.txt (mapping ids; labels work too), shown with the rebinding's outputs.
    pub game_bindings: Vec<(InputId, Vec<String>)>,
    /// Why some rebinding rules cannot work now (e.g. keys as sources), shown in the menu.
    pub rebind_note: Option<String>,
    /// Keys and buttons of the rebinding rules that the game's window library cannot report
    /// (as a source) or give the game (as an output); the menu notes the rules that use them.
    pub unsupported_inputs: Vec<InputId>,
}

/// What the platform hook has to act on after a frame.
#[derive(Debug, Default, PartialEq)]
pub struct FrameOutput {
    /// The UI's close button was clicked.
    pub close_ui: bool,
    /// Capture the next key for a hotkey ([`crate::InputRouter::start_capture`]).
    pub start_capture: bool,
    /// Capture the next key or mouse button for the rebinding
    /// ([`crate::InputRouter::start_input_capture`]).
    pub start_input_capture: bool,
    pub cancel_capture: bool,
    /// The settings changed: apply the hotkeys and save.
    pub settings: Option<Settings>,
    /// Asked for in the menu's waypoint section, in order.
    pub waypoint_commands: Vec<WaypointCommand>,
}

/// The user's hotkeys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkeys {
    /// Opens and closes the menu.
    pub menu: Hotkey,
    /// Zooms while held.
    pub zoom: Hotkey,
    /// Records a waypoint at the player's position.
    pub waypoint: Hotkey,
    /// Refreshes the player's position and shows the way to the selected waypoint.
    pub navigate: Hotkey,
}

impl Hotkeys {
    pub const DEFAULT: Hotkeys = Hotkeys {
        menu: Hotkey::DEFAULT_MENU,
        zoom: Hotkey::DEFAULT_ZOOM,
        waypoint: Hotkey::DEFAULT_WAYPOINT,
        navigate: Hotkey::DEFAULT_NAVIGATE,
    };

    pub(crate) fn get(&self, action: Action) -> Hotkey {
        match action {
            Action::Menu => self.menu,
            Action::Zoom => self.zoom,
            Action::Waypoint => self.waypoint,
            Action::Navigate => self.navigate,
        }
    }

    fn set(&mut self, action: Action, hotkey: Hotkey) {
        match action {
            Action::Menu => self.menu = hotkey,
            Action::Zoom => self.zoom = hotkey,
            Action::Waypoint => self.waypoint = hotkey,
            Action::Navigate => self.navigate = hotkey,
        }
    }
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The hotkeys in `settings`. Unreadable ones fall back to the defaults, and so does one an
/// earlier hotkey would hide (see `hides`), which only a hand-edited or older settings file
/// can have.
pub fn hotkeys(settings: &Settings) -> Hotkeys {
    let mut keys = Hotkeys::DEFAULT;
    for action in Action::ALL {
        let text = action.setting(settings);
        let default = action.default_hotkey();
        let hotkey = Hotkey::parse(text).unwrap_or_else(|| {
            log::warn!(
                "settings: unknown {} key {text:?}; using {default}",
                action.id()
            );
            default
        });
        let hotkey = if action.uses_f3c() && hotkey.ctrl {
            log::warn!(
                "settings: the {} key {hotkey} needs Ctrl, which F3+C cannot be sent with; using {default}",
                action.id()
            );
            default
        } else {
            hotkey
        };
        keys.set(action, hotkey);
    }
    for action in Action::ROUTER_ORDER {
        let Some(earlier) = hidden_by(&keys, action) else {
            continue;
        };
        let default = action.default_hotkey();
        log::warn!(
            "settings: the {} key {} is hidden by the {} key {}; using {default}",
            action.id(),
            keys.get(action),
            earlier.id(),
            keys.get(earlier)
        );
        keys.set(action, default);
        if let Some(earlier) = hidden_by(&keys, action) {
            log::warn!(
                "settings: the {} key {default} is hidden by the {} key {} too",
                action.id(),
                earlier.id(),
                keys.get(earlier)
            );
        }
    }
    keys
}

/// Whether `earlier`, which the input router checks first, takes every press of `later`:
/// the same trigger, and no modifier `later` does not need too (extra modifiers still match).
/// The zoom is checked last, so its match ignoring modifiers never hides another key.
fn hides(earlier: Hotkey, later: Hotkey) -> bool {
    earlier.trigger == later.trigger
        && (!earlier.ctrl || later.ctrl)
        && (!earlier.shift || later.shift)
        && (!earlier.alt || later.alt)
}

/// The action checked before `action` whose hotkey hides `action`'s.
fn hidden_by(keys: &Hotkeys, action: Action) -> Option<Action> {
    Action::ROUTER_ORDER
        .into_iter()
        .take_while(|&earlier| earlier != action)
        .find(|&earlier| hides(keys.get(earlier), keys.get(action)))
}

/// Another action whose hotkey hides `action`'s, or is hidden by it.
fn conflict(keys: &Hotkeys, action: Action) -> Option<Action> {
    hidden_by(keys, action).or_else(|| {
        Action::ROUTER_ORDER
            .into_iter()
            .skip_while(|&earlier| earlier != action)
            .skip(1)
            .find(|&later| hides(keys.get(action), keys.get(later)))
    })
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
pub(crate) enum Action {
    Menu,
    Zoom,
    Waypoint,
    Navigate,
}

impl Action {
    /// As listed in the menu.
    pub(crate) const ALL: [Action; 4] = [
        Action::Menu,
        Action::Zoom,
        Action::Waypoint,
        Action::Navigate,
    ];
    /// The order in which the input router matches the hotkeys.
    const ROUTER_ORDER: [Action; 4] = [
        Action::Menu,
        Action::Waypoint,
        Action::Navigate,
        Action::Zoom,
    ];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Action::Menu => "メニューを開く",
            Action::Zoom => "ズーム",
            Action::Waypoint => "ウェイポイントを記録",
            Action::Navigate => "現在地を更新",
        }
    }

    /// For the log.
    pub(crate) fn id(self) -> &'static str {
        match self {
            Action::Menu => "menu",
            Action::Zoom => "zoom",
            Action::Waypoint => "waypoint",
            Action::Navigate => "navigate",
        }
    }

    fn default_hotkey(self) -> Hotkey {
        Hotkeys::DEFAULT.get(self)
    }

    fn setting(self, settings: &Settings) -> &str {
        match self {
            Action::Menu => &settings.menu_key,
            Action::Zoom => &settings.zoom_key,
            Action::Waypoint => &settings.waypoint_key,
            Action::Navigate => &settings.navigate_key,
        }
    }

    fn setting_mut(self, settings: &mut Settings) -> &mut String {
        match self {
            Action::Menu => &mut settings.menu_key,
            Action::Zoom => &mut settings.zoom_key,
            Action::Waypoint => &mut settings.waypoint_key,
            Action::Navigate => &mut settings.navigate_key,
        }
    }

    /// Records or refreshes the position with F3+C, so it cannot use F3+C's own keys.
    fn uses_f3c(self) -> bool {
        matches!(self, Action::Waypoint | Action::Navigate)
    }
}

/// The settings and the menu's own state.
struct UiState {
    settings: Settings,
    keys: Hotkeys,
    /// [`FrameInput::reserved_keys`] of the current frame.
    reserved_keys: Vec<egui::Key>,
    /// Waiting for a key for this hotkey.
    capturing: Option<Action>,
    /// Why the last captured key was not taken.
    key_note: Option<String>,
    waypoints: WaypointMenu,
    rebinds: RebindMenu,
    /// [`FrameInput::game_bindings`], [`FrameInput::rebind_note`] and
    /// [`FrameInput::unsupported_inputs`] of the current frame.
    game_bindings: Vec<(InputId, Vec<String>)>,
    rebind_note: Option<String>,
    unsupported_inputs: Vec<InputId>,
}

impl UiState {
    fn new(mut settings: Settings) -> Self {
        let keys = hotkeys(&settings);
        // What the router uses: a key that fell back to its default must not come back when
        // the key hiding it moves.
        for action in Action::ALL {
            *action.setting_mut(&mut settings) = keys.get(action).to_string();
        }
        Self {
            settings,
            keys,
            reserved_keys: Vec::new(),
            capturing: None,
            key_note: None,
            waypoints: WaypointMenu::default(),
            rebinds: RebindMenu::default(),
            game_bindings: Vec::new(),
            rebind_note: None,
            unsupported_inputs: Vec::new(),
        }
    }

    fn set_hotkey(&mut self, action: Action, hotkey: Hotkey) {
        self.keys.set(action, hotkey);
        *action.setting_mut(&mut self.settings) = hotkey.to_string();
    }

    /// Assigns a captured key unless another hotkey would become unreachable (the router
    /// checks them in order and matches with extra modifiers held), it is one of F3+C's keys
    /// for an action that sends F3+C, or a rebinding rule's source (the router would take its
    /// presses for the hotkey before the rule).
    fn assign(&mut self, action: Action, hotkey: Hotkey) {
        if action.uses_f3c()
            && self
                .reserved_keys
                .iter()
                .any(|&key| hotkey.trigger == Trigger::Key(key))
        {
            self.key_note = Some(format!(
                "{} は F3+C に使うキーなので使えない",
                Hotkey::plain(hotkey.trigger).label()
            ));
            return;
        }
        // The key events would reach the game with Ctrl held (Ctrl+drop throws a whole stack).
        if action.uses_f3c() && hotkey.ctrl {
            self.key_note = Some(format!(
                "Ctrl との組み合わせ（{}）は「{}」に使えない",
                hotkey.label(),
                action.name()
            ));
            return;
        }
        if let Some(source) =
            rule_sources(&self.settings).find(|&id| input_trigger(id) == Some(hotkey.trigger))
        {
            self.key_note = Some(format!("{} は置き換えに使っている", input_label(source)));
            return;
        }
        let mut keys = self.keys;
        keys.set(action, hotkey);
        if let Some(other) = conflict(&keys, action) {
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

    /// Puts the hotkeys back to their defaults, but as [`assign`](Self::assign) refuses a
    /// rebinding rule's source: an action whose default is one keeps its key (with a note),
    /// and so does one whose default would then hide it or be hidden by it.
    fn reset_hotkeys(&mut self) {
        let mut note = None;
        let mut keep: Vec<Action> = Vec::new();
        for action in Action::ALL {
            let default = action.default_hotkey();
            if let Some(source) =
                rule_sources(&self.settings).find(|&id| input_trigger(id) == Some(default.trigger))
            {
                // Only a key that stays away from its default needs saying so.
                if self.keys.get(action) != default {
                    note.get_or_insert_with(|| {
                        format!("{} は置き換えに使っている", input_label(source))
                    });
                }
                keep.push(action);
            }
        }
        let keys = loop {
            let mut keys = Hotkeys::DEFAULT;
            for &action in &keep {
                keys.set(action, self.keys.get(action));
            }
            match Action::ALL
                .into_iter()
                .find(|&action| !keep.contains(&action) && conflict(&keys, action).is_some())
            {
                Some(action) => keep.push(action),
                None => break keys,
            }
        };
        for action in Action::ALL {
            self.set_hotkey(action, keys.get(action));
        }
        self.key_note = note;
    }
}

/// What the menu asked for this frame.
#[derive(Default)]
struct MenuActions {
    close: bool,
    capture: Option<Action>,
    /// Capture a key or button for the rebinding.
    capture_input: bool,
    cancel_capture: bool,
    waypoint_commands: Vec<WaypointCommand>,
}

/// egui running on the agent's own GL context, drawing into the default framebuffer.
pub struct Overlay {
    ctx: egui::Context,
    painter: egui_glow::Painter,
    zoom: Zoom,
    state: UiState,
    notices: NoticeList,
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
            notices: NoticeList::default(),
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
        state.reserved_keys = input.reserved_keys;
        state.game_bindings = input.game_bindings;
        state.rebind_note = input.rebind_note;
        state.unsupported_inputs = input.unsupported_inputs;
        let mut capture_again = false;
        if let Some(captured) = input.captured {
            if let (Captured::Hotkey(hotkey), Some(action)) = (captured, state.capturing.take()) {
                state.assign(action, hotkey);
            }
            capture_again = state
                .rebinds
                .on_captured(captured, &mut state.settings, &state.keys);
        }
        if !input.ui_open {
            state.capturing = None;
            state.rebinds.cancel();
            capture_again = false;
        }
        state.waypoints.sync(input.ui_open, &input.waypoints);
        self.notices.update(input.notices, params.time);
        let notices = &self.notices;
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
                actions = main_window(
                    ui.ctx(),
                    state,
                    &info,
                    params.status,
                    high_res_note,
                    &input.waypoints,
                );
            } else if state.settings.show_status {
                status_window(ui.ctx(), &info, params.status);
            }
            match input.zoom {
                ZoomView::Off => {}
                ZoomView::Magnify => zoom_badge(ui.ctx(), state.settings.zoom_factor),
                ZoomView::HighRes { factor } => zoom_badge(ui.ctx(), factor),
            }
            if show_hint {
                hint(ui.ctx(), state.keys.menu);
            }
            if !input.ui_open {
                draw_destination(ui.ctx(), &input.waypoints, &state.keys);
            }
            draw_notices(ui.ctx(), notices);
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
            waypoint_commands: actions.waypoint_commands,
            ..Default::default()
        };
        if let Some(action) = actions.capture {
            state.capturing = Some(action);
            state.key_note = None;
            state.rebinds.cancel();
            out.start_capture = true;
        } else if (actions.capture_input || capture_again) && state.rebinds.adding() {
            state.capturing = None;
            out.start_input_capture = true;
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
    waypoints: &WaypointView,
) -> MenuActions {
    let mut actions = MenuActions::default();
    let mut open = true;
    // Room for the contents up to the screen's height (egui keeps the window inside the
    // screen), from the screen of this frame: egui only ever lowers a size it remembers, so
    // the menu would not grow back after the window got bigger. The window's own scrolling
    // would fill this room, so the scroll area is ours: it shrinks to the contents and scrolls
    // what does not fit.
    let room = ctx.content_rect().height();
    egui::Window::new("reminedog")
        .id(egui::Id::new("reminedog-main"))
        .open(&mut open)
        .default_pos([40.0, 40.0])
        .resizable(false)
        // Folded, the menu would keep waiting for a key for a hotkey or a rule unseen.
        .collapsible(false)
        .resize(move |r| r.min_height(room))
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("reminedog-menu")
                .show(ui, |ui| {
                    menu_contents(
                        ui,
                        state,
                        info,
                        status,
                        high_res_note,
                        waypoints,
                        &mut actions,
                    );
                });
        });
    actions.close = !open;
    actions
}

fn menu_contents(
    ui: &mut egui::Ui,
    state: &mut UiState,
    info: &FrameInfo,
    status: &[StatusLine],
    high_res_note: Option<&str>,
    waypoints: &WaypointView,
    actions: &mut MenuActions,
) {
    ui.label(format!("{} か Esc で閉じる", state.keys.menu.label()));
    ui.separator();

    ui.label(RichText::new("ズーム").strong());
    ui.label(format!(
        "ゲーム中に {} を押している間、画面の中央を拡大する",
        state.keys.zoom.label()
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
            for action in Action::ALL {
                ui.label(action.name());
                let waiting = state.capturing == Some(action);
                let text = if waiting {
                    "キーを押す…".to_owned()
                } else {
                    state.keys.get(action).label()
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
        ui.label(RichText::new("割り当てるキーかマウスのボタンを押す（Esc で取り消し）").weak());
    } else if let Some(note) = &state.key_note {
        ui.label(RichText::new(note).color(ui.visuals().warn_fg_color));
    }
    if ui.button("キーを元に戻す").clicked() {
        state.capturing = None;
        actions.cancel_capture = true;
        state.reset_hotkeys();
    }
    ui.separator();

    waypoint_section(
        ui,
        &mut state.waypoints,
        waypoints,
        &state.keys,
        &mut actions.waypoint_commands,
    );
    ui.separator();

    let rebinds = rebind_section(
        ui,
        &mut state.rebinds,
        &mut state.settings,
        &state.keys,
        &state.game_bindings,
        state.rebind_note.as_deref(),
        &state.unsupported_inputs,
    );
    actions.capture_input |= rebinds.capture;
    actions.cancel_capture |= rebinds.cancel_capture;
    ui.separator();

    egui::CollapsingHeader::new("状態")
        .id_salt("reminedog-status-section")
        .show(ui, |ui| status_grid(ui, info, status));
    ui.checkbox(
        &mut state.settings.show_status,
        "メニューを閉じても状態を表示する",
    );
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
        assert_eq!(state.keys.zoom, Hotkey::parse("C").unwrap());
        assert_eq!(state.key_note, None);
        state.assign(Action::Waypoint, Hotkey::parse("Mouse4").unwrap());
        assert_eq!(state.settings.waypoint_key, "Mouse4");
        state.assign(Action::Navigate, Hotkey::parse("Shift+N").unwrap());
        assert_eq!(state.settings.navigate_key, "Shift+N");
        assert_eq!(state.keys.navigate, Hotkey::parse("Shift+N").unwrap());
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

    fn refused(state: &mut UiState, action: Action, text: &str) -> Option<String> {
        let before = state.keys;
        state.assign(action, Hotkey::parse(text).unwrap());
        assert_eq!(state.keys, before, "{action:?} on {text}");
        state.key_note.take()
    }

    #[test]
    fn waypoint_hotkeys_must_not_hide_or_be_hidden() {
        let mut state = state();
        // Checked before the zoom: J would take every zoom press, with or without Ctrl.
        let note = refused(&mut state, Action::Zoom, "J");
        assert_eq!(
            note.as_deref(),
            Some("J は「ウェイポイントを記録」と重なるので使えない")
        );
        refused(&mut state, Action::Zoom, "Ctrl+K");
        refused(&mut state, Action::Waypoint, "Z");
        refused(&mut state, Action::Navigate, "Z");
        // The menu comes first (on a key without Ctrl, which these two cannot use).
        state.assign(Action::Menu, Hotkey::parse("Alt+I").unwrap());
        let note = refused(&mut state, Action::Waypoint, "Alt+I");
        assert_eq!(
            note.as_deref(),
            Some("Alt+I は「メニューを開く」と重なるので使えない")
        );
        refused(&mut state, Action::Navigate, "Alt+Shift+I");
        state.assign(Action::Menu, Hotkey::DEFAULT_MENU);
        let note = refused(&mut state, Action::Menu, "K");
        assert_eq!(
            note.as_deref(),
            Some("K は「現在地を更新」と重なるので使えない")
        );
        // The waypoint key comes before the navigate key.
        refused(&mut state, Action::Navigate, "J");
        refused(&mut state, Action::Waypoint, "K");
        refused(&mut state, Action::Navigate, "Alt+J");
        // Needing more modifiers than the earlier key is fine.
        state.assign(Action::Waypoint, Hotkey::parse("Alt+K").unwrap());
        assert_eq!(state.key_note, None);
        assert_eq!(state.settings.waypoint_key, "Alt+K");
        state.assign(Action::Zoom, Hotkey::parse("J").unwrap());
        assert_eq!(state.settings.zoom_key, "J");
        state.assign(Action::Waypoint, Hotkey::parse("Shift+J").unwrap());
        assert_eq!(state.settings.waypoint_key, "Shift+J");
        // Mouse buttons follow the same rule.
        state.assign(Action::Menu, Hotkey::parse("Mouse5").unwrap());
        refused(&mut state, Action::Navigate, "Mouse5");
        state.assign(Action::Navigate, Hotkey::parse("Shift+Mouse5").unwrap());
        assert_eq!(
            state.key_note.as_deref(),
            Some("Shift+マウスのボタン5 は「メニューを開く」と重なるので使えない")
        );
    }

    #[test]
    fn f3c_hotkeys_cannot_need_ctrl() {
        let mut state = state();
        let note = refused(&mut state, Action::Waypoint, "Ctrl+L");
        assert_eq!(
            note.as_deref(),
            Some("Ctrl との組み合わせ（Ctrl+L）は「ウェイポイントを記録」に使えない")
        );
        refused(&mut state, Action::Navigate, "Ctrl+Shift+L");
        // Other hotkeys may.
        state.assign(Action::Zoom, Hotkey::parse("Ctrl+L").unwrap());
        assert_eq!(state.settings.zoom_key, "Ctrl+L");
        // A saved one falls back to the default.
        let settings = Settings {
            waypoint_key: "Ctrl+L".into(),
            ..Settings::default()
        };
        assert_eq!(hotkeys(&settings).waypoint, Hotkey::DEFAULT_WAYPOINT);
    }

    #[test]
    fn the_menu_and_the_router_agree_on_fallen_back_keys() {
        // The zoom on J falls back to Z; moving the waypoint key off J must not bring J back.
        let mut state = UiState::new(Settings {
            zoom_key: "J".into(),
            ..Settings::default()
        });
        assert_eq!(state.settings.zoom_key, "Z");
        state.assign(Action::Waypoint, Hotkey::parse("L").unwrap());
        assert_eq!(state.key_note, None);
        assert_eq!(hotkeys(&state.settings), state.keys);
        assert_eq!(state.keys.zoom, Hotkey::DEFAULT_ZOOM);
    }

    #[test]
    fn f3c_keys_are_refused_for_the_waypoint_hotkeys() {
        let mut state = state();
        state.reserved_keys = vec![egui::Key::F3, egui::Key::C];
        let note = refused(&mut state, Action::Waypoint, "Ctrl+C");
        assert_eq!(note.as_deref(), Some("C は F3+C に使うキーなので使えない"));
        refused(&mut state, Action::Navigate, "F3");
        // The zoom may: the router hands its key to the game while F3 is held.
        state.assign(Action::Zoom, Hotkey::parse("C").unwrap());
        assert_eq!(state.settings.zoom_key, "C");
    }

    #[test]
    fn rule_sources_are_refused_as_hotkeys() {
        let mut state = UiState::new(Settings {
            rebinds: vec![
                reminedog_core::Rebind {
                    from: "key.keyboard.keypad.1".into(),
                    to: "key.keyboard.f3".into(),
                },
                reminedog_core::Rebind {
                    from: "key.mouse.4".into(),
                    to: "key.keyboard.c".into(),
                },
            ],
            ..Settings::default()
        });
        // The hooks give keypad 1 as 1: a hotkey on 1 would take its presses.
        let note = refused(&mut state, Action::Zoom, "1");
        assert_eq!(note.as_deref(), Some("テンキー 1 は置き換えに使っている"));
        let note = refused(&mut state, Action::Waypoint, "Shift+Mouse4");
        assert_eq!(
            note.as_deref(),
            Some("マウスのボタン4 は置き換えに使っている")
        );
        state.assign(Action::Zoom, Hotkey::parse("Mouse5").unwrap());
        assert_eq!(state.settings.zoom_key, "Mouse5");
    }

    /// The menu's window after a few frames on a screen of `size` points.
    fn menu_rect(size: egui::Vec2, state: &mut UiState, waypoints: &WaypointView) -> Rect {
        menu_rect_in(&egui::Context::default(), size, state, waypoints)
    }

    /// [`menu_rect`] in `ctx`, which remembers the earlier frames.
    fn menu_rect_in(
        ctx: &egui::Context,
        size: egui::Vec2,
        state: &mut UiState,
        waypoints: &WaypointView,
    ) -> Rect {
        let info = FrameInfo {
            frames: 1,
            fps: 60.0,
            size: [size.x as u32, size.y as u32],
            ppp: 1.0,
        };
        for _ in 0..4 {
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                ..Default::default()
            };
            let mut output = ctx.run_ui(raw, |ui| {
                main_window(ui.ctx(), state, &info, &[], None, waypoints);
            });
            output.textures_delta.clear();
        }
        ctx.memory(|m| m.area_rect(egui::Id::new("reminedog-main")))
            .expect("the menu was shown")
    }

    fn many_waypoints() -> WaypointView {
        WaypointView {
            world: crate::WorldLabel::Singleplayer("w".into()),
            waypoints: (0..20)
                .map(|id| reminedog_core::Waypoint {
                    id,
                    name: format!("地点 {id}"),
                    dimension: "minecraft:overworld".into(),
                    x: 0.0,
                    y: 64.0,
                    z: 0.0,
                    created_unix: 0,
                })
                .collect(),
            playing: true,
            ..WaypointView::default()
        }
    }

    #[test]
    fn the_menu_scrolls_inside_a_small_window() {
        let waypoints = many_waypoints();
        let mut state = state();
        // Minecraft's default 854×480, at a UI scale of 1.5 and 1.
        for small in [vec2(854.0 / 1.5, 480.0 / 1.5), vec2(854.0, 480.0)] {
            let rect = menu_rect(small, &mut state, &waypoints);
            let screen = Rect::from_min_size(Pos2::ZERO, small);
            assert!(screen.contains_rect(rect), "{rect:?} on {small:?}");
            assert!(rect.height() > small.y - 20.0, "uses the height: {rect:?}");
        }
        // On a large screen the window is as tall as its contents.
        let large = vec2(1920.0, 1080.0);
        let rect = menu_rect(large, &mut state, &waypoints);
        assert!(rect.height() < 900.0, "{rect:?}");
        assert_eq!(rect.min, egui::pos2(40.0, 40.0));
    }

    #[test]
    fn the_menu_grows_back_when_the_screen_gets_bigger() {
        let waypoints = many_waypoints();
        let mut state = state();
        let large = vec2(1920.0, 1080.0);
        let fresh = menu_rect(large, &mut state, &waypoints);
        // Opened in a small window, which is then maximized (and back).
        let ctx = egui::Context::default();
        let small = vec2(854.0, 480.0);
        let rect = menu_rect_in(&ctx, small, &mut state, &waypoints);
        assert!(rect.height() <= small.y, "{rect:?}");
        let rect = menu_rect_in(&ctx, large, &mut state, &waypoints);
        assert!(rect.height() > small.y, "{rect:?}");
        assert_eq!(rect.height(), fresh.height(), "as tall as its contents");
        let rect = menu_rect_in(&ctx, small, &mut state, &waypoints);
        assert!(rect.height() <= small.y, "{rect:?}");
    }

    #[test]
    fn the_menu_cannot_be_folded() {
        let ctx = egui::Context::default();
        let mut state = state();
        let rect = menu_rect_in(&ctx, vec2(1920.0, 1080.0), &mut state, &many_waypoints());
        // A double click on the title bar folds a collapsible window.
        let title = rect.min + vec2(rect.width() / 2.0, 12.0);
        let click = |pressed| egui::Event::PointerButton {
            pos: title,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        let info = FrameInfo {
            frames: 1,
            fps: 60.0,
            size: [1920, 1080],
            ppp: 1.0,
        };
        for events in [
            vec![egui::Event::PointerMoved(title), click(true), click(false)],
            vec![click(true), click(false)],
            vec![],
            vec![],
        ] {
            let raw = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1920.0, 1080.0))),
                events,
                ..Default::default()
            };
            let mut output = ctx.run_ui(raw, |ui| {
                main_window(ui.ctx(), &mut state, &info, &[], None, &many_waypoints());
            });
            output.textures_delta.clear();
        }
        let after = ctx
            .memory(|m| m.area_rect(egui::Id::new("reminedog-main")))
            .expect("shown");
        assert_eq!(after.height(), rect.height());
    }

    fn rule(from: &str, to: &str) -> reminedog_core::Rebind {
        reminedog_core::Rebind {
            from: from.into(),
            to: to.into(),
        }
    }

    #[test]
    fn resetting_the_hotkeys_keeps_those_on_rule_sources() {
        let mut state = state();
        state.assign(Action::Zoom, Hotkey::parse("Mouse5").unwrap());
        state.assign(Action::Menu, Hotkey::parse("F6").unwrap());
        // Z is free now: a rule may use it.
        state
            .settings
            .rebinds
            .push(rule("key.keyboard.z", "key.keyboard.left.shift"));
        state.reset_hotkeys();
        assert_eq!(state.settings.zoom_key, "Mouse5");
        assert_eq!(state.settings.menu_key, "Ctrl+I");
        assert_eq!(state.key_note.as_deref(), Some("Z は置き換えに使っている"));
        assert_eq!(hotkeys(&state.settings), state.keys);
        // Nothing in the way: all back, no note.
        state.settings.rebinds.clear();
        state.reset_hotkeys();
        assert_eq!(state.keys, Hotkeys::DEFAULT);
        assert_eq!(state.key_note, None);
        // A rule on the default's key that changes nothing (the menu is on Ctrl+I already):
        // no note either.
        state
            .settings
            .rebinds
            .push(rule("key.keyboard.i", "key.keyboard.o"));
        state.reset_hotkeys();
        assert_eq!(state.keys, Hotkeys::DEFAULT);
        assert_eq!(state.key_note, None);
    }

    #[test]
    fn resetting_the_hotkeys_keeps_the_ones_a_kept_key_would_hide() {
        let mut state = state();
        state.assign(Action::Waypoint, Hotkey::parse("M").unwrap());
        state.assign(Action::Zoom, Hotkey::parse("J").unwrap());
        state
            .settings
            .rebinds
            .push(rule("key.keyboard.z", "key.keyboard.c"));
        state.reset_hotkeys();
        // The zoom stays on J, so the waypoint key cannot go back to J.
        assert_eq!(state.settings.zoom_key, "J");
        assert_eq!(state.settings.waypoint_key, "M");
        assert_eq!(state.settings.navigate_key, "K");
        assert_eq!(state.key_note.as_deref(), Some("Z は置き換えに使っている"));
        assert_eq!(hotkeys(&state.settings), state.keys);
    }

    #[test]
    fn unreadable_hotkeys_fall_back_to_the_defaults() {
        assert_eq!(hotkeys(&Settings::default()), Hotkeys::DEFAULT);
        let settings = Settings {
            menu_key: "Nope".into(),
            zoom_key: "Mouse4".into(),
            waypoint_key: "".into(),
            navigate_key: "Shift+L".into(),
            ..Settings::default()
        };
        let keys = hotkeys(&settings);
        assert_eq!(keys.menu, Hotkey::DEFAULT_MENU);
        assert_eq!(keys.zoom.to_string(), "Mouse4");
        assert_eq!(keys.waypoint, Hotkey::DEFAULT_WAYPOINT);
        assert_eq!(keys.navigate.to_string(), "Shift+L");
    }

    #[test]
    fn saved_hotkeys_that_would_be_hidden_fall_back_to_the_defaults() {
        // An older settings file with the zoom on J, now the waypoint key's default.
        let settings = Settings {
            zoom_key: "J".into(),
            ..Settings::default()
        };
        assert_eq!(hotkeys(&settings).zoom, Hotkey::DEFAULT_ZOOM);
        let settings = Settings {
            menu_key: "Alt+I".into(),
            waypoint_key: "Alt+I".into(),
            navigate_key: "J".into(),
            ..Settings::default()
        };
        let keys = hotkeys(&settings);
        // The waypoint key went back to J, which then hides the navigate key.
        assert_eq!(keys.waypoint, Hotkey::DEFAULT_WAYPOINT);
        assert_eq!(keys.navigate, Hotkey::DEFAULT_NAVIGATE);
        // A menu on J (allowed before there was a waypoint key) hides the default too: the
        // default is kept (and logged) and the menu shows both on J.
        let settings = Settings {
            menu_key: "J".into(),
            ..Settings::default()
        };
        let keys = hotkeys(&settings);
        assert_eq!(keys.menu.to_string(), "J");
        assert_eq!(keys.waypoint, Hotkey::DEFAULT_WAYPOINT);
        assert_eq!(keys.zoom, Hotkey::DEFAULT_ZOOM);
    }
}
