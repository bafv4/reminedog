//! Routing of the game's input while the overlay is in use: the hotkeys, and whether each
//! event goes to the overlay (egui) or on to the game.
//!
//! Platform-neutral: the GLFW and SDL3 hooks translate their events into these calls and
//! drop or forward each one as told. While the UI is open everything goes to egui except
//! releases (keys, mouse buttons), which the game still gets so nothing stays held down.

use egui::{Event, Key, Modifiers, MouseWheelUnit, PointerButton, Pos2, TouchPhase, pos2, vec2};

/// What to do with an input event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Pass it on to the game.
    Forward,
    /// The overlay used it; the game must not see it.
    Consume,
}

/// Toggles the UI (with Ctrl).
pub const UI_KEY: Key = Key::I;
/// Zooms while held, in game (cursor captured) with the UI closed.
pub const ZOOM_KEY: Key = Key::Z;

pub struct InputRouter {
    /// Off until the overlay can draw; while off every event is forwarded, so a UI that
    /// could not be shown never swallows input.
    enabled: bool,
    ui_open: bool,
    /// UI opened or closed since the last `take_ui_changed`.
    ui_changed: bool,
    zoom_held: bool,
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
            modifiers: Modifiers::NONE,
            pointer: Pos2::ZERO,
            screen_px: [0.0, 0.0],
            pixels_per_point: 1.0,
            events: Vec::new(),
            last_cursor: None,
            last_captured: false,
            offset: (0.0, 0.0),
        }
    }

    /// Turns routing on once the overlay renders, and off (closing the UI) if it stops.
    pub fn set_enabled(&mut self, enabled: bool) {
        if !enabled {
            self.set_ui_open(false);
            self.zoom_held = false;
            self.events.clear();
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
        if pixels_per_point.is_finite() && pixels_per_point > 0.0 {
            self.pixels_per_point = pixels_per_point;
        }
    }

    /// Where to draw the overlay's own cursor (in points), when the game hides the real
    /// one: the UI is open while the cursor is captured.
    pub fn software_cursor(&self) -> Option<Pos2> {
        (self.ui_open && self.last_captured).then(|| self.pointer_points())
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
        self.zoom_held = false;
        if open {
            if self.last_captured || self.pointer == Pos2::ZERO {
                self.pointer = pos2(self.screen_px[0] / 2.0, self.screen_px[1] / 2.0);
            }
            self.push_event(Event::PointerMoved(self.pointer_points()));
        } else {
            self.push_event(Event::PointerGone);
        }
    }

    /// A key press, repeat or release. `key` is `None` for keys egui has no name for.
    /// `captured`: the game has grabbed the cursor, i.e. it is being played rather than
    /// showing a menu.
    pub fn key(
        &mut self,
        key: Option<Key>,
        pressed: bool,
        repeat: bool,
        modifiers: Modifiers,
        captured: bool,
    ) -> Route {
        self.modifiers = modifiers;
        if !self.enabled {
            return Route::Forward;
        }
        let toggle = key == Some(UI_KEY) && modifiers.ctrl && !modifiers.alt && !modifiers.shift;
        if toggle && pressed {
            if !repeat {
                self.set_ui_open(!self.ui_open);
            }
            return Route::Consume;
        }
        if self.ui_open {
            if key == Some(Key::Escape) && pressed {
                if !repeat {
                    self.set_ui_open(false);
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
            }
            return if pressed {
                Route::Consume
            } else {
                Route::Forward
            };
        }
        if key == Some(ZOOM_KEY) {
            if pressed && (self.zoom_held || (captured && modifiers.is_none())) {
                self.zoom_held = true;
                return Route::Consume;
            }
            if !pressed && self.zoom_held {
                self.zoom_held = false;
                return Route::Consume;
            }
        }
        Route::Forward
    }

    /// Typed text (after the keyboard layout and IME).
    pub fn text(&mut self, text: &str) -> Route {
        if self.zoom_held {
            // The zoom key's own characters (and their auto-repeat).
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

    pub fn button(&mut self, button: PointerButton, pressed: bool) -> Route {
        if !self.ui_open {
            return Route::Forward;
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
        if captured != self.last_captured {
            // The game re-centres the cursor and its reference point when it grabs or
            // releases the cursor, so earlier motion no longer matters.
            self.last_captured = captured;
            self.last_cursor = None;
            self.offset = (0.0, 0.0);
        }
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
        Some(if captured {
            (x - self.offset.0, y - self.offset.1)
        } else {
            (x, y)
        })
    }

    /// Relative mouse motion with the absolute position (SDL3).
    pub fn cursor_motion(&mut self, dx: f32, dy: f32, x: f32, y: f32, captured: bool) -> Route {
        self.last_captured = captured;
        if !self.ui_open {
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

    /// The window lost focus: the zoom key's release may never arrive.
    pub fn focus_lost(&mut self) {
        self.zoom_held = false;
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
        let [w, h] = self.screen_px;
        let x = (self.pointer.x + dx).clamp(0.0, (w - 1.0).max(0.0));
        let y = (self.pointer.y + dy).clamp(0.0, (h - 1.0).max(0.0));
        self.set_pointer(x, y);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL: Modifiers = Modifiers::CTRL;
    const NONE: Modifiers = Modifiers::NONE;

    fn router() -> InputRouter {
        let mut r = InputRouter::new();
        r.set_screen([800, 600], 1.0);
        r.set_enabled(true);
        r
    }

    #[test]
    fn disabled_router_forwards_everything() {
        let mut r = InputRouter::new();
        assert_eq!(r.key(Some(Key::I), true, false, CTRL, true), Route::Forward);
        assert!(!r.ui_open());
        assert_eq!(r.key(Some(Key::Z), true, false, NONE, true), Route::Forward);
        let mut r = router();
        ctrl_i(&mut r);
        r.set_enabled(false);
        assert!(!r.ui_open(), "stopping the overlay closes the UI");
        assert_eq!(r.key(Some(Key::W), true, false, NONE, true), Route::Forward);
        assert!(r.take_events().is_empty());
    }

    fn ctrl_i(r: &mut InputRouter) -> Route {
        r.key(Some(Key::I), true, false, CTRL, true)
    }

    #[test]
    fn ctrl_i_toggles_the_ui_and_is_never_forwarded() {
        let mut r = router();
        assert_eq!(ctrl_i(&mut r), Route::Consume);
        assert!(r.ui_open());
        assert_eq!(r.take_ui_changed(), Some(true));
        assert_eq!(r.take_ui_changed(), None);
        assert_eq!(
            r.key(Some(Key::I), true, true, CTRL, true),
            Route::Consume,
            "repeat"
        );
        assert!(r.ui_open(), "repeats do not toggle");
        assert_eq!(ctrl_i(&mut r), Route::Consume);
        assert!(!r.ui_open());
        assert_eq!(r.take_ui_changed(), Some(false));
        // Plain I and Ctrl+Shift+I are the game's.
        assert_eq!(r.key(Some(Key::I), true, false, NONE, true), Route::Forward);
        let ctrl_shift = Modifiers {
            shift: true,
            ..CTRL
        };
        assert_eq!(
            r.key(Some(Key::I), true, false, ctrl_shift, true),
            Route::Forward
        );
    }

    #[test]
    fn open_ui_takes_presses_but_forwards_releases() {
        let mut r = router();
        ctrl_i(&mut r);
        r.take_events();
        assert_eq!(r.key(Some(Key::W), true, false, NONE, true), Route::Consume);
        assert_eq!(
            r.key(Some(Key::W), false, false, NONE, true),
            Route::Forward
        );
        assert_eq!(
            r.key(None, true, false, NONE, true),
            Route::Consume,
            "unnamed keys too"
        );
        assert_eq!(r.button(PointerButton::Primary, true), Route::Consume);
        assert_eq!(r.button(PointerButton::Primary, false), Route::Forward);
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
            r.key(Some(Key::Escape), true, false, NONE, true),
            Route::Consume
        );
        assert!(!r.ui_open());
        // With the UI closed, Escape is the game's again.
        assert_eq!(
            r.key(Some(Key::Escape), true, false, NONE, true),
            Route::Forward
        );
    }

    #[test]
    fn closed_ui_forwards_everything_but_hotkeys() {
        let mut r = router();
        assert_eq!(r.key(Some(Key::W), true, false, NONE, true), Route::Forward);
        assert_eq!(r.text("w"), Route::Forward);
        assert_eq!(r.button(PointerButton::Primary, true), Route::Forward);
        assert_eq!(r.scroll(0.0, 1.0), Route::Forward);
        assert_eq!(r.cursor_position(10.0, 20.0, true), Some((10.0, 20.0)));
        assert!(r.take_events().is_empty());
    }

    #[test]
    fn zoom_is_held_in_game_only() {
        let mut r = router();
        assert_eq!(r.key(Some(Key::Z), true, false, NONE, true), Route::Consume);
        assert!(r.zoom_active());
        assert_eq!(r.text("z"), Route::Consume, "the key's characters too");
        assert_eq!(
            r.key(Some(Key::Z), true, true, NONE, true),
            Route::Consume,
            "repeat"
        );
        assert_eq!(
            r.key(Some(Key::Z), false, false, NONE, true),
            Route::Consume
        );
        assert!(!r.zoom_active());
        // In a menu (cursor free) or with modifiers, Z is the game's (typing in chat).
        assert_eq!(
            r.key(Some(Key::Z), true, false, NONE, false),
            Route::Forward
        );
        assert_eq!(
            r.key(Some(Key::Z), false, false, NONE, false),
            Route::Forward
        );
        assert_eq!(r.key(Some(Key::Z), true, false, CTRL, true), Route::Forward);
        assert!(!r.zoom_active());
    }

    #[test]
    fn opening_the_ui_or_losing_focus_ends_the_zoom() {
        let mut r = router();
        r.key(Some(Key::Z), true, false, NONE, true);
        ctrl_i(&mut r);
        assert!(!r.zoom_active());
        ctrl_i(&mut r);
        assert!(!r.zoom_active(), "stays off after closing");
        r.key(Some(Key::Z), true, false, NONE, true);
        r.focus_lost();
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
    fn relative_motion_moves_the_software_cursor() {
        let mut r = router();
        assert_eq!(r.cursor_motion(5.0, 5.0, 0.0, 0.0, true), Route::Forward);
        ctrl_i(&mut r);
        assert_eq!(r.cursor_motion(10.0, -20.0, 0.0, 0.0, true), Route::Consume);
        assert_eq!(r.software_cursor(), Some(pos2(410.0, 280.0)));
    }
}
