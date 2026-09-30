//! Minecraft's key bindings from `<game_dir>/options.txt`.
//!
//! F3+C is sent as key events for the debug modifier and copy-location mappings. Recent
//! versions (1.21.11, 26.x) let the user rebind both and store them as
//! `key_key.debug.modifier:key.keyboard.f3`; older ones (1.16) have no such lines and hard-code
//! F3 and C, which are also the defaults here. Only the keys a debug binding is likely to use
//! (letters, digits, F-keys, keypad digits) are mapped to GLFW key codes and SDL scancodes and
//! keycodes.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The key name options.txt stores for a mapping without a key.
pub const UNBOUND: &str = "key.keyboard.unknown";

const MODIFIER_ID: &str = "key.debug.modifier";
const COPY_LOCATION_ID: &str = "key.debug.copyLocation";
const CRASH_ID: &str = "key.debug.crash";
/// Mappings the game only reads as "held" during its tick. A refused F3+C presses and
/// releases the copy key within one poll, so these never see it.
const HELD_ONLY_IDS: [&str; 8] = [
    "key.forward",
    "key.left",
    "key.back",
    "key.right",
    "key.jump",
    "key.playerlist",
    "key.saveToolbarActivator",
    "key.loadToolbarActivator",
];
/// Drops the held item on a press; with Ctrl physically held, the whole stack.
const DROP_ID: &str = "key.drop";

/// The keys F3+C is made of, as the key names Minecraft stores in options.txt, e.g.
/// "key.keyboard.f3".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DebugKeys {
    pub modifier: String,
    pub copy_location: String,
    pub crash: String,
    /// Other (non-debug) mappings bound to the copy-location key that act on a press, by id
    /// (e.g. "key.drop"). A refused F3+C presses each of them once.
    pub shared_with_copy: Vec<String>,
}

impl DebugKeys {
    /// Whether a refused F3+C drops the held item (a whole stack with Ctrl held).
    pub fn copy_drops_items(&self) -> bool {
        self.shared_with_copy.iter().any(|id| id == DROP_ID)
    }
}

impl Default for DebugKeys {
    fn default() -> Self {
        Self {
            modifier: "key.keyboard.f3".into(),
            copy_location: "key.keyboard.c".into(),
            crash: "key.keyboard.c".into(),
            shared_with_copy: Vec::new(),
        }
    }
}

/// Reads the debug keys from the text of options.txt.
///
/// Every `key_<id>:<name>` line is read, and the last one wins for a repeated id. A missing
/// debug line keeps its default (1.16 has none). CRLF, a BOM and whitespace around the parts
/// are tolerated. `shared_with_copy` lists, in file order, the mappings outside `key.debug.*`
/// bound to the copy-location key, except those only read as held; an unbound copy-location
/// key shares nothing.
pub fn parse_debug_keys(options_txt: &str) -> DebugKeys {
    let text = options_txt.strip_prefix('\u{feff}').unwrap_or(options_txt);
    let mut bindings: Vec<(&str, &str)> = Vec::new();
    for line in text.lines() {
        let Some((id, name)) = line
            .trim()
            .strip_prefix("key_")
            .and_then(|rest| rest.split_once(':'))
        else {
            continue;
        };
        let (id, name) = (id.trim(), name.trim());
        if id.is_empty() || name.is_empty() {
            continue;
        }
        match bindings.iter_mut().find(|(i, _)| *i == id) {
            Some(binding) => binding.1 = name,
            None => bindings.push((id, name)),
        }
    }
    let bound = |id: &str| {
        bindings
            .iter()
            .find(|(i, _)| *i == id)
            .map(|(_, name)| (*name).to_owned())
    };
    let defaults = DebugKeys::default();
    let copy_location = bound(COPY_LOCATION_ID).unwrap_or(defaults.copy_location);
    let shared_with_copy = if copy_location == UNBOUND {
        Vec::new()
    } else {
        bindings
            .iter()
            .filter(|(id, name)| {
                !id.starts_with("key.debug.")
                    && !HELD_ONLY_IDS.contains(id)
                    && *name == copy_location
            })
            .map(|(id, _)| (*id).to_owned())
            .collect()
    };
    DebugKeys {
        modifier: bound(MODIFIER_ID).unwrap_or(defaults.modifier),
        copy_location,
        crash: bound(CRASH_ID).unwrap_or(defaults.crash),
        shared_with_copy,
    }
}

/// `<game_dir>/options.txt`: the game's options, including the key bindings.
pub fn options_path(game_dir: &Path) -> PathBuf {
    game_dir.join("options.txt")
}

/// Reads the debug keys from `<game_dir>/options.txt`. A missing file (a fresh instance) gives
/// the defaults; text that is not UTF-8 is read lossily.
pub fn load_debug_keys(game_dir: &Path) -> io::Result<DebugKeys> {
    match fs::read(options_path(game_dir)) {
        Ok(bytes) => Ok(parse_debug_keys(&String::from_utf8_lossy(&bytes))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(DebugKeys::default()),
        Err(e) => Err(e),
    }
}

/// The keyboard keys this module knows, from their options.txt names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyName {
    /// `a`..`z` as 0..26.
    Letter(u8),
    Digit(u8),
    /// F1..F25 as 1..=25.
    F(u8),
    KeypadDigit(u8),
}

fn key_name(name: &str) -> Option<KeyName> {
    let rest = name.strip_prefix("key.keyboard.")?;
    if let Some(digit) = rest.strip_prefix("keypad.") {
        return match digit.as_bytes() {
            [d @ b'0'..=b'9'] => Some(KeyName::KeypadDigit(d - b'0')),
            _ => None,
        };
    }
    match rest.as_bytes() {
        [c @ b'a'..=b'z'] => Some(KeyName::Letter(c - b'a')),
        [d @ b'0'..=b'9'] => Some(KeyName::Digit(d - b'0')),
        // "f1".."f25", without leading zeros.
        [b'f', digits @ ..]
            if (1..=2).contains(&digits.len())
                && digits[0] != b'0'
                && digits.iter().all(u8::is_ascii_digit) =>
        {
            let n = digits.iter().fold(0u8, |n, d| n * 10 + (d - b'0'));
            (n <= 25).then_some(KeyName::F(n))
        }
        _ => None,
    }
}

/// The GLFW key code of a key name (what 1.21.11 matches bindings on).
pub fn glfw_key(name: &str) -> Option<i32> {
    Some(match key_name(name)? {
        KeyName::Letter(i) => 65 + i32::from(i),
        KeyName::Digit(d) => 48 + i32::from(d),
        KeyName::F(n) => 289 + i32::from(n),
        KeyName::KeypadDigit(d) => 320 + i32::from(d),
    })
}

/// The SDL3 scancode of a key name (what 26.x matches bindings on). SDL has no F25.
pub fn sdl_scancode(name: &str) -> Option<u32> {
    Some(match key_name(name)? {
        KeyName::Letter(i) => 4 + u32::from(i),
        // The digit row runs 1..9, then 0.
        KeyName::Digit(0) => 39,
        KeyName::Digit(d) => 29 + u32::from(d),
        KeyName::F(n @ 1..=12) => 57 + u32::from(n),
        KeyName::F(n @ 13..=24) => 91 + u32::from(n),
        KeyName::F(_) => return None,
        KeyName::KeypadDigit(0) => 98,
        KeyName::KeypadDigit(d) => 88 + u32::from(d),
    })
}

/// The SDL3 keycode of a key name, for the `key` field of synthetic key events.
pub fn sdl_keycode(name: &str) -> Option<u32> {
    /// SDLK_SCANCODE_MASK: keys without a character use their scancode with this bit set.
    const SCANCODE_MASK: u32 = 1 << 30;
    match key_name(name)? {
        KeyName::Letter(i) => Some(u32::from(b'a' + i)),
        KeyName::Digit(d) => Some(u32::from(b'0' + d)),
        KeyName::F(_) | KeyName::KeypadDigit(_) => {
            sdl_scancode(name).map(|code| code | SCANCODE_MASK)
        }
    }
}

/// A short label for a key name, e.g. "F3", "C", "テンキー 1"; other names are shown as they are.
pub fn key_label(name: &str) -> String {
    match key_name(name) {
        Some(KeyName::Letter(i)) => char::from(b'A' + i).to_string(),
        Some(KeyName::Digit(d)) => d.to_string(),
        Some(KeyName::F(n)) => format!("F{n}"),
        Some(KeyName::KeypadDigit(d)) => format!("テンキー {d}"),
        None => name.to_owned(),
    }
}

/// The Japanese name of a vanilla mapping id such as "key.drop"; other ids are returned as
/// they are.
pub fn mapping_label(id: &str) -> String {
    let label = match id {
        "key.forward" => "前進",
        "key.left" => "左",
        "key.back" => "後退",
        "key.right" => "右",
        "key.jump" => "ジャンプ",
        "key.sneak" => "スニーク",
        "key.sprint" => "ダッシュ",
        "key.inventory" => "インベントリの開閉",
        "key.swapOffhand" => "アイテムをオフハンドと交換",
        "key.drop" => "アイテムを捨てる",
        "key.use" => "アイテムの使用／ブロックの設置",
        "key.attack" => "攻撃する／壊す",
        "key.pickItem" => "ブロック選択",
        "key.chat" => "チャットを開く",
        "key.playerlist" => "プレイヤーリストの表示",
        "key.command" => "コマンドラインを開く",
        "key.socialInteractions" => "社交設定画面",
        "key.screenshot" => "スクリーンショットの撮影",
        "key.togglePerspective" => "視点の切り替え",
        "key.smoothCamera" => "滑らかなカメラ動作の切り替え",
        "key.fullscreen" => "フルスクリーンの切り替え",
        "key.spectatorOutlines" => "プレイヤーの強調表示",
        "key.advancements" => "進捗",
        "key.saveToolbarActivator" => "ホットバーの保存",
        "key.loadToolbarActivator" => "ホットバーの読み込み",
        _ => match id.strip_prefix("key.hotbar.") {
            Some(n @ ("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")) => {
                return format!("ホットバースロット{n}");
            }
            _ => id,
        },
    };
    label.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(modifier: &str, copy: &str, crash: &str, shared: &[&str]) -> DebugKeys {
        DebugKeys {
            modifier: format!("key.keyboard.{modifier}"),
            copy_location: format!("key.keyboard.{copy}"),
            crash: format!("key.keyboard.{crash}"),
            shared_with_copy: shared.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn defaults_are_f3_and_c() {
        assert_eq!(DebugKeys::default(), keys("f3", "c", "c", &[]));
        assert_eq!(parse_debug_keys(""), DebugKeys::default());
        assert_eq!(
            parse_debug_keys("version:3953\nlang:ja_jp\nresourcePacks:[\"vanilla\"]\n"),
            DebugKeys::default()
        );
    }

    #[test]
    fn default_bindings_of_recent_versions() {
        // The shape of the user's 26.3 options.txt: the debug keys at their defaults,
        // "drop" moved to C and the toolbar activator moved away from it.
        let text = "version:5023\n\
                    key_key.attack:key.mouse.left\n\
                    key_key.drop:key.keyboard.c\n\
                    key_key.saveToolbarActivator:key.keyboard.9\n\
                    key_key.hotbar.1:key.keyboard.1\n\
                    key_key.debug.overlay:key.keyboard.f3\n\
                    key_key.debug.modifier:key.keyboard.f3\n\
                    key_key.debug.crash:key.keyboard.c\n\
                    key_key.debug.copyLocation:key.keyboard.c\n\
                    key_key.debug.copyRecreateCommand:key.keyboard.i\n\
                    soundCategory_master:1.0\n";
        assert_eq!(parse_debug_keys(text), keys("f3", "c", "c", &["key.drop"]));
    }

    #[test]
    fn old_versions_without_debug_lines_compare_against_c() {
        // 1.16.1 hard-codes F3 and C and writes no debug lines.
        let text = "key_key.drop:key.keyboard.c\nkey_key.inventory:key.keyboard.e\n\
                    key_key.saveToolbarActivator:key.keyboard.c\n";
        // The hotbar saver on C is only read as held, so a refused F3+C cannot trigger it.
        let parsed = parse_debug_keys(text);
        assert_eq!(parsed, keys("f3", "c", "c", &["key.drop"]));
        assert!(parsed.copy_drops_items());
        let held_only = parse_debug_keys(
            "key_key.saveToolbarActivator:key.keyboard.c\nkey_key.jump:key.keyboard.c\n",
        );
        assert!(held_only.shared_with_copy.is_empty());
        assert!(!held_only.copy_drops_items());
    }

    #[test]
    fn rebound_debug_keys() {
        let text = "key_key.debug.modifier:key.keyboard.f4\n\
                    key_key.debug.copyLocation:key.keyboard.x\n\
                    key_key.debug.crash:key.keyboard.keypad.5\n\
                    key_key.drop:key.keyboard.c\n\
                    key_key.inventory:key.keyboard.x\n\
                    key_key.debug.overlay:key.keyboard.x\n\
                    key_modid.custom_action:key.keyboard.x\n";
        let parsed = parse_debug_keys(text);
        assert_eq!(parsed.modifier, "key.keyboard.f4");
        assert_eq!(parsed.copy_location, "key.keyboard.x");
        assert_eq!(parsed.crash, "key.keyboard.keypad.5");
        // Debug mappings are left out; modded ones are listed by id.
        assert_eq!(
            parsed.shared_with_copy,
            ["key.inventory", "modid.custom_action"]
        );
    }

    #[test]
    fn unbound_keys() {
        let text = "key_key.debug.copyLocation:key.keyboard.unknown\n\
                    key_key.debug.crash:key.keyboard.unknown\n\
                    key_key.smoothCamera:key.keyboard.unknown\n\
                    key_key.spectatorOutlines:key.keyboard.unknown\n";
        let parsed = parse_debug_keys(text);
        assert_eq!(parsed.modifier, "key.keyboard.f3");
        assert_eq!(parsed.copy_location, UNBOUND);
        assert_eq!(parsed.crash, UNBOUND);
        // Other unbound mappings do not "share" the missing key.
        assert!(parsed.shared_with_copy.is_empty());
    }

    #[test]
    fn crlf_bom_and_whitespace() {
        let text = "\u{feff}key_key.debug.modifier:key.keyboard.f6\r\n\
                    \r\n\
                    \x20 key_key.drop : key.keyboard.v \r\n\
                    \tkey_key.debug.copyLocation:key.keyboard.v\r\n";
        assert_eq!(parse_debug_keys(text), keys("f6", "v", "c", &["key.drop"]));
        // A BOM right before the first key line.
        assert_eq!(
            parse_debug_keys("\u{feff}key_key.debug.copyLocation:key.keyboard.b").copy_location,
            "key.keyboard.b"
        );
    }

    #[test]
    fn repeated_and_malformed_lines() {
        let text = "key_key.debug.copyLocation:key.keyboard.x\n\
                    key_key.drop:key.keyboard.c\n\
                    key_key.drop:key.keyboard.q\n\
                    key_key.debug.copyLocation:key.keyboard.q\n\
                    key_key.debug.modifier:\n\
                    key_:key.keyboard.q\n\
                    key_key.chat\n\
                    keykey.jump:key.keyboard.q\n";
        let parsed = parse_debug_keys(text);
        // The last line wins, and the mapping keeps its first position.
        assert_eq!(parsed.copy_location, "key.keyboard.q");
        assert_eq!(parsed.shared_with_copy, ["key.drop"]);
        // An empty value is ignored rather than taken as a key.
        assert_eq!(parsed.modifier, "key.keyboard.f3");
    }

    #[test]
    fn options_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(options_path(dir.path()), dir.path().join("options.txt"));
        assert_eq!(load_debug_keys(dir.path()).unwrap(), DebugKeys::default());
        fs::write(
            options_path(dir.path()),
            b"key_key.debug.modifier:key.keyboard.f7\nlang:\xff\xfe\nkey_key.drop:key.keyboard.c\n",
        )
        .unwrap();
        assert_eq!(
            load_debug_keys(dir.path()).unwrap(),
            keys("f7", "c", "c", &["key.drop"])
        );
    }

    #[test]
    fn glfw_key_codes() {
        let cases = [
            ("a", 65),
            ("c", 67),
            // The letter, not a function key.
            ("f", 70),
            ("z", 90),
            ("0", 48),
            ("9", 57),
            ("f1", 290),
            ("f3", 292),
            ("f12", 301),
            ("f25", 314),
            ("keypad.0", 320),
            ("keypad.9", 329),
        ];
        for (name, code) in cases {
            assert_eq!(
                glfw_key(&format!("key.keyboard.{name}")),
                Some(code),
                "{name}"
            );
        }
    }

    #[test]
    fn sdl_scancodes() {
        let cases = [
            ("a", 4),
            ("c", 6),
            ("z", 29),
            ("1", 30),
            ("9", 38),
            ("0", 39),
            ("f1", 58),
            ("f3", 60),
            ("f12", 69),
            ("f13", 104),
            ("f24", 115),
            ("keypad.1", 89),
            ("keypad.9", 97),
            ("keypad.0", 98),
        ];
        for (name, code) in cases {
            assert_eq!(
                sdl_scancode(&format!("key.keyboard.{name}")),
                Some(code),
                "{name}"
            );
        }
        assert_eq!(sdl_scancode("key.keyboard.f25"), None);
    }

    #[test]
    fn sdl_keycodes() {
        let cases = [
            ("a", 0x61),
            ("c", 0x63),
            ("z", 0x7a),
            ("0", 0x30),
            ("1", 0x31),
            ("9", 0x39),
            ("f1", 0x4000_003a),
            ("f3", 0x4000_003c),
            ("f12", 0x4000_0045),
            ("f13", 0x4000_0068),
            ("f24", 0x4000_0073),
            ("keypad.1", 0x4000_0059),
            ("keypad.9", 0x4000_0061),
            ("keypad.0", 0x4000_0062),
        ];
        for (name, code) in cases {
            assert_eq!(
                sdl_keycode(&format!("key.keyboard.{name}")),
                Some(code),
                "{name}"
            );
        }
        assert_eq!(sdl_keycode("key.keyboard.f25"), None);
    }

    #[test]
    fn unsupported_key_names() {
        for name in [
            UNBOUND,
            "",
            "c",
            "f3",
            "key.keyboard.",
            "key.keyboard.C",
            "key.keyboard.F3",
            "key.keyboard.ab",
            "key.keyboard.f0",
            "key.keyboard.f01",
            "key.keyboard.f26",
            "key.keyboard.f100",
            "key.keyboard.f-1",
            "key.keyboard.keypad.",
            "key.keyboard.keypad.10",
            "key.keyboard.keypad.enter",
            "key.keyboard.left.shift",
            "key.keyboard.space",
            "key.keyboard.67",
            "key.mouse.left",
            "key.mouse.4",
            " key.keyboard.c",
        ] {
            assert_eq!(glfw_key(name), None, "{name}");
            assert_eq!(sdl_scancode(name), None, "{name}");
            assert_eq!(sdl_keycode(name), None, "{name}");
        }
    }

    #[test]
    fn key_labels() {
        assert_eq!(key_label("key.keyboard.f3"), "F3");
        assert_eq!(key_label("key.keyboard.f25"), "F25");
        assert_eq!(key_label("key.keyboard.c"), "C");
        assert_eq!(key_label("key.keyboard.z"), "Z");
        assert_eq!(key_label("key.keyboard.f"), "F");
        assert_eq!(key_label("key.keyboard.0"), "0");
        assert_eq!(key_label("key.keyboard.keypad.1"), "テンキー 1");
        assert_eq!(key_label("key.mouse.left"), "key.mouse.left");
        assert_eq!(key_label(UNBOUND), UNBOUND);
    }

    #[test]
    fn mapping_labels() {
        assert_eq!(mapping_label("key.drop"), "アイテムを捨てる");
        // The names the game's own ja_jp key settings show.
        assert_eq!(mapping_label("key.attack"), "攻撃する／壊す");
        assert_eq!(
            mapping_label("key.loadToolbarActivator"),
            "ホットバーの読み込み"
        );
        assert_eq!(mapping_label("key.hotbar.1"), "ホットバースロット1");
        assert_eq!(mapping_label("key.hotbar.9"), "ホットバースロット9");
        assert_eq!(mapping_label("key.hotbar.0"), "key.hotbar.0");
        assert_eq!(mapping_label("key.hotbar.10"), "key.hotbar.10");
        assert_eq!(mapping_label("modid.custom_action"), "modid.custom_action");
    }
}
