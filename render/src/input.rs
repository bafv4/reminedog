//! Routing of the game's input while the overlay is in use: the hotkeys, whether each event
//! goes to the overlay (egui) or on to the game, and the key rebinding.
//!
//! Platform-neutral: the GLFW and SDL3 hooks translate their events into these calls and
//! drop, forward or replace each one as told. While the UI is open everything goes to egui
//! except releases (keys, mouse buttons), which the game still gets so nothing stays held
//! down. The hotkeys and the UI go by the physical key; the rebinding then decides what the
//! game gets for the presses they leave to it (see [`crate::rebind`]).

use crate::hotkey::{self, Hotkey, Trigger};
use crate::overlay::{Action, Hotkeys};
use crate::pointer::PointerSpeed;
use crate::rebind::{Delivery, Output, Phase, RebindState, Rebinder};
use egui::{Event, Key, Modifiers, MouseWheelUnit, PointerButton, Pos2, TouchPhase, pos2, vec2};
use reminedog_core::{InputId, modifier_kind};

/// What to do with an input event other than a key or mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Pass it on to the game.
    Forward,
    /// The overlay used it; the game must not see it.
    Consume,
}

/// A hotkey that asks the platform hook to do something, see [`InputRouter::take_actions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyAction {
    RecordWaypoint,
    Navigate,
}

/// A browser hotkey, see [`InputRouter::take_browser_actions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserAction {
    Toggle,
    PageUp,
    PageDown,
    PlayPause,
    SeekBack,
    SeekForward,
}

impl BrowserAction {
    /// Held down, the key acts again on each of its repeats.
    fn repeats(self) -> bool {
        matches!(
            self,
            BrowserAction::PageUp
                | BrowserAction::PageDown
                | BrowserAction::SeekBack
                | BrowserAction::SeekForward
        )
    }
}

/// Actions kept until the next frame takes them.
const MAX_ACTIONS: usize = 4;
/// The most characters a paste into the UI takes.
const MAX_PASTE: usize = 4096;
/// Browser actions kept until the next frame takes them (a held key repeats).
const MAX_BROWSER_ACTIONS: usize = 8;

/// The hotkeys acted on in game: the waypoint and navigate keys, then the browser's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InGame {
    Hotkey(HotkeyAction),
    Browser(BrowserAction),
}

impl InGame {
    fn action(self) -> Action {
        match self {
            InGame::Hotkey(HotkeyAction::RecordWaypoint) => Action::Waypoint,
            InGame::Hotkey(HotkeyAction::Navigate) => Action::Navigate,
            InGame::Browser(action) => Action::Browser(action),
        }
    }
}

/// In the order of [`Action::ROUTER_ORDER`], between the menu and the zoom.
const IN_GAME: [InGame; 8] = [
    InGame::Hotkey(HotkeyAction::RecordWaypoint),
    InGame::Hotkey(HotkeyAction::Navigate),
    InGame::Browser(BrowserAction::Toggle),
    InGame::Browser(BrowserAction::PageUp),
    InGame::Browser(BrowserAction::PageDown),
    InGame::Browser(BrowserAction::PlayPause),
    InGame::Browser(BrowserAction::SeekBack),
    InGame::Browser(BrowserAction::SeekForward),
];

/// The keys of [`IN_GAME`], in its order.
fn in_game_keys(keys: &Hotkeys) -> [Option<Hotkey>; IN_GAME.len()] {
    IN_GAME.map(|action| keys.get(action.action()))
}

/// The outcome of [`InputRouter::start_capture`] and [`InputRouter::start_input_capture`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Captured {
    Hotkey(Hotkey),
    /// A key or mouse button as the game knows it, for the rebinding.
    Input(InputId),
    /// Esc was pressed, or the UI closed.
    Cancelled,
}

/// What a capture waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Capture {
    /// A key with egui's name, or an extra mouse button, with the modifiers held.
    Hotkey,
    /// Any key the game knows (modifier keys too), or the middle or a side mouse button.
    Input,
}

/// Minecraft's default debug modifier, F3.
const DEFAULT_DEBUG_MODIFIER: InputId = InputId::Key(60);
/// The Esc key, which cancels a capture.
const ESCAPE: InputId = InputId::Key(41);

pub struct InputRouter {
    /// Off until the overlay can draw; while off every event is forwarded, so a UI that
    /// could not be shown never swallows input.
    enabled: bool,
    ui_open: bool,
    /// UI opened or closed since the last `take_ui_changed`.
    ui_changed: bool,
    zoom_held: bool,
    /// The key or button whose press started the zoom.
    zoom_raw: Option<InputId>,
    modifiers: Modifiers,
    /// Overlay pointer in window pixels.
    pointer: Pos2,
    screen_px: [f32; 2],
    pixels_per_point: f32,
    events: Vec<Event>,
    /// Last absolute cursor position the platform reported (GLFW), and whether the cursor
    /// was captured then.
    last_cursor: Option<(f64, f64)>,
    last_captured: bool,
    /// Motion swallowed while the UI was open over a captured cursor. GLFW reports captured
    /// cursors as ever-growing virtual positions and the game turns the camera by the
    /// difference to the last position it saw, so positions forwarded afterwards have this
    /// subtracted; otherwise the view would jump when the UI closes.
    offset: (f64, f64),
    /// Applied to relative motion that moves the overlay's own cursor.
    pointer_speed: PointerSpeed,
    /// `menu` toggles the UI; `zoom` zooms while held, and the others ([`IN_GAME`]) queue an
    /// action; these in game (cursor captured) with the UI closed.
    keys: Hotkeys,
    /// The key of [`IN_GAME`] at the same index was pressed and consumed, so its release is
    /// ours too.
    action_held: [bool; IN_GAME.len()],
    /// The key or button of each held [`IN_GAME`] press.
    action_raw: [Option<InputId>; IN_GAME.len()],
    actions: Vec<HotkeyAction>,
    browser_actions: Vec<BrowserAction>,
    /// The browser shows: its keys other than the toggle are taken.
    browser_shown: bool,
    /// Minecraft's debug modifier (F3 unless rebound in options.txt). While the game has it
    /// down the in-game hotkeys go to the game, so its F3+<key> combinations keep working.
    debug_modifier: Option<InputId>,
    /// Follows what the game gets, not the physical key: a press rebound to the modifier
    /// holds it, and the modifier rebound to another key does not.
    debug_held: bool,
    /// The next key or mouse button pressed in the UI is captured.
    capturing: Option<Capture>,
    /// A modifier key went down during an input capture: taken when it goes up alone, but
    /// a combination with it (Ctrl+I, which closes the menu) is no capture of it.
    pending_modifier: Option<InputId>,
    captured: Option<Captured>,
    /// The keys or buttons whose press a capture took: their repeats and release are not the
    /// game's either (F3's release alone toggles the debug screen behind the menu; a repeat
    /// without its release would leave the key held in the game).
    capture_held: Vec<InputId>,
    rebinder: Rebinder,
    /// A key the game got as another key, or a hotkey, was pressed or repeated: its
    /// characters are not the game's either (X → T would type an x into the chat T opened).
    /// Until the next text or key event.
    suppress_text: bool,
    /// A widget of the UI has the keyboard (a text field, the browser's page): a menu key
    /// that types text is typed there instead.
    text_focus: bool,
    /// Ctrl+V (or Shift+Insert) went to the UI: the platform hands over the clipboard's
    /// text ([`paste`](Self::paste)).
    paste_requested: bool,
}

impl Default for InputRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl InputRouter {
    pub const fn new() -> Self {
        Self {
            enabled: false,
            ui_open: false,
            ui_changed: false,
            zoom_held: false,
            zoom_raw: None,
            modifiers: Modifiers::NONE,
            pointer: Pos2::ZERO,
            screen_px: [0.0, 0.0],
            pixels_per_point: 1.0,
            events: Vec::new(),
            last_cursor: None,
            last_captured: false,
            offset: (0.0, 0.0),
            pointer_speed: PointerSpeed::RAW,
            keys: Hotkeys::DEFAULT,
            action_held: [false; IN_GAME.len()],
            action_raw: [None; IN_GAME.len()],
            actions: Vec::new(),
            browser_actions: Vec::new(),
            browser_shown: false,
            debug_modifier: Some(DEFAULT_DEBUG_MODIFIER),
            debug_held: false,
            capturing: None,
            pending_modifier: None,
            captured: None,
            capture_held: Vec::new(),
            rebinder: Rebinder::new(),
            suppress_text: false,
            text_focus: false,
            paste_requested: false,
        }
    }

    /// Turns routing on once the overlay renders, and off (closing the UI) if it stops.
    pub fn set_enabled(&mut self, enabled: bool) {
        if !enabled {
            self.set_ui_open(false);
            self.zoom_held = false;
            self.zoom_raw = None;
            self.action_held = [false; IN_GAME.len()];
            self.action_raw = [None; IN_GAME.len()];
            self.debug_held = false;
            self.suppress_text = false;
            self.capture_held.clear();
            self.events.clear();
            self.actions.clear();
            self.browser_actions.clear();
        }
        self.enabled = enabled;
    }

    pub fn ui_open(&self) -> bool {
        self.ui_open
    }

    pub fn modifiers(&self) -> Modifiers {
        self.modifiers
    }

    pub fn zoom_active(&self) -> bool {
        self.zoom_held && !self.ui_open
    }

    /// `Some(open)` once after the UI was opened or closed, for platform-side follow-up
    /// (SDL3 needs text input switched on for the UI's text fields).
    pub fn take_ui_changed(&mut self) -> Option<bool> {
        std::mem::take(&mut self.ui_changed).then_some(self.ui_open)
    }

    /// Size of the window in pixels and the UI scale, updated every frame.
    pub fn set_screen(&mut self, size_px: [u32; 2], pixels_per_point: f32) {
        self.screen_px = [size_px[0] as f32, size_px[1] as f32];
        self.pixels_per_point = crate::overlay::clamp_pixels_per_point(pixels_per_point);
    }

    /// Whether a widget of the UI has the keyboard (from the last frame).
    pub fn set_text_focus(&mut self, focus: bool) {
        self.text_focus = focus;
    }

    /// Whether the UI asked for the clipboard's text since the last call.
    pub fn take_paste_request(&mut self) -> bool {
        std::mem::take(&mut self.paste_requested)
    }

    /// The clipboard's text for a paste the UI asked for: one line, at most
    /// [`MAX_PASTE`] characters (the UI's text fields are single lines).
    pub fn paste(&mut self, text: &str) {
        if !self.ui_open {
            return;
        }
        let text: String = text
            .chars()
            .map(|c| {
                if matches!(c, '\r' | '\n' | '\t') {
                    ' '
                } else {
                    c
                }
            })
            .filter(|c| !c.is_control())
            .take(MAX_PASTE)
            .collect();
        let text = text.trim();
        if !text.is_empty() {
            self.push_event(Event::Paste(text.to_owned()));
        }
    }

    /// Where to draw the overlay's own cursor (in points), when the game hides the real
    /// one: the UI is open while the cursor is captured.
    pub fn software_cursor(&self) -> Option<Pos2> {
        (self.ui_open && self.last_captured).then(|| self.pointer_points())
    }

    /// How relative motion moves the overlay's own cursor: the system's pointer speed when
    /// the game reads raw mouse counts, [`PointerSpeed::RAW`] when the counts already went
    /// through the system's pointer ballistics.
    pub fn set_pointer_speed(&mut self, speed: PointerSpeed) {
        self.pointer_speed = speed;
    }

    pub fn set_hotkeys(&mut self, keys: Hotkeys) {
        if keys.zoom != self.keys.zoom {
            self.drop_zoom();
        }
        let (old, new) = (in_game_keys(&self.keys), in_game_keys(&keys));
        for (i, (old, new)) in old.into_iter().zip(new).enumerate() {
            if old != new {
                self.drop_action(i);
            }
        }
        self.keys = keys;
    }

    /// Forgets the zoom's press; its release (and repeats) stay out of the game, which
    /// never got the press.
    fn drop_zoom(&mut self) {
        if std::mem::take(&mut self.zoom_held) {
            let raw = self.zoom_raw.take();
            self.hold_captured(raw);
        }
    }

    /// As [`drop_zoom`](Self::drop_zoom), for the [`IN_GAME`] press at `i`.
    fn drop_action(&mut self, i: usize) {
        if std::mem::take(&mut self.action_held[i]) {
            let raw = self.action_raw[i].take();
            self.hold_captured(raw);
        }
    }

    /// The keys and buttons whose press the router holds (the zoom's, the in-game hotkeys',
    /// the debug modifier the game has down), for the platform's check of releases that
    /// never come ([`forget_press`](Self::forget_press)).
    pub fn held_presses(&self) -> Vec<InputId> {
        let mut held: Vec<InputId> = self
            .zoom_raw
            .filter(|_| self.zoom_held)
            .into_iter()
            .collect();
        for (i, raw) in self.action_raw.iter().enumerate() {
            if self.action_held[i]
                && let Some(id) = *raw
                && !held.contains(&id)
            {
                held.push(id);
            }
        }
        if self.debug_held
            && let Some(id) = self.debug_modifier
            && !held.contains(&id)
        {
            held.push(id);
        }
        held
    }

    /// The key or button `id` was found to be up although its release never came (26.x drops
    /// input events while loading a world): forgets the router's presses of it, as the
    /// release would have.
    pub fn forget_press(&mut self, id: InputId) {
        if self.zoom_held && self.zoom_raw == Some(id) {
            self.zoom_held = false;
            self.zoom_raw = None;
        }
        for i in 0..IN_GAME.len() {
            if self.action_held[i] && self.action_raw[i] == Some(id) {
                self.action_held[i] = false;
                self.action_raw[i] = None;
            }
        }
        if self.debug_held && self.debug_modifier == Some(id) && !self.rebinder.rule_holds(id) {
            self.debug_held = false;
        }
    }

    /// Minecraft's debug modifier key or button; `None` when it is unbound.
    /// Whether the game has the debug modifier down, as far as the router passed it on.
    pub fn debug_held(&self) -> bool {
        self.debug_held
    }

    pub fn set_debug_modifier(&mut self, id: Option<InputId>) {
        if id != self.debug_modifier {
            self.debug_modifier = id;
            self.debug_held = false;
        }
    }

    /// The rebinding's rules for presses from now on (from [`crate::resolve`]); presses held
    /// now keep what they did until their release. Without `keyboard_sources` the rules from
    /// keys are left out (the platform could not hide a held key from the game).
    pub fn set_rebinds(&mut self, rules: Vec<(InputId, InputId)>, keyboard_sources: bool) {
        let rules = rules
            .into_iter()
            .filter(|(source, _)| keyboard_sources || !matches!(source, InputId::Key(_)))
            .collect();
        self.rebinder.set_rules(rules);
    }

    /// The rules in use: (source, output).
    pub fn rebinds(&self) -> &[(InputId, InputId)] {
        self.rebinder.rules()
    }

    /// What the rebinding holds down in the game and hides from it right now.
    pub fn rebind_state(&self) -> RebindState {
        self.rebinder.state()
    }

    /// A source was found to be up although its release never came (the platform's check of
    /// the real key state): forgets its press as the release would, and returns the event to
    /// give the game, if any.
    #[must_use = "the release must reach the game"]
    pub fn release_source(&mut self, id: InputId) -> Option<Output> {
        let output = self.rebinder.release_source(id);
        if let Some(output) = output {
            self.delivered(None, Phase::Release, Delivery::Send(output));
        }
        output
    }

    /// A key or button of [`RebindState::passthrough`] was found to be up although its release
    /// never came: forgets its press without a release for the game (which let go of it by
    /// itself). Returns whether it had such a press.
    pub fn forget_passthrough(&mut self, id: InputId) -> bool {
        let forgotten = self.rebinder.forget_passthrough(id);
        if forgotten && self.debug_modifier == Some(id) && !self.rebinder.rule_holds(id) {
            self.debug_held = false;
        }
        forgotten
    }

    /// Whether the rebinding applies to a press now: in game with the UI closed.
    fn rebind_active(&self, captured: bool) -> bool {
        self.enabled && captured && !self.ui_open && self.capturing.is_none()
    }

    /// The waypoint and navigate hotkeys pressed since the last call, oldest first.
    pub fn take_actions(&mut self) -> Vec<HotkeyAction> {
        std::mem::take(&mut self.actions)
    }

    /// The browser's hotkeys pressed (or repeated) since the last call, oldest first.
    pub fn take_browser_actions(&mut self) -> Vec<BrowserAction> {
        std::mem::take(&mut self.browser_actions)
    }

    /// Whether the browser shows: only then do its keys (other than the toggle) work. Set
    /// every frame.
    pub fn set_browser_shown(&mut self, shown: bool) {
        self.browser_shown = shown;
    }

    /// Takes the next key (with its modifiers) or extra mouse button pressed while the UI
    /// is open, instead of handing it to egui or acting on hotkeys; see
    /// [`take_captured`](Self::take_captured).
    pub fn start_capture(&mut self) {
        self.begin_capture(Capture::Hotkey);
    }

    /// Takes the next key the game knows (modifier keys too) or middle or side mouse button
    /// pressed while the UI is open, as [`Captured::Input`]; Esc cancels. Clicks still work
    /// the UI. See [`take_captured`](Self::take_captured).
    pub fn start_input_capture(&mut self) {
        self.begin_capture(Capture::Input);
    }

    fn begin_capture(&mut self, capture: Capture) {
        if self.ui_open {
            self.capturing = Some(capture);
            self.pending_modifier = None;
            self.captured = None;
        }
    }

    pub fn cancel_capture(&mut self) {
        self.pending_modifier = None;
        if self.capturing.take().is_some() {
            self.captured = Some(Captured::Cancelled);
        }
    }

    pub fn take_captured(&mut self) -> Option<Captured> {
        self.captured.take()
    }

    fn finish_capture(&mut self, result: Captured) {
        self.capturing = None;
        self.pending_modifier = None;
        self.captured = Some(result);
    }

    /// Whether the event is the release of the key or button a capture took, which the game
    /// must not get either. A new press of it means that release went missing: forgotten.
    fn capture_release(&mut self, raw: Option<InputId>, phase: Phase) -> bool {
        let Some(i) = raw.and_then(|id| self.capture_held.iter().position(|&held| held == id))
        else {
            return false;
        };
        match phase {
            Phase::Repeat => true,
            Phase::Release => {
                self.capture_held.remove(i);
                true
            }
            // Pressed again: its release never came.
            Phase::Press => {
                self.capture_held.remove(i);
                false
            }
        }
    }

    /// Remembers a captured press, so its repeats and release stay out of the game.
    fn hold_captured(&mut self, raw: Option<InputId>) {
        const MAX: usize = 4;
        if let Some(id) = raw
            && !self.capture_held.contains(&id)
        {
            if self.capture_held.len() == MAX {
                self.capture_held.remove(0);
            }
            self.capture_held.push(id);
        }
    }

    /// egui events collected since the last call.
    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    pub fn set_ui_open(&mut self, open: bool) {
        if open == self.ui_open {
            return;
        }
        self.ui_open = open;
        self.ui_changed = true;
        log::info!("ui {}", if open { "opened" } else { "closed" });
        self.drop_zoom();
        for i in 0..IN_GAME.len() {
            self.drop_action(i);
        }
        self.cancel_capture();
        if !open && matches!(self.captured, Some(Captured::Input(_))) {
            // Taken in the same frame the menu closed (a key, then Esc): the menu that waited
            // for it is gone.
            self.captured = Some(Captured::Cancelled);
        }
        self.paste_requested = false;
        if open {
            if self.last_captured || self.pointer == Pos2::ZERO {
                self.pointer = pos2(self.screen_px[0] / 2.0, self.screen_px[1] / 2.0);
            }
            self.push_event(Event::PointerMoved(self.pointer_points()));
        } else {
            self.push_event(Event::PointerGone);
        }
    }

    /// A key press, repeat or release.
    ///
    /// `key` is what the hotkeys and the UI match (`None` for keys egui has no name for), `raw`
    /// the physical key as the game knows it (`None` for keys it does not), which the
    /// rebinding and the debug modifier go by. `captured`: the game has grabbed the cursor,
    /// i.e. it is being played rather than showing a menu. `stateless`: the platform keeps no
    /// state for this key, sending its auto-repeat as presses and no release on focus loss
    /// (GLFW's key -1).
    #[allow(clippy::too_many_arguments)]
    pub fn key(
        &mut self,
        key: Option<Key>,
        raw: Option<InputId>,
        pressed: bool,
        repeat: bool,
        modifiers: Modifiers,
        captured: bool,
        stateless: bool,
    ) -> Delivery {
        // The menu key may come before any motion since the game grabbed or released the
        // cursor (a chat closed): the menu must know which cursor to show.
        self.observe_capture(captured);
        self.modifiers = modifiers;
        self.suppress_text = false;
        let phase = phase(pressed, repeat);
        let delivery = self
            .rebound(raw, phase, captured, stateless)
            .unwrap_or_else(|| {
                let route = self.key_route(key, raw, pressed, repeat, modifiers, captured);
                self.after_route(route, raw, phase, captured)
            });
        self.delivered(raw, phase, delivery);
        if phase != Phase::Release
            && let Some(id @ InputId::Key(_)) = raw
            && self.rebinder.takes(id)
        {
            self.suppress_text = true;
        }
        delivery
    }

    /// The rebinding's first say: on a source it latched before, it decides alone (repeats
    /// and releases of a rebound press must follow that press, whatever the UI or the
    /// hotkeys do now).
    fn rebound(
        &mut self,
        raw: Option<InputId>,
        phase: Phase,
        captured: bool,
        stateless: bool,
    ) -> Option<Delivery> {
        let active = self.rebind_active(captured);
        self.rebinder.latched(raw?, phase, active, stateless)
    }

    /// After the router's own decision: a forwarded press goes to the rebinding, which may
    /// rebind it, and a release ends what its press did.
    fn after_route(
        &mut self,
        route: Route,
        raw: Option<InputId>,
        phase: Phase,
        captured: bool,
    ) -> Delivery {
        let consumed = route == Route::Consume;
        let Some(id) = raw else {
            return if consumed {
                Delivery::Consume
            } else {
                Delivery::Forward
            };
        };
        match phase {
            Phase::Press if !consumed => {
                let active = self.rebind_active(captured);
                self.rebinder.press(id, active)
            }
            Phase::Release => {
                let delivery = self.rebinder.release(id);
                if consumed {
                    Delivery::Consume
                } else {
                    delivery
                }
            }
            Phase::Press | Phase::Repeat if consumed => Delivery::Consume,
            Phase::Press | Phase::Repeat => Delivery::Forward,
        }
    }

    /// Keeps `debug_held` with what the game gets.
    fn delivered(&mut self, raw: Option<InputId>, phase: Phase, delivery: Delivery) {
        let delivered = match delivery {
            Delivery::Forward => raw.map(|id| Output { id, phase }),
            Delivery::Send(output) => Some(output),
            Delivery::Consume => None,
        };
        if let Some(output) = delivered
            && self.debug_modifier == Some(output.id)
        {
            self.debug_held = output.phase != Phase::Release;
        }
    }

    /// The router's own handling of a key, by the physical key: capture, the menu key, the
    /// UI, the in-game hotkeys.
    fn key_route(
        &mut self,
        key: Option<Key>,
        raw: Option<InputId>,
        pressed: bool,
        repeat: bool,
        modifiers: Modifiers,
        captured: bool,
    ) -> Route {
        if !self.enabled {
            return Route::Forward;
        }
        if self.capture_release(raw, phase(pressed, repeat)) {
            return Route::Consume;
        }
        let menu_key = pressed
            && self.keys.menu.matches_key(key, modifiers)
            && !self.menu_key_types_here(key, captured);
        if self.ui_open
            && let Some(capture) = self.capturing
        {
            if pressed && !repeat {
                let escape = key == Some(Key::Escape) || raw == Some(ESCAPE);
                let combined = self.pending_modifier.is_some() && menu_key;
                match (capture, key, raw) {
                    _ if escape => self.finish_capture(Captured::Cancelled),
                    // The menu key with the modifier held: it closes the menu.
                    _ if combined => {
                        self.hold_captured(raw);
                        self.set_ui_open(false);
                    }
                    (Capture::Hotkey, Some(k), _) if hotkey::assignable(Trigger::Key(k)) => {
                        self.finish_capture(Captured::Hotkey(Hotkey::with_modifiers(
                            Trigger::Key(k),
                            modifiers,
                        )));
                        self.hold_captured(raw);
                    }
                    (Capture::Input, _, Some(id)) if modifier_kind(id).is_some() => {
                        self.pending_modifier = Some(id);
                    }
                    (Capture::Input, _, Some(id)) => {
                        self.finish_capture(Captured::Input(id));
                        self.hold_captured(raw);
                    }
                    // Modifier keys alone (for a hotkey), and keys without a name: keep
                    // waiting.
                    _ => {}
                }
            } else if !pressed
                && let Some(id) = raw
                && self.pending_modifier == Some(id)
            {
                // The modifier went up alone: it is the key.
                self.finish_capture(Captured::Input(id));
                return Route::Consume;
            }
            return if pressed {
                Route::Consume
            } else {
                Route::Forward
            };
        }
        if menu_key {
            if !repeat {
                self.set_ui_open(!self.ui_open);
                // Its release is not the game's either (F3 as the menu key would toggle the
                // debug screen).
                self.hold_captured(raw);
            }
            return Route::Consume;
        }
        if self.ui_open {
            if key == Some(Key::Escape) && pressed {
                if !repeat {
                    self.set_ui_open(false);
                    self.hold_captured(raw);
                }
                return Route::Consume;
            }
            if let Some(key) = key {
                self.push_event(Event::Key {
                    key,
                    physical_key: None,
                    pressed,
                    repeat,
                    modifiers,
                });
                if pressed {
                    self.clipboard_key(key, modifiers);
                }
            }
            return if pressed {
                Route::Consume
            } else {
                Route::Forward
            };
        }
        let Some(key) = key else {
            return Route::Forward;
        };
        self.game_hotkey(Trigger::Key(key), raw, pressed, repeat, modifiers, captured)
    }

    /// A menu key without Ctrl or Alt that types text (M, Space) types it where text goes:
    /// in the UI's text field or page that has the keyboard, and in the game's screens (chat,
    /// signs) rather than opening the menu there.
    fn menu_key_types_here(&self, key: Option<Key>, captured: bool) -> bool {
        let menu = self.keys.menu;
        !menu.ctrl
            && !menu.alt
            && key.is_some_and(hotkey::types_text)
            && if self.ui_open {
                self.text_focus
            } else {
                !captured
            }
    }

    /// The UI's clipboard keys: copy, cut and paste for its text fields (the browser's page
    /// gets the keys themselves).
    fn clipboard_key(&mut self, key: Key, modifiers: Modifiers) {
        let command = modifiers.ctrl || modifiers.command;
        match key {
            Key::V if command && !modifiers.alt => self.paste_requested = true,
            Key::Insert if modifiers.shift => self.paste_requested = true,
            Key::C if command && !modifiers.alt => self.push_event(Event::Copy),
            Key::Insert if command => self.push_event(Event::Copy),
            Key::X if command && !modifiers.alt => self.push_event(Event::Cut),
            _ => {}
        }
    }

    /// The in-game hotkeys (UI closed): the waypoint and navigate keys, the browser's (other
    /// than its toggle only while it shows), then the zoom, whose match ignores the modifiers.
    /// While the debug modifier is held new presses go to the game; the releases of keys taken
    /// before are still ours.
    fn game_hotkey(
        &mut self,
        trigger: Trigger,
        raw: Option<InputId>,
        pressed: bool,
        repeat: bool,
        modifiers: Modifiers,
        captured: bool,
    ) -> Route {
        let keys = in_game_keys(&self.keys);
        for (i, (action, hotkey)) in IN_GAME.into_iter().zip(keys).enumerate() {
            let Some(hotkey) = hotkey.filter(|hotkey| hotkey.trigger == trigger) else {
                continue;
            };
            if !pressed {
                if std::mem::take(&mut self.action_held[i]) {
                    self.action_raw[i] = None;
                    return Route::Consume;
                }
            } else if repeat {
                if self.action_held[i] {
                    if let InGame::Browser(action) = action
                        && action.repeats()
                    {
                        self.queue(InGame::Browser(action));
                    }
                    self.suppress_own_text(trigger);
                    return Route::Consume;
                }
            } else {
                let active = match action {
                    InGame::Browser(action) => {
                        action == BrowserAction::Toggle || self.browser_shown
                    }
                    InGame::Hotkey(_) => true,
                };
                if active && captured && !self.debug_held && hotkey.modifiers_held(modifiers) {
                    self.action_held[i] = true;
                    self.action_raw[i] = raw;
                    self.queue(action);
                    self.suppress_own_text(trigger);
                    return Route::Consume;
                }
            }
        }
        // A zoom on the debug modifier itself is not a combination with it.
        let is_modifier = raw.is_some() && raw == self.debug_modifier;
        if self.keys.zoom.trigger == trigger && (self.zoom_held || !self.debug_held || is_modifier)
        {
            let route = self.zoom_input(raw, pressed, repeat, modifiers, captured);
            if route == Route::Consume && pressed {
                self.suppress_own_text(trigger);
            }
            return route;
        }
        Route::Forward
    }

    /// The characters a hotkey's own press or repeat types are not the game's (the next text
    /// event only: the hotkey held while typing in a chat opened meanwhile must not swallow
    /// the chat's characters).
    fn suppress_own_text(&mut self, trigger: Trigger) {
        if matches!(trigger, Trigger::Key(_)) {
            self.suppress_text = true;
        }
    }

    fn queue(&mut self, action: InGame) {
        let (len, max) = match action {
            InGame::Hotkey(_) => (self.actions.len(), MAX_ACTIONS),
            InGame::Browser(_) => (self.browser_actions.len(), MAX_BROWSER_ACTIONS),
        };
        if len >= max {
            log::debug!("hotkey {action:?} dropped: {max} actions pending");
            return;
        }
        match action {
            InGame::Hotkey(action) => self.actions.push(action),
            InGame::Browser(action) => self.browser_actions.push(action),
        }
    }

    /// The zoom hotkey went down or up (UI closed). Only a fresh press starts it: a repeat of
    /// a press the game got (the modifier was held then) must leave the release to the game.
    fn zoom_input(
        &mut self,
        raw: Option<InputId>,
        pressed: bool,
        repeat: bool,
        modifiers: Modifiers,
        captured: bool,
    ) -> Route {
        if pressed {
            let starts = !repeat && captured && self.keys.zoom.modifiers_held(modifiers);
            if self.zoom_held || starts {
                if !self.zoom_held {
                    self.zoom_raw = raw;
                }
                self.zoom_held = true;
                return Route::Consume;
            }
        } else if self.zoom_held {
            self.zoom_held = false;
            self.zoom_raw = None;
            return Route::Consume;
        }
        Route::Forward
    }

    /// Typed text (after the keyboard layout and IME).
    pub fn text(&mut self, text: &str) -> Route {
        if std::mem::take(&mut self.suppress_text) {
            // The characters of a key the game got as another key, or of a hotkey.
            return Route::Consume;
        }
        if !self.ui_open || !self.enabled {
            return Route::Forward;
        }
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        if !text.is_empty() {
            self.push_event(Event::Text(text));
        }
        Route::Consume
    }

    /// A mouse button press or release. `raw` and `captured` as for [`key`](Self::key).
    pub fn button(
        &mut self,
        button: PointerButton,
        raw: Option<InputId>,
        pressed: bool,
        captured: bool,
    ) -> Delivery {
        self.observe_capture(captured);
        let phase = phase(pressed, false);
        let delivery = self
            .rebound(raw, phase, captured, false)
            .unwrap_or_else(|| {
                let route = self.button_route(button, raw, pressed, captured);
                self.after_route(route, raw, phase, captured)
            });
        self.delivered(raw, phase, delivery);
        delivery
    }

    /// The router's own handling of a mouse button, as [`key_route`](Self::key_route).
    fn button_route(
        &mut self,
        button: PointerButton,
        raw: Option<InputId>,
        pressed: bool,
        captured: bool,
    ) -> Route {
        if !self.enabled {
            return Route::Forward;
        }
        if self.capture_release(raw, phase(pressed, false)) {
            return Route::Consume;
        }
        if self.ui_open
            && let Some(capture) = self.capturing
        {
            let result = match capture {
                Capture::Hotkey => hotkey::assignable(Trigger::Mouse(button)).then(|| {
                    Captured::Hotkey(Hotkey::with_modifiers(
                        Trigger::Mouse(button),
                        self.modifiers,
                    ))
                }),
                // Left and right clicks work the UI.
                Capture::Input => raw
                    .filter(|id| matches!(id, InputId::Mouse(2 | 4 | 5)))
                    .map(Captured::Input),
            };
            if pressed && let Some(result) = result {
                self.finish_capture(result);
                self.hold_captured(raw);
                return Route::Consume;
            }
        } else if pressed && self.keys.menu.matches_button(button, self.modifiers) {
            self.set_ui_open(!self.ui_open);
            self.hold_captured(raw);
            return Route::Consume;
        }
        if !self.ui_open {
            let modifiers = self.modifiers;
            return self.game_hotkey(
                Trigger::Mouse(button),
                raw,
                pressed,
                false,
                modifiers,
                captured,
            );
        }
        self.push_event(Event::PointerButton {
            pos: self.pointer_points(),
            button,
            pressed,
            modifiers: self.modifiers,
        });
        if pressed {
            Route::Consume
        } else {
            Route::Forward
        }
    }

    /// An absolute cursor position (GLFW). Returns the position to forward to the game, or
    /// `None` to drop the event.
    pub fn cursor_position(&mut self, x: f64, y: f64, captured: bool) -> Option<(f64, f64)> {
        self.observe_capture(captured);
        let (dx, dy) = self
            .last_cursor
            .map_or((0.0, 0.0), |(lx, ly)| (x - lx, y - ly));
        self.last_cursor = Some((x, y));
        if self.ui_open {
            if captured {
                self.offset.0 += dx;
                self.offset.1 += dy;
                self.move_pointer(dx as f32, dy as f32);
            } else {
                self.set_pointer(x as f32, y as f32);
            }
            return None;
        }
        if !captured {
            // Where the free cursor is, for the menu opening over it.
            self.pointer = pos2(x as f32, y as f32);
        }
        Some(if captured {
            (x - self.offset.0, y - self.offset.1)
        } else {
            (x, y)
        })
    }

    /// Whether the game has the cursor, from any event: the game re-centres the cursor and its
    /// reference point when it grabs or releases it, so earlier motion no longer matters.
    fn observe_capture(&mut self, captured: bool) {
        if captured != self.last_captured {
            self.last_captured = captured;
            self.last_cursor = None;
            self.offset = (0.0, 0.0);
        }
    }

    /// Relative mouse motion with the absolute position (SDL3).
    pub fn cursor_motion(&mut self, dx: f32, dy: f32, x: f32, y: f32, captured: bool) -> Route {
        self.observe_capture(captured);
        if !self.ui_open {
            if !captured {
                // Where the free cursor is, for the menu opening over it.
                self.pointer = pos2(x, y);
            }
            return Route::Forward;
        }
        if captured {
            self.move_pointer(dx, dy);
        } else {
            self.set_pointer(x, y);
        }
        Route::Consume
    }

    /// Wheel motion in lines (positive y = away from the user).
    pub fn scroll(&mut self, dx: f32, dy: f32) -> Route {
        if !self.ui_open {
            return Route::Forward;
        }
        self.push_event(Event::MouseWheel {
            unit: MouseWheelUnit::Line,
            delta: vec2(dx, dy),
            phase: TouchPhase::Move,
            modifiers: self.modifiers,
        });
        Route::Consume
    }

    /// The window lost focus: the releases of held keys may never arrive. Returns the releases
    /// of the rebinding's outputs, which the game must get now (before the platform's own
    /// releases of the sources, which are dropped then). `platform_releases(source)`: the
    /// platform will send that key's or button's release after the focus loss (GLFW: every
    /// key but key -1, and every button; SDL3: keys only).
    #[must_use = "the releases must reach the game"]
    pub fn focus_lost(&mut self, platform_releases: impl Fn(InputId) -> bool) -> Vec<Output> {
        if self.ui_open {
            // The UI lets go of what it holds (the browser's page of its buttons and keys).
            self.push_event(Event::WindowFocused(false));
        }
        self.zoom_held = false;
        self.zoom_raw = None;
        self.action_held = [false; IN_GAME.len()];
        self.action_raw = [None; IN_GAME.len()];
        self.debug_held = false;
        self.suppress_text = false;
        self.rebinder.focus_lost(platform_releases)
    }

    fn push_event(&mut self, event: Event) {
        // Nobody drains the queue while frames are not drawn (minimized window).
        const MAX_QUEUED: usize = 1024;
        if self.events.len() >= MAX_QUEUED {
            self.events.clear();
        }
        self.events.push(event);
    }

    fn pointer_points(&self) -> Pos2 {
        (self.pointer.to_vec2() / self.pixels_per_point).to_pos2()
    }

    fn set_pointer(&mut self, x: f32, y: f32) {
        self.pointer = pos2(x, y);
        self.push_event(Event::PointerMoved(self.pointer_points()));
    }

    fn move_pointer(&mut self, dx: f32, dy: f32) {
        let (dx, dy) = self.pointer_speed.apply(dx, dy);
        let [w, h] = self.screen_px;
        let x = (self.pointer.x + dx).clamp(0.0, (w - 1.0).max(0.0));
        let y = (self.pointer.y + dy).clamp(0.0, (h - 1.0).max(0.0));
        self.set_pointer(x, y);
    }
}

fn phase(pressed: bool, repeat: bool) -> Phase {
    match (pressed, repeat) {
        (false, _) => Phase::Release,
        (true, true) => Phase::Repeat,
        (true, false) => Phase::Press,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reminedog_core::{Naming, input_by_name};

    const CTRL: Modifiers = Modifiers::CTRL;
    const NONE: Modifiers = Modifiers::NONE;

    fn router() -> InputRouter {
        let mut r = InputRouter::new();
        r.set_screen([800, 600], 1.0);
        r.set_enabled(true);
        r
    }

    #[test]
    fn in_game_hotkeys_match_in_the_menus_order() {
        // The menu's overlap checks assume this order.
        let order = Action::ROUTER_ORDER;
        assert_eq!(order[0], Action::Menu);
        assert_eq!(order[order.len() - 1], Action::Zoom);
        assert_eq!(IN_GAME.map(InGame::action), order[1..order.len() - 1]);
    }

    /// The key the game knows for an egui key with the same name (letters, F-keys, Esc).
    fn raw(key: Key) -> Option<InputId> {
        let name = format!("key.keyboard.{}", key.name().to_ascii_lowercase());
        input_by_name(&name, Naming::Modern)
    }

    fn mouse_raw(button: PointerButton) -> InputId {
        InputId::Mouse(match button {
            PointerButton::Primary => 1,
            PointerButton::Middle => 2,
            PointerButton::Secondary => 3,
            PointerButton::Extra1 => 4,
            PointerButton::Extra2 => 5,
        })
    }

    fn route(delivery: Delivery) -> Route {
        match delivery {
            Delivery::Forward => Route::Forward,
            Delivery::Consume => Route::Consume,
            Delivery::Send(output) => panic!("rebound to {output:?}"),
        }
    }

    /// The router's own routing, where nothing is rebound.
    impl InputRouter {
        fn route_key(
            &mut self,
            key: Option<Key>,
            pressed: bool,
            repeat: bool,
            modifiers: Modifiers,
            captured: bool,
        ) -> Route {
            let raw = key.and_then(raw);
            route(self.key(key, raw, pressed, repeat, modifiers, captured, false))
        }

        fn route_button(&mut self, button: PointerButton, pressed: bool, captured: bool) -> Route {
            route(self.button(button, Some(mouse_raw(button)), pressed, captured))
        }
    }

    #[test]
    fn disabled_router_forwards_everything() {
        let mut r = InputRouter::new();
        assert_eq!(
            r.route_key(Some(Key::I), true, false, CTRL, true),
            Route::Forward
        );
        assert!(!r.ui_open());
        assert_eq!(
            r.route_key(Some(Key::Z), true, false, NONE, true),
            Route::Forward
        );
        let mut r = router();
        ctrl_i(&mut r);
        r.set_enabled(false);
        assert!(!r.ui_open(), "stopping the overlay closes the UI");
        assert_eq!(
            r.route_key(Some(Key::W), true, false, NONE, true),
            Route::Forward
        );
        assert!(r.take_events().is_empty());
    }

    fn ctrl_i(r: &mut InputRouter) -> Route {
        r.route_key(Some(Key::I), true, false, CTRL, true)
    }

    #[test]
    fn ctrl_i_toggles_the_ui_and_is_never_forwarded() {
        let mut r = router();
        assert_eq!(ctrl_i(&mut r), Route::Consume);
        assert!(r.ui_open());
        assert_eq!(r.take_ui_changed(), Some(true));
        assert_eq!(r.take_ui_changed(), None);
        assert_eq!(
            r.route_key(Some(Key::I), true, true, CTRL, true),
            Route::Consume,
            "repeat"
        );
        assert!(r.ui_open(), "repeats do not toggle");
        assert_eq!(ctrl_i(&mut r), Route::Consume);
        assert!(!r.ui_open());
        assert_eq!(r.take_ui_changed(), Some(false));
        // Plain I is the game's; extra modifiers still toggle.
        assert_eq!(
            r.route_key(Some(Key::I), true, false, NONE, true),
            Route::Forward
        );
        let ctrl_shift = Modifiers {
            shift: true,
            ..CTRL
        };
        assert_eq!(
            r.route_key(Some(Key::I), true, false, ctrl_shift, true),
            Route::Consume
        );
    }

    #[test]
    fn open_ui_takes_presses_but_forwards_releases() {
        let mut r = router();
        ctrl_i(&mut r);
        r.take_events();
        assert_eq!(
            r.route_key(Some(Key::W), true, false, NONE, true),
            Route::Consume
        );
        assert_eq!(
            r.route_key(Some(Key::W), false, false, NONE, true),
            Route::Forward
        );
        assert_eq!(
            r.route_key(None, true, false, NONE, true),
            Route::Consume,
            "unnamed keys too"
        );
        assert_eq!(
            r.route_button(PointerButton::Primary, true, true),
            Route::Consume
        );
        assert_eq!(
            r.route_button(PointerButton::Primary, false, true),
            Route::Forward
        );
        assert_eq!(r.text("あ"), Route::Consume);
        assert_eq!(r.scroll(0.0, 1.0), Route::Consume);
        let events = r.take_events();
        assert!(events.contains(&Event::Text("あ".into())));
        assert!(events.iter().any(|e| matches!(
            e,
            Event::Key {
                key: Key::W,
                pressed: false,
                ..
            }
        )));
    }

    #[test]
    fn escape_closes_the_ui_without_reaching_the_game() {
        let mut r = router();
        ctrl_i(&mut r);
        assert_eq!(
            r.route_key(Some(Key::Escape), true, false, NONE, true),
            Route::Consume
        );
        assert!(!r.ui_open());
        // With the UI closed, Escape is the game's again.
        assert_eq!(
            r.route_key(Some(Key::Escape), true, false, NONE, true),
            Route::Forward
        );
    }

    #[test]
    fn closed_ui_forwards_everything_but_hotkeys() {
        let mut r = router();
        assert_eq!(
            r.route_key(Some(Key::W), true, false, NONE, true),
            Route::Forward
        );
        assert_eq!(r.text("w"), Route::Forward);
        assert_eq!(
            r.route_button(PointerButton::Primary, true, true),
            Route::Forward
        );
        assert_eq!(r.scroll(0.0, 1.0), Route::Forward);
        assert_eq!(r.cursor_position(10.0, 20.0, true), Some((10.0, 20.0)));
        assert!(r.take_events().is_empty());
    }

    #[test]
    fn zoom_is_held_in_game_only() {
        let mut r = router();
        assert_eq!(
            r.route_key(Some(Key::Z), true, false, NONE, true),
            Route::Consume
        );
        assert!(r.zoom_active());
        assert_eq!(r.text("z"), Route::Consume, "the key's characters too");
        assert_eq!(
            r.route_key(Some(Key::Z), true, true, NONE, true),
            Route::Consume,
            "repeat"
        );
        assert_eq!(
            r.route_key(Some(Key::Z), false, false, NONE, true),
            Route::Consume
        );
        assert!(!r.zoom_active());
        // In a menu (cursor free), Z is the game's (typing in chat).
        assert_eq!(
            r.route_key(Some(Key::Z), true, false, NONE, false),
            Route::Forward
        );
        assert_eq!(
            r.route_key(Some(Key::Z), false, false, NONE, false),
            Route::Forward
        );
        assert!(!r.zoom_active());
    }

    #[test]
    fn opening_the_ui_or_losing_focus_ends_the_zoom() {
        let mut r = router();
        r.route_key(Some(Key::Z), true, false, NONE, true);
        ctrl_i(&mut r);
        assert!(!r.zoom_active());
        ctrl_i(&mut r);
        assert!(!r.zoom_active(), "stays off after closing");
        r.route_key(Some(Key::Z), true, false, NONE, true);
        assert!(r.focus_lost(|_| true).is_empty());
        assert!(!r.zoom_active());
    }

    #[test]
    fn captured_cursor_motion_under_the_ui_does_not_turn_the_camera() {
        let mut r = router();
        assert_eq!(
            r.cursor_position(1000.0, 1000.0, true),
            Some((1000.0, 1000.0))
        );
        ctrl_i(&mut r);
        assert_eq!(r.software_cursor(), Some(pos2(400.0, 300.0)));
        assert_eq!(r.cursor_position(1050.0, 980.0, true), None);
        assert_eq!(r.software_cursor(), Some(pos2(450.0, 280.0)));
        ctrl_i(&mut r);
        // The game last saw (1000, 1000): the next position continues from there.
        assert_eq!(
            r.cursor_position(1060.0, 980.0, true),
            Some((1010.0, 1000.0))
        );
    }

    #[test]
    fn software_cursor_stays_inside_the_window() {
        let mut r = router();
        r.cursor_position(0.0, 0.0, true);
        ctrl_i(&mut r);
        r.cursor_position(-5000.0, 9000.0, true);
        assert_eq!(r.software_cursor(), Some(pos2(0.0, 599.0)));
    }

    #[test]
    fn grabbing_or_releasing_the_cursor_resets_the_correction() {
        let mut r = router();
        r.cursor_position(100.0, 100.0, true);
        ctrl_i(&mut r);
        r.cursor_position(200.0, 100.0, true);
        ctrl_i(&mut r);
        // The game opens a menu: the cursor is free and positions pass through as is.
        assert_eq!(r.cursor_position(300.0, 300.0, false), Some((300.0, 300.0)));
        // Back in game: the game re-centred, no correction is left.
        assert_eq!(r.cursor_position(400.0, 300.0, true), Some((400.0, 300.0)));
    }

    #[test]
    fn free_cursor_positions_drive_the_ui_pointer_in_points() {
        let mut r = router();
        r.set_screen([800, 600], 2.0);
        r.cursor_position(0.0, 0.0, false);
        ctrl_i(&mut r);
        r.take_events();
        assert_eq!(r.cursor_position(100.0, 50.0, false), None);
        assert_eq!(r.take_events(), vec![Event::PointerMoved(pos2(50.0, 25.0))]);
        assert_eq!(r.software_cursor(), None, "the real cursor is visible");
    }

    #[test]
    fn the_menu_opens_where_the_free_cursor_is() {
        let mut r = router();
        r.cursor_position(100.0, 100.0, false);
        ctrl_i(&mut r);
        ctrl_i(&mut r);
        // A screen of the game is open: the menu's pointer follows the real cursor.
        r.cursor_position(120.0, 80.0, false);
        r.take_events();
        r.route_key(Some(Key::I), true, false, CTRL, false);
        assert!(r.ui_open());
        assert_eq!(r.software_cursor(), None, "the real cursor is visible");
        assert!(
            r.take_events()
                .contains(&Event::PointerMoved(pos2(120.0, 80.0)))
        );
        r.route_key(Some(Key::I), true, false, CTRL, false);
        // SDL3 reports the free cursor's position with its motion too.
        r.cursor_motion(1.0, 1.0, 300.0, 200.0, false);
        r.take_events();
        r.route_key(Some(Key::I), true, false, CTRL, false);
        assert!(
            r.take_events()
                .contains(&Event::PointerMoved(pos2(300.0, 200.0)))
        );
    }

    #[test]
    fn the_menu_key_tells_that_the_game_took_the_cursor() {
        let mut r = router();
        // The last motion was on a screen (a chat), which closed without a motion since.
        r.cursor_position(120.0, 80.0, false);
        ctrl_i(&mut r);
        // The game hides its cursor: the menu draws its own, from the middle.
        assert_eq!(r.software_cursor(), Some(pos2(400.0, 300.0)));
    }

    #[test]
    fn relative_motion_moves_the_software_cursor() {
        let mut r = router();
        assert_eq!(r.cursor_motion(5.0, 5.0, 0.0, 0.0, true), Route::Forward);
        ctrl_i(&mut r);
        assert_eq!(r.cursor_motion(10.0, -20.0, 0.0, 0.0, true), Route::Consume);
        assert_eq!(r.software_cursor(), Some(pos2(410.0, 280.0)));
    }

    #[test]
    fn pointer_speed_scales_captured_motion_only() {
        let mut r = router();
        r.set_pointer_speed(PointerSpeed::Linear(0.5));
        ctrl_i(&mut r);
        r.cursor_motion(10.0, -20.0, 0.0, 0.0, true);
        assert_eq!(r.software_cursor(), Some(pos2(405.0, 290.0)));
        // A free cursor is the system's own pointer, already at the system's speed.
        r.cursor_motion(0.0, 0.0, 100.0, 50.0, false);
        r.take_events();
        r.cursor_motion(10.0, 10.0, 110.0, 60.0, false);
        assert_eq!(
            r.take_events(),
            vec![Event::PointerMoved(pos2(110.0, 60.0))]
        );
    }

    #[test]
    fn pointer_speed_does_not_change_the_camera_correction() {
        let mut r = router();
        r.set_pointer_speed(PointerSpeed::Linear(2.0));
        r.cursor_position(100.0, 100.0, true);
        ctrl_i(&mut r);
        r.cursor_position(110.0, 100.0, true);
        assert_eq!(r.software_cursor(), Some(pos2(420.0, 300.0)));
        ctrl_i(&mut r);
        // The game still sees no motion for what the UI took.
        assert_eq!(r.cursor_position(111.0, 100.0, true), Some((101.0, 100.0)));
    }

    #[test]
    fn rebound_hotkeys_replace_the_defaults() {
        let mut r = router();
        let menu = Hotkey::parse("F6").unwrap();
        let zoom = Hotkey::parse("C").unwrap();
        r.set_hotkeys(Hotkeys {
            menu,
            zoom,
            ..Hotkeys::DEFAULT
        });
        assert_eq!(ctrl_i(&mut r), Route::Forward, "Ctrl+I is the game's again");
        assert!(!r.ui_open());
        assert_eq!(
            r.route_key(Some(Key::Z), true, false, NONE, true),
            Route::Forward
        );
        assert_eq!(
            r.route_key(Some(Key::C), true, false, NONE, true),
            Route::Consume
        );
        assert!(r.zoom_active());
        assert_eq!(
            r.route_key(Some(Key::C), false, false, NONE, true),
            Route::Consume
        );
        assert_eq!(
            r.route_key(Some(Key::F6), true, false, NONE, true),
            Route::Consume
        );
        assert!(r.ui_open());
    }

    #[test]
    fn zoom_starts_with_extra_modifiers_held() {
        let mut r = router();
        // Sprinting with Ctrl held.
        assert_eq!(
            r.route_key(Some(Key::Z), true, false, CTRL, true),
            Route::Consume
        );
        assert!(r.zoom_active());
    }

    #[test]
    fn zoom_on_a_mouse_button() {
        let mut r = router();
        r.set_hotkeys(Hotkeys {
            zoom: Hotkey::plain(Trigger::Mouse(PointerButton::Extra1)),
            ..Hotkeys::DEFAULT
        });
        // Not in game (a menu of the game is open): the game gets the button.
        assert_eq!(
            r.route_button(PointerButton::Extra1, true, false),
            Route::Forward
        );
        assert!(!r.zoom_active());
        r.route_button(PointerButton::Extra1, false, false);
        assert_eq!(
            r.route_button(PointerButton::Extra1, true, true),
            Route::Consume
        );
        assert!(r.zoom_active());
        assert_eq!(
            r.route_button(PointerButton::Extra1, false, true),
            Route::Consume
        );
        assert!(!r.zoom_active());
        assert_eq!(
            r.route_button(PointerButton::Extra2, true, true),
            Route::Forward
        );
    }

    #[test]
    fn menu_on_a_mouse_button() {
        let mut r = router();
        r.set_hotkeys(Hotkeys {
            menu: Hotkey::plain(Trigger::Mouse(PointerButton::Middle)),
            ..Hotkeys::DEFAULT
        });
        assert_eq!(
            r.route_button(PointerButton::Middle, true, true),
            Route::Consume
        );
        assert!(r.ui_open());
        assert_eq!(
            r.route_button(PointerButton::Middle, true, true),
            Route::Consume
        );
        assert!(!r.ui_open());
    }

    #[test]
    fn capture_takes_the_next_key_with_its_modifiers() {
        let mut r = router();
        ctrl_i(&mut r);
        r.take_events();
        r.start_capture();
        // Modifiers alone (unnamed keys) keep waiting.
        assert_eq!(r.route_key(None, true, false, CTRL, true), Route::Consume);
        assert_eq!(r.take_captured(), None);
        // The menu hotkey itself can be captured instead of closing the menu.
        assert_eq!(ctrl_i(&mut r), Route::Consume);
        assert!(r.ui_open());
        assert_eq!(
            r.take_captured(),
            Some(Captured::Hotkey(Hotkey::DEFAULT_MENU))
        );
        assert!(r.take_events().is_empty(), "egui does not see the key");
        // Capture is over: the next Ctrl+I closes the menu.
        ctrl_i(&mut r);
        assert!(!r.ui_open());
    }

    #[test]
    fn capture_takes_extra_mouse_buttons_but_not_clicks() {
        let mut r = router();
        ctrl_i(&mut r);
        // Ctrl and I released.
        r.route_key(Some(Key::I), false, false, NONE, true);
        r.start_capture();
        assert_eq!(
            r.route_button(PointerButton::Primary, true, true),
            Route::Consume
        );
        assert_eq!(r.take_captured(), None, "a click still works the UI");
        assert_eq!(
            r.route_button(PointerButton::Extra2, true, true),
            Route::Consume
        );
        assert_eq!(
            r.take_captured(),
            Some(Captured::Hotkey(Hotkey::plain(Trigger::Mouse(
                PointerButton::Extra2
            ))))
        );
    }

    #[test]
    fn capture_is_cancelled_by_escape_or_closing() {
        let mut r = router();
        ctrl_i(&mut r);
        r.start_capture();
        assert_eq!(
            r.route_key(Some(Key::Escape), true, false, NONE, true),
            Route::Consume
        );
        assert_eq!(r.take_captured(), Some(Captured::Cancelled));
        assert!(r.ui_open(), "Escape cancelled the capture, not the menu");
        r.start_capture();
        r.set_ui_open(false);
        assert_eq!(r.take_captured(), Some(Captured::Cancelled));
        // Capture needs the UI.
        r.start_capture();
        assert_eq!(
            r.route_key(Some(Key::Z), true, false, NONE, true),
            Route::Consume
        );
        assert_eq!(r.take_captured(), None);
    }

    #[test]
    fn waypoint_and_navigate_keys_queue_actions_in_game() {
        let mut r = router();
        assert_eq!(
            r.route_key(Some(Key::J), true, false, NONE, true),
            Route::Consume
        );
        assert_eq!(
            r.route_key(Some(Key::J), true, true, NONE, true),
            Route::Consume,
            "repeat"
        );
        assert_eq!(r.text("j"), Route::Consume, "the key's characters too");
        assert_eq!(
            r.route_key(Some(Key::J), false, false, NONE, true),
            Route::Consume
        );
        // Sprinting with Ctrl held.
        assert_eq!(
            r.route_key(Some(Key::K), true, false, CTRL, true),
            Route::Consume
        );
        r.route_key(Some(Key::K), false, false, NONE, true);
        assert_eq!(
            r.take_actions(),
            vec![HotkeyAction::RecordWaypoint, HotkeyAction::Navigate]
        );
        assert!(r.take_actions().is_empty());
        assert!(r.take_events().is_empty());
        assert!(!r.zoom_active());
    }

    #[test]
    fn waypoint_keys_are_the_games_outside_play_and_with_the_ui_open() {
        let mut r = router();
        // A game screen is open (typing "j" in chat).
        assert_eq!(
            r.route_key(Some(Key::J), true, false, NONE, false),
            Route::Forward
        );
        assert_eq!(
            r.route_key(Some(Key::J), true, true, NONE, false),
            Route::Forward,
            "repeat"
        );
        assert_eq!(r.text("j"), Route::Forward);
        assert_eq!(
            r.route_key(Some(Key::J), false, false, NONE, false),
            Route::Forward
        );
        // Our menu takes presses for egui instead.
        ctrl_i(&mut r);
        assert_eq!(
            r.route_key(Some(Key::K), true, false, NONE, true),
            Route::Consume
        );
        assert!(r.take_actions().is_empty());
    }

    #[test]
    fn a_release_is_consumed_only_after_a_consumed_press() {
        let mut r = router();
        // Pressed in a game screen, released in game: the game saw the press.
        r.route_key(Some(Key::J), true, false, NONE, false);
        assert_eq!(
            r.route_key(Some(Key::J), false, false, NONE, true),
            Route::Forward
        );
        // Pressed in game, released after a game screen opened: still ours.
        r.route_key(Some(Key::J), true, false, NONE, true);
        assert_eq!(
            r.route_key(Some(Key::J), false, false, NONE, false),
            Route::Consume
        );
        // Opening the menu forgets the press; its release stays out of the game, which never
        // got the press (F3 as a hotkey would toggle the debug screen).
        r.route_key(Some(Key::J), true, false, NONE, true);
        ctrl_i(&mut r);
        ctrl_i(&mut r);
        assert_eq!(
            r.route_key(Some(Key::J), false, false, NONE, true),
            Route::Consume
        );
        assert_eq!(
            r.route_key(Some(Key::J), false, false, NONE, true),
            Route::Forward,
            "once"
        );
        assert_eq!(r.take_actions().len(), 2);
    }

    #[test]
    fn debug_modifier_hands_the_in_game_hotkeys_to_the_game() {
        let mut r = router();
        assert_eq!(
            r.route_key(Some(Key::F3), true, false, NONE, true),
            Route::Forward,
            "the modifier itself is the game's"
        );
        for key in [Key::J, Key::K, Key::Z] {
            assert_eq!(
                r.route_key(Some(key), true, false, NONE, true),
                Route::Forward,
                "{key:?}"
            );
            assert_eq!(
                r.route_key(Some(key), false, false, NONE, true),
                Route::Forward,
                "{key:?}"
            );
        }
        assert!(r.take_actions().is_empty());
        assert!(!r.zoom_active());
        // The menu key still works.
        assert_eq!(ctrl_i(&mut r), Route::Consume);
        ctrl_i(&mut r);
        r.route_key(Some(Key::F3), false, false, NONE, true);
        assert_eq!(
            r.route_key(Some(Key::J), true, false, NONE, true),
            Route::Consume
        );
        assert_eq!(r.take_actions(), vec![HotkeyAction::RecordWaypoint]);
    }

    #[test]
    fn keys_taken_before_the_debug_modifier_are_released_by_us() {
        let mut r = router();
        r.route_key(Some(Key::Z), true, false, NONE, true);
        r.route_key(Some(Key::J), true, false, NONE, true);
        r.route_key(Some(Key::F3), true, false, NONE, true);
        assert!(r.zoom_active(), "a zoom already held goes on");
        assert_eq!(
            r.route_key(Some(Key::Z), true, true, NONE, true),
            Route::Consume
        );
        assert_eq!(
            r.route_key(Some(Key::J), true, true, NONE, true),
            Route::Consume
        );
        assert_eq!(
            r.route_key(Some(Key::Z), false, false, NONE, true),
            Route::Consume
        );
        assert!(!r.zoom_active());
        assert_eq!(
            r.route_key(Some(Key::J), false, false, NONE, true),
            Route::Consume
        );
        // A new zoom press waits for F3's release.
        assert_eq!(
            r.route_key(Some(Key::Z), true, false, NONE, true),
            Route::Forward
        );
        assert!(!r.zoom_active());
    }

    #[test]
    fn the_debug_modifier_can_be_rebound_or_unbound() {
        let mut r = router();
        r.set_debug_modifier(raw(Key::F6));
        r.route_key(Some(Key::F3), true, false, NONE, true);
        assert_eq!(
            r.route_key(Some(Key::J), true, false, NONE, true),
            Route::Consume
        );
        r.route_key(Some(Key::J), false, false, NONE, true);
        r.route_key(Some(Key::F6), true, false, NONE, true);
        assert_eq!(
            r.route_key(Some(Key::J), true, false, NONE, true),
            Route::Forward
        );
        r.route_key(Some(Key::J), false, false, NONE, true);
        // Changing the key forgets the held state.
        r.set_debug_modifier(None);
        assert_eq!(
            r.route_key(Some(Key::K), true, false, NONE, true),
            Route::Consume
        );
        assert_eq!(
            r.take_actions(),
            vec![HotkeyAction::RecordWaypoint, HotkeyAction::Navigate]
        );
    }

    #[test]
    fn debug_modifier_state_resets_on_focus_loss_and_disable() {
        let mut r = router();
        r.route_key(Some(Key::F3), true, false, NONE, true);
        assert!(r.focus_lost(|_| true).is_empty());
        assert_eq!(
            r.route_key(Some(Key::J), true, false, NONE, true),
            Route::Consume
        );
        r.route_key(Some(Key::J), false, false, NONE, true);
        r.route_key(Some(Key::F3), true, false, NONE, true);
        r.set_enabled(false);
        r.set_enabled(true);
        assert_eq!(
            r.route_key(Some(Key::J), true, false, NONE, true),
            Route::Consume
        );
        // Tracked while disabled too.
        r.set_enabled(false);
        r.route_key(Some(Key::F3), true, false, NONE, true);
        r.set_enabled(true);
        assert_eq!(
            r.route_key(Some(Key::K), true, false, NONE, true),
            Route::Forward
        );
    }

    #[test]
    fn at_most_four_actions_wait() {
        let mut r = router();
        for _ in 0..6 {
            r.route_key(Some(Key::J), true, false, NONE, true);
            r.route_key(Some(Key::J), false, false, NONE, true);
        }
        assert_eq!(r.take_actions().len(), 4);
        r.route_key(Some(Key::K), true, false, NONE, true);
        r.set_enabled(false);
        assert!(r.take_actions().is_empty(), "disabling drops them");
    }

    /// A press and its release in game.
    fn tap(r: &mut InputRouter, key: Key) -> Route {
        let route = r.route_key(Some(key), true, false, NONE, true);
        assert_eq!(r.route_key(Some(key), false, false, NONE, true), route);
        route
    }

    #[test]
    fn browser_keys_work_in_game_while_it_shows() {
        let mut r = router();
        // Hidden: the arrows and Page Down are the game's.
        assert_eq!(tap(&mut r, Key::PageDown), Route::Forward);
        assert_eq!(tap(&mut r, Key::ArrowDown), Route::Forward);
        assert!(r.take_browser_actions().is_empty());

        r.set_browser_shown(true);
        assert_eq!(tap(&mut r, Key::PageDown), Route::Consume);
        assert_eq!(tap(&mut r, Key::PageUp), Route::Consume);
        assert_eq!(tap(&mut r, Key::ArrowDown), Route::Consume);
        assert_eq!(tap(&mut r, Key::ArrowLeft), Route::Consume);
        assert_eq!(tap(&mut r, Key::ArrowRight), Route::Consume);
        assert_eq!(
            r.take_browser_actions(),
            vec![
                BrowserAction::PageDown,
                BrowserAction::PageUp,
                BrowserAction::PlayPause,
                BrowserAction::SeekBack,
                BrowserAction::SeekForward,
            ]
        );
        assert!(r.take_actions().is_empty(), "not the waypoint queue");

        // A game screen is open (the arrows move the caret in chat).
        assert_eq!(
            r.route_key(Some(Key::ArrowLeft), true, false, NONE, false),
            Route::Forward
        );
        assert_eq!(
            r.route_key(Some(Key::ArrowLeft), false, false, NONE, false),
            Route::Forward
        );
        // F3 held: the game's.
        r.route_key(Some(Key::F3), true, false, NONE, true);
        assert_eq!(
            r.route_key(Some(Key::PageDown), true, false, NONE, true),
            Route::Forward
        );
        assert!(r.take_browser_actions().is_empty());
    }

    #[test]
    fn scrolling_and_seeking_repeat_while_held() {
        let mut r = router();
        r.set_browser_shown(true);
        for key in [Key::PageDown, Key::ArrowLeft, Key::ArrowDown] {
            r.route_key(Some(key), true, false, NONE, true);
            assert_eq!(
                r.route_key(Some(key), true, true, NONE, true),
                Route::Consume
            );
            r.route_key(Some(key), true, true, NONE, true);
            r.route_key(Some(key), false, false, NONE, true);
        }
        assert_eq!(
            r.take_browser_actions(),
            vec![
                BrowserAction::PageDown,
                BrowserAction::PageDown,
                BrowserAction::PageDown,
                BrowserAction::SeekBack,
                BrowserAction::SeekBack,
                BrowserAction::SeekBack,
                BrowserAction::PlayPause,
            ]
        );
    }

    #[test]
    fn hiding_the_browser_mid_press_keeps_the_release_ours() {
        let mut r = router();
        r.set_browser_shown(true);
        r.route_key(Some(Key::PageDown), true, false, NONE, true);
        r.set_browser_shown(false);
        assert_eq!(
            r.route_key(Some(Key::PageDown), false, false, NONE, true),
            Route::Consume
        );
    }

    #[test]
    fn the_browser_toggle_works_whether_it_shows_or_not() {
        let mut r = router();
        let toggle = Hotkey::parse("Ctrl+B").unwrap();
        r.set_hotkeys(Hotkeys {
            browser: crate::overlay::BrowserKeys {
                toggle: Some(toggle),
                page_down: None,
                ..Hotkeys::DEFAULT.browser
            },
            ..Hotkeys::DEFAULT
        });
        assert_eq!(
            r.route_key(Some(Key::B), true, false, CTRL, true),
            Route::Consume
        );
        assert_eq!(
            r.route_key(Some(Key::B), true, true, CTRL, true),
            Route::Consume,
            "repeats do nothing"
        );
        r.route_key(Some(Key::B), false, false, NONE, true);
        assert_eq!(r.take_browser_actions(), vec![BrowserAction::Toggle]);
        assert_eq!(
            r.route_key(Some(Key::B), true, false, NONE, true),
            Route::Forward,
            "without Ctrl"
        );
        // An unassigned key is the game's even while the browser shows.
        r.set_browser_shown(true);
        assert_eq!(tap(&mut r, Key::PageDown), Route::Forward);
    }

    #[test]
    fn at_most_eight_browser_actions_wait() {
        let mut r = router();
        r.set_browser_shown(true);
        r.route_key(Some(Key::PageDown), true, false, NONE, true);
        for _ in 0..20 {
            r.route_key(Some(Key::PageDown), true, true, NONE, true);
        }
        assert_eq!(r.take_browser_actions().len(), 8);
        r.route_key(Some(Key::PageDown), true, true, NONE, true);
        r.set_enabled(false);
        assert!(r.take_browser_actions().is_empty(), "disabling drops them");
    }

    #[test]
    fn waypoint_hotkeys_with_modifiers_and_mouse_buttons() {
        let mut r = router();
        r.set_hotkeys(Hotkeys {
            waypoint: Hotkey::parse("Shift+Z").unwrap(),
            navigate: Hotkey::plain(Trigger::Mouse(PointerButton::Extra2)),
            ..Hotkeys::DEFAULT
        });
        // Plain Z still zooms; Shift+Z records.
        assert_eq!(
            r.route_key(Some(Key::Z), true, false, NONE, true),
            Route::Consume
        );
        assert!(r.zoom_active());
        r.route_key(Some(Key::Z), false, false, NONE, true);
        let shift = Modifiers::SHIFT;
        assert_eq!(
            r.route_key(Some(Key::Z), true, false, shift, true),
            Route::Consume
        );
        assert!(!r.zoom_active());
        // Shift let go first: the release is still ours.
        assert_eq!(
            r.route_key(Some(Key::Z), false, false, NONE, true),
            Route::Consume
        );
        assert_eq!(
            r.route_button(PointerButton::Extra2, true, false),
            Route::Forward
        );
        r.route_button(PointerButton::Extra2, false, false);
        assert_eq!(
            r.route_button(PointerButton::Extra2, true, true),
            Route::Consume
        );
        assert_eq!(
            r.route_button(PointerButton::Extra2, false, true),
            Route::Consume
        );
        r.route_key(Some(Key::F3), true, false, NONE, true);
        assert_eq!(
            r.route_button(PointerButton::Extra2, true, true),
            Route::Forward
        );
        assert_eq!(
            r.take_actions(),
            vec![HotkeyAction::RecordWaypoint, HotkeyAction::Navigate]
        );
    }

    #[test]
    fn a_zoom_on_the_debug_modifier_still_zooms() {
        let mut r = router();
        r.set_hotkeys(Hotkeys {
            zoom: Hotkey::plain(Trigger::Key(Key::F3)),
            ..Hotkeys::DEFAULT
        });
        assert_eq!(
            r.route_key(Some(Key::F3), true, false, NONE, true),
            Route::Consume
        );
        assert!(r.zoom_active());
        assert_eq!(
            r.route_key(Some(Key::F3), false, false, NONE, true),
            Route::Consume
        );
        assert!(!r.zoom_active());
    }

    #[test]
    fn a_repeat_never_starts_the_zoom() {
        let mut r = router();
        // Z went to the game while F3 was held; F3 is released before Z.
        r.route_key(Some(Key::F3), true, false, NONE, true);
        assert_eq!(
            r.route_key(Some(Key::Z), true, false, NONE, true),
            Route::Forward
        );
        r.route_key(Some(Key::F3), false, false, NONE, true);
        assert_eq!(
            r.route_key(Some(Key::Z), true, true, NONE, true),
            Route::Forward
        );
        assert!(!r.zoom_active());
        assert_eq!(
            r.route_key(Some(Key::Z), false, false, NONE, true),
            Route::Forward,
            "the game gets the release of the press it saw"
        );
    }

    const F3: InputId = InputId::Key(60);
    const C: InputId = InputId::Key(6);
    const Q: InputId = InputId::Key(20);
    const T: InputId = InputId::Key(23);
    const W: InputId = InputId::Key(26);
    const X: InputId = InputId::Key(27);
    const LCTRL: InputId = InputId::Key(224);
    const MOUSE4: InputId = InputId::Mouse(4);

    fn send(id: InputId, phase: Phase) -> Delivery {
        Delivery::Send(Output { id, phase })
    }

    /// A key event with the raw id of `key`.
    fn key(r: &mut InputRouter, key: Key, pressed: bool, repeat: bool, captured: bool) -> Delivery {
        r.key(Some(key), raw(key), pressed, repeat, NONE, captured, false)
    }

    fn mouse4(r: &mut InputRouter, pressed: bool) -> Delivery {
        r.button(PointerButton::Extra1, Some(MOUSE4), pressed, true)
    }

    #[test]
    fn rebound_presses_reach_the_game_as_their_output() {
        let mut r = router();
        r.set_rebinds(vec![(X, Q)], true);
        assert_eq!(
            key(&mut r, Key::X, true, false, true),
            send(Q, Phase::Press)
        );
        assert_eq!(
            key(&mut r, Key::X, true, true, true),
            send(Q, Phase::Repeat)
        );
        assert_eq!(
            key(&mut r, Key::X, false, false, true),
            send(Q, Phase::Release)
        );
        // In a game screen (chat, inventory) the key is the game's as it is.
        assert_eq!(key(&mut r, Key::X, true, false, false), Delivery::Forward);
        assert_eq!(key(&mut r, Key::X, false, false, true), Delivery::Forward);
        assert_eq!(r.rebinds(), [(X, Q)]);
    }

    #[test]
    fn a_rebound_debug_modifier_hands_the_hotkeys_to_the_game() {
        let mut r = router();
        r.set_rebinds(vec![(MOUSE4, F3)], true);
        assert_eq!(mouse4(&mut r, true), send(F3, Phase::Press));
        // The game has F3 down: J is F3+J.
        assert_eq!(key(&mut r, Key::J, true, false, true), Delivery::Forward);
        assert_eq!(key(&mut r, Key::J, false, false, true), Delivery::Forward);
        assert_eq!(mouse4(&mut r, false), send(F3, Phase::Release));
        assert_eq!(key(&mut r, Key::J, true, false, true), Delivery::Consume);
        // F3 rebound to another key: the game never sees F3, J stays ours.
        let mut r = router();
        r.set_rebinds(vec![(F3, C)], true);
        assert_eq!(
            key(&mut r, Key::F3, true, false, true),
            send(C, Phase::Press)
        );
        assert_eq!(key(&mut r, Key::J, true, false, true), Delivery::Consume);
        assert_eq!(r.take_actions(), [HotkeyAction::RecordWaypoint]);
    }

    #[test]
    fn the_debug_modifier_can_be_a_mouse_button() {
        let mut r = router();
        r.set_debug_modifier(Some(MOUSE4));
        assert_eq!(mouse4(&mut r, true), Delivery::Forward);
        assert_eq!(key(&mut r, Key::K, true, false, true), Delivery::Forward);
        mouse4(&mut r, false);
        assert_eq!(key(&mut r, Key::K, true, false, true), Delivery::Consume);
    }

    #[test]
    fn hotkeys_and_the_menu_go_by_the_physical_key() {
        let mut r = router();
        r.set_rebinds(
            vec![(raw(Key::Z).unwrap(), C), (raw(Key::I).unwrap(), X)],
            true,
        );
        // The zoom takes Z before any rule.
        assert_eq!(key(&mut r, Key::Z, true, false, true), Delivery::Consume);
        assert!(r.zoom_active());
        assert_eq!(key(&mut r, Key::Z, false, false, true), Delivery::Consume);
        // Plain I is rebound; Ctrl+I opens the menu.
        assert_eq!(
            key(&mut r, Key::I, true, false, true),
            send(X, Phase::Press)
        );
        assert_eq!(
            key(&mut r, Key::I, false, false, true),
            send(X, Phase::Release)
        );
        assert_eq!(ctrl_i(&mut r), Route::Consume);
        assert!(r.ui_open());
        // With the menu open nothing is rebound; releases still reach the game.
        assert_eq!(key(&mut r, Key::I, true, false, true), Delivery::Consume);
        assert_eq!(key(&mut r, Key::I, false, false, true), Delivery::Forward);
    }

    #[test]
    fn a_held_rebind_survives_the_menu_without_repeating() {
        let mut r = router();
        r.set_rebinds(vec![(X, Q)], true);
        assert_eq!(
            key(&mut r, Key::X, true, false, true),
            send(Q, Phase::Press)
        );
        r.set_ui_open(true);
        // Q is not dropped again and again while the menu is open.
        assert_eq!(key(&mut r, Key::X, true, true, true), Delivery::Consume);
        assert_eq!(r.text("x"), Route::Consume, "nor typed into the menu");
        r.set_enabled(false);
        assert_eq!(
            key(&mut r, Key::X, false, false, true),
            send(Q, Phase::Release),
            "the release is Q's whatever happened since"
        );
    }

    #[test]
    fn the_characters_of_a_rebound_key_are_dropped() {
        let mut r = router();
        r.set_rebinds(vec![(X, T)], true);
        // X → T opens the chat; the x must not be typed into it.
        assert_eq!(
            key(&mut r, Key::X, true, false, true),
            send(T, Phase::Press)
        );
        assert_eq!(r.text("x"), Route::Consume);
        assert_eq!(r.text("a"), Route::Forward, "only once");
        // Auto-repeat while the chat is open (the cursor is free).
        assert_eq!(key(&mut r, Key::X, true, true, false), Delivery::Consume);
        assert_eq!(r.text("x"), Route::Consume);
        assert_eq!(
            key(&mut r, Key::X, false, false, false),
            send(T, Phase::Release)
        );
        assert_eq!(key(&mut r, Key::A, true, false, false), Delivery::Forward);
        assert_eq!(r.text("a"), Route::Forward);
        // Mouse sources type nothing.
        r.set_rebinds(vec![(MOUSE4, T)], true);
        mouse4(&mut r, true);
        assert_eq!(r.text("t"), Route::Forward);
    }

    #[test]
    fn focus_loss_returns_the_releases_of_held_outputs() {
        let mut r = router();
        r.set_rebinds(vec![(MOUSE4, F3), (X, Q)], true);
        assert_eq!(key(&mut r, Key::W, true, false, true), Delivery::Forward);
        mouse4(&mut r, true);
        key(&mut r, Key::X, true, false, true);
        // SDL3: keys get their release after the focus loss, mouse buttons do not.
        let mut releases = r.focus_lost(|id| matches!(id, InputId::Key(_)));
        releases.sort_by_key(|o| o.id);
        let release = |id| Output {
            id,
            phase: Phase::Release,
        };
        assert_eq!(releases, [release(Q), release(F3)]);
        // SDL's own releases: W's is the game's, X's was given as Q's already.
        assert_eq!(key(&mut r, Key::W, false, false, false), Delivery::Forward);
        assert_eq!(key(&mut r, Key::X, false, false, false), Delivery::Consume);
        assert_eq!(r.rebind_state(), RebindState::default());
        // Back in the game, the button works at once.
        assert_eq!(mouse4(&mut r, true), send(F3, Phase::Press));
        assert_eq!(
            key(&mut r, Key::J, true, false, true),
            Delivery::Forward,
            "F3+J"
        );
    }

    #[test]
    fn keyboard_sources_can_be_left_out() {
        let mut r = router();
        r.set_rebinds(vec![(X, Q), (MOUSE4, F3)], false);
        assert_eq!(r.rebinds(), [(MOUSE4, F3)]);
        assert_eq!(key(&mut r, Key::X, true, false, true), Delivery::Forward);
    }

    #[test]
    fn a_stateless_key_repeats_with_presses() {
        // ろ under GLFW: key -1, auto-repeat as presses, no egui name.
        let ro = InputId::Key(135);
        let mut r = router();
        r.set_rebinds(vec![(ro, W)], true);
        let press = |r: &mut InputRouter, pressed: bool| {
            r.key(None, Some(ro), pressed, false, NONE, true, true)
        };
        assert_eq!(press(&mut r, true), send(W, Phase::Press));
        assert_eq!(press(&mut r, true), send(W, Phase::Repeat));
        assert_eq!(press(&mut r, false), send(W, Phase::Release));
    }

    #[test]
    fn release_source_lets_go_of_the_output() {
        let mut r = router();
        r.set_rebinds(vec![(MOUSE4, F3)], true);
        mouse4(&mut r, true);
        assert_eq!(r.rebind_state().held, [(MOUSE4, F3)]);
        assert_eq!(
            r.release_source(MOUSE4),
            Some(Output {
                id: F3,
                phase: Phase::Release
            })
        );
        // The game let go of F3: J is ours again.
        assert_eq!(key(&mut r, Key::J, true, false, true), Delivery::Consume);
        assert_eq!(mouse4(&mut r, false), Delivery::Forward);
    }

    #[test]
    fn input_capture_takes_any_key_and_some_buttons() {
        let mut r = router();
        ctrl_i(&mut r);
        r.key(Some(Key::I), raw(Key::I), false, false, NONE, true, false);
        r.take_events();
        r.start_input_capture();
        // Left and right clicks work the UI.
        assert_eq!(
            r.route_button(PointerButton::Primary, true, true),
            Route::Consume
        );
        assert_eq!(r.take_captured(), None);
        assert_eq!(r.take_events().len(), 1);
        // A modifier key alone is a key like any other, taken when it goes up alone.
        let ctrl = r.key(None, Some(LCTRL), true, false, CTRL, true, false);
        assert_eq!(ctrl, Delivery::Consume);
        assert_eq!(r.take_captured(), None);
        assert!(r.take_events().is_empty(), "egui does not see it");
        let ctrl = r.key(None, Some(LCTRL), false, false, NONE, true, false);
        assert_eq!(
            ctrl,
            Delivery::Consume,
            "its release is not the game's either"
        );
        assert_eq!(r.take_captured(), Some(Captured::Input(LCTRL)));
        let ctrl = r.key(None, Some(LCTRL), false, false, NONE, true, false);
        assert_eq!(ctrl, Delivery::Forward, "capture is over");
        r.start_input_capture();
        assert_eq!(
            r.route_button(PointerButton::Extra1, true, true),
            Route::Consume
        );
        assert_eq!(r.take_captured(), Some(Captured::Input(MOUSE4)));
        assert_eq!(
            r.route_button(PointerButton::Extra1, false, true),
            Route::Consume
        );
        // Repeats are not new presses; Esc cancels.
        r.start_input_capture();
        assert_eq!(key(&mut r, Key::Q, true, true, true), Delivery::Consume);
        assert_eq!(r.take_captured(), None);
        assert_eq!(
            key(&mut r, Key::Escape, true, false, true),
            Delivery::Consume
        );
        assert_eq!(r.take_captured(), Some(Captured::Cancelled));
        assert!(r.ui_open(), "Esc cancelled the capture, not the menu");
        // A hotkey capture after it takes egui keys again.
        r.start_capture();
        r.key(None, Some(LCTRL), true, false, CTRL, true, false);
        assert_eq!(r.take_captured(), None, "a modifier alone is no hotkey");
    }

    #[test]
    fn ctrl_i_while_waiting_for_a_key_closes_the_menu_without_taking_ctrl() {
        let mut r = router();
        ctrl_i(&mut r);
        r.key(Some(Key::I), raw(Key::I), false, false, NONE, true, false);
        r.start_input_capture();
        assert_eq!(
            r.key(None, Some(LCTRL), true, false, CTRL, true, false),
            Delivery::Consume
        );
        assert_eq!(
            r.key(Some(Key::I), raw(Key::I), true, false, CTRL, true, false),
            Delivery::Consume
        );
        assert!(!r.ui_open());
        assert_eq!(r.take_captured(), Some(Captured::Cancelled));
        assert_eq!(
            r.key(Some(Key::I), raw(Key::I), false, false, CTRL, true, false),
            Delivery::Consume,
            "the menu key's release is not the game's"
        );
        r.key(None, Some(LCTRL), false, false, NONE, true, false);
        assert_eq!(r.take_captured(), None, "nothing is taken afterwards");

        // A key taken in the frame the menu closes (a key, then Esc) is dropped too.
        ctrl_i(&mut r);
        r.start_input_capture();
        key(&mut r, Key::B, true, false, true);
        key(&mut r, Key::Escape, true, false, true);
        assert!(!r.ui_open());
        assert_eq!(r.take_captured(), Some(Captured::Cancelled));
    }

    #[test]
    fn the_ui_gets_copy_cut_and_the_clipboard_on_paste() {
        let mut r = router();
        ctrl_i(&mut r);
        r.take_events();
        r.route_key(Some(Key::V), true, false, CTRL, true);
        assert!(r.take_paste_request());
        assert!(!r.take_paste_request(), "once");
        r.paste("https://example.com/\r\nnext");
        r.route_key(Some(Key::C), true, false, CTRL, true);
        r.route_key(Some(Key::X), true, false, CTRL, true);
        let events: Vec<_> = r
            .take_events()
            .into_iter()
            .filter(|e| !matches!(e, Event::Key { .. }))
            .collect();
        assert_eq!(
            events,
            vec![
                Event::Paste("https://example.com/  next".into()),
                Event::Copy,
                Event::Cut
            ]
        );
        // Not while the menu is closed.
        ctrl_i(&mut r);
        r.take_events();
        r.paste("text");
        assert!(
            r.take_events()
                .iter()
                .all(|e| !matches!(e, Event::Paste(_)))
        );
    }

    #[test]
    fn a_menu_key_that_types_text_types_it_where_text_goes() {
        let mut r = router();
        r.set_hotkeys(Hotkeys {
            menu: Hotkey::parse("M").unwrap(),
            ..Hotkeys::DEFAULT
        });
        // In a game screen (chat): the game gets it.
        assert_eq!(
            r.route_key(Some(Key::M), true, false, NONE, false),
            Route::Forward
        );
        assert!(!r.ui_open());
        // In game it opens the menu.
        assert_eq!(
            r.route_key(Some(Key::M), true, false, NONE, true),
            Route::Consume
        );
        assert!(r.ui_open());
        r.route_key(Some(Key::M), false, false, NONE, true);
        // With a text field focused it is typed there.
        r.set_text_focus(true);
        r.take_events();
        assert_eq!(
            r.route_key(Some(Key::M), true, false, NONE, true),
            Route::Consume
        );
        assert!(r.ui_open());
        assert!(
            r.take_events()
                .iter()
                .any(|e| matches!(e, Event::Key { key: Key::M, .. }))
        );
        r.set_text_focus(false);
        r.route_key(Some(Key::M), true, false, NONE, true);
        assert!(!r.ui_open());
    }

    #[test]
    fn hotkey_presses_whose_release_never_came_are_forgotten() {
        let mut r = router();
        assert_eq!(key(&mut r, Key::Z, true, false, true), Delivery::Consume);
        assert!(r.zoom_active());
        let z = raw(Key::Z).unwrap();
        assert_eq!(r.held_presses(), vec![z]);
        // The hotkey's own character goes, but only that one.
        assert_eq!(r.text("z"), Route::Consume);
        assert_eq!(r.text("t"), Route::Forward);
        r.forget_press(z);
        assert!(!r.zoom_active());
        assert!(r.held_presses().is_empty());
    }

    #[test]
    fn the_release_of_a_captured_key_stays_out_of_the_game() {
        let mut r = router();
        ctrl_i(&mut r);
        r.key(Some(Key::I), raw(Key::I), false, false, NONE, true, false);
        // F3 for a rule: its release alone would toggle the game's debug screen.
        r.start_input_capture();
        assert_eq!(key(&mut r, Key::F3, true, false, true), Delivery::Consume);
        assert_eq!(r.take_captured(), Some(Captured::Input(F3)));
        assert_eq!(key(&mut r, Key::F3, true, true, true), Delivery::Consume);
        assert_eq!(key(&mut r, Key::F3, false, false, true), Delivery::Consume);
        // Also after the menu closed in between.
        r.start_capture();
        assert_eq!(key(&mut r, Key::F6, true, false, true), Delivery::Consume);
        assert!(matches!(r.take_captured(), Some(Captured::Hotkey(_))));
        ctrl_i(&mut r);
        assert!(!r.ui_open());
        assert_eq!(key(&mut r, Key::F6, false, false, true), Delivery::Consume);
        assert_eq!(key(&mut r, Key::F6, true, false, true), Delivery::Forward);
        assert_eq!(key(&mut r, Key::F6, false, false, true), Delivery::Forward);
        // A release that never came is not waited for past the next press.
        ctrl_i(&mut r);
        r.start_input_capture();
        assert_eq!(mouse4(&mut r, true), Delivery::Consume);
        assert_eq!(r.take_captured(), Some(Captured::Input(MOUSE4)));
        ctrl_i(&mut r);
        assert_eq!(mouse4(&mut r, true), Delivery::Forward);
        assert_eq!(mouse4(&mut r, false), Delivery::Forward);
    }

    #[test]
    fn a_captured_key_held_past_the_menu_never_half_reaches_the_game() {
        // W captured and still held when the menu closes: its auto-repeat must not reach the
        // game either, or the game would get W down without the release.
        let mut r = router();
        ctrl_i(&mut r);
        r.start_input_capture();
        assert_eq!(key(&mut r, Key::W, true, false, true), Delivery::Consume);
        assert_eq!(
            r.take_captured(),
            Some(Captured::Input(raw(Key::W).unwrap()))
        );
        r.set_ui_open(false);
        assert_eq!(key(&mut r, Key::W, true, true, true), Delivery::Consume);
        assert_eq!(key(&mut r, Key::W, false, false, true), Delivery::Consume);
        assert_eq!(key(&mut r, Key::W, true, false, true), Delivery::Forward);
        assert_eq!(key(&mut r, Key::W, false, false, true), Delivery::Forward);

        // F3 still held while its target is captured: both releases stay out.
        ctrl_i(&mut r);
        r.start_input_capture();
        assert_eq!(key(&mut r, Key::F3, true, false, true), Delivery::Consume);
        r.take_captured();
        r.start_input_capture();
        assert_eq!(key(&mut r, Key::B, true, false, true), Delivery::Consume);
        r.take_captured();
        assert_eq!(key(&mut r, Key::B, false, false, true), Delivery::Consume);
        assert_eq!(key(&mut r, Key::F3, false, false, true), Delivery::Consume);
    }

    #[test]
    fn a_release_of_a_key_a_rule_holds_waits_for_the_rule() {
        // CapsLock → left Ctrl held, and Ctrl+I twice: the menu took the second Ctrl press.
        const CAPS: InputId = InputId::Key(57);
        let mut r = router();
        r.set_rebinds(vec![(CAPS, LCTRL)], true);
        let caps = |r: &mut InputRouter, pressed| {
            r.key(None, Some(CAPS), pressed, false, NONE, true, false)
        };
        let ctrl = |r: &mut InputRouter, pressed| {
            r.key(None, Some(LCTRL), pressed, false, CTRL, true, false)
        };
        assert_eq!(caps(&mut r, true), send(LCTRL, Phase::Press));
        assert_eq!(
            ctrl(&mut r, true),
            Delivery::Consume,
            "shares the rule's press"
        );
        assert_eq!(ctrl_i(&mut r), Route::Consume);
        assert!(r.ui_open());
        assert_eq!(ctrl(&mut r, false), Delivery::Consume);
        assert_eq!(ctrl(&mut r, true), Delivery::Consume, "the menu's");
        assert_eq!(ctrl_i(&mut r), Route::Consume);
        assert!(!r.ui_open());
        // The game keeps left Ctrl down while CapsLock is held.
        assert_eq!(ctrl(&mut r, false), Delivery::Consume);
        assert!(r.rebind_state().holds(LCTRL));
        assert_eq!(caps(&mut r, false), send(LCTRL, Phase::Release));
        assert_eq!(r.rebind_state(), RebindState::default());
    }
}
