//! Hotkeys the user can change: a key or an extra mouse button, with modifiers.
//!
//! Stored as text in the settings file (`"Ctrl+I"`, `"Z"`, `"Mouse4"`), using egui's key
//! names so the text round-trips.

use std::fmt;

use egui::{Key, Modifiers, PointerButton};

/// What is pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Key(Key),
    /// Middle or an extra button; the left and right buttons are the UI's own.
    Mouse(PointerButton),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hotkey {
    pub trigger: Trigger,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Hotkey {
    pub const DEFAULT_MENU: Hotkey = Hotkey {
        trigger: Trigger::Key(Key::I),
        ctrl: true,
        shift: false,
        alt: false,
    };
    pub const DEFAULT_ZOOM: Hotkey = Hotkey::plain(Trigger::Key(Key::Z));
    pub const DEFAULT_WAYPOINT: Hotkey = Hotkey::plain(Trigger::Key(Key::J));
    pub const DEFAULT_NAVIGATE: Hotkey = Hotkey::plain(Trigger::Key(Key::K));

    pub const fn plain(trigger: Trigger) -> Self {
        Self {
            trigger,
            ctrl: false,
            shift: false,
            alt: false,
        }
    }

    /// `trigger` with the modifiers held in `modifiers`.
    pub fn with_modifiers(trigger: Trigger, modifiers: Modifiers) -> Self {
        Self {
            trigger,
            ctrl: modifiers.ctrl,
            shift: modifiers.shift,
            alt: modifiers.alt,
        }
    }

    /// Whether a key press matches: the hotkey's modifiers must be held; others may be too
    /// (sprinting with Ctrl held still zooms).
    pub fn matches_key(&self, key: Option<Key>, modifiers: Modifiers) -> bool {
        key.is_some_and(|k| self.trigger == Trigger::Key(k)) && self.modifiers_held(modifiers)
    }

    pub fn matches_button(&self, button: PointerButton, modifiers: Modifiers) -> bool {
        self.trigger == Trigger::Mouse(button) && self.modifiers_held(modifiers)
    }

    pub(crate) fn modifiers_held(&self, m: Modifiers) -> bool {
        (!self.ctrl || m.ctrl) && (!self.shift || m.shift) && (!self.alt || m.alt)
    }

    /// Parses the settings-file form, e.g. `"Ctrl+Shift+M"`, `"F6"`, `"Mouse4"`.
    pub fn parse(text: &str) -> Option<Hotkey> {
        let mut parts: Vec<&str> = text.split('+').map(str::trim).collect();
        let last = parts.pop()?;
        let mut hotkey = Hotkey::plain(trigger_from_name(last)?);
        for part in parts {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => hotkey.ctrl = true,
                "shift" => hotkey.shift = true,
                "alt" => hotkey.alt = true,
                _ => return None,
            }
        }
        Some(hotkey)
    }

    /// For the menu: the mouse buttons in Japanese, keys as on the keyboard.
    pub fn label(&self) -> String {
        let mut text = self.modifier_prefix();
        match self.trigger {
            Trigger::Key(key) => text.push_str(key.symbol_or_name()),
            Trigger::Mouse(button) => text.push_str(match button {
                PointerButton::Middle => "ホイールクリック",
                PointerButton::Extra1 => "マウスのボタン4",
                PointerButton::Extra2 => "マウスのボタン5",
                PointerButton::Primary => "左クリック",
                PointerButton::Secondary => "右クリック",
            }),
        }
        text
    }

    fn modifier_prefix(&self) -> String {
        let mut text = String::new();
        for (held, name) in [
            (self.ctrl, "Ctrl+"),
            (self.shift, "Shift+"),
            (self.alt, "Alt+"),
        ] {
            if held {
                text.push_str(name);
            }
        }
        text
    }
}

/// The settings-file form.
impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.modifier_prefix())?;
        match self.trigger {
            Trigger::Key(key) => f.write_str(key.name()),
            Trigger::Mouse(button) => f.write_str(match button {
                PointerButton::Middle => "MouseMiddle",
                PointerButton::Extra1 => "Mouse4",
                PointerButton::Extra2 => "Mouse5",
                PointerButton::Primary => "MouseLeft",
                PointerButton::Secondary => "MouseRight",
            }),
        }
    }
}

fn trigger_from_name(name: &str) -> Option<Trigger> {
    let button = match name.to_ascii_lowercase().as_str() {
        "mousemiddle" | "mouse3" => Some(PointerButton::Middle),
        "mouse4" => Some(PointerButton::Extra1),
        "mouse5" => Some(PointerButton::Extra2),
        _ => None,
    };
    if let Some(button) = button {
        return Some(Trigger::Mouse(button));
    }
    let key = Key::from_name(name).or_else(|| {
        // Letters in either case ("z").
        (name.len() == 1)
            .then(|| Key::from_name(&name.to_ascii_uppercase()))
            .flatten()
    })?;
    // Escape closes the menu and cancels key capture; modifier keys cannot be hotkeys alone.
    let reserved = matches!(
        key,
        Key::Escape
            | Key::ShiftLeft
            | Key::ShiftRight
            | Key::ControlLeft
            | Key::ControlRight
            | Key::AltLeft
            | Key::AltRight
            | Key::SuperLeft
            | Key::SuperRight
    );
    (!reserved).then_some(Trigger::Key(key))
}

/// Whether a key or button can be assigned (for key capture).
pub fn assignable(trigger: Trigger) -> bool {
    match trigger {
        Trigger::Key(key) => trigger_from_name(key.name()) == Some(trigger),
        Trigger::Mouse(button) => matches!(
            button,
            PointerButton::Middle | PointerButton::Extra1 | PointerButton::Extra2
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip() {
        assert_eq!(Hotkey::DEFAULT_MENU.to_string(), "Ctrl+I");
        assert_eq!(Hotkey::parse("Ctrl+I"), Some(Hotkey::DEFAULT_MENU));
        assert_eq!(Hotkey::DEFAULT_ZOOM.to_string(), "Z");
        assert_eq!(Hotkey::parse("Z"), Some(Hotkey::DEFAULT_ZOOM));
        assert_eq!(Hotkey::parse("J"), Some(Hotkey::DEFAULT_WAYPOINT));
        assert_eq!(Hotkey::parse("K"), Some(Hotkey::DEFAULT_NAVIGATE));
    }

    #[test]
    fn every_assignable_key_round_trips() {
        for &key in Key::ALL {
            let trigger = Trigger::Key(key);
            if !assignable(trigger) {
                continue;
            }
            let hotkey = Hotkey {
                trigger,
                ctrl: true,
                shift: true,
                alt: true,
            };
            assert_eq!(Hotkey::parse(&hotkey.to_string()), Some(hotkey), "{key:?}");
        }
    }

    #[test]
    fn parse_is_lenient_about_case_and_spaces() {
        let hotkey = Hotkey::parse(" control + shift + m ").unwrap();
        assert_eq!(hotkey.trigger, Trigger::Key(Key::M));
        assert!(hotkey.ctrl && hotkey.shift && !hotkey.alt);
        assert_eq!(
            Hotkey::parse("mouse4"),
            Some(Hotkey::plain(Trigger::Mouse(PointerButton::Extra1)))
        );
        assert_eq!(
            Hotkey::parse("Alt+Mouse5").unwrap().to_string(),
            "Alt+Mouse5"
        );
    }

    #[test]
    fn parse_rejects_nonsense_and_reserved_keys() {
        for text in [
            "",
            "Ctrl+",
            "Hyper+I",
            "Escape",
            "Ctrl+Esc",
            "MouseLeft",
            "NoSuchKey",
        ] {
            assert_eq!(Hotkey::parse(text), None, "{text:?}");
        }
        assert!(!assignable(Trigger::Key(Key::Escape)));
        assert!(!assignable(Trigger::Mouse(PointerButton::Primary)));
        assert!(assignable(Trigger::Mouse(PointerButton::Extra2)));
    }

    #[test]
    fn extra_modifiers_still_match() {
        let zoom = Hotkey::DEFAULT_ZOOM;
        assert!(zoom.matches_key(Some(Key::Z), Modifiers::NONE));
        assert!(zoom.matches_key(Some(Key::Z), Modifiers::CTRL));
        assert!(!zoom.matches_key(Some(Key::X), Modifiers::NONE));
        assert!(!zoom.matches_key(None, Modifiers::NONE));
        let menu = Hotkey::DEFAULT_MENU;
        assert!(menu.matches_key(Some(Key::I), Modifiers::CTRL));
        assert!(!menu.matches_key(Some(Key::I), Modifiers::NONE));
        assert!(!menu.matches_key(Some(Key::I), Modifiers::SHIFT));
    }

    #[test]
    fn labels() {
        assert_eq!(Hotkey::DEFAULT_MENU.label(), "Ctrl+I");
        assert_eq!(
            Hotkey::parse("Shift+Mouse4").unwrap().label(),
            "Shift+マウスのボタン4"
        );
    }
}
