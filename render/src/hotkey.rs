//! Hotkeys the user can change: a key or an extra mouse button, with modifiers.
//!
//! Stored as text in the settings file (`"Ctrl+I"`, `"Z"`, `"Mouse4"`), using egui's key
//! names so the text round-trips.

use std::fmt;

use egui::{Key, Modifiers, PointerButton};
use reminedog_core::InputId;

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
    pub const DEFAULT_BROWSER_PAGE_UP: Hotkey = Hotkey::plain(Trigger::Key(Key::PageUp));
    pub const DEFAULT_BROWSER_PAGE_DOWN: Hotkey = Hotkey::plain(Trigger::Key(Key::PageDown));
    pub const DEFAULT_BROWSER_PLAY_PAUSE: Hotkey = Hotkey::plain(Trigger::Key(Key::ArrowDown));
    pub const DEFAULT_BROWSER_SEEK_BACK: Hotkey = Hotkey::plain(Trigger::Key(Key::ArrowLeft));
    pub const DEFAULT_BROWSER_SEEK_FORWARD: Hotkey = Hotkey::plain(Trigger::Key(Key::ArrowRight));

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
            Trigger::Key(key) => text.push_str(key_symbol(key)),
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

/// The trigger a key or mouse button sets off, as the platform hooks name keys with a US
/// layout: the keypad's digits, Enter, `+`, `-`, `.`, `/` and `=` are the main keys' (so a
/// hotkey on 1 takes keypad 1 too). `None` for keys no hotkey can be on.
pub(crate) fn input_trigger(id: InputId) -> Option<Trigger> {
    const LETTERS: [Key; 26] = [
        Key::A,
        Key::B,
        Key::C,
        Key::D,
        Key::E,
        Key::F,
        Key::G,
        Key::H,
        Key::I,
        Key::J,
        Key::K,
        Key::L,
        Key::M,
        Key::N,
        Key::O,
        Key::P,
        Key::Q,
        Key::R,
        Key::S,
        Key::T,
        Key::U,
        Key::V,
        Key::W,
        Key::X,
        Key::Y,
        Key::Z,
    ];
    const DIGITS: [Key; 10] = [
        Key::Num0,
        Key::Num1,
        Key::Num2,
        Key::Num3,
        Key::Num4,
        Key::Num5,
        Key::Num6,
        Key::Num7,
        Key::Num8,
        Key::Num9,
    ];
    const FUNCTION_KEYS: [Key; 24] = [
        Key::F1,
        Key::F2,
        Key::F3,
        Key::F4,
        Key::F5,
        Key::F6,
        Key::F7,
        Key::F8,
        Key::F9,
        Key::F10,
        Key::F11,
        Key::F12,
        Key::F13,
        Key::F14,
        Key::F15,
        Key::F16,
        Key::F17,
        Key::F18,
        Key::F19,
        Key::F20,
        Key::F21,
        Key::F22,
        Key::F23,
        Key::F24,
    ];
    let sc = match id {
        InputId::Key(sc) => usize::from(sc),
        InputId::Mouse(button) => {
            return match button {
                2 => Some(Trigger::Mouse(PointerButton::Middle)),
                4 => Some(Trigger::Mouse(PointerButton::Extra1)),
                5 => Some(Trigger::Mouse(PointerButton::Extra2)),
                _ => None,
            };
        }
    };
    let key = match sc {
        4..=29 => LETTERS[sc - 4],
        30..=38 => DIGITS[sc - 29],
        39 | 98 => Key::Num0,
        89..=97 => DIGITS[sc - 88],
        58..=69 => FUNCTION_KEYS[sc - 58],
        104..=115 => FUNCTION_KEYS[sc - 92],
        40 | 88 => Key::Enter,
        41 => Key::Escape,
        42 => Key::Backspace,
        43 => Key::Tab,
        44 => Key::Space,
        45 | 86 => Key::Minus,
        46 | 103 => Key::Equals,
        47 => Key::OpenBracket,
        48 => Key::CloseBracket,
        49 => Key::Backslash,
        51 => Key::Semicolon,
        52 => Key::Quote,
        53 => Key::Backtick,
        54 => Key::Comma,
        55 | 99 => Key::Period,
        56 | 84 => Key::Slash,
        73 => Key::Insert,
        74 => Key::Home,
        75 => Key::PageUp,
        76 => Key::Delete,
        77 => Key::End,
        78 => Key::PageDown,
        79 => Key::ArrowRight,
        80 => Key::ArrowLeft,
        81 => Key::ArrowDown,
        82 => Key::ArrowUp,
        87 => Key::Plus,
        _ => return None,
    };
    Some(Trigger::Key(key))
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

/// A key as the menu shows it, with the same arrows and minus as the rest of the menu (the
/// game's key names, [`reminedog_core::input_label`]).
fn key_symbol(key: Key) -> &'static str {
    match key {
        Key::ArrowUp => "↑",
        Key::ArrowDown => "↓",
        Key::ArrowLeft => "←",
        Key::ArrowRight => "→",
        Key::Minus => "-",
        _ => key.symbol_or_name(),
    }
}

/// Whether a key types text without Ctrl or Alt: letters, digits, punctuation, Space, Enter,
/// Backspace and Tab.
pub(crate) fn types_text(key: Key) -> bool {
    use Key::*;
    matches!(
        key,
        A | B
            | C
            | D
            | E
            | F
            | G
            | H
            | I
            | J
            | K
            | L
            | M
            | N
            | O
            | P
            | Q
            | R
            | S
            | T
            | U
            | V
            | W
            | X
            | Y
            | Z
            | Num0
            | Num1
            | Num2
            | Num3
            | Num4
            | Num5
            | Num6
            | Num7
            | Num8
            | Num9
            | Colon
            | Comma
            | Backslash
            | Slash
            | Pipe
            | Questionmark
            | Exclamationmark
            | OpenBracket
            | CloseBracket
            | OpenCurlyBracket
            | CloseCurlyBracket
            | Backtick
            | Minus
            | Period
            | Plus
            | Equals
            | Semicolon
            | Quote
            | IntlBackslash
            | Space
            | Enter
            | Backspace
            | Tab
    )
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
    fn triggers_of_keys_and_buttons() {
        let trigger = input_trigger;
        assert_eq!(trigger(InputId::Key(4)), Some(Trigger::Key(Key::A)));
        assert_eq!(trigger(InputId::Key(29)), Some(Trigger::Key(Key::Z)));
        assert_eq!(trigger(InputId::Key(30)), Some(Trigger::Key(Key::Num1)));
        assert_eq!(trigger(InputId::Key(39)), Some(Trigger::Key(Key::Num0)));
        // The keypad's digits are the main digits' to the hooks.
        assert_eq!(trigger(InputId::Key(89)), Some(Trigger::Key(Key::Num1)));
        assert_eq!(trigger(InputId::Key(98)), Some(Trigger::Key(Key::Num0)));
        assert_eq!(trigger(InputId::Key(88)), Some(Trigger::Key(Key::Enter)));
        assert_eq!(trigger(InputId::Key(60)), Some(Trigger::Key(Key::F3)));
        assert_eq!(trigger(InputId::Key(115)), Some(Trigger::Key(Key::F24)));
        assert_eq!(
            trigger(InputId::Key(80)),
            Some(Trigger::Key(Key::ArrowLeft))
        );
        // Modifier keys and keys the hooks have no egui name for.
        assert_eq!(trigger(InputId::Key(224)), None);
        assert_eq!(trigger(InputId::Key(57)), None);
        assert_eq!(trigger(InputId::Key(135)), None);
        assert_eq!(
            trigger(InputId::Mouse(4)),
            Some(Trigger::Mouse(PointerButton::Extra1))
        );
        assert_eq!(
            trigger(InputId::Mouse(2)),
            Some(Trigger::Mouse(PointerButton::Middle))
        );
        assert_eq!(trigger(InputId::Mouse(1)), None, "the UI's own buttons");
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
